//! The checks. Every pointer the loader hands over is first proven mapped by
//! walking the live page tables (so a bad pointer is a FAIL line, not a
//! triple fault), and an IDT catches any fault the checks provoke on purpose
//! (W^X probes) or by accident (reported as a FAIL with the faulting RIP).

use crate::phys::Phys;
use crate::req::{self, Raw};
use crate::{ENTRY_STATE, println, serial};
use core::arch::{asm, global_asm};
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering::SeqCst};
use optimal_boot_core::acpi::PhysRead as _;
use optimal_boot_core::memmap::in_hhdm;
use optimal_boot_core::paging::{self, FrameSource, PAGE, Translation};
use optimal_boot_core::protocol::{self as lp, MemmapEntry};
use optimal_boot_core::{acpi, sig, tcg};

// ---------------------------------------------------------------- reporting

struct T {
    pass: u32,
    fail: u32,
}

impl T {
    fn ok(&mut self, name: &str) {
        self.pass += 1;
        println!("ok {name}");
    }
    fn fail(&mut self, name: &str, why: fmt::Arguments) {
        self.fail += 1;
        println!("FAIL {name}: {why}");
    }
    fn skip(&self, name: &str, why: &str) {
        println!("skip {name}: {why}");
    }
}

macro_rules! check {
    ($t:expr, $name:expr, $cond:expr, $($why:tt)*) => {
        if $cond { $t.ok($name) } else { $t.fail($name, format_args!($($why)*)) }
    };
}

// ---------------------------------------------------------------- CPU access

fn cr0() -> u64 {
    let v;
    // SAFETY: reading a control register has no side effects.
    unsafe { asm!("mov {}, cr0", out(reg) v) };
    v
}
fn cr3() -> u64 {
    let v;
    // SAFETY: as above.
    unsafe { asm!("mov {}, cr3", out(reg) v) };
    v
}
fn cr4() -> u64 {
    let v;
    // SAFETY: as above.
    unsafe { asm!("mov {}, cr4", out(reg) v) };
    v
}
fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: only architectural MSRs that exist on every x86-64 CPU are read.
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi) };
    (hi as u64) << 32 | lo as u64
}
fn rdtsc() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: rdtsc has no side effects.
    unsafe { asm!("lfence; rdtsc", out("eax") lo, out("edx") hi) };
    (hi as u64) << 32 | lo as u64
}
fn cpuid(leaf: u32, sub: u32) -> [u32; 4] {
    let r = core::arch::x86_64::__cpuid_count(leaf, sub);
    [r.eax, r.ebx, r.ecx, r.edx]
}
fn outb(p: u16, v: u8) {
    // SAFETY: legacy PIT/PIC ports, written only to read state or time.
    unsafe { asm!("out dx, al", in("dx") p, in("al") v) };
}
fn inb(p: u16) -> u8 {
    serial::inb(p)
}
fn cpu_has_nx() -> bool {
    cpuid(0x8000_0000, 0)[0] >= 0x8000_0001 && cpuid(0x8000_0001, 0)[3] & (1 << 20) != 0
}
fn cpu_has_la57() -> bool {
    cpuid(0, 0)[0] >= 7 && cpuid(7, 0)[2] & (1 << 16) != 0
}

// ---------------------------------------------------------------- faults

static RECOVER: AtomicU64 = AtomicU64::new(0);
static RECOVER_RSP: AtomicU64 = AtomicU64::new(0);
static FAULT_VEC: AtomicU64 = AtomicU64::new(u64::MAX);
static FAULT_ERR: AtomicU64 = AtomicU64::new(0);
static FAULT_CR2: AtomicU64 = AtomicU64::new(0);

// 32 stubs, 16 bytes apart, push a dummy error code where the CPU pushes
// none, then the vector. With RECOVER set, the fault is recorded and
// execution resumes at RECOVER with RECOVER_RSP; otherwise it is reported.
global_asm!(
    ".global exc_stubs",
    ".balign 16",
    "exc_stubs:",
    ".irp v, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31",
    ".balign 16",
    ".if (\\v == 8) || ((\\v >= 10) && (\\v <= 14)) || (\\v == 17) || (\\v == 21) || (\\v == 29) || (\\v == 30)",
    ".else",
    "    push 0",
    ".endif",
    "    push \\v",
    "    jmp exc_common",
    ".endr",
    "exc_common:",
    "    push rax",
    "    mov rax, [rip + {rec}]",
    "    test rax, rax",
    "    jz 3f",
    "    mov [rsp + 24], rax",
    "    mov rax, [rip + {rec_rsp}]",
    "    mov [rsp + 48], rax",
    "    mov rax, [rsp + 8]",
    "    mov [rip + {vec}], rax",
    "    mov rax, [rsp + 16]",
    "    mov [rip + {err}], rax",
    "    mov rax, cr2",
    "    mov [rip + {cr2}], rax",
    "    mov qword ptr [rip + {rec}], 0",
    "    pop rax",
    "    add rsp, 16",
    "    iretq",
    "3:  pop rax",
    "    mov rdi, rsp",
    "    and rsp, -16",
    "    call {unexpected}",
    "    ud2",
    rec = sym RECOVER,
    rec_rsp = sym RECOVER_RSP,
    vec = sym FAULT_VEC,
    err = sym FAULT_ERR,
    cr2 = sym FAULT_CR2,
    unexpected = sym exc_unexpected,
);

unsafe extern "C" {
    static exc_stubs: u8;
}

extern "C" fn exc_unexpected(frame: *const u64) -> ! {
    // SAFETY: the stub passes a pointer to [vector, error, rip, cs, rflags, rsp, ss].
    let f = unsafe { core::slice::from_raw_parts(frame, 7) };
    let cr2 = {
        let v: u64;
        // SAFETY: reading CR2 has no side effects.
        unsafe { asm!("mov {}, cr2", out(reg) v) };
        v
    };
    println!(
        "FAIL exception: vector {} error {:#x} rip {:#x} rsp {:#x} cr2 {:#x}",
        f[0], f[1], f[2], f[5], cr2
    );
    println!("OPTIMAL-CONFORMANCE: FAIL");
    crate::halt()
}

static mut IDT: [u64; 64] = [0; 64];

fn install_idt() {
    let cs: u16;
    // SAFETY: reading CS.
    unsafe { asm!("mov {0:x}, cs", out(reg) cs) };
    let base = &raw const exc_stubs as u64;
    // SAFETY: IDT is only touched here, before any fault can use it.
    #[allow(clippy::deref_addrof)]
    let idt = unsafe { &mut *(&raw mut IDT) };
    for v in 0..32 {
        let h = base + 16 * v as u64;
        idt[2 * v] = (h & 0xffff) | (cs as u64) << 16 | 0x8e00u64 << 32 | (h >> 16 & 0xffff) << 48;
        idt[2 * v + 1] = h >> 32;
    }
    let mut idtr = [0u8; 10];
    idtr[..2].copy_from_slice(&(32u16 * 16 - 1).to_le_bytes());
    idtr[2..].copy_from_slice(&(idt.as_ptr() as u64).to_le_bytes());
    // SAFETY: the IDT and its handlers stay valid for the life of the kernel.
    unsafe { asm!("lidt [{}]", in(reg) idtr.as_ptr()) };
}

#[derive(Clone, Copy, Debug)]
struct Fault {
    vector: u64,
    error: u64,
    cr2: u64,
}

fn take_fault() -> Option<Fault> {
    let v = FAULT_VEC.swap(u64::MAX, SeqCst);
    (v != u64::MAX).then(|| Fault {
        vector: v,
        error: FAULT_ERR.load(SeqCst),
        cr2: FAULT_CR2.load(SeqCst),
    })
}

/// Reads one byte at `addr` and writes it back; returns the fault, if any.
fn probe_write(addr: u64) -> Option<Fault> {
    FAULT_VEC.store(u64::MAX, SeqCst);
    // SAFETY: a fault is caught by the IDT and resumes at label 2; if no fault
    // happens the byte is rewritten with its own value.
    unsafe {
        asm!(
            "lea rax, [rip + 2f]",
            "mov [rip + {rec}], rax",
            "mov [rip + {rsp}], rsp",
            "mov cl, [rdi]",
            "mov [rdi], cl",
            "mov qword ptr [rip + {rec}], 0",
            "2:",
            rec = sym RECOVER,
            rsp = sym RECOVER_RSP,
            in("rdi") addr,
            out("rax") _,
            out("rcx") _,
        )
    };
    take_fault()
}

/// Reads one byte at `addr`; returns the fault, if any.
fn probe_read(addr: u64) -> Option<Fault> {
    FAULT_VEC.store(u64::MAX, SeqCst);
    // SAFETY: as probe_write, without the write.
    unsafe {
        asm!(
            "lea rax, [rip + 2f]",
            "mov [rip + {rec}], rax",
            "mov [rip + {rsp}], rsp",
            "mov cl, [rdi]",
            "mov qword ptr [rip + {rec}], 0",
            "2:",
            rec = sym RECOVER,
            rsp = sym RECOVER_RSP,
            in("rdi") addr,
            out("rax") _,
            out("rcx") _,
        )
    };
    take_fault()
}

/// Calls `addr`, which must hold a `ret`; returns the fault, if any.
fn probe_exec(addr: u64) -> Option<Fault> {
    FAULT_VEC.store(u64::MAX, SeqCst);
    // SAFETY: the target is a single `ret`; a fetch fault resumes at label 2
    // with the stack pointer from before the call.
    unsafe {
        asm!(
            "lea rax, [rip + 2f]",
            "mov [rip + {rec}], rax",
            "mov [rip + {rsp}], rsp",
            "call rdi",
            "mov qword ptr [rip + {rec}], 0",
            "2:",
            rec = sym RECOVER,
            rsp = sym RECOVER_RSP,
            in("rdi") addr,
            out("rax") _,
        )
    };
    take_fault()
}

