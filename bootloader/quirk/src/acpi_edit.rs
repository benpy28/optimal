//! ACPI quirks: the firmware's own tables, broken in RAM the ways real
//! firmware ships them broken, so an OS can be tested on exactly the bytes
//! it reads at boot.
//!
//! [`plan`] runs in `efi_main` (boot services up): it finds every table a
//! requested word needs and checks the word can really be applied on this
//! machine, so a missing table stops the boot instead of quietly running a
//! weaker test. [`apply`] runs in the ExitBootServices hook, after the real
//! ExitBootServices: nothing in the firmware can recompute a checksum or
//! reinstall a table after that, and the loader has already read whatever
//! it reads before the handoff. Each edit is read back and checked with the
//! bootloader core's own ACPI reader where that reader can tell (an
//! independent parser), and by its exact bytes otherwise.
//!
//! Edits that change a table's content fix its checksum afterwards, so each
//! word tests one thing: a table that is wrong but checksums correctly is
//! the harder case, since no checksum test can catch it. The checksum words
//! run last and break exactly one byte.

use core::ptr;

use optimal_boot_core::acpi::{Acpi, PhysRead};

use crate::Identity;

/// Everything `apply` needs, found while boot services still ran.
#[derive(Clone, Copy, Default)]
pub struct Plan {
    pub rsdp: u64,
    /// (address, length) of the first MADT, FADT and HPET table in the
    /// root table the OS prefers (the XSDT when it is usable).
    pub madt: (u64, u32),
    pub fadt: (u64, u32),
    pub hpet: (u64, u32),
    pub xsdt: u64,
    /// Offsets in the MADT of its first three Processor Local APIC
    /// entries, and of its last entry.
    pub madt_cpu1: usize,
    pub madt_cpu2: usize,
    pub madt_cpu3: usize,
    pub madt_last: usize,
    /// The DSDT's true address (X_DSDT, else DSDT).
    pub dsdt: u64,
    /// PCIe configuration space for the first bus of the first MCFG entry.
    pub ecam: u64,
}

/// Which ACPI edits to make; see `Quirk` in main.rs for what each does.
#[derive(Clone, Copy, Default)]
pub struct Want {
    pub madt_junk: bool,
    pub hpet_misaligned: bool,
    pub dsdt_clash: bool,
    pub dsdt_in_ecam: bool,
    pub madt_checksum: bool,
    pub fadt_checksum: bool,
    pub xsdt_signature: bool,
    pub rsdp_ext_checksum: bool,
}

impl Want {
    fn madt(&self) -> bool {
        self.madt_junk || self.madt_checksum
    }
    fn fadt(&self) -> bool {
        self.dsdt_clash || self.dsdt_in_ecam || self.fadt_checksum
    }
}

/// FADT field offsets (ACPI 6.5 Table 5.9).
const FADT_DSDT: usize = 40;
const FADT_X_DSDT: usize = 140;
/// The HPET table's register block address (HPET 1.0a Table 3: the GAS at
/// offset 40, its address 4 bytes in).
const HPET_ADDRESS: usize = 44;
/// MADT entries start after the header and two 32-bit fields.
const MADT_ENTRIES: usize = 44;

