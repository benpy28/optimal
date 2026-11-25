//! Kernel tests for ACPI on the real (virtual) machine: the firmware reader
//! refuses everything that isn't firmware memory and never leaves its
//! window mapped, every table copy is whole, the MADT agrees with the CPU,
//! and the reclaim adds exactly the ACPI reclaimable memory.
//!
//! On a machine without ACPI (the tough-conditions `fw-no-acpi` run) the
//! table tests have nothing to check and say so; that run's ACPI line is
//! checked by the tough-conditions script instead.

use core::arch::x86_64::__cpuid_count;

use optimal_acpi::checksum;
use optimal_acpi::tables::PhysRead;
use optimal_mm::PAGE_SIZE;

use super::{BIOS_AREA, DEVICE_WINDOW, FirmwareMem, window_page};
use crate::bootinfo::{self, Kind};
use crate::ktest;
use crate::mm::{LOW_MEMORY_END, frames, paging};

/// A reader as it is after boot: ACPI reclaimable memory belongs to the
/// frame allocator now, so it is refused like any other kernel RAM.
fn reader() -> FirmwareMem {
    FirmwareMem::new(bootinfo::get(), false, window_page())
}

/// Reads `len` bytes (at most 64) into a buffer pre-filled with 0xa5, and
/// returns whether the read succeeded plus whether the buffer was left
/// untouched.
fn try_read(mem: &FirmwareMem, phys: u64, len: usize) -> (bool, bool) {
    let mut buf = [0xa5u8; 64];
    let ok = mem.read(phys, &mut buf[..len]);
    (ok, buf.iter().all(|&b| b == 0xa5))
}

ktest!(reader_refuses_kernel_ram_and_devices, {
    let mem = reader();
    // A frame the allocator just handed out: kernel RAM.
    let f = frames::alloc(0).expect("no frame for the test");
    assert_eq!(try_read(&mem, f, 8), (false, true));
    assert_eq!(try_read(&mem, f + PAGE_SIZE - 1, 1), (false, true));
    frames::free(f, 0).unwrap();
    // The kernel image.
    let k = bootinfo::get().kernel_phys;
    assert_eq!(try_read(&mem, k, 16), (false, true));
    // Device registers: local APIC, I/O APIC, HPET, whatever the map says.
    for d in [
        0xfee0_0000u64,
        0xfec0_0000,
        0xfed0_0000,
        DEVICE_WINDOW.end - 1,
    ] {
        assert_eq!(try_read(&mem, d, 4), (false, true), "device {d:#x}");
    }
    // Reads that wrap around the address space.
    assert_eq!(try_read(&mem, u64::MAX - 3, 8), (false, true));
    assert!(!mem.allows(1, u64::MAX));
    // Every usable or bootloader entry, at its first and last byte.
    for e in bootinfo::get().memory_map() {
        if matches!(
            e.kind,
            Kind::Usable | Kind::BootloaderReclaimable | Kind::KernelAndModules
        ) && e.len > 0
        {
            assert!(!mem.allows(e.base, 1), "{:#x} {:?}", e.base, e.kind);
            assert!(!mem.allows(e.base + e.len - 1, 1));
        }
    }
});

ktest!(reader_refuses_reads_straddling_into_ram, {
    let mem = reader();
    let map = bootinfo::get().memory_map();
    let mut checked = 0;
    for pair in map.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let firmware =
            |k: Kind| matches!(k, Kind::AcpiReclaimable | Kind::AcpiNvs | Kind::Reserved);
        let ram = |k: Kind| {
            matches!(
                k,
                Kind::Usable | Kind::BootloaderReclaimable | Kind::KernelAndModules
            )
        };
        if a.base + a.len != b.base || a.len < 8 || b.len < 8 {
            continue;
        }
        if firmware(a.kind) && ram(b.kind) && mem.allows(b.base - 4, 4) {
            assert_eq!(try_read(&mem, b.base - 4, 8), (false, true));
            assert!(try_read(&mem, b.base - 4, 4).0);
            checked += 1;
        }
        if ram(a.kind) && firmware(b.kind) && mem.allows(b.base, 4) {
            assert_eq!(try_read(&mem, b.base - 4, 8), (false, true));
            assert!(try_read(&mem, b.base, 4).0);
            checked += 1;
        }
    }
    crate::println!("ktest: {checked} firmware/RAM boundaries checked");
});