#[unsafe(link_section = ".data")]
static RET_DATA: [u8; 64] = [0xc3; 64];

// ---------------------------------------------------------------- memory views

struct Hhdm(u64);
impl FrameSource for Hhdm {
    fn alloc(&mut self) -> Option<u64> {
        None
    }
    fn table(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: page tables live in bootloader-reclaimable memory, which the
        // spec puts in the HHDM; a bad pointer faults into the IDT and FAILs.
        unsafe { &mut *((phys + self.0) as *mut [u64; 512]) }
    }
}

pub struct Ctx {
    rev: u64,
    pub hhdm: u64,
    pub levels: u32,
    pub nx: bool,
    pub root: u64,
    map: [MemmapEntry; 512],
    map_len: usize,
    /// Booted by Optimal Boot: its stronger guarantees are checked too.
    ours: bool,
}

impl Ctx {
    pub fn tr(&self, virt: u64) -> Option<Translation> {
        if self.hhdm == 0 || !paging::canonical(virt, self.levels) {
            return None;
        }
        paging::translate(&mut Hhdm(self.hhdm), self.root, self.levels, self.nx, virt)
    }
    /// Whether [virt, virt+len) is mapped readable (and writable if asked).
    fn mapped(&self, virt: u64, len: u64, write: bool) -> bool {
        if len == 0 {
            return self.tr(virt).is_some();
        }
        let Some(end) = virt.checked_add(len) else {
            return false;
        };
        let mut p = virt & !(PAGE - 1);
        while p < end {
            match self.tr(p) {
                Some(t) if !write || t.write => {}
                _ => return false,
            }
            p += PAGE;
        }
        true
    }
    fn map(&self) -> &[MemmapEntry] {
        &self.map[..self.map_len]
    }
    /// Type of the memory map entry wholly containing [phys, phys+len).
    fn type_of(&self, phys: u64, len: u64) -> Option<u64> {
        let end = phys.checked_add(len.max(1))?;
        self.map()
            .iter()
            .find(|e| e.base <= phys && end <= e.end())
            .map(|e| e.typ)
    }
    /// Physical address behind a virtual one, if mapped.
    pub fn phys(&self, virt: u64) -> Option<u64> {
        self.tr(virt).map(|t| t.phys)
    }
    /// Whether the object at virtual `v` of `len` bytes sits in memory of type `typ`.
    fn virt_in(&self, v: u64, len: u64, typ: u64) -> bool {
        if !self.mapped(v, len, false) {
            return false;
        }
        let mut p = v & !(PAGE - 1);
        while p < v + len.max(1) {
            match self.phys(p).and_then(|ph| self.type_of(ph, 1)) {
                Some(t) if t == typ => {}
                _ => return false,
            }
            p += PAGE;
        }
        true
    }
    /// A response the spec requires: reported FAIL if missing or misplaced.
    fn need<const N: usize>(&self, t: &mut T, name: &str, r: &Raw<N>, words: u64) -> Option<u64> {
        let p = self.resp(t, name, r, words);
        if p == Some(0) {
            t.fail(name, format_args!("no response"));
        }
        p.filter(|&p| p != 0)
    }
    /// An optional response: a skip line if missing.
    fn want<const N: usize>(
        &self,
        t: &mut T,
        name: &str,
        r: &Raw<N>,
        words: u64,
        why: &str,
    ) -> Option<u64> {
        let p = self.resp(t, name, r, words);
        if p == Some(0) {
            t.skip(name, why);
        }
        p.filter(|&p| p != 0)
    }
    /// Some(0) if absent, None if present but misplaced (reported), else the pointer.
    fn resp<const N: usize>(&self, t: &mut T, name: &str, r: &Raw<N>, words: u64) -> Option<u64> {
        let p = req::response(r);
        if p == 0 {
            return Some(0);
        }
        if !self.virt_in(p, words * 8, lp::MEMMAP_BOOTLOADER_RECLAIMABLE) {
            t.fail(
                name,
                format_args!("response {p:#x} is not mapped bootloader-reclaimable memory"),
            );
            return None;
        }
        Some(p)
    }
}

fn rd64(v: u64) -> u64 {
    // SAFETY: callers prove `v` mapped first.
    unsafe { core::ptr::read_volatile(v as *const u64) }
}
fn bytes(v: u64, len: u64) -> &'static [u8] {
    // SAFETY: callers prove [v, v+len) mapped first.
    unsafe { core::slice::from_raw_parts(v as *const u8, len as usize) }
}
/// A NUL-terminated UTF-8 string of at most 4095 bytes, checked mapped page by page.
fn cstr(c: &Ctx, v: u64) -> Option<&'static str> {
    if v == 0 {
        return None;
    }
    let mut n = 0u64;
    loop {
        let a = v.checked_add(n)?;
        if n >= 4096 || (n == 0 || a & (PAGE - 1) == 0) && !c.mapped(a, 1, false) {
            return None;
        }
        if bytes(a, 1)[0] == 0 {
            break;
        }
        n += 1;
    }
    core::str::from_utf8(bytes(v, n)).ok()
}
fn sum8(b: &[u8]) -> u8 {
    b.iter().fold(0u8, |a, x| a.wrapping_add(*x))
}

unsafe extern "C" {
    static __image_start: u8;
    static __image_end: u8;
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __data_end: u8;
}
fn sym_addr(s: *const u8) -> u64 {
    s as u64
}

// ---------------------------------------------------------------- the run

struct Tables {
    idtr: (u16, u64),
    tr: u16,
    ldtr: u16,
}

fn entry_tables() -> Tables {
    let mut idtr = [0u8; 10];
    let (tr, ldtr): (u16, u16);
    // SAFETY: sidt/str/sldt only store state.
    unsafe {
        asm!("sidt [{}]", in(reg) idtr.as_mut_ptr());
        asm!("str {0:x}", out(reg) tr);
        asm!("sldt {0:x}", out(reg) ldtr);
    }
    Tables {
        idtr: (
            u16::from_le_bytes([idtr[0], idtr[1]]),
            u64::from_le_bytes(idtr[2..].try_into().unwrap()),
        ),
        tr,
        ldtr,
    }
}

pub fn run() {
    let tables = entry_tables();
    install_idt();
    let mut t = T { pass: 0, fail: 0 };
    // SAFETY: written once by the entry stub before Rust ran.
    #[allow(clippy::deref_addrof)]
    let es = unsafe { &*(&raw const ENTRY_STATE) };
    println!(
        "OPTIMAL-CONFORMANCE: start (base revision {}, PIE {}, paging request {})",
        req::REVISION,
        option_env!("OB_PIE").unwrap_or("0"),
        if req::WANT_5LVL { 5 } else { 4 }
    );

    // Base revision: supported (word 2 cleared) and loaded at exactly ours.
    let br = (req::BASE_REVISION.word(1), req::BASE_REVISION.word(2));
    check!(
        t,
        "base-revision",
        br.1 == 0 && br.0 == req::REVISION,
        "tag words are {:#x} {:#x}, expected {} 0",
        br.0,
        br.1,
        req::REVISION
    );

    entry_state(&mut t, es);
    let Some(mut c) = control_registers(&mut t) else {
        println!("OPTIMAL-CONFORMANCE: FAIL");
        return;
    };
    memory_map(&mut t, &mut c);
    gdt(&mut t, &c);
    hhdm_mapping(&mut t, &c);
    kernel_image(&mut t, &c);
    stack(&mut t, &c, es);
    wx_probes(&mut t, &c);
    simple_responses(&mut t, &c);
    framebuffer(&mut t, &c);
    files(&mut t, &c);
    firmware_tables(&mut t, &c);
    efi_memmap(&mut t, &c);
    entropy(&mut t, &c);
    timing(&mut t, &c);
    tpm_log(&mut t, &c);
    strict_state(&mut t, &c, es, &tables);
    platform(&mut t, &c);
    address_space(&mut t, &c);

    if t.fail == 0 {
        println!("OPTIMAL-CONFORMANCE: PASS ({} checks)", t.pass);
    } else {
        println!(
            "OPTIMAL-CONFORMANCE: FAIL ({} of {} checks failed)",
            t.fail,
            t.pass + t.fail
        );
    }
}

fn entry_state(t: &mut T, es: &crate::EntryState) {
    let via_want = if req::ENTRY_REQ { 2 } else { 1 };
    check!(
        t,
        "entry-point",
        es.via == via_want,
        "entered via {}, expected {}",
        es.via,
        via_want
    );
    let nonzero = (0..16).filter(|&i| i != 7 && es.gprs[i] != 0).count();
    check!(
        t,
        "entry-gprs-zero",
        nonzero == 0,
        "{nonzero} general registers other than rsp are non-zero: {:x?}",
        es.gprs
    );
    let f = es.rflags;
    check!(
        t,
        "entry-rflags",
        f & (1 << 9) == 0 && f & (1 << 10) == 0 && f & 2 != 0,
        "rflags {f:#x} (IF and DF must be clear)"
    );
    check!(
        t,
        "entry-return-address",
        es.ret_addr == 0,
        "return address {:#x}, expected 0",
        es.ret_addr
    );
    let rsp = es.gprs[7];
    check!(
        t,
        "entry-stack-alignment",
        (rsp + 8).is_multiple_of(16),
        "rsp {rsp:#x} is not 16-byte aligned before the return address"
    );
    check!(
        t,
        "entry-segments",
        es.cs == 0x28
            && [es.ds, es.es, es.ss, es.fs, es.gs]
                .iter()
                .all(|&s| s == 0x30),
        "cs {:#x} ds {:#x} es {:#x} ss {:#x} fs {:#x} gs {:#x}",
        es.cs,
        es.ds,
        es.es,
        es.ss,
        es.fs,
        es.gs
    );
}

