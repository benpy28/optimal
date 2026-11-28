//! TCG event log parsing (TCG PC Client Platform Firmware Profile, crypto
//! agile format), just enough to find where the log ends.

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(o..o.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// The digest size the TCG Algorithm Registry fixes for an algorithm, for
/// the algorithms a PC firmware log can use. None for any other id.
pub fn known_digest_size(alg: u16) -> Option<u16> {
    Some(match alg {
        0x0004 => 20,                   // SHA-1
        0x000b | 0x0012 | 0x0027 => 32, // SHA-256, SM3-256, SHA3-256
        0x000c | 0x0028 => 48,          // SHA-384, SHA3-384
        0x000d | 0x0029 => 64,          // SHA-512, SHA3-512
        _ => return None,
    })
}

pub const EV_NO_ACTION: u32 = 3;

/// (header event size, up to 8 (algorithm id, digest size) pairs, pair count).
pub type SpecId = (usize, [(u16, u16); 8], usize);

/// Reads the Spec ID event that starts a crypto-agile log (PC Client
/// Platform Firmware Profile, section 10.4.5.1). Refused, rather than
/// guessed at: a first event that is not PCR 0 / EV_NO_ACTION, no algorithm
/// or more than 8, an algorithm listed twice, a digest size of 0, a size
/// that contradicts the registry for a known algorithm, or a vendor-info
/// field running past the event. Every later event is sized with these
/// digest sizes, so one wrong size would misread the whole log.
pub fn spec_id(log: &[u8]) -> Option<SpecId> {
    // TCG_PCR_EVENT: pcr u32, type u32, digest[20], eventSize u32, event.
    if u32_at(log, 0)? != 0 || u32_at(log, 4)? != EV_NO_ACTION {
        return None;
    }
    let event_size = u32_at(log, 28)? as usize;
    let event = log.get(32..32usize.checked_add(event_size)?)?;
    if event.get(..16)? != b"Spec ID Event03\0" {
        return None;
    }
    let count = u32_at(event, 24)? as usize;
    if count == 0 || count > 8 {
        return None;
    }
    let mut algs = [(0u16, 0u16); 8];
    for i in 0..count {
        let a = (u16_at(event, 28 + i * 4)?, u16_at(event, 30 + i * 4)?);
        if a.1 == 0
            || algs[..i].iter().any(|b| b.0 == a.0)
            || known_digest_size(a.0).is_some_and(|s| s != a.1)
        {
            return None;
        }
        algs[i] = a;
    }
    // vendorInfoSize u8, then vendorInfo, must fit in the event.
    let vendor = 28 + count * 4;
    let vendor_len = *event.get(vendor)? as usize;
    if vendor + 1 + vendor_len > event.len() {
        return None;
    }
    Some((32 + event_size, algs, count))
}

/// Size in bytes of the crypto-agile event (TCG_PCR_EVENT2) at `at`: pcr
/// u32, type u32, digest count u32, (algorithm u16, digest) per bank, event
/// size u32, event. None if it does not fit in `log` or names an algorithm
/// the Spec ID event did not declare.
pub fn event2_size(log: &[u8], at: usize, algs: &[(u16, u16)]) -> Option<usize> {
    let count = u32_at(log, at.checked_add(8)?)? as usize;
    if count > 8 {
        return None;
    }
    let mut o = at + 12;
    for _ in 0..count {
        let alg = u16_at(log, o)?;
        let size = algs.iter().find(|(a, _)| *a == alg)?.1 as usize;
        o = o.checked_add(2 + size)?;
    }
    let event_size = u32_at(log, o)? as usize;
    let end = o.checked_add(4)?.checked_add(event_size)?;
    (end <= log.len()).then_some(end - at)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    fn header() -> Vec<u8> {
        let mut ev = Vec::new();
        ev.extend_from_slice(b"Spec ID Event03\0");
        ev.extend_from_slice(&[0; 8]); // class, versions, uintn size
        ev.extend_from_slice(&2u32.to_le_bytes());
        ev.extend_from_slice(&4u16.to_le_bytes()); // sha1
        ev.extend_from_slice(&20u16.to_le_bytes());
        ev.extend_from_slice(&11u16.to_le_bytes()); // sha256
        ev.extend_from_slice(&32u16.to_le_bytes());
        ev.push(0);
        let mut l = Vec::new();
        l.extend_from_slice(&0u32.to_le_bytes());
        l.extend_from_slice(&3u32.to_le_bytes());
        l.extend_from_slice(&[0; 20]);
        l.extend_from_slice(&(ev.len() as u32).to_le_bytes());
        l.extend_from_slice(&ev);
        l
    }

    #[test]
    fn walks_a_log() {
        let mut log = header();
        let (hlen, algs, n) = spec_id(&log).unwrap();
        assert_eq!(hlen, log.len());
        assert_eq!(n, 2);
        let at = log.len();
        log.extend_from_slice(&9u32.to_le_bytes());
        log.extend_from_slice(&0xdu32.to_le_bytes());
        log.extend_from_slice(&2u32.to_le_bytes());
        log.extend_from_slice(&4u16.to_le_bytes());
        log.extend_from_slice(&[1; 20]);
        log.extend_from_slice(&11u16.to_le_bytes());
        log.extend_from_slice(&[2; 32]);
        log.extend_from_slice(&5u32.to_le_bytes());
        log.extend_from_slice(b"hello");
        assert_eq!(event2_size(&log, at, &algs[..n]), Some(log.len() - at));
        assert_eq!(event2_size(&log[..log.len() - 1], at, &algs[..n]), None);
    }

    /// Spec ID header from (algorithm, size) pairs and vendor info.
    fn header_with(algs: &[(u16, u16)], vendor: &[u8]) -> Vec<u8> {
        let mut ev = Vec::new();
        ev.extend_from_slice(b"Spec ID Event03\0");
        ev.extend_from_slice(&[0; 8]);
        ev.extend_from_slice(&(algs.len() as u32).to_le_bytes());
        for &(a, s) in algs {
            ev.extend_from_slice(&a.to_le_bytes());
            ev.extend_from_slice(&s.to_le_bytes());
        }
        ev.push(vendor.len() as u8);
        ev.extend_from_slice(vendor);
        let mut l = Vec::new();
        l.extend_from_slice(&0u32.to_le_bytes());
        l.extend_from_slice(&EV_NO_ACTION.to_le_bytes());
        l.extend_from_slice(&[0; 20]);
        l.extend_from_slice(&(ev.len() as u32).to_le_bytes());
        l.extend_from_slice(&ev);
        l
    }

    /// Each refusal, by hand. The PFP spec fixes the first event (PCR 0,
    /// EV_NO_ACTION) and the registry fixes known digest sizes; every later
    /// event is sized from this header, so a lie here misreads the log.
    #[test]
    fn spec_id_refusals() {
        let good = [(4, 20), (11, 32)];
        assert!(spec_id(&header_with(&good, b"")).is_some());
        assert!(spec_id(&header_with(&good, b"vendor")).is_some());
        // An unknown algorithm keeps its declared size (that is what the
        // size field is for).
        let (_, a, n) = spec_id(&header_with(&[(0x7777, 5)], b"")).unwrap();
        assert_eq!(&a[..n], &[(0x7777, 5)]);
        // SHA-256 declared as 20 bytes contradicts the registry.
        assert!(spec_id(&header_with(&[(4, 20), (11, 20)], b"")).is_none());
        assert!(spec_id(&header_with(&[(0x7777, 0)], b"")).is_none());
        assert!(spec_id(&header_with(&[(11, 32), (11, 32)], b"")).is_none());
        assert!(spec_id(&header_with(&[], b"")).is_none());
        let nine = [(0x100, 1); 9];
        assert!(spec_id(&header_with(&nine, b"")).is_none());
        let mut l = header_with(&good, b"");
        l[4] = 1; // not EV_NO_ACTION
        assert!(spec_id(&l).is_none());
        let mut l = header_with(&good, b"");
        l[0] = 7; // not PCR 0
        assert!(spec_id(&l).is_none());
        // vendorInfoSize says 3 bytes, the event has 2.
        let mut l = header_with(&good, b"ab");
        let vs = l.len() - 3;
        l[vs] = 3;
        assert!(spec_id(&l).is_none());
    }

    #[test]
    fn event_at_the_end_of_the_address_range_is_refused() {
        let (_, a, n) = spec_id(&header()).unwrap();
        let log = header();
        for at in [
            usize::MAX,
            usize::MAX - 7,
            usize::MAX - 11,
            log.len(),
            log.len() + 1,
        ] {
            assert_eq!(event2_size(&log, at, &a[..n]), None, "at {at}");
        }
    }

    /// A random valid log: Spec ID header with a random subset of algorithms
    /// (sizes from the registry, plus sometimes one unknown algorithm), then
    /// events with random digests and data. Returns the log and the event
    /// boundaries a correct walk must find.
    fn random_log(rng: &mut crate::testrng::Rng) -> (Vec<u8>, Vec<usize>) {
        let pool = [4u16, 0xb, 0xc, 0xd, 0x12, 0x27, 0x28, 0x29];
        let mut algs: Vec<(u16, u16)> = Vec::new();
        for &a in &pool {
            if rng.below(3) == 0 {
                algs.push((a, known_digest_size(a).unwrap()));
            }
        }
        if algs.is_empty() || (algs.len() < 8 && rng.below(6) == 0) {
            algs.push((0x4000 + rng.below(0x100) as u16, 1 + rng.below(80) as u16));
        }
        let vendor: Vec<u8> = (0..rng.below(20)).map(|_| rng.next() as u8).collect();
        let mut log = header_with(&algs, &vendor);
        let mut bounds = vec![log.len()];
        for _ in 0..rng.below(24) {
            log.extend_from_slice(&(rng.below(24) as u32).to_le_bytes());
            log.extend_from_slice(&(rng.next() as u32).to_le_bytes());
            log.extend_from_slice(&(algs.len() as u32).to_le_bytes());
            for &(a, s) in &algs {
                log.extend_from_slice(&a.to_le_bytes());
                log.extend((0..s).map(|_| rng.next() as u8));
            }
            let data = rng.below(300) as usize;
            log.extend_from_slice(&(data as u32).to_le_bytes());
            log.extend((0..data).map(|_| rng.next() as u8));
            bounds.push(log.len());
        }
        (log, bounds)
    }

    #[test]
    fn fuzz_random_logs_walk_exactly() {
        crate::fuzzkit::run("tcg-valid", 3, |rng| {
            let (log, bounds) = random_log(rng);
            let (hlen, algs, n) = spec_id(&log).expect("valid header");
            assert_eq!(hlen, bounds[0]);
            for w in bounds.windows(2) {
                assert_eq!(event2_size(&log, w[0], &algs[..n]), Some(w[1] - w[0]));
                // Cut anywhere inside the event: refused, never over-read.
                let cut = w[0] + rng.below((w[1] - w[0]) as u64) as usize;
                assert_eq!(event2_size(&log[..cut], w[0], &algs[..n]), None);
            }
        });
    }

    #[test]
    fn fuzz_mutated_logs_never_overrun() {
        crate::fuzzkit::run("tcg-mutated", 4, |rng| {
            let (mut log, _) = random_log(rng);
            crate::fuzzkit::mutate(rng, &mut log);
            let Some((hlen, algs, n)) = spec_id(&log) else {
                return;
            };
            assert!(hlen <= log.len() && (1..=8).contains(&n));
            for &(a, s) in &algs[..n] {
                assert!(s != 0 && known_digest_size(a).is_none_or(|k| k == s));
            }
            let mut at = hlen;
            while let Some(size) = event2_size(&log, at, &algs[..n]) {
                assert!(size >= 16 && at + size <= log.len());
                at += size;
            }
        });
    }
}
