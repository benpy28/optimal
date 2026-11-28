//! The Limine requests this kernel makes: every feature the spec defines
//! for x86-64. Defined from the spec directly (not via the limine crate) so
//! the newest features are covered too.

use core::cell::UnsafeCell;
use optimal_boot_core::protocol::{self as lp, Feature};

#[repr(C, align(8))]
pub struct Raw<const N: usize>(UnsafeCell<[u64; N]>);
// SAFETY: written only by the bootloader before we run; read with volatile reads.
unsafe impl<const N: usize> Sync for Raw<N> {}

impl<const N: usize> Raw<N> {
    pub fn word(&self, i: usize) -> u64 {
        // SAFETY: in-bounds volatile read of our own static.
        unsafe { core::ptr::read_volatile((self.0.get() as *const u64).add(i)) }
    }
}

const fn id(f: Feature) -> [u64; 4] {
    let mut i = 0;
    while i < lp::FEATURES.len() {
        if lp::FEATURES[i].0 as u32 == f as u32 {
            let w = lp::FEATURES[i].1;
            return [lp::COMMON_MAGIC[0], lp::COMMON_MAGIC[1], w[0], w[1]];
        }
        i += 1;
    }
    panic!("unknown feature")
}

const fn request<const N: usize>(f: Feature, revision: u64, extra: [u64; 4]) -> Raw<N> {
    let i = id(f);
    let mut w = [0u64; N];
    w[0] = i[0];
    w[1] = i[1];
    w[2] = i[2];
    w[3] = i[3];
    w[4] = revision;
    w[5] = 0;
    let mut k = 6;
    while k < N {
        w[k] = extra[k - 6];
        k += 1;
    }
    Raw(UnsafeCell::new(w))
}

const fn env_u64(v: Option<&str>, default: u64) -> u64 {
    match v {
        None => default,
        Some(s) => {
            let b = s.as_bytes();
            let mut n = 0u64;
            let mut i = 0;
            while i < b.len() {
                n = n * 10 + (b[i] - b'0') as u64;
                i += 1;
            }
            n
        }
    }
}

pub const REVISION: u64 = env_u64(option_env!("OB_REVISION"), 3);
pub const WANT_5LVL: bool = env_u64(option_env!("OB_PAGING"), 4) == 5;
pub const ENTRY_REQ: bool = env_u64(option_env!("OB_ENTRY_REQ"), 0) == 1;
pub const STACK_SIZE: u64 = 256 * 1024;
pub const ENTROPY_COUNT: u64 = 16;
/// OB_X2APIC=1 sets the MP request's "enable x2APIC if possible" flag.
pub const WANT_X2APIC: bool = env_u64(option_env!("OB_X2APIC"), 0) == 1;
/// OB_MP=0 leaves the MP request out (cases that test the loader's
/// handling of firmware APIC state without the MP request's mode change).
pub const WANT_MP: bool = env_u64(option_env!("OB_MP"), 1) == 1;

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static START: [u64; 4] = lp::REQUESTS_START_MARKER;

#[used]
#[unsafe(link_section = ".requests")]
pub static BASE_REVISION: Raw<3> = Raw(UnsafeCell::new([
    lp::BASE_REVISION_MAGIC[0],
    lp::BASE_REVISION_MAGIC[1],
    REVISION,
]));

macro_rules! req {
    ($name:ident, $n:literal, $f:expr, $rev:expr, $extra:expr) => {
        #[used]
        #[unsafe(link_section = ".requests")]
        pub static $name: Raw<$n> = request::<$n>($f, $rev, $extra);
    };
}

req!(BOOTLOADER_INFO, 6, Feature::BootloaderInfo, 0, [0; 4]);
req!(CMDLINE, 6, Feature::ExecutableCmdline, 0, [0; 4]);
req!(FIRMWARE_TYPE, 6, Feature::FirmwareType, 0, [0; 4]);
req!(STACK, 7, Feature::StackSize, 0, [STACK_SIZE, 0, 0, 0]);
req!(HHDM, 6, Feature::Hhdm, 0, [0; 4]);
req!(FRAMEBUFFER, 6, Feature::Framebuffer, 0, [0; 4]);
req!(
    PAGING,
    9,
    Feature::PagingMode,
    1,
    if WANT_5LVL {
        [1, 1, 0, 0]
    } else {
        [0, 0, 0, 0]
    }
);
req!(MEMMAP, 6, Feature::MemoryMap, 0, [0; 4]);
req!(EXEC_FILE, 6, Feature::ExecutableFile, 0, [0; 4]);
req!(MODULES, 8, Feature::Module, 1, [0, 0, 0, 0]);
req!(RSDP, 6, Feature::Rsdp, 0, [0; 4]);
req!(SMBIOS, 6, Feature::Smbios, 0, [0; 4]);
req!(EFI_ST, 6, Feature::EfiSystemTable, 0, [0; 4]);
req!(TPM_LOG, 6, Feature::TpmEventLog, 0, [0; 4]);
req!(EFI_MEMMAP, 6, Feature::EfiMemoryMap, 0, [0; 4]);
req!(DATE, 6, Feature::DateAtBoot, 0, [0; 4]);
req!(EXEC_ADDR, 6, Feature::ExecutableAddress, 0, [0; 4]);
req!(PERF, 6, Feature::BootloaderPerformance, 0, [0; 4]);
req!(TSC, 6, Feature::TscFrequency, 0, [0; 4]);
req!(ENTROPY, 7, Feature::Entropy, 0, [ENTROPY_COUNT, 0, 0, 0]);
req!(DTB, 6, Feature::Dtb, 0, [0; 4]);
#[used]
#[unsafe(link_section = ".requests")]
pub static MP: Raw<7> = if WANT_MP {
    request::<7>(Feature::Mp, 0, [if WANT_X2APIC { 1 } else { 0 }, 0, 0, 0])
} else {
    Raw(UnsafeCell::new([0; 7]))
};

/// The Entry Point request holds a pointer to alt_start, so in a PIE build
/// it needs a relocation, which also tests that the loader relocates before
/// reading requests.
#[repr(C, align(8))]
pub struct EntryReq {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: UnsafeCell<u64>,
    pub entry: unsafe extern "C" fn(),
}
// SAFETY: as Raw.
unsafe impl Sync for EntryReq {}

#[used]
#[unsafe(link_section = ".requests")]
pub static ENTRY_POINT: EntryReq = EntryReq {
    id: if ENTRY_REQ {
        id(Feature::EntryPoint)
    } else {
        [0; 4]
    },
    revision: 0,
    response: UnsafeCell::new(0),
    entry: crate::alt_start,
};

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static END: [u64; 2] = lp::REQUESTS_END_MARKER;

/// Response pointer of a request (word 5), or 0.
pub fn response<const N: usize>(r: &Raw<N>) -> u64 {
    r.word(5)
}

pub fn entry_response() -> u64 {
    // SAFETY: volatile read of our own static.
    unsafe { core::ptr::read_volatile(ENTRY_POINT.response.get()) }
}