fn control_registers(t: &mut T) -> Option<Ctx> {
    let (c0, c4, efer, pat) = (cr0(), cr4(), rdmsr(0xc000_0080), rdmsr(0x277));
    check!(
        t,
        "cr0",
        c0 & 1 != 0 && c0 & (1 << 31) != 0 && c0 & (1 << 16) != 0,
        "cr0 {c0:#x}: PE, PG and WP must be set"
    );
    check!(t, "cr4-pae", c4 & (1 << 5) != 0, "cr4 {c4:#x}");
    let nx = cpu_has_nx();
    check!(
        t,
        "efer",
        efer & (1 << 8) != 0 && efer & (1 << 10) != 0 && (efer & (1 << 11) != 0) == nx,
        "efer {efer:#x}: LME and LMA must be set, NXE exactly when the CPU has NX ({nx})"
    );

    let levels = if c4 & (1 << 12) != 0 { 5 } else { 4 };
    let want = if req::WANT_5LVL && cpu_has_la57() {
        5
    } else {
        4
    };
    check!(
        t,
        "paging-levels",
        levels == want,
        "{levels}-level paging, expected {want}"
    );

    let hhdm_resp = req::response(&req::HHDM);
    if hhdm_resp == 0 {
        t.fail("hhdm", format_args!("no HHDM response"));
        return None;
    }
    let root = cr3() & paging::ADDR_MASK;
    // Bootstrap: the offset is needed to walk the page tables, so it is read
    // before it can be proven; a bad pointer faults into the IDT and FAILs.
    // The read is then proven legitimate: the response must translate to
    // offset + its own physical address.
    let offset = rd64(hhdm_resp + 8);
    let mut c = Ctx {
        rev: req::REVISION,
        hhdm: offset,
        levels,
        nx: nx && efer & (1 << 11) != 0,
        root,
        map: [MemmapEntry {
            base: 0,
            length: 0,
            typ: 0,
        }; 512],
        map_len: 0,
        ours: false,
    };
    c.ours = bootloader_name(&c) == Some("Optimal Boot");
    // PA0..PA5 are promised (WB WT UC- UC WP WC); PA6 and PA7 are not.
    // Optimal Boot also fixes PA6 = UC- and PA7 = UC.
    let mask = if c.ours {
        u64::MAX
    } else {
        0x0000_ffff_ffff_ffff
    };
    check!(
        t,
        "pat",
        pat & mask == paging::PAT_VALUE & mask,
        "PAT MSR {pat:#x}, expected {:#x}",
        paging::PAT_VALUE
    );
    let ok = offset & (PAGE - 1) == 0
        && paging::canonical(offset, levels)
        && offset >> 63 == 1
        && c.phys(hhdm_resp).is_some_and(|p| p + offset == hhdm_resp);
    check!(
        t,
        "hhdm",
        ok,
        "offset {offset:#x} does not map the HHDM response at {hhdm_resp:#x}"
    );
    if !ok {
        return None;
    }
    let pm = req::response(&req::PAGING);
    if c.mapped(pm, 16, false) {
        let mode = rd64(pm + 8);
        let want_mode = if levels == 5 {
            lp::PAGING_MODE_5LVL
        } else {
            lp::PAGING_MODE_4LVL
        };
        check!(
            t,
            "paging-mode-response",
            mode == want_mode,
            "response says mode {mode}, CR4 says {levels} levels"
        );
    } else {
        t.fail(
            "paging-mode-response",
            format_args!("missing or unmapped ({pm:#x})"),
        );
    }
    Some(c)
}

const GDT_WANT: [u64; 7] = [
    0,
    0x0000_9a00_0000_ffff,
    0x0000_9200_0000_ffff,
    0x00cf_9a00_0000_ffff,
    0x00cf_9200_0000_ffff,
    0x00af_9a00_0000_ffff,
    0x00cf_9200_0000_ffff,
];

fn gdt(t: &mut T, c: &Ctx) {
    let mut gdtr = [0u8; 10];
    // SAFETY: sgdt stores 10 bytes.
    unsafe { asm!("sgdt [{}]", in(reg) gdtr.as_mut_ptr()) };
    let limit = u16::from_le_bytes([gdtr[0], gdtr[1]]) as u64;
    let base = u64::from_le_bytes(gdtr[2..].try_into().unwrap());
    if limit < 7 * 8 - 1 || !c.mapped(base, 7 * 8, false) {
        t.fail(
            "gdt",
            format_args!("GDTR base {base:#x} limit {limit:#x} is too small or unmapped"),
        );
        return;
    }
    let mut bad = None;
    for (i, w) in GDT_WANT.iter().enumerate() {
        // The CPU may set the accessed bit (bit 40) on loaded descriptors.
        // In long mode a data segment's base, limit and flags are ignored,
        // so for the 64-bit data descriptor only the access byte is promised
        // (Limine uses limit 0, Optimal Boot the flat 4 GiB form).
        // The same holds for the 64-bit code descriptor apart from L and D.
        let mask = match i {
            5 if !c.ours => 0x0060_ff00_0000_0000,
            6 if !c.ours => 0x0000_ff00_0000_0000,
            _ => u64::MAX,
        };
        let got = rd64(base + 8 * i as u64) & !(1 << 40) & mask;
        if got != *w & mask {
            bad = Some((i, got));
        }
    }
    check!(
        t,
        "gdt",
        bad.is_none(),
        "descriptor {:?} differs from the spec",
        bad
    );
    let p = c.phys(base).unwrap_or(0);
    check!(
        t,
        "gdt-memory",
        matches!(
            c.type_of(p, 56),
            Some(lp::MEMMAP_BOOTLOADER_RECLAIMABLE) | Some(lp::MEMMAP_EXECUTABLE_AND_MODULES)
        ),
        "GDT at physical {p:#x} is in memory of type {:?}",
        c.type_of(p, 56)
    );
}

fn memory_map(t: &mut T, c: &mut Ctx) {
    // The map is needed to judge memory types, so read it first through
    // plain mappings and check where it lives afterwards.
    let r = req::response(&req::MEMMAP);
    if r == 0 || !c.mapped(r, 24, false) {
        t.fail("memmap", format_args!("no response, or unmapped at {r:#x}"));
        return;
    }
    let (n, arr) = (rd64(r + 8), rd64(r + 16));
    if n == 0 || n > 512 || !c.mapped(arr, n * 8, false) {
        t.fail("memmap", format_args!("{n} entries at {arr:#x}"));
        return;
    }
    for i in 0..n {
        let e = rd64(arr + 8 * i);
        if !c.mapped(e, 24, false) {
            t.fail("memmap", format_args!("entry {i} at unmapped {e:#x}"));
            return;
        }
        c.map[i as usize] = MemmapEntry {
            base: rd64(e),
            length: rd64(e + 8),
            typ: rd64(e + 16),
        };
    }
    c.map_len = n as usize;
    let rc = lp::MEMMAP_BOOTLOADER_RECLAIMABLE;
    let bad_ptr = (0..n)
        .filter(|&i| !c.virt_in(rd64(arr + 8 * i), 24, rc))
        .count()
        + !c.virt_in(r, 24, rc) as usize
        + !c.virt_in(arr, n * 8, rc) as usize;
    check!(
        t,
        "memmap-reclaimable",
        bad_ptr == 0,
        "{bad_ptr} pieces of the response are not in bootloader-reclaimable memory"
    );
    let m = c.map();
    let max_type = if c.rev >= 4 {
        lp::MEMMAP_RESERVED_MAPPED
    } else {
        lp::MEMMAP_FRAMEBUFFER
    };
    let bad_type = m.iter().position(|e| e.typ > max_type);
    check!(
        t,
        "memmap-types",
        bad_type.is_none(),
        "entry {:?} has type {}",
        bad_type,
        bad_type.map_or(0, |i| m[i].typ)
    );
    let empty = m
        .iter()
        .position(|e| e.length == 0 || e.base.checked_add(e.length).is_none());
    check!(
        t,
        "memmap-nonempty",
        empty.is_none(),
        "entry {empty:?} is empty or wraps"
    );
    let order = (1..m.len()).find(|&i| m[i].base < m[i - 1].end());
    check!(
        t,
        "memmap-sorted-disjoint",
        order.is_none(),
        "entry {order:?} overlaps or precedes the one before it"
    );
    let aligned = m.iter().position(|e| {
        matches!(e.typ, lp::MEMMAP_USABLE | lp::MEMMAP_BOOTLOADER_RECLAIMABLE)
            && (e.base | e.length) & (PAGE - 1) != 0
    });
    check!(
        t,
        "memmap-usable-aligned",
        aligned.is_none(),
        "usable/reclaimable entry {aligned:?} is not page aligned"
    );
    let usable: u64 = m
        .iter()
        .filter(|e| e.typ == lp::MEMMAP_USABLE)
        .map(|e| e.length)
        .sum();
    check!(
        t,
        "memmap-has-usable",
        usable >= 16 << 20,
        "only {usable} bytes usable"
    );
    println!(
        "info memmap: {} entries, {} MiB usable",
        m.len(),
        usable >> 20
    );
}