ktest!(reader_reads_the_rsdp_and_leaves_no_window, {
    let info = bootinfo::get();
    let Some(rsdp) = info.rsdp else {
        crate::println!("ktest: no RSDP on this machine; nothing to read");
        return;
    };
    // Before the reclaim the RSDP was readable (init found the tables). Now
    // it is readable again only if it isn't in reclaimed memory.
    let mem = reader();
    let reclaimed = info
        .memory_map()
        .iter()
        .any(|e| e.kind == Kind::AcpiReclaimable && e.base <= rsdp && rsdp - e.base < e.len);
    let mut buf = [0u8; 20];
    if reclaimed {
        assert!(!mem.read(rsdp, &mut buf), "reclaimed RSDP still readable");
        crate::println!("ktest: RSDP was in reclaimed memory; refused as it should be");
    } else {
        assert!(mem.read(rsdp, &mut buf));
        assert_eq!(&buf[..8], b"RSD PTR ");
        assert_eq!(checksum(&buf), 0);
    }
    assert!(
        paging::translate(mem.window()).is_none(),
        "read window left mapped"
    );
});

ktest!(reads_across_pages_match_the_copies, {
    // Every table still readable after the reclaim (reserved or NVS memory)
    // and crossing a page boundary: read 32 bytes around the boundary, once
    // whole and once a byte at a time, and compare both with the heap copy.
    // On SeaBIOS these go through the read window, on OVMF through the
    // direct map.
    let Ok(a) = crate::acpi::get() else {
        crate::println!("ktest: no ACPI on this machine");
        return;
    };
    let mem = reader();
    let mut found = 0;
    for t in &a.tables {
        let len = t.bytes.len() as u64;
        let boundary = (t.phys + 1).next_multiple_of(PAGE_SIZE);
        if boundary < t.phys + 16 || boundary + 16 > t.phys + len || !mem.allows(t.phys, len) {
            continue;
        }
        let at = boundary - 16;
        let off = (at - t.phys) as usize;
        let mut whole = [0u8; 32];
        assert!(mem.read(at, &mut whole));
        assert_eq!(&whole[..], &t.bytes[off..off + 32], "{:?}", t.signature);
        for (i, &b) in whole.iter().enumerate() {
            let mut one = [0u8; 1];
            assert!(mem.read(at + i as u64, &mut one));
            assert_eq!(one[0], b, "byte {:#x}", at + i as u64);
        }
        found += 1;
    }
    assert!(
        paging::translate(mem.window()).is_none(),
        "read window left mapped"
    );
    crate::println!("ktest: {found} tables read across a page boundary");
});

ktest!(bios_area_holes_are_readable_only_where_the_map_is_silent, {
    let mem = reader();
    let map = bootinfo::get().memory_map();
    let mut at = BIOS_AREA.start;
    while at < BIOS_AREA.end {
        let entry = map.iter().find(|e| e.base <= at && at - e.base < e.len);
        let expect = match entry {
            None => true,
            Some(e) => matches!(e.kind, Kind::Reserved | Kind::AcpiNvs),
        };
        assert_eq!(mem.allows(at, 1), expect, "{at:#x} {entry:?}");
        at += 0x400;
    }
});

ktest!(every_table_copy_is_whole, {
    let Ok(a) = crate::acpi::get() else {
        crate::println!("ktest: no ACPI on this machine");
        return;
    };
    assert!(!a.tables.is_empty());
    for t in &a.tables {
        assert!(t.bytes.len() >= 36, "{:?} too short", t.signature);
        assert_eq!(&t.bytes[..4], &t.signature);
        let len = u32::from_le_bytes(t.bytes[4..8].try_into().unwrap()) as usize;
        assert_eq!(
            len,
            t.bytes.len(),
            "{:?} copied with the wrong length",
            t.signature
        );
        assert_eq!(checksum(&t.bytes) == 0, t.checksum_ok);
        assert!(t.phys != 0);
    }
    // A FADT on a machine that isn't hardware-reduced means a DSDT copy.
    if let Some(f) = &a.fadt
        && !f.hw_reduced
    {
        assert!(a.table(b"DSDT").is_some(), "FADT but no DSDT copied");
    }
    // The heap copies hold no pointer into firmware memory: they are in
    // the direct map's view of kernel RAM.
    for t in &a.tables {
        let phys = frames::virt_to_phys(t.bytes.as_ptr() as u64);
        let map = bootinfo::get().memory_map();
        assert!(
            map.iter().any(|e| matches!(
                e.kind,
                Kind::Usable | Kind::BootloaderReclaimable | Kind::AcpiReclaimable
            ) && e.base <= phys
                && phys - e.base < e.len),
            "copy of {:?} is not in kernel RAM",
            t.signature
        );
    }
});

