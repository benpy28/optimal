//! TCG event log parsing (TCG PC Client Platform Firmware Profile, crypto
//! agile format), just enough to find where the log ends.

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Digest sizes declared by the Spec ID event at the start of the log:
/// up to 8 (algorithm id, size) pairs. Returns the header event's size too.
pub fn spec_id(log: &[u8]) -> Option<(usize, [(u16, u16); 8], usize)> {
    // TCG_PCR_EVENT: pcr u32, type u32, digest[20], eventSize u32, event.
    let event_size = u32_at(log, 28)? as usize;
    let event = log.get(32..32 + event_size)?;
    if event.get(..16)? != b"Spec ID Event03\0" {
        return None;
    }
    let count = u32_at(event, 24)? as usize;
    if count == 0 || count > 8 {
        return None;
    }
    let mut algs = [(0u16, 0u16); 8];
    for (i, a) in algs.iter_mut().enumerate().take(count) {
        *a = (u16_at(event, 28 + i * 4)?, u16_at(event, 30 + i * 4)?);
    }
    Some((32 + event_size, algs, count))
}

/// Size in bytes of the crypto-agile event (TCG_PCR_EVENT2) at `at`.
pub fn event2_size(log: &[u8], at: usize, algs: &[(u16, u16)]) -> Option<usize> {
    let count = u32_at(log, at + 8)? as usize;
    if count > 8 {
        return None;
    }
    let mut o = at + 12;
    for _ in 0..count {
        let alg = u16_at(log, o)?;
        let size = algs.iter().find(|(a, _)| *a == alg)?.1 as usize;
        o += 2 + size;
    }
    let event_size = u32_at(log, o)? as usize;
    let end = o.checked_add(4)?.checked_add(event_size)?;
    (end <= log.len()).then_some(end - at)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
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
}