/// Every page the spec says is in the HHDM is mapped there, at the right
/// physical address, writable, never executable, with the right cache type;
/// and the identity map is gone.
fn hhdm_mapping(t: &mut T, c: &Ctx) {
    let mut wrong = 0u64;
    let mut first_bad = (0u64, None);
    let mut pages = 0u64;
    let mut reserved_mapped = 0u64;
    for e in c.map() {
        let start = e.base & !(PAGE - 1);
        let end = (e.end() + PAGE - 1) & !(PAGE - 1);
        let span = (end - start) / PAGE;
        // Probe every page of small entries, and 64 spread pages plus both ends of large ones.
        let step = if span <= 256 { 1 } else { span / 64 };
        let mut i = 0;
        while i < span {
            let p = start + i * PAGE;
            let tr = c.tr(p + c.hhdm);
            if in_hhdm(e.typ, c.rev) {
                pages += 1;
                let wc = e.typ == lp::MEMMAP_FRAMEBUFFER;
                let good = tr.is_some_and(|x| {
                    x.phys == p
                        && x.write
                        && (!c.nx || !x.exec || !c.ours)
                        && !x.user
                        && (if wc {
                            x.pat_index == 5
                        } else {
                            x.pat_index == 0
                        })
                });
                if !good {
                    wrong += 1;
                    if first_bad.1.is_none() {
                        first_bad = (p, Some(tr));
                    }
                }
            } else if tr.is_some() && e.typ != lp::MEMMAP_BAD_MEMORY {
                reserved_mapped += 1;
            } else if tr.is_some() {
                wrong += 1;
            }
            i = if i + step >= span && i != span - 1 {
                span - 1
            } else {
                i + step
            };
        }
    }
    check!(
        t,
        "hhdm-map",
        wrong == 0,
        "{wrong} of {pages} probed pages wrong; first at physical {:#x}: {:?}",
        first_bad.0,
        first_bad.1
    );
    if reserved_mapped != 0 {
        println!("info hhdm: {reserved_mapped} probed pages of unmapped types are mapped anyway");
    }
    // Identity map removed: the lowest usable page must not be mapped at its physical address.
    let low = c
        .map()
        .iter()
        .find(|e| e.typ == lp::MEMMAP_USABLE)
        .map(|e| e.base);
    if let Some(l) = low {
        check!(
            t,
            "no-identity-map",
            c.tr(l).is_none() && c.tr(0).is_none(),
            "physical {l:#x} is identity mapped"
        );
    }
}

fn kernel_image(t: &mut T, c: &Ctx) {
    let start = sym_addr(&raw const __image_start);
    let end = sym_addr(&raw const __image_end);
    let Some(r) = c.need(t, "executable-address", &req::EXEC_ADDR, 3) else {
        return;
    };
    let (pbase, vbase) = (rd64(r + 8), rd64(r + 16));
    let pie = option_env!("OB_PIE") == Some("1");
    let vbase_ok =
        vbase == start && vbase >= lp::KERNEL_BASE_MIN && (pie || vbase == 0xffff_ffff_8000_0000);
    check!(
        t,
        "executable-address",
        vbase_ok && pbase & (PAGE - 1) == 0,
        "virtual base {vbase:#x} (image starts at {start:#x}), physical base {pbase:#x}"
    );
    // Contiguous: every page of the image maps to physical base + offset.
    let mut bad = None;
    let mut p = start;
    while p < end {
        if c.phys(p) != Some(pbase + (p - start)) {
            bad = Some(p);
            break;
        }
        p += PAGE;
    }
    check!(
        t,
        "kernel-contiguous",
        bad.is_none(),
        "page {bad:?} is not at physical base + offset"
    );
    let len = end - start;
    check!(
        t,
        "kernel-memory-type",
        c.type_of(pbase, len) == Some(lp::MEMMAP_EXECUTABLE_AND_MODULES),
        "kernel at {pbase:#x}+{len:#x} is in memory of type {:?}",
        c.type_of(pbase, len)
    );
    // Permissions per segment.
    let seg = |a: u64, b: u64, w: bool, x: bool| -> Option<u64> {
        let mut p = a & !(PAGE - 1);
        while p < b {
            match c.tr(p) {
                Some(tr) if tr.write == w && (!c.nx || tr.exec == x) && !tr.user => {}
                _ => return Some(p),
            }
            p += PAGE;
        }
        None
    };
    let text = seg(
        sym_addr(&raw const __text_start),
        sym_addr(&raw const __text_end),
        false,
        true,
    );
    check!(
        t,
        "kernel-text-rx",
        text.is_none(),
        "text page {text:x?} is not read+execute only"
    );
    let ro = seg(
        sym_addr(&raw const __rodata_start),
        sym_addr(&raw const __rodata_end),
        false,
        false,
    );
    check!(
        t,
        "kernel-rodata-r",
        ro.is_none(),
        "rodata page {ro:x?} is not read only"
    );
    let data = seg(
        sym_addr(&raw const __data_start),
        sym_addr(&raw const __data_end),
        true,
        false,
    );
    check!(
        t,
        "kernel-data-rw",
        data.is_none(),
        "data page {data:x?} is not read+write, no execute"
    );
    // Relocations applied: a pointer stored in data equals the code address.
    let stored = req::ENTRY_POINT.entry as usize as u64;
    // SAFETY: a volatile read of our own static defeats constant folding.
    let stored_v =
        unsafe { core::ptr::read_volatile(&raw const req::ENTRY_POINT.entry as *const u64) };
    let actual: u64;
    // SAFETY: lea has no side effects.
    unsafe { asm!("lea {}, [rip + {}]", out(reg) actual, sym crate::alt_start) };
    check!(
        t,
        "relocations",
        stored_v == actual && stored == actual,
        "stored pointer {stored_v:#x}, code is at {actual:#x}"
    );
}

fn stack(t: &mut T, c: &Ctx, es: &crate::EntryState) {
    let top = es.gprs[7] + 8;
    let bottom = top - req::STACK_SIZE;
    let mut bad = None;
    let mut p = bottom;
    while p < top {
        let ok = c
            .tr(p)
            .is_some_and(|x| x.write && (!c.nx || !x.exec || !c.ours))
            && c.phys(p).and_then(|ph| c.type_of(ph, 1)) == Some(lp::MEMMAP_BOOTLOADER_RECLAIMABLE);
        if !ok {
            bad = Some(p);
            break;
        }
        p += PAGE;
    }
    check!(
        t,
        "stack",
        bad.is_none() && top >> 63 == 1,
        "the {} KiB stack below {top:#x} is not all writable bootloader-reclaimable memory (page {bad:x?})",
        req::STACK_SIZE / 1024
    );
    if c.need(t, "stack-size", &req::STACK, 1).is_some() {
        t.ok("stack-size");
    }
}

fn wx_probes(t: &mut T, c: &Ctx) {
    let text = sym_addr(&raw const __text_start);
    let ro = sym_addr(&raw const __rodata_start);
    let data = RET_DATA.as_ptr() as u64;
    let pf_write = |f: Option<Fault>, a: u64| {
        f.is_some_and(|f| f.vector == 14 && f.error & 3 == 3 && f.cr2 == a)
    };
    let f = probe_write(text);
    check!(
        t,
        "wx-text-not-writable",
        pf_write(f, text),
        "writing kernel text gave {f:?}"
    );
    let f = probe_write(ro);
    check!(
        t,
        "wx-rodata-not-writable",
        pf_write(f, ro),
        "writing kernel rodata gave {f:?}"
    );
    let f = probe_write(data);
    check!(
        t,
        "wx-data-writable",
        f.is_none(),
        "writing kernel data gave {f:?}"
    );
    if c.nx {
        let f = probe_exec(data);
        check!(
            t,
            "wx-data-not-executable",
            f.is_some_and(|f| f.vector == 14 && f.error & 0x11 == 0x11 && f.cr2 == data),
            "executing kernel data gave {f:?}"
        );
        let alias = c.phys(data).map_or(0, |p| p + c.hhdm);
        let f = if c.ours { probe_exec(alias) } else { None };
        if !c.ours {
            t.skip(
                "ob-hhdm-not-executable",
                "an Optimal Boot guarantee, not a spec promise",
            );
        } else {
            check!(
                t,
                "ob-hhdm-not-executable",
                f.is_some_and(|f| f.vector == 14 && f.cr2 == alias),
                "executing the HHDM alias {alias:#x} gave {f:?}; translation {:?}",
                c.tr(alias)
            );
        }
    } else {
        t.skip("wx-data-not-executable", "the CPU has no NX");
    }
    let f = probe_exec(ret_in_text());
    check!(
        t,
        "wx-text-executable",
        f.is_none(),
        "executing kernel text gave {f:?}"
    );
    let f = probe_read(0);
    check!(
        t,
        "null-unmapped",
        f.is_some_and(|f| f.vector == 14 && f.error & 1 == 0),
        "reading address 0 gave {f:?}"
    );
}

/// Address of a `ret` in kernel text.
fn ret_in_text() -> u64 {
    let a: u64;
    // SAFETY: lea has no side effects; the label is a ret in text.
    unsafe { asm!("lea {}, [rip + {}]", out(reg) a, sym text_ret) };
    a
}
#[unsafe(naked)]
unsafe extern "C" fn text_ret() {
    core::arch::naked_asm!("ret");
}