ktest!(madt_lists_the_boot_cpu, {
    let Ok(a) = crate::acpi::get() else {
        crate::println!("ktest: no ACPI on this machine");
        return;
    };
    let m = a.madt.as_ref().expect("no MADT");
    // The boot CPU's APIC ID: the x2APIC ID from leaf 0xB when the CPU has
    // it, else the 8-bit xAPIC ID from leaf 1 (EBX bits 31..24).
    let id = if __cpuid_count(0, 0).eax >= 0xb && __cpuid_count(0xb, 0).ebx != 0 {
        __cpuid_count(0xb, 0).edx
    } else {
        __cpuid_count(1, 0).ebx >> 24
    };
    let me = m.cpus.iter().find(|c| c.apic_id == id);
    assert!(
        me.is_some_and(|c| c.enabled),
        "boot CPU APIC ID {id} not an enabled MADT CPU"
    );
    for (i, c) in m.cpus.iter().enumerate() {
        assert!(
            m.cpus[..i].iter().all(|d| d.apic_id != c.apic_id),
            "APIC ID {} twice",
            c.apic_id
        );
    }
    assert!(!m.ioapics.is_empty(), "no I/O APIC");
    assert!(m.ioapics.iter().all(|io| io.address != 0));
    assert!(m.lapic_address != 0);
});

ktest!(reclaim_adds_exactly_the_acpi_reclaimable_pages, {
    let Ok(a) = crate::acpi::get() else {
        crate::println!("ktest: no ACPI on this machine");
        return;
    };
    // Independent count, page by page: whole pages inside an ACPI
    // reclaimable entry, at or above 1 MiB, not sharing a page with the FACS.
    let keep = a
        .facs
        .map(|(s, e)| (s & !(PAGE_SIZE - 1), e.next_multiple_of(PAGE_SIZE)));
    let mut expect = 0;
    for e in bootinfo::get().memory_map() {
        if e.kind != Kind::AcpiReclaimable {
            continue;
        }
        let mut p = e.base.next_multiple_of(PAGE_SIZE).max(LOW_MEMORY_END);
        while p + PAGE_SIZE <= e.base + e.len {
            if !keep.is_some_and(|(s, t)| s <= p && p < t) {
                expect += PAGE_SIZE;
            }
            p += PAGE_SIZE;
        }
    }
    assert_eq!(a.reclaimed, expect);
    let (before, after) = a.free_around_reclaim;
    assert_eq!(
        after - before,
        a.reclaimed,
        "allocator didn't gain exactly the reclaimed bytes"
    );
    // After the reclaim, the reader refuses all of it.
    let mem = reader();
    for e in bootinfo::get().memory_map() {
        if e.kind == Kind::AcpiReclaimable && e.len > 0 {
            assert!(!mem.allows(e.base, 1));
            assert!(!mem.allows(e.base + e.len - 1, 1));
        }
    }
    crate::println!(
        "ktest: {} KiB of ACPI memory reclaimed, as counted",
        expect / 1024
    );
});

ktest!(direct_map_spans_are_mapped, {
    // The reader trusts `for_each_direct_span` to say what the direct map
    // covers. Check that against the page tables: the first and last page of
    // each span and up to 64 pages spread across it.
    let direct = paging::layout().direct_map;
    let mut checked = 0;
    paging::for_each_direct_span(bootinfo::get(), |start, end| {
        if start >= end {
            return;
        }
        let first = start & !(PAGE_SIZE - 1);
        let last = (end - 1) & !(PAGE_SIZE - 1);
        let step = ((last - first) / 64).max(PAGE_SIZE) & !(PAGE_SIZE - 1);
        let mut p = first;
        loop {
            let t =
                paging::translate(direct + p).unwrap_or_else(|| panic!("{p:#x} not in direct map"));
            assert_eq!(t.phys, p);
            checked += 1;
            if p == last {
                break;
            }
            p = (p + step).min(last);
        }
    });
    crate::println!("ktest: {checked} direct-map pages checked");
});
