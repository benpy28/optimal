//! The boot menu, as in the UEFI loader: entries with a countdown; any key
//! stops it, up/down choose, Enter boots. Keys come from INT 16h, and time
//! from INT 15h AH=86h (the TSC is not calibrated yet at this point).

use crate::bios;
use crate::{print, println};
use optimal_boot_core::config::Config;

const SCAN_UP: u8 = 0x48;
const SCAN_DOWN: u8 = 0x50;

/// Waits up to `ms` milliseconds for a key.
pub fn wait_key(ms: u64) -> Option<(u8, u8)> {
    for _ in 0..ms / 10 {
        if let Some(k) = bios::read_key() {
            return Some(k);
        }
        if !bios::wait_us(10_000) {
            // No BIOS wait service: a TSC spin of roughly 10 ms at 1 GHz
            // or more (only the countdown's length depends on it).
            let t = crate::hw::rdtsc();
            while crate::hw::rdtsc() - t < 10_000_000 {
                core::hint::spin_loop();
            }
        }
    }
    None
}

pub fn choose(cfg: &Config) -> usize {
    while bios::read_key().is_some() {} // drop keys pressed before the menu
    let mut sel = cfg.default;
    let n = cfg.entry_count();
    if cfg.timeout == 0 || n == 0 {
        return sel;
    }
    let show = |sel: usize| {
        for (i, e) in cfg.entries().enumerate() {
            println!("  {} {}", if i == sel { ">" } else { " " }, e.title);
        }
    };
    println!();
    show(sel);
    print!(
        "Booting \"{}\" in {} s (any key stops)",
        cfg.entry(sel).title,
        cfg.timeout
    );
    let mut left = cfg.timeout as u64;
    loop {
        if left == 0 {
            println!();
            return sel;
        }
        if wait_key(1000).is_some() {
            break;
        }
        left -= 1;
        if left > 0 {
            print!(" {left}");
        }
    }
    println!();
    println!("Countdown stopped. Up/down to choose, Enter to boot.");
    loop {
        let Some((scan, ascii)) = wait_key(60_000) else {
            continue;
        };
        match (scan, ascii) {
            (SCAN_UP, _) => sel = (sel + n - 1) % n,
            (SCAN_DOWN, _) => sel = (sel + 1) % n,
            (_, 13) => {
                println!();
                return sel;
            }
            _ => continue,
        }
        show(sel);
    }
}