fn simple_responses(t: &mut T, c: &Ctx) {
    if let Some(r) = c.need(t, "bootloader-info", &req::BOOTLOADER_INFO, 3) {
        let (n, v) = (cstr(c, rd64(r + 8)), cstr(c, rd64(r + 16)));
        check!(
            t,
            "bootloader-info",
            n.is_some_and(|s| !s.is_empty()) && v.is_some_and(|s| !s.is_empty()),
            "name {n:?} version {v:?}"
        );
        println!("info bootloader: {} {}", n.unwrap_or("?"), v.unwrap_or("?"));
    }
    if let Some(r) = c.need(t, "firmware-type", &req::FIRMWARE_TYPE, 2) {
        let ft = rd64(r + 8);
        check!(
            t,
            "firmware-type",
            ft == lp::FIRMWARE_TYPE_EFI64 || ft == lp::FIRMWARE_TYPE_X86BIOS,
            "type {ft}"
        );
    }
    if let Some(r) = c.need(t, "cmdline", &req::CMDLINE, 2) {
        let s = cstr(c, rd64(r + 8));
        let want = option_env!("OB_EXPECT_CMDLINE");
        check!(
            t,
            "cmdline",
            s.is_some() && want.is_none_or(|w| s == Some(w)),
            "cmdline {s:?}, expected {want:?}"
        );
        println!("info cmdline: {:?}", s.unwrap_or(""));
    }
    if let Some(r) = c.need(t, "date", &req::DATE, 2) {
        let ts = rd64(r + 8) as i64;
        // 2024-01-01 .. 2100-01-01.
        check!(
            t,
            "date",
            (1_704_067_200..4_102_444_800).contains(&ts),
            "timestamp {ts}"
        );
    }
    check!(
        t,
        "dtb-absent",
        req::response(&req::DTB) == 0,
        "a device tree was given on x86-64 ACPI firmware"
    );
    if req::ENTRY_REQ {
        let r = req::entry_response();
        check!(
            t,
            "entry-point-response",
            c.virt_in(r, 8, lp::MEMMAP_BOOTLOADER_RECLAIMABLE),
            "response {r:#x}"
        );
    }
}

fn framebuffer(t: &mut T, c: &Ctx) {
    let Some(r) = c.want(
        t,
        "framebuffer",
        &req::FRAMEBUFFER,
        3,
        "no framebuffer (headless)",
    ) else {
        return;
    };
    let (rrev, n, arr) = (rd64(r), rd64(r + 8), rd64(r + 16));
    if n == 0 || n > 64 || !c.mapped(arr, n * 8, false) {
        t.fail("framebuffer", format_args!("{n} framebuffers at {arr:#x}"));
        return;
    }
    for i in 0..n {
        let f = rd64(arr + 8 * i);
        let size = if rrev >= 2 {
            88
        } else if rrev >= 1 {
            80
        } else {
            64
        };
        if !c.virt_in(f, size, lp::MEMMAP_BOOTLOADER_RECLAIMABLE) {
            t.fail(
                "framebuffer",
                format_args!("descriptor {f:#x} is not in reclaimable memory"),
            );
            continue;
        }
        // SAFETY: proven mapped above; the struct is read field by field.
        let fb: lp::Framebuffer = unsafe { core::ptr::read_volatile(f as *const lp::Framebuffer) };
        let bpp = fb.bpp as u64;
        let masks = [
            (fb.red_mask_size, fb.red_mask_shift),
            (fb.green_mask_size, fb.green_mask_shift),
            (fb.blue_mask_size, fb.blue_mask_shift),
        ];
        let mut bits = 0u64;
        let mut overlap = false;
        for (s, sh) in masks {
            let m = ((1u64 << s) - 1) << sh;
            overlap |= bits & m != 0 || sh as u64 + s as u64 > bpp;
            bits |= m;
        }
        let geom = fb.width > 0
            && fb.height > 0
            && bpp >= 8
            && fb.pitch >= fb.width * bpp / 8
            && fb.memory_model == lp::FRAMEBUFFER_RGB
            && !overlap;
        check!(
            t,
            "framebuffer-geometry",
            geom,
            "{}x{} pitch {} bpp {} masks {masks:?}",
            fb.width,
            fb.height,
            fb.pitch,
            bpp
        );
        let len = fb.pitch * fb.height;
        let phys = fb.address.wrapping_sub(c.hhdm);
        check!(
            t,
            "framebuffer-memory",
            c.type_of(phys, len) == Some(lp::MEMMAP_FRAMEBUFFER) && c.mapped(fb.address, len, true),
            "framebuffer {phys:#x}+{len:#x} is type {:?}",
            c.type_of(phys, len)
        );
        if c.mapped(fb.address, len, true) && bpp == 32 {
            // Paint a gradient and read a few pixels back.
            let mut ok = true;
            for y in 0..fb.height {
                for x in 0..fb.width {
                    let px = (x * 255 / fb.width) << fb.red_mask_shift
                        | (y * 255 / fb.height) << fb.green_mask_shift
                        | 0x80 << fb.blue_mask_shift;
                    let a = fb.address + y * fb.pitch + x * 4;
                    // SAFETY: inside the mapped, writable framebuffer.
                    unsafe { core::ptr::write_volatile(a as *mut u32, px as u32) };
                    if (x ^ y) & 0x3ff == 0 {
                        // SAFETY: as above.
                        ok &= unsafe { core::ptr::read_volatile(a as *const u32) } == px as u32;
                    }
                }
            }
            check!(t, "framebuffer-write", ok, "pixels did not read back");
        }
        if fb.edid_size > 0 {
            let ok = c.mapped(fb.edid, fb.edid_size, false) && fb.edid_size >= 128 && {
                let e = bytes(fb.edid, 128);
                e[..8] == [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0] && sum8(e) == 0
            };
            check!(
                t,
                "framebuffer-edid",
                ok,
                "EDID of {} bytes at {:#x} is malformed",
                fb.edid_size,
                fb.edid
            );
        }
        if rrev >= 1 && fb.mode_count > 0 {
            let mut ok = c.mapped(fb.modes, fb.mode_count * 8, false);
            let mut current = false;
            for k in 0..fb.mode_count {
                if !ok {
                    break;
                }
                let m = rd64(fb.modes + 8 * k);
                ok &= c.virt_in(m, 40, lp::MEMMAP_BOOTLOADER_RECLAIMABLE);
                if ok {
                    // SAFETY: proven mapped.
                    let vm: lp::VideoMode =
                        unsafe { core::ptr::read_volatile(m as *const lp::VideoMode) };
                    ok &= vm.width > 0 && vm.height > 0 && vm.pitch >= vm.width * vm.bpp as u64 / 8;
                    current |= vm.width == fb.width && vm.height == fb.height;
                }
            }
            check!(
                t,
                "framebuffer-modes",
                ok && current,
                "{} modes; valid {ok}, current mode listed {current}",
                fb.mode_count
            );
        }
        println!(
            "info framebuffer {i}: {}x{} bpp {} at physical {phys:#x}",
            fb.width, fb.height, bpp
        );
    }
}

fn file_ok(c: &Ctx, f: u64) -> Result<lp::File, &'static str> {
    file_ok_in(c, f, &[lp::MEMMAP_EXECUTABLE_AND_MODULES])
}

/// The executable file's copy may sit in bootloader-reclaimable memory (as
/// Limine does) or with the modules (as Optimal Boot does): the spec only
/// promises the modules' memory type.
fn exec_file_ok(c: &Ctx, f: u64) -> Result<lp::File, &'static str> {
    if c.ours {
        file_ok(c, f)
    } else {
        file_ok_in(
            c,
            f,
            &[
                lp::MEMMAP_EXECUTABLE_AND_MODULES,
                lp::MEMMAP_BOOTLOADER_RECLAIMABLE,
            ],
        )
    }
}

fn file_ok_in(c: &Ctx, f: u64, types: &[u64]) -> Result<lp::File, &'static str> {
    if !c.virt_in(f, 112, lp::MEMMAP_BOOTLOADER_RECLAIMABLE) {
        return Err("descriptor not in reclaimable memory");
    }
    // SAFETY: proven mapped.
    let file: lp::File = unsafe { core::ptr::read_volatile(f as *const lp::File) };
    if file.size == 0 || file.address & (PAGE - 1) != 0 {
        return Err("empty or unaligned");
    }
    let phys = file.address.wrapping_sub(c.hhdm);
    if !c
        .type_of(phys, file.size)
        .is_some_and(|t| types.contains(&t))
        || !c.mapped(file.address, file.size, false)
    {
        return Err("contents not in the right memory type");
    }
    if cstr(c, file.path).is_none_or(|p| p.is_empty()) || cstr(c, file.string).is_none() {
        return Err("bad path or string");
    }
    if file.media_type > 2 {
        return Err("bad media type");
    }
    Ok(file)
}

/// The disk scripts/build-images.sh writes: GPT disk GUID
/// 0B0071A1-0000-4000-8000-00000000D15C, one partition with GUID
/// 0B0071A1-0000-4000-8000-0000000AA27A. Limine's UUID struct is the GPT
/// on-disk encoding: first three fields little endian, last 8 bytes as is.
const DISK_GUID: lp::Uuid = lp::Uuid {
    a: 0x0b00_71a1,
    b: 0,
    c: 0x4000,
    d: [0x80, 0, 0, 0, 0, 0, 0xd1, 0x5c],
};
const PART_GUID: lp::Uuid = lp::Uuid {
    a: 0x0b00_71a1,
    b: 0,
    c: 0x4000,
    d: [0x80, 0, 0, 0, 0, 0x0a, 0xa2, 0x7a],
};

