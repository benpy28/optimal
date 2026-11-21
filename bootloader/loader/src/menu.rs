//! The boot menu: entries with a countdown. Any key stops the countdown;
//! up/down choose, Enter boots.

use crate::console::{boot_services, system_table};
use crate::efi;
use crate::{print, println};
use optimal_boot_core::config::Config;

fn read_key() -> Option<efi::InputKey> {
    let cin = system_table().con_in;
    if cin.is_null() {
        return None;
    }
    let mut k = efi::InputKey { scan_code: 0, unicode_char: 0 };
    // SAFETY: con_in is the firmware's console input protocol.
    (unsafe { ((*cin).read_key_stroke)(cin, &mut k) } == efi::SUCCESS).then_some(k)
}

/// Waits up to `ms` milliseconds for a key.
pub fn wait_key(ms: u64) -> Option<efi::InputKey> {
    for _ in 0..ms / 10 {
        if let Some(k) = read_key() {
            return Some(k);
        }
        (boot_services().stall)(10_000);
    }
    None
}

pub fn choose(cfg: &Config) -> usize {
    while read_key().is_some() {} // drop keys pressed before the menu
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
    let mut left_ms = cfg.timeout as u64 * 1000;
    let mut counting = true;
    print!("Booting \"{}\" in {} s (any key stops)", cfg.entry(sel).title, cfg.timeout);
    loop {
        if let Some(k) = read_key() {
            if counting {
                counting = false;
                println!();
                println!("Countdown stopped. Up/down to choose, Enter to boot.");
            }
            match (k.scan_code, k.unicode_char) {
                (1, _) => sel = (sel + n - 1) % n,
                (2, _) => sel = (sel + 1) % n,
                (_, 13) => {
                    println!();
                    return sel;
                }
                _ => {}
            }
            if k.scan_code == 1 || k.scan_code == 2 {
                show(sel);
            }
            continue;
        }
        if counting {
            if left_ms == 0 {
                println!();
                return sel;
            }
            if left_ms % 1000 == 0 && left_ms != cfg.timeout as u64 * 1000 {
                print!(" {}", left_ms / 1000);
            }
            left_ms -= 10;
        }
        (boot_services().stall)(10_000);
    }
}
