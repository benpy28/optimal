//! The HPET's registers (IA-PC HPET specification 1.0a, section 2.3).
//!
//! The block is 1 KiB of memory-mapped registers, all 64 bits wide.

use crate::freq::Freq;

/// General Capabilities and ID (read-only).
pub const CAPABILITIES: u64 = 0x000;
/// General Configuration.
pub const CONFIG: u64 = 0x010;
/// General Interrupt Status (level-triggered comparators; write 1 to clear).
pub const INTERRUPT_STATUS: u64 = 0x020;
/// Main Counter Value.
pub const MAIN_COUNTER: u64 = 0x0F0;

/// Comparator `n`'s Configuration and Capability register.
pub const fn timer_config(n: u8) -> u64 {
    0x100 + 0x20 * n as u64
}

/// Comparator `n`'s Comparator Value register.
pub const fn timer_comparator(n: u8) -> u64 {
    0x108 + 0x20 * n as u64
}

/// The longest period the specification allows: 10^8 fs, 100 ns, so every
/// HPET runs at 10 MHz or faster (section 2.3.4).
pub const MAX_PERIOD_FS: u32 = 100_000_000;

/// The shortest period the kernel accepts: 910 fs, about 1.1 THz, the
/// fastest clock a [`Freq`] can hold (2^40 Hz; 909 fs would be just past
/// it). Real HPETs run at 10 to 100 MHz (10^7 to 10^8 fs), so a shorter
/// period is a misread register.
pub const MIN_PERIOD_FS: u32 = 910;

/// General Capabilities and ID, decoded and checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// REV_ID: never 0 on a real HPET.
    pub revision: u8,
    /// Comparators, 1 to 32 (NUM_TIM_CAP is the last one's number).
    pub comparators: u8,
    /// COUNT_SIZE_CAP: the main counter is 64 bits wide, else 32.
    pub counter_64bit: bool,
    /// LEG_RT_CAP: can take over IRQ 0 and IRQ 8 (legacy replacement).
    pub legacy_replacement: bool,
    pub vendor: u16,
    /// COUNTER_CLK_PERIOD, in femtoseconds.
    pub period_fs: u32,
}

/// Why the capabilities register shows no usable HPET.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bad {
    /// Every bit set: nothing answers at that address.
    NothingThere,
    /// REV_ID is 0, which the specification forbids.
    Revision,
    /// COUNTER_CLK_PERIOD is 0.
    ZeroPeriod,
    /// COUNTER_CLK_PERIOD is over 10^8 fs (slower than 10 MHz).
    PeriodTooLong(u32),
    /// COUNTER_CLK_PERIOD is under [`MIN_PERIOD_FS`].
    PeriodTooShort(u32),
}

impl Capabilities {
    pub fn decode(raw: u64) -> Result<Capabilities, Bad> {
        if raw == u64::MAX {
            return Err(Bad::NothingThere);
        }
        let c = Capabilities {
            revision: raw as u8,
            comparators: ((raw >> 8) & 0x1f) as u8 + 1,
            counter_64bit: raw & (1 << 13) != 0,
            legacy_replacement: raw & (1 << 15) != 0,
            vendor: (raw >> 16) as u16,
            period_fs: (raw >> 32) as u32,
        };
        if c.revision == 0 {
            Err(Bad::Revision)
        } else if c.period_fs == 0 {
            Err(Bad::ZeroPeriod)
        } else if c.period_fs > MAX_PERIOD_FS {
            Err(Bad::PeriodTooLong(c.period_fs))
        } else if c.period_fs < MIN_PERIOD_FS {
            Err(Bad::PeriodTooShort(c.period_fs))
        } else {
            Ok(c)
        }
    }

    /// The main counter's frequency, exactly 10^15 / period Hz.
    pub fn freq(self) -> Freq {
        Freq::from_period_fs(u64::from(self.period_fs))
            .expect("decode accepts only 910 to 10^8 fs: 10 MHz to under 2^40 Hz")
    }

    /// Bits in the main counter.
    pub fn counter_bits(self) -> u32 {
        if self.counter_64bit { 64 } else { 32 }
    }
}

/// Where the ACPI HPET table and the hardware disagree. The hardware's
/// registers are what the kernel uses; the table only found it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TableCheck {
    /// (table, hardware) comparator counts, if they differ.
    pub comparators: Option<(u8, u8)>,
    /// (table, hardware) counter widths in bits, if they differ.
    pub counter_bits: Option<(u32, u32)>,
}

pub fn check_table(caps: Capabilities, table_comparators: u8, table_64bit: bool) -> TableCheck {
    let table_bits = if table_64bit { 64 } else { 32 };
    TableCheck {
        comparators: (table_comparators != caps.comparators)
            .then_some((table_comparators, caps.comparators)),
        counter_bits: (table_bits != caps.counter_bits())
            .then_some((table_bits, caps.counter_bits())),
    }
}

/// General Configuration bits.
pub const ENABLE: u64 = 0x1;
pub const LEGACY_ROUTE: u64 = 0x2;

/// Comparator configuration bits (section 2.3.8).
pub const T_LEVEL: u64 = 0x2;
pub const T_INT_ENABLE: u64 = 0x4;
pub const T_PERIODIC: u64 = 0x8;
pub const T_PERIODIC_CAPABLE: u64 = 0x10;
pub const T_64BIT_CAPABLE: u64 = 0x20;
pub const T_VAL_SET: u64 = 0x40;
pub const T_32BIT_MODE: u64 = 0x100;
pub const T_FSB_ENABLE: u64 = 0x4000;
pub const T_FSB_CAPABLE: u64 = 0x8000;