/// With OB_EXPECT_VOLUME=gpt (the kernel is booted from build-images.sh's
/// disk), the executable file's volume must be that disk's partition 1.
/// This is the end-to-end check of the device-path reading in core::devpath
/// and of the GPT header read that finds the disk GUID.
fn boot_volume(t: &mut T, c: &Ctx, f: &lp::File) {
    if option_env!("OB_EXPECT_VOLUME") != Some("gpt") {
        return;
    }
    if !c.ours {
        t.skip(
            "ob-boot-volume",
            "the reference boots its own image, with other GUIDs",
        );
        return;
    }
    check!(
        t,
        "ob-boot-volume",
        f.media_type == 0
            && f.partition_index == 1
            && f.mbr_disk_id == 0
            && f.gpt_disk_uuid == DISK_GUID
            && f.gpt_part_uuid == PART_GUID,
        "media {} partition {} mbr {:#x} disk {:x?} part {:x?}",
        f.media_type,
        f.partition_index,
        f.mbr_disk_id,
        f.gpt_disk_uuid,
        f.gpt_part_uuid
    );
}

fn files(t: &mut T, c: &Ctx) {
    match c
        .need(t, "executable-file", &req::EXEC_FILE, 2)
        .map(|r| exec_file_ok(c, rd64(r + 8)))
    {
        Some(Ok(f)) => {
            let b = bytes(f.address, f.size.min(64));
            let elf = b.len() >= 20 && b[..4] == *b"\x7fELF" && b[4] == 2 && b[18] == 0x3e;
            check!(t, "executable-file", elf, "the file is not an x86-64 ELF64");
            let cmd = req::response(&req::CMDLINE);
            if cmd != 0 && c.mapped(cmd, 16, false) {
                check!(
                    t,
                    "executable-file-string",
                    cstr(c, f.string) == cstr(c, rd64(cmd + 8)),
                    "file string differs from the cmdline"
                );
            }
            println!(
                "info executable: {} ({} bytes)",
                cstr(c, f.path).unwrap_or("?"),
                f.size
            );
            boot_volume(t, c, &f);
        }
        Some(Err(e)) => t.fail("executable-file", format_args!("{e}")),
        None => {}
    }
    let want: Option<u64> = option_env!("OB_EXPECT_MODULES").map(|s| s.parse().unwrap_or(0));
    let r = match c.resp(t, "modules", &req::MODULES, 3) {
        Some(0) => {
            check!(
                t,
                "modules",
                want.is_none_or(|w| w == 0),
                "no module response, expected {want:?}"
            );
            return;
        }
        Some(r) => r,
        None => return,
    };
    let (n, arr) = (rd64(r + 8), rd64(r + 16));
    if !c.mapped(arr, n * 8, false) {
        t.fail("modules", format_args!("{n} modules at unmapped {arr:#x}"));
        return;
    }
    check!(
        t,
        "modules-count",
        want.is_none_or(|w| w == n),
        "{n} modules, expected {want:?}"
    );
    for i in 0..n {
        match file_ok(c, rd64(arr + 8 * i)) {
            Ok(f) => {
                let s = cstr(c, f.string).unwrap_or("");
                // A module whose string is "pattern" holds byte i = i*167+13.
                if s == "pattern" {
                    let b = bytes(f.address, f.size);
                    let bad = b
                        .iter()
                        .enumerate()
                        .position(|(i, &x)| x != (i as u64 * 167 + 13) as u8);
                    check!(
                        t,
                        "module-contents",
                        bad.is_none(),
                        "pattern module differs at byte {bad:?}"
                    );
                } else {
                    t.ok("module");
                }
                println!(
                    "info module {i}: {} ({} bytes) {s:?}",
                    cstr(c, f.path).unwrap_or("?"),
                    f.size
                );
            }
            Err(e) => t.fail("module", format_args!("module {i}: {e}")),
        }
    }
}

/// Pointers to firmware tables: physical in old revisions, HHDM in new ones.
fn table_ptr(c: &Ctx, raw: u64, virt_from: u64) -> (u64, bool) {
    if c.rev >= virt_from {
        (raw, raw >> 63 == 1)
    } else {
        (raw + c.hhdm, raw >> 63 == 0)
    }
}

fn firmware_tables(t: &mut T, c: &Ctx) {
    if let Some(r) = c.need(t, "rsdp", &req::RSDP, 2) {
        {
            let (v, form_ok) = table_ptr(c, rd64(r + 8), 4);
            if !form_ok {
                t.fail(
                    "rsdp",
                    format_args!(
                        "pointer {:#x} has the wrong form for revision {}",
                        rd64(r + 8),
                        c.rev
                    ),
                );
            } else if c.mapped(v, 36, false) {
                let b = bytes(v, 36);
                let ok =
                    b[..8] == *b"RSD PTR " && sum8(&b[..20]) == 0 && (b[15] < 2 || sum8(b) == 0);
                check!(t, "rsdp", ok, "bad signature or checksum");
            } else if c.rev >= 4 {
                t.fail("rsdp", format_args!("RSDP at {v:#x} is not mapped"));
            } else {
                t.skip("rsdp-contents", "revision 3 does not map ACPI memory");
                t.ok("rsdp");
            }
        }
    }
    if let Some(r) = c.want(t, "smbios", &req::SMBIOS, 3, "no SMBIOS") {
        {
            let (e32, e64) = (rd64(r + 8), rd64(r + 16));
            let mut ok = e32 != 0 || e64 != 0;
            for (raw, sig, len_at) in [(e32, &b"_SM_"[..], 5usize), (e64, &b"_SM3_"[..], 6)] {
                if raw == 0 {
                    continue;
                }
                let (v, form) = table_ptr(c, raw, 5);
                ok &= form;
                if form && c.mapped(v, 32, false) {
                    let len = bytes(v, 32)[len_at] as u64;
                    ok &= c.mapped(v, len, false)
                        && bytes(v, len.min(32))[..sig.len()] == *sig
                        && sum8(bytes(v, len)) == 0;
                } else if c.rev >= 5 {
                    ok = false;
                }
            }
            check!(
                t,
                "smbios",
                ok,
                "entry points {e32:#x} {e64:#x} are malformed"
            );
        }
    }
    if let Some(r) = c.need(t, "efi-system-table", &req::EFI_ST, 2) {
        {
            let (v, form) = table_ptr(c, rd64(r + 8), 5);
            let ok = form
                && if c.mapped(v, 8, false) {
                    rd64(v) == 0x5453_5953_2049_4249
                } else {
                    c.rev < 5
                };
            check!(
                t,
                "efi-system-table",
                ok,
                "system table {:#x} is malformed or unmapped",
                rd64(r + 8)
            );
        }
    }
}

fn efi_memmap(t: &mut T, c: &Ctx) {
    let Some(r) = c.need(t, "efi-memmap", &req::EFI_MEMMAP, 5) else {
        return;
    };
    let (m, size, dsize, dver) = (rd64(r + 8), rd64(r + 16), rd64(r + 24), rd64(r + 32));
    if dsize < 40 || size == 0 || size % dsize != 0 || dver != 1 || !c.mapped(m, size, false) {
        t.fail(
            "efi-memmap",
            format_args!("map {m:#x} size {size} descriptor {dsize} version {dver}"),
        );
        return;
    }
    t.ok("efi-memmap");
    // Usable Limine memory must be memory UEFI called free (or boot-time),
    // and nothing UEFI keeps (runtime, ACPI, MMIO, reserved) may be usable.
    let mut bad = None;
    for i in 0..size / dsize {
        let d = m + i * dsize;
        let (typ, phys, pages) = (rd64(d) as u32, rd64(d + 8), rd64(d + 24));
        let keep = !matches!(typ, 1..=4 | 7);
        if !keep {
            continue;
        }
        let end = phys + pages * PAGE;
        if let Some(e) = c
            .map()
            .iter()
            .find(|e| e.typ == lp::MEMMAP_USABLE && e.base < end && phys < e.end())
        {
            bad = Some((typ, phys, e.base));
            break;
        }
    }
    check!(
        t,
        "efi-memmap-agrees",
        bad.is_none(),
        "UEFI type/base/usable base {bad:x?}: firmware memory is marked usable"
    );
}

fn entropy(t: &mut T, c: &Ctx) {
    let Some(r) = c.want(
        t,
        "entropy",
        &req::ENTROPY,
        3,
        "not supported by this loader",
    ) else {
        return;
    };
    let (n, a) = (rd64(r + 8), rd64(r + 16));
    if n != req::ENTROPY_COUNT || !c.virt_in(a, n * 8, lp::MEMMAP_BOOTLOADER_RECLAIMABLE) {
        t.fail(
            "entropy",
            format_args!("{n} values at {a:#x}, expected {}", req::ENTROPY_COUNT),
        );
        return;
    }
    let mut v = [0u64; 16];
    for (i, x) in v.iter_mut().enumerate() {
        *x = rd64(a + 8 * i as u64);
    }
    let distinct = (0..16).all(|i| (0..i).all(|j| v[i] != v[j]));
    // 16 x 64 bits: expect ~512 ones; < 400 or > 624 is a 7-sigma event.
    let ones: u32 = v.iter().map(|x| x.count_ones()).sum();
    check!(
        t,
        "entropy",
        distinct && (400..=624).contains(&ones),
        "values not distinct ({distinct}) or biased ({ones} ones of 1024)"
    );
}

