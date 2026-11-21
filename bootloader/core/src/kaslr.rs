//! Choosing randomised addresses: the HHDM base and the kernel's physical
//! placement (the virtual slide comes from `elf::slide_range`).

use crate::rng::Drbg;

pub const GIB: u64 = 1 << 30;

/// Lowest higher-half address for the paging mode.
pub fn higher_half_start(levels: u32) -> u64 {
    if levels == 5 {
        0xff00_0000_0000_0000
    } else {
        0xffff_8000_0000_0000
    }
}

/// HHDM offset. It must cover physical memory up to `phys_end`, leave a
/// gap before the kernel's top 2 GiB, and be 1 GiB aligned (so 1 GiB pages
/// can be used). Without randomisation it is the start of the higher half,
/// as every Limine-protocol loader does by default.
pub fn hhdm_offset(levels: u32, phys_end: u64, rng: Option<&mut Drbg>) -> Option<u64> {
    let start = higher_half_start(levels);
    let span = phys_end.checked_add(GIB - 1)? & !(GIB - 1);
    // Keep the HHDM out of the top 64 GiB entirely (kernel lives in the top 2).
    let limit = 0xffff_fff0_0000_0000u64;
    let last = limit.checked_sub(span)?;
    if last < start {
        return None;
    }
    match rng {
        None => Some(start),
        Some(r) => {
            let slots = (last - start) / GIB + 1;
            Some(start + r.below(slots) * GIB)
        }
    }
}

/// Picks a random `align`-aligned address for `size` bytes inside one of the
/// free `regions` (start, end), at or above `min_addr`. Every possible
/// position is equally likely.
pub fn pick_physical(
    regions: &[(u64, u64)],
    size: u64,
    align: u64,
    min_addr: u64,
    rng: &mut Drbg,
) -> Option<u64> {
    let slots = |&(s, e): &(u64, u64)| -> u64 {
        let s = s.max(min_addr);
        let Some(first) = s.checked_add(align - 1).map(|v| v & !(align - 1)) else {
            return 0;
        };
        match e.checked_sub(size) {
            Some(last) if last >= first => (last - first) / align + 1,
            _ => 0,
        }
    };
    let total: u64 = regions
        .iter()
        .map(slots)
        .fold(0u64, |a, b| a.saturating_add(b));
    if total == 0 {
        return None;
    }
    let mut pick = rng.below(total);
    for r in regions {
        let n = slots(r);
        if pick < n {
            let first = (r.0.max(min_addr) + align - 1) & !(align - 1);
            return Some(first + pick * align);
        }
        pick -= n;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hhdm_bounds() {
        assert_eq!(hhdm_offset(4, 8 * GIB, None), Some(0xffff_8000_0000_0000));
        let mut r = Drbg::new();
        r.mix(b"t", b"1", 0);
        for _ in 0..1000 {
            let o = hhdm_offset(4, 64 * GIB, Some(&mut r)).unwrap();
            assert!(o >= 0xffff_8000_0000_0000);
            assert_eq!(o % GIB, 0);
            assert!(o + 64 * GIB <= 0xffff_fff0_0000_0000);
            let o5 = hhdm_offset(5, 64 * GIB, Some(&mut r)).unwrap();
            assert!(o5 >= 0xff00_0000_0000_0000);
        }
        // Too much memory for 4-level paging.
        assert_eq!(hhdm_offset(4, 1 << 47, None), None);
    }

    #[test]
    fn physical_pick_stays_inside() {
        let mut r = Drbg::new();
        r.mix(b"t", b"2", 0);
        let regions = [
            (0x1000, 0x9f000),
            (0x10_0000, 0x800_0000),
            (0x900_0000, 0x980_0000),
        ];
        let mut seen_high = false;
        for _ in 0..2000 {
            let a = pick_physical(&regions, 0x20_0000, 0x20_0000, 0x10_0000, &mut r).unwrap();
            assert_eq!(a % 0x20_0000, 0);
            assert!(regions.iter().any(|&(s, e)| a >= s && a + 0x20_0000 <= e));
            seen_high |= a >= 0x900_0000;
        }
        assert!(seen_high);
        assert_eq!(
            pick_physical(&regions, 0x1000_0000, 0x1000, 0, &mut r),
            None
        );
    }
}