/// General Configuration with the counter halted and legacy replacement
/// off; reserved bits are written back as read.
pub const fn halted(config: u64) -> u64 {
    config & !(ENABLE | LEGACY_ROUTE)
}

/// General Configuration with the counter running and legacy replacement
/// off (the kernel routes comparators through the I/O APIC itself, and
/// legacy replacement would take IRQ 0 and IRQ 8 away from the PIT and
/// the RTC behind the kernel's back).
pub const fn running(config: u64) -> u64 {
    (config & !LEGACY_ROUTE) | ENABLE
}

/// A comparator's configuration with its interrupt off, in one-shot mode
/// and not delivering by FSB: nothing firmware left armed can fire.
/// Capability bits are read-only, and other bits are written back as read.
pub const fn quiet(timer_config: u64) -> u64 {
    timer_config & !(T_INT_ENABLE | T_PERIODIC | T_FSB_ENABLE | T_VAL_SET)
}

/// The writes that start the HPET safely from whatever firmware left:
/// halt the counter, quiet every comparator, then run the counter. Each is
/// (register offset, value). `timers` holds each comparator's current
/// configuration register, comparator 0 first.
pub fn start(config: u64, timers: &[u64]) -> impl Iterator<Item = (u64, u64)> + '_ {
    core::iter::once((CONFIG, halted(config)))
        .chain(
            timers
                .iter()
                .enumerate()
                .map(|(n, &t)| (timer_config(n as u8), quiet(t))),
        )
        .chain(core::iter::once((CONFIG, running(config))))
}

/// Comparator configuration: Tn_INT_ROUTE_CNF, the I/O APIC input the
/// comparator's interrupt goes to (bits 13:9).
pub const T_ROUTE_SHIFT: u32 = 9;
pub const T_ROUTE: u64 = 0x1F << T_ROUTE_SHIFT;

/// Tn_INT_ROUTE_CAP (bits 63:32): bit n set means the comparator can be
/// routed to I/O APIC input n.
pub const fn route_cap(timer_config: u64) -> u32 {
    (timer_config >> 32) as u32
}

/// A comparator's configuration for a one-shot, level-triggered
/// interrupt on I/O APIC input `gsi`, interrupt enabled or not. Periodic
/// mode, FSB delivery and the forced 32-bit mode are off, and other bits
/// are written back as read. None if the comparator cannot be routed to
/// `gsi` (Tn_INT_ROUTE_CAP).
pub fn level_one_shot(timer_config: u64, gsi: u32, enabled: bool) -> Option<u64> {
    if gsi >= 32 || route_cap(timer_config) & (1 << gsi) == 0 {
        return None;
    }
    Some(
        quiet(timer_config) & !(T_ROUTE | T_32BIT_MODE)
            | u64::from(gsi) << T_ROUTE_SHIFT
            | T_LEVEL
            | if enabled { T_INT_ENABLE } else { 0 },
    )
}

/// The comparator value `ticks` after main counter value `now`. A
/// comparator without Tn_SIZE_CAP compares only the low 32 bits, so its
/// value wraps there.
pub fn deadline(now: u64, ticks: u64, timer_config: u64) -> u64 {
    let t = now.wrapping_add(ticks);
    if timer_config & T_64BIT_CAPABLE != 0 {
        t
    } else {
        t & 0xFFFF_FFFF
    }
}

/// A comparator and I/O APIC input for a level-triggered interrupt: the
/// highest-numbered comparator that can reach an input `free` accepts, on
/// the lowest such input. Comparators 0 and 1 go last because they are
/// the legacy-replacement ones and the only ones the specification
/// requires; comparator 0 is also the only one it requires to be
/// periodic-capable. `configs` holds each comparator's configuration
/// register, comparator 0 first.
pub fn pick_level(configs: &[u64], free: impl Fn(u32) -> bool) -> Option<(u8, u32)> {
    configs.iter().enumerate().rev().find_map(|(n, &c)| {
        (0..32)
            .find(|&g| route_cap(c) & (1 << g) != 0 && free(g))
            .map(|g| (n as u8, g))
    })
}

/// Has main counter value `now` reached comparator value `deadline`?
/// Compared in the comparator's width, as a signed distance, so it is
/// right across a wrap while the two are within half the width apart. A
/// comparator matches only on equality, so a deadline the counter passed
/// before the comparator was enabled is missed until the counter wraps
/// (2^64 ticks: never); after arming, the caller checks this and arms
/// again further ahead.
pub fn passed(now: u64, deadline: u64, timer_config: u64) -> bool {
    if timer_config & T_64BIT_CAPABLE != 0 {
        now.wrapping_sub(deadline) as i64 >= 0
    } else {
        (now as u32).wrapping_sub(deadline as u32) as i32 >= 0
    }
}

/// A comparator's configuration for a one-shot, edge-triggered interrupt
/// on I/O APIC input `gsi`: [`level_one_shot`] with Tn_INT_TYPE_CNF
/// clear. Edge needs no acknowledgement (no status bit to clear, no
/// masking from interrupt to done), so it is what timer events use.
/// None if the comparator cannot be routed to `gsi`.
pub fn edge_one_shot(timer_config: u64, gsi: u32, enabled: bool) -> Option<u64> {
    level_one_shot(timer_config, gsi, enabled).map(|c| c & !T_LEVEL)
}