/// TSC ticks per second measured against PIT channel 2 over ~50 ms.
fn pit_tsc_hz() -> u64 {
    let g = inb(0x61);
    outb(0x61, (g & !0x02) | 0x01);
    outb(0x43, 0b1011_0000); // channel 2, lo/hi, mode 0, binary
    let count: u64 = 59_659;
    outb(0x42, count as u8);
    let t0 = rdtsc();
    outb(0x42, (count >> 8) as u8);
    let mut guard = 0u64;
    while inb(0x61) & 0x20 == 0 && guard < 100_000_000 {
        guard += 1;
    }
    let t1 = rdtsc();
    outb(0x61, g);
    (t1 - t0) * 1_193_182 / count
}

fn timing(t: &mut T, c: &Ctx) {
    let tsc = c
        .want(t, "tsc-frequency", &req::TSC, 2, "not given")
        .map(|r| rd64(r + 8));
    if let Some(hz) = tsc {
        let pit = pit_tsc_hz();
        let diff = hz.abs_diff(pit);
        check!(
            t,
            "tsc-frequency",
            (100_000_000..20_000_000_000).contains(&hz) && diff * 50 <= pit,
            "loader says {hz} Hz, PIT measures {pit} Hz (more than 2% apart)"
        );
    }
    if let Some(r) = c.need(t, "performance", &req::PERF, 4) {
        let (reset, init, exec) = (rd64(r + 8), rd64(r + 16), rd64(r + 24));
        check!(
            t,
            "performance",
            reset <= init && init <= exec && exec < 600_000_000,
            "reset {reset} init {init} exec {exec} us"
        );
        println!(
            "info performance: init at {} us, kernel entry at {} us",
            init, exec
        );
    }
}

fn tpm_log(t: &mut T, c: &Ctx) {
    let Some(r) = c.want(t, "tpm-event-log", &req::TPM_LOG, 4, "no TPM") else {
        return;
    };
    let (fmt, size, log) = (rd64(r + 8), rd64(r + 16), rd64(r + 24));
    if fmt != lp::TPM_EVENT_LOG_FORMAT_TCG_2 || size < 32 || !c.mapped(log, size, false) {
        t.fail(
            "tpm-event-log",
            format_args!("format {fmt}, {size} bytes at {log:#x}"),
        );
        return;
    }
    let b = bytes(log, size);
    let Some((mut at, algs, n)) = tcg::spec_id(b) else {
        t.fail("tpm-event-log", format_args!("no Spec ID event"));
        return;
    };
    let algs = &algs[..n];
    let mut events = 0;
    let mut kernel_digest_ok = None;
    let file = req::response(&req::EXEC_FILE);
    while at < b.len() {
        let Some(len) = tcg::event2_size(b, at, algs) else {
            break;
        };
        let pcr = u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        let typ = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap());
        if pcr == 0 && typ == 0 {
            break; // zeroed tail
        }
        events += 1;
        // Find the SHA-256 digest and the event data.
        let count = u32::from_le_bytes(b[at + 8..at + 12].try_into().unwrap());
        let mut o = at + 12;
        let mut sha256 = None;
        for _ in 0..count {
            let alg = u16::from_le_bytes([b[o], b[o + 1]]);
            let sz = algs.iter().find(|a| a.0 == alg).map_or(0, |a| a.1 as usize);
            if alg == 0x000b {
                sha256 = Some(&b[o + 2..o + 2 + sz]);
            }
            o += 2 + sz;
        }
        let dlen = u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
        let data = &b[o + 4..o + 4 + dlen];
        if pcr == 9
            && typ == 0xd
            && data.starts_with(b"optimal-boot kernel ")
            && file != 0
            && let (Some(d), Ok(f)) = (sha256, exec_file_ok(c, rd64(file + 8)))
        {
            kernel_digest_ok = Some(d == sig::sha256(bytes(f.address, f.size)));
        }
        at += len;
    }
    check!(t, "tpm-event-log", events > 0, "no events after the header");
    if c.ours {
        check!(
            t,
            "tpm-kernel-measured",
            kernel_digest_ok == Some(true),
            "PCR 9 kernel event: {kernel_digest_ok:?}"
        );
    }
    println!("info tpm: {events} events");
}

fn bootloader_name(c: &Ctx) -> Option<&'static str> {
    let r = req::response(&req::BOOTLOADER_INFO);
    if r == 0 || !c.mapped(r, 24, false) {
        return None;
    }
    cstr(c, rd64(r + 8))
}

/// Calls `f(virt, size, write, exec)` for every present leaf mapping.
fn walk(
    c: &Ctx,
    table: u64,
    level: u32,
    base: u64,
    w: bool,
    x: bool,
    f: &mut impl FnMut(u64, u64, bool, bool),
) {
    let entries = *Hhdm(c.hhdm).table(table);
    for (i, &e) in entries.iter().enumerate() {
        if e & paging::P == 0 {
            continue;
        }
        let shift = 12 + 9 * (level - 1);
        let mut v = base | (i as u64) << shift;
        if level == c.levels && i >= 256 {
            v |= if c.levels == 5 {
                0xff00_0000_0000_0000
            } else {
                0xffff_0000_0000_0000
            };
        }
        let w = w && e & paging::RW != 0;
        let x = x && !(c.nx && e & paging::NX != 0);
        if level == 1 || (level <= 3 && e & paging::PS != 0) {
            f(v, 1 << shift, w, x);
        } else {
            walk(c, e & paging::ADDR_MASK, level - 1, v, w, x, f);
        }
    }
}

/// Optimal Boot's address-space guarantees: nothing is writable and
/// executable, and the only executable pages are the kernel's own and the
/// one handoff page just below it.
fn address_space(t: &mut T, c: &Ctx) {
    let start = sym_addr(&raw const __image_start);
    let end = sym_addr(&raw const __image_end);
    let mut wx = 0u64;
    let mut stray = None;
    let mut exec_pages = 0u64;
    walk(c, c.root, c.levels, 0, true, true, &mut |v, size, w, x| {
        if !x || !c.nx {
            return;
        }
        exec_pages += size / PAGE;
        if w {
            wx += size / PAGE;
        }
        let inside = v >= start && v + size <= (end + PAGE - 1) & !(PAGE - 1);
        let handoff = v == start - PAGE && size == PAGE;
        if !inside && !handoff && stray.is_none() {
            stray = Some(v);
        }
    });
    if !c.nx {
        t.skip("address-space", "the CPU has no NX");
        return;
    }
    println!("info address space: {exec_pages} executable pages, {wx} writable and executable");
    if c.ours {
        check!(
            t,
            "ob-no-writable-executable",
            wx == 0,
            "{wx} pages are writable and executable"
        );
        check!(
            t,
            "ob-only-kernel-executable",
            stray.is_none(),
            "executable mapping at {stray:#x?} outside the kernel"
        );
    } else {
        t.skip(
            "ob-address-space",
            "an Optimal Boot guarantee, not a spec promise",
        );
    }
}

/// Base revision 5 and later: exact control registers, RFLAGS and
/// descriptor-table registers.
fn strict_state(t: &mut T, c: &Ctx, es: &crate::EntryState, tb: &Tables) {
    if c.rev < 5 {
        return;
    }
    let want_cr0 = 1 | 1 << 4 | 1 << 16 | 1 << 31;
    let want_cr4 = 1 << 5 | if c.levels == 5 { 1 << 12 } else { 0 };
    let want_efer = 1 << 8 | 1 << 10 | if c.nx { 1 << 11 } else { 0 };
    let (c0, c4, efer) = (cr0(), cr4(), rdmsr(0xc000_0080));
    check!(
        t,
        "rev5-control-registers",
        c0 == want_cr0 && c4 == want_cr4 && efer == want_efer,
        "cr0 {c0:#x} cr4 {c4:#x} efer {efer:#x}, expected {want_cr0:#x} {want_cr4:#x} {want_efer:#x}"
    );
    check!(t, "rev5-rflags", es.rflags == 2, "rflags {:#x}", es.rflags);
    check!(
        t,
        "rev5-idtr",
        tb.idtr == (0, 0),
        "IDTR limit {:#x} base {:#x}",
        tb.idtr.0,
        tb.idtr.1
    );
    check!(t, "rev5-ldtr", tb.ldtr == 0, "LDTR {:#x}", tb.ldtr);
    // TR: a loaded selector whose descriptor has base 0 and limit 0.
    let mut gdtr = [0u8; 10];
    // SAFETY: sgdt stores 10 bytes.
    unsafe { asm!("sgdt [{}]", in(reg) gdtr.as_mut_ptr()) };
    let gbase = u64::from_le_bytes(gdtr[2..].try_into().unwrap());
    let sel = (tb.tr & !7) as u64;
    let ok = sel != 0 && c.mapped(gbase + sel, 16, false) && {
        let (lo, hi) = (rd64(gbase + sel), rd64(gbase + sel + 8));
        let base = (lo >> 16 & 0xff_ffff) | (lo >> 56) << 24 | (hi & 0xffff_ffff) << 32;
        let limit = (lo & 0xffff) | (lo >> 48 & 0xf) << 16;
        base == 0 && limit == 0
    };
    check!(t, "rev5-task-register", ok, "TR selector {:#x}", tb.tr);
}

const LAPIC_LVTS: [(u32, &str); 7] = [
    (0x320, "timer"),
    (0x330, "thermal"),
    (0x340, "perf"),
    (0x350, "lint0"),
    (0x360, "lint1"),
    (0x370, "error"),
    (0x2f0, "cmci"),
];