/// Finds what the requested edits need. `Err` names what is missing.
pub fn plan(rsdp: Option<u64>, want: Want) -> Result<Plan, &'static str> {
    let mem = Identity;
    let rsdp = rsdp.ok_or("no RSDP in the UEFI configuration table")?;
    let a = Acpi::new(&mem, rsdp).ok_or("the RSDP is unusable")?;
    let mut p = Plan {
        rsdp,
        ..Plan::default()
    };
    let need = |sig: &[u8; 4], what: &'static str| a.find(sig).ok_or(what);
    if want.xsdt_signature || want.rsdp_ext_checksum {
        // Both make the OS fall back to the RSDT, so there must be one, and
        // an XSDT (reached through a version 2 RSDP) to fall back from.
        if a.revision < 2 || a.rsdp_len < 36 {
            return Err("the RSDP is not a usable version 2 RSDP");
        }
        a.rsdt.ok_or("there is no RSDT to fall back to")?;
        p.xsdt = a.xsdt.ok_or("there is no XSDT")?;
    }
    if want.madt() {
        p.madt = need(b"APIC", "there is no MADT")?;
    }
    if want.madt_junk {
        // Walk the entries: the second and third Processor Local APIC
        // entries get junk, the last entry gets length 0. They must be
        // three different entries, so the OS sees all three faults.
        let (base, len) = p.madt;
        let mut cpus = [0usize; 3];
        let mut n = 0;
        let mut off = MADT_ENTRIES;
        let mut last = 0;
        while off + 2 <= len as usize {
            let kind = mem.u8(base + off as u64).ok_or("MADT unreadable")?;
            let elen = mem.u8(base + off as u64 + 1).ok_or("MADT unreadable")? as usize;
            if elen < 2 || off + elen > len as usize {
                return Err("the MADT is already malformed");
            }
            if kind == 0 && elen >= 8 && n < 3 {
                cpus[n] = off;
                n += 1;
            }
            last = off;
            off += elen;
        }
        if n < 3 || last <= cpus[2] {
            return Err(
                "the MADT needs 3 Processor Local APIC entries and one entry after them (use -smp 3 or more)",
            );
        }
        (p.madt_cpu1, p.madt_cpu2, p.madt_cpu3, p.madt_last) = (cpus[0], cpus[1], cpus[2], last);
    }
    if want.fadt() {
        p.fadt = need(b"FACP", "there is no FADT")?;
    }
    if want.dsdt_clash || want.dsdt_in_ecam {
        let (f, len) = p.fadt;
        if (len as usize) < FADT_X_DSDT + 8 {
            return Err("the FADT is too old to have X_DSDT");
        }
        let d32 = mem.u32(f + FADT_DSDT as u64).ok_or("FADT unreadable")?;
        let x = mem.u64(f + FADT_X_DSDT as u64).ok_or("FADT unreadable")?;
        p.dsdt = if x != 0 { x } else { u64::from(d32) };
        if p.dsdt == 0 || p.dsdt >= 1 << 32 {
            return Err("the FADT's DSDT is not below 4 GiB");
        }
    }
    if want.dsdt_in_ecam {
        let (m, len) = need(b"MCFG", "there is no MCFG")?;
        if len < 44 + 16 {
            return Err("the MCFG lists no ECAM range");
        }
        let base = mem.u64(m + 44).ok_or("MCFG unreadable")?;
        let start_bus = mem.u8(m + 44 + 10).ok_or("MCFG unreadable")?;
        p.ecam = base + (u64::from(start_bus) << 20);
        if base == 0 || p.ecam >= 1 << 32 {
            return Err("the first ECAM range is not below 4 GiB");
        }
    }
    if want.hpet_misaligned {
        p.hpet = need(b"HPET", "there is no HPET table")?;
    }
    Ok(p)
}

/// The table at `phys`, `len` bytes, as a mutable slice.
///
/// # Safety
/// Only after ExitBootServices (nothing else runs) and while the firmware's
/// identity mapping is still live (the loader has not switched page tables
/// yet), for a table `plan` found.
unsafe fn table(phys: u64, len: u32) -> &'static mut [u8] {
    // SAFETY: caller's promise; ACPI tables are in RAM the firmware maps
    // read-write (each edit is read back, so a read-only mapping would
    // fault here rather than pass silently: firmware maps them writable).
    unsafe { core::slice::from_raw_parts_mut(phys as *mut u8, len as usize) }
}

fn sum(b: &[u8]) -> u8 {
    b.iter().fold(0u8, |s, &x| s.wrapping_add(x))
}

/// Makes the table's checksum byte (offset 9) right again.
fn fix_checksum(t: &mut [u8]) {
    t[9] = 0;
    t[9] = 0u8.wrapping_sub(sum(t));
}

/// Writes through volatile stores and reads back, so the edit is really in
/// memory (and not just in a register the compiler kept).
fn put(t: &mut [u8], at: usize, bytes: &[u8]) -> bool {
    for (i, &b) in bytes.iter().enumerate() {
        // SAFETY: in bounds of `t` (indexing checks it first).
        unsafe { ptr::write_volatile(&mut t[at + i], b) };
    }
    // SAFETY: as above.
    (0..bytes.len()).all(|i| unsafe { ptr::read_volatile(&t[at + i]) } == bytes[i])
}

/// What `apply` managed, word by word, plus a line of detail.
#[derive(Default)]
pub struct Done {
    pub madt_junk: bool,
    pub hpet_misaligned: bool,
    pub dsdt_clash: bool,
    pub dsdt_in_ecam: bool,
    pub madt_checksum: bool,
    pub fadt_checksum: bool,
    pub xsdt_signature: bool,
    pub rsdp_ext_checksum: bool,
    pub apic_ids: (u8, u8),
    pub last_kind: u8,
}