/// PICs, I/O APICs, the local APIC, IOMMUs and the ACPI tables' placement,
/// read straight from the hardware.
fn platform(t: &mut T, c: &Ctx) {
    let (m, s) = (inb(0x21), inb(0xa1));
    check!(
        t,
        "pic-masked",
        m == 0xff && s == 0xff,
        "PIC masks {m:#x} {s:#x}"
    );
    let ph = Phys(c);
    let rsdp = req::response(&req::RSDP);
    if rsdp == 0 || !c.mapped(rsdp, 16, false) {
        return;
    }
    let raw = rd64(rsdp + 8);
    let rsdp_phys = if c.rev >= 4 {
        raw.wrapping_sub(c.hhdm)
    } else {
        raw
    };
    let Some(acpi) = acpi::Acpi::new(&ph, rsdp_phys) else {
        t.fail(
            "acpi",
            format_args!("the RSDP at physical {rsdp_phys:#x} does not parse"),
        );
        return;
    };
    // Every table: valid checksum; in base revision 4+, inside ACPI or
    // reserved-mapped memory and reachable through the HHDM.
    let okt = |p: u64, len: u64| -> bool {
        c.rev < 4
            || (matches!(
                c.type_of(p, len),
                Some(
                    lp::MEMMAP_ACPI_RECLAIMABLE | lp::MEMMAP_ACPI_NVS | lp::MEMMAP_RESERVED_MAPPED
                )
            ) && c.mapped(p + c.hhdm, len, false))
    };
    let mut tables = 0;
    let mut bad: Option<([u8; 4], u64, &str)> = None;
    let sum = |p: u64, len: u64| -> Option<u8> {
        let mut acc = 0u8;
        let mut buf = [0u8; 256];
        let mut o = 0;
        while o < len {
            let n = (len - o).min(256) as usize;
            if !optimal_boot_core::acpi::PhysRead::read(&ph, p + o, &mut buf[..n]) {
                return None;
            }
            acc = buf[..n].iter().fold(acc, |a, b| a.wrapping_add(*b));
            o += n as u64;
        }
        Some(acc)
    };
    let mut fadt = None;
    acpi.for_each_table(|sig, p, len| {
        tables += 1;
        if &sig == b"FACP" {
            fadt = Some((p, len));
        }
        if bad.is_some() {
            return;
        }
        if sum(p, len as u64) != Some(0) {
            bad = Some((sig, p, "checksum"));
        } else if !okt(p, len as u64) {
            bad = Some((sig, p, "placement"));
        }
    });
    for (name, p, len) in [("RSDT", acpi.rsdt, 36u64), ("XSDT", acpi.xsdt, 36)] {
        if let Some(p) = p
            && bad.is_none()
            && !okt(p, len)
        {
            bad = Some((*b"SDT ", p, name));
        }
    }
    // FACS and DSDT, from the FADT (32- and 64-bit fields).
    if let Some((f, len)) = fadt {
        let r32 = |o: u64| ph.u32(f + o).unwrap_or(0) as u64;
        let r64 = |o: u64| {
            if (len as u64) >= o + 8 {
                ph.u64(f + o).unwrap_or(0)
            } else {
                0
            }
        };
        for (sig, p) in [
            (*b"FACS", r32(36)),
            (*b"DSDT", r32(40)),
            (*b"FACS", r64(132)),
            (*b"DSDT", r64(140)),
        ] {
            if p == 0 || bad.is_some() {
                continue;
            }
            let l = ph.u32(p + 4).unwrap_or(0) as u64;
            if &sig == b"DSDT" && sum(p, l) != Some(0) {
                bad = Some((sig, p, "checksum"));
            } else if !okt(p, l.max(64)) {
                bad = Some((sig, p, "placement"));
            }
        }
    }
    check!(
        t,
        "acpi-tables",
        bad.is_none() && tables > 0,
        "{tables} tables; {:?} at {:#x}: {}",
        bad.map(|b| b.0),
        bad.map_or(0, |b| b.1),
        bad.map_or("", |b| b.2)
    );

    let Some((maddr, mlen)) = acpi.find(b"APIC") else {
        t.skip("ioapic", "no MADT");
        return;
    };
    let Some(madt) = acpi::Madt::parse(&ph, maddr, mlen) else {
        t.fail("madt", format_args!("MADT does not parse"));
        return;
    };
    // I/O APICs: Fixed and Lowest Priority entries masked always; NMI and
    // ExtINT too from base revision 5.
    let mut unmasked = None;
    for &base in madt.ioapics() {
        if !ph.w32(base, 1) {
            unmasked = Some((base, u32::MAX));
            break;
        }
        let max = (ph.r32(base + 0x10).unwrap_or(0) >> 16) & 0xff;
        for i in 0..=max {
            ph.w32(base, 0x10 + 2 * i);
            let lo = ph.r32(base + 0x10).unwrap_or(0);
            let dm = lo >> 8 & 7;
            let must = matches!(dm, 0 | 1) || (c.rev >= 5 && matches!(dm, 4 | 7));
            if must && lo & (1 << 16) == 0 {
                unmasked = Some((base, i));
            }
        }
    }
    check!(
        t,
        "ioapic-masked",
        unmasked.is_none(),
        "I/O APIC {:#x?} entry {:?} is unmasked",
        unmasked.map(|u| u.0),
        unmasked.map(|u| u.1)
    );

    if c.rev >= 5 {
        let ab = rdmsr(0x1b);
        let x2 = ab & (1 << 10) != 0;
        // "If available": a globally disabled APIC that the CPU will not
        // re-enable before a reset (CPUID.1:EDX.APIC then reads 0) is not.
        // The quirk-apic-off case refuses this skip, so it is never how a
        // loader passes that test.
        let cpuid_apic = core::arch::x86_64::__cpuid(1).edx & (1 << 9) != 0;
        let lapic_off = ab & (1 << 11) == 0 && !cpuid_apic;
        if lapic_off {
            t.skip(
                "rev5-lapic",
                "local APIC not available (globally disabled, cannot be re-enabled)",
            );
        }
        let base = ab & 0x000f_ffff_ffff_f000;
        let rd = |off: u32| -> u32 {
            if x2 {
                rdmsr(0x800 + (off >> 4)) as u32
            } else {
                ph.r32(base + off as u64).unwrap_or(0)
            }
        };
        let (svr, tpr) = (rd(0xf0), rd(0x80));
        check!(
            t,
            "rev5-lapic",
            lapic_off || ab & (1 << 11) != 0 && svr & 0x3ff == 0x1ff && tpr == 0,
            "APIC base {ab:#x}, SVR {svr:#x}, TPR {tpr:#x}"
        );
        let max_lvt = rd(0x30) >> 16 & 0xff;
        let mut bad = None;
        for (off, name) in LAPIC_LVTS.iter().copied().filter(|_| !lapic_off) {
            let present = match off {
                0x330 => max_lvt >= 5,
                0x340 => max_lvt >= 4,
                0x2f0 => max_lvt >= 6,
                _ => true,
            };
            if !present || (x2 && off == 0x2f0 && max_lvt < 6) {
                continue;
            }
            let v = rd(off);
            let dm = v >> 8 & 7;
            if matches!(dm, 0 | 1 | 4 | 7) && v & (1 << 16) == 0 {
                bad = Some((name, v));
            }
        }
        check!(
            t,
            "rev5-lapic-lvt",
            bad.is_none(),
            "LVT {:?} is unmasked",
            bad
        );

        // IOMMUs off (unless the Keep IOMMU feature was asked for, which we do not).
        let mut units = [0u64; acpi::MAX_IOMMUS];
        let mut on = None;
        let mut count = 0;
        if let Some((a, l)) = acpi.find(b"DMAR") {
            let n = acpi::dmar_units(&ph, a, l, &mut units);
            count += n;
            for &u in &units[..n] {
                let gsts = ph.r32(u + 0x1c).unwrap_or(u32::MAX);
                if gsts & (1 << 31 | 1 << 25) != 0 {
                    on = Some((u, gsts as u64));
                }
            }
        }
        if let Some((a, l)) = acpi.find(b"IVRS") {
            let n = acpi::ivrs_units(&ph, a, l, &mut units);
            count += n;
            for &u in &units[..n] {
                let ctrl = ph.r64(u + 0x18).unwrap_or(u64::MAX);
                if ctrl & 1 != 0 {
                    on = Some((u, ctrl));
                }
            }
        }
        check!(
            t,
            "rev5-iommu-off",
            on.is_none(),
            "IOMMU {on:x?} is still on"
        );
        println!(
            "info platform: {} I/O APICs, {count} IOMMUs, {} ACPI tables, x2APIC {x2}",
            madt.ioapics().len(),
            tables
        );

        // EFI runtime regions are reserved-mapped, and so is SMBIOS.
        let em = req::response(&req::EFI_MEMMAP);
        if em != 0 && c.mapped(em, 40, false) {
            let (m, size, ds) = (rd64(em + 8), rd64(em + 16), rd64(em + 24));
            let mut bad = None;
            if ds >= 40 && c.mapped(m, size, false) {
                for i in 0..size / ds {
                    let d = m + i * ds;
                    let (typ, p, pages) = (rd64(d) as u32, rd64(d + 8), rd64(d + 24));
                    if matches!(typ, 5 | 6)
                        && c.type_of(p, pages * PAGE) != Some(lp::MEMMAP_RESERVED_MAPPED)
                    {
                        bad = Some(p);
                    }
                }
            }
            check!(
                t,
                "rev5-runtime-reserved-mapped",
                bad.is_none(),
                "EFI runtime region at {bad:#x?} is not reserved-mapped"
            );
        }
    }
}