/// Makes the edits. See [`table`] for when this may run.
pub fn apply(p: &Plan, want: Want) -> Done {
    let mut d = Done::default();
    let mem = Identity;
    // SAFETY (every `table` call below): we run in the ExitBootServices
    // hook, after the real one, on the firmware's page tables; `plan`
    // found each table.
    if want.madt_junk {
        // SAFETY: see the top of this function.
        let t = unsafe { table(p.madt.0, p.madt.1) };
        let first_id = t[p.madt_cpu1 + 3];
        d.apic_ids = (t[p.madt_cpu2 + 3], t[p.madt_cpu3 + 3]);
        d.last_kind = t[p.madt_last];
        d.madt_junk = put(t, p.madt_cpu2 + 3, &[first_id])
            && put(t, p.madt_cpu3 + 3, &[0xff])
            && put(t, p.madt_last + 1, &[0]);
        fix_checksum(t);
        d.madt_junk &= sum(t) == 0;
    }
    if want.hpet_misaligned {
        // SAFETY: see the top of this function.
        let t = unsafe { table(p.hpet.0, p.hpet.1) };
        let a = u64::from_le_bytes(
            t[HPET_ADDRESS..HPET_ADDRESS + 8]
                .try_into()
                .unwrap_or([0; 8]),
        );
        // Half a kilobyte off: the block must be 1 KiB aligned.
        d.hpet_misaligned = put(t, HPET_ADDRESS, &(a + 0x200).to_le_bytes());
        fix_checksum(t);
        d.hpet_misaligned &= sum(t) == 0;
    }
    if want.dsdt_clash || want.dsdt_in_ecam {
        // SAFETY: see the top of this function.
        let t = unsafe { table(p.fadt.0, p.fadt.1) };
        let (x, d32) = if want.dsdt_in_ecam {
            // Both fields agree, and both point at PCIe config space.
            (p.ecam, p.ecam as u32)
        } else {
            // X_DSDT stays right; the 32-bit field points a page away.
            let other = if p.dsdt + 0x1000 < 1 << 32 {
                p.dsdt + 0x1000
            } else {
                p.dsdt - 0x1000
            };
            (p.dsdt, other as u32)
        };
        let ok = put(t, FADT_X_DSDT, &x.to_le_bytes()) && put(t, FADT_DSDT, &d32.to_le_bytes());
        fix_checksum(t);
        let ok = ok && sum(t) == 0;
        if want.dsdt_in_ecam {
            d.dsdt_in_ecam = ok;
        } else {
            d.dsdt_clash = ok;
        }
    }
    // Checksum breaks last, after every content edit has fixed its own.
    for (on, (phys, len), done) in [
        (want.madt_checksum, p.madt, &mut d.madt_checksum),
        (want.fadt_checksum, p.fadt, &mut d.fadt_checksum),
    ] {
        if on {
            // SAFETY: see the top of this function.
            let t = unsafe { table(phys, len) };
            *done = put(t, 9, &[t[9].wrapping_add(1)]) && sum(t) == 1;
        }
    }
    if want.xsdt_signature {
        // SAFETY: see the top of this function.
        let t = unsafe { table(p.xsdt, 36) };
        d.xsdt_signature = put(t, 0, b"xSDT");
    }
    if want.rsdp_ext_checksum {
        // The 36-byte RSDP: the first 20 bytes keep their checksum (the
        // RSDP is still found), the extended checksum at byte 32 breaks.
        // SAFETY: see the top of this function.
        let t = unsafe { table(p.rsdp, 36) };
        d.rsdp_ext_checksum = put(t, 32, &[t[32].wrapping_add(1)]) && sum(&t[..20]) == 0;
    }
    // Independent check with the bootloader core's reader: after either
    // root-table word it must no longer find a usable XSDT.
    if want.xsdt_signature || want.rsdp_ext_checksum {
        let fell_back = Acpi::new(&mem, p.rsdp).is_some_and(|a| {
            let mut sig = [0u8; 4];
            let xsdt_ok = a
                .xsdt
                .is_some_and(|x| mem.read(x, &mut sig) && &sig == b"XSDT");
            !xsdt_ok && a.find(b"FACP").is_some()
        });
        d.xsdt_signature &= fell_back;
        d.rsdp_ext_checksum &= fell_back;
    }
    d
}
