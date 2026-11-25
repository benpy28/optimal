//! Finding Limine requests and the base revision tag in a loaded image.

use crate::protocol::{
    self, BASE_REVISION_MAGIC, COMMON_MAGIC, Feature, REQUESTS_END_MARKER, REQUESTS_START_MARKER,
};

/// Most requests we track in one image. The spec defines 26 features, so a
/// kernel with more than this is corrupt or hostile.
pub const MAX_REQUESTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub feature: Feature,
    /// Byte offset of the request within the image.
    pub offset: usize,
    pub revision: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ScanError {
    DuplicateRequest { offset: usize },
    DuplicateBaseRevision { offset: usize },
    TooManyRequests,
    RequestTruncated { offset: usize },
}

#[derive(Debug)]
pub struct Scan {
    /// Offset of the base revision tag, if the kernel has one.
    pub base_revision_tag: Option<usize>,
    /// The revision the tag asks for (0 if there is no tag, as the spec says).
    pub requested_revision: u64,
    found: [Option<Found>; MAX_REQUESTS],
    count: usize,
}

impl Scan {
    pub fn requests(&self) -> impl Iterator<Item = &Found> {
        self.found[..self.count].iter().flatten()
    }

    pub fn get(&self, feature: Feature) -> Option<&Found> {
        self.requests().find(|f| f.feature == feature)
    }
}

fn word(image: &[u8], offset: usize) -> Option<u64> {
    let bytes = image.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn words<const N: usize>(image: &[u8], offset: usize) -> Option<[u64; N]> {
    let mut out = [0u64; N];
    for (i, w) in out.iter_mut().enumerate() {
        *w = word(image, offset + i * 8)?;
    }
    Some(out)
}

/// Scans `image` (the kernel as loaded in memory, starting at its lowest
/// virtual address, which is page aligned) on 8-byte boundaries.
///
/// The spec says requests are only honoured between the last start marker and
/// the first end marker after it, when markers exist; base revision tags are
/// covered by the same rule.
pub fn scan(image: &[u8]) -> Result<Scan, ScanError> {
    let len = image.len() & !7;
    let mut start = 0usize;
    let mut off = 0usize;
    while off + 32 <= len {
        if words::<4>(image, off) == Some(REQUESTS_START_MARKER) {
            start = off + 32;
        }
        off += 8;
    }
    let mut end = len;
    let mut off = start;
    while off + 16 <= len {
        if words::<2>(image, off) == Some(REQUESTS_END_MARKER) {
            end = off;
            break;
        }
        off += 8;
    }

    let mut scan = Scan {
        base_revision_tag: None,
        requested_revision: 0,
        found: [None; MAX_REQUESTS],
        count: 0,
    };
    let mut seen_ids: [[u64; 2]; MAX_REQUESTS] = [[0; 2]; MAX_REQUESTS];
    let mut seen = 0usize;

    let mut off = start;
    while off + 16 <= end {
        let head = words::<2>(image, off).unwrap();
        if head == BASE_REVISION_MAGIC {
            if off + 24 > end {
                off += 8;
                continue;
            }
            if scan.base_revision_tag.is_some() {
                return Err(ScanError::DuplicateBaseRevision { offset: off });
            }
            scan.base_revision_tag = Some(off);
            scan.requested_revision = word(image, off + 16).unwrap();
            off += 24;
            continue;
        }
        if head == COMMON_MAGIC {
            if off + protocol::REQUEST_HEADER_SIZE > end {
                return Err(ScanError::RequestTruncated { offset: off });
            }
            let id = words::<2>(image, off + 16).unwrap();
            if seen_ids[..seen].contains(&id) {
                return Err(ScanError::DuplicateRequest { offset: off });
            }
            if seen == MAX_REQUESTS {
                return Err(ScanError::TooManyRequests);
            }
            seen_ids[seen] = id;
            seen += 1;
            if let Some(feature) = protocol::feature_for_id(id) {
                let revision = word(image, off + 32).unwrap();
                if off + protocol::request_size(feature, revision) > end {
                    return Err(ScanError::RequestTruncated { offset: off });
                }
                scan.found[scan.count] = Some(Found {
                    feature,
                    offset: off,
                    revision,
                });
                scan.count += 1;
            }
            off += protocol::REQUEST_HEADER_SIZE;
            continue;
        }
        off += 8;
    }
    Ok(scan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_for_feature;
    extern crate std;
    use std::vec::Vec;

    fn put(img: &mut Vec<u8>, words: &[u64]) -> usize {
        let at = img.len();
        for w in words {
            img.extend_from_slice(&w.to_le_bytes());
        }
        at
    }

    fn request(img: &mut Vec<u8>, f: Feature, rev: u64) -> usize {
        let id = id_for_feature(f);
        put(img, &[id[0], id[1], id[2], id[3], rev, 0])
    }

    #[test]
    fn finds_requests_and_tag() {
        let mut img = Vec::new();
        put(&mut img, &[1, 2, 3]);
        put(&mut img, &REQUESTS_START_MARKER);
        let tag = put(
            &mut img,
            &[BASE_REVISION_MAGIC[0], BASE_REVISION_MAGIC[1], 3],
        );
        let hhdm = request(&mut img, Feature::Hhdm, 0);
        let mm = request(&mut img, Feature::MemoryMap, 0);
        put(&mut img, &REQUESTS_END_MARKER);
        // A request after the end marker must be ignored.
        request(&mut img, Feature::Rsdp, 0);
        let s = scan(&img).unwrap();
        assert_eq!(s.base_revision_tag, Some(tag));
        assert_eq!(s.requested_revision, 3);
        assert_eq!(s.get(Feature::Hhdm).unwrap().offset, hhdm);
        assert_eq!(s.get(Feature::MemoryMap).unwrap().offset, mm);
        assert!(s.get(Feature::Rsdp).is_none());
    }

    #[test]
    fn honours_last_start_marker() {
        let mut img = Vec::new();
        put(&mut img, &REQUESTS_START_MARKER);
        request(&mut img, Feature::Rsdp, 0);
        put(&mut img, &REQUESTS_START_MARKER);
        request(&mut img, Feature::Hhdm, 0);
        let s = scan(&img).unwrap();
        assert!(s.get(Feature::Rsdp).is_none());
        assert!(s.get(Feature::Hhdm).is_some());
    }

    #[test]
    fn no_markers_scans_everything() {
        let mut img = Vec::new();
        put(&mut img, &[0]);
        request(&mut img, Feature::Hhdm, 0);
        let s = scan(&img).unwrap();
        assert!(s.get(Feature::Hhdm).is_some());
        assert_eq!(s.requested_revision, 0);
        assert_eq!(s.base_revision_tag, None);
    }

    #[test]
    fn duplicates_are_refused() {
        let mut img = Vec::new();
        request(&mut img, Feature::Hhdm, 0);
        request(&mut img, Feature::Hhdm, 0);
        assert!(matches!(
            scan(&img),
            Err(ScanError::DuplicateRequest { .. })
        ));

        let mut img = Vec::new();
        put(
            &mut img,
            &[BASE_REVISION_MAGIC[0], BASE_REVISION_MAGIC[1], 3],
        );
        put(
            &mut img,
            &[BASE_REVISION_MAGIC[0], BASE_REVISION_MAGIC[1], 3],
        );
        assert!(matches!(
            scan(&img),
            Err(ScanError::DuplicateBaseRevision { .. })
        ));
    }

    #[test]
    fn unknown_duplicates_are_refused_too() {
        let mut img = Vec::new();
        put(&mut img, &[COMMON_MAGIC[0], COMMON_MAGIC[1], 7, 7, 0, 0]);
        put(&mut img, &[COMMON_MAGIC[0], COMMON_MAGIC[1], 7, 7, 0, 0]);
        assert!(matches!(
            scan(&img),
            Err(ScanError::DuplicateRequest { .. })
        ));
    }

    #[test]
    fn truncated_request_at_end() {
        let mut img = Vec::new();
        let id = id_for_feature(Feature::StackSize);
        put(&mut img, &[id[0], id[1], id[2], id[3], 0, 0]); // missing stack_size
        assert!(matches!(
            scan(&img),
            Err(ScanError::RequestTruncated { .. })
        ));
    }

    #[test]
    fn random_images_never_panic() {
        let mut rng = crate::testrng::Rng::new(1);
        for _ in 0..20_000 {
            let n = (rng.next() % 600) as usize;
            let mut img: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            // Sprinkle real magic so the interesting paths get hit.
            if n >= 64 && rng.next().is_multiple_of(2) {
                let at = (rng.next() as usize % (n - 48)) & !7;
                let magic = match rng.next() % 4 {
                    0 => [COMMON_MAGIC[0], COMMON_MAGIC[1]],
                    1 => BASE_REVISION_MAGIC,
                    2 => [REQUESTS_START_MARKER[0], REQUESTS_START_MARKER[1]],
                    _ => REQUESTS_END_MARKER,
                };
                img[at..at + 8].copy_from_slice(&magic[0].to_le_bytes());
                img[at + 8..at + 16].copy_from_slice(&magic[1].to_le_bytes());
            }
            let _ = scan(&img);
        }
    }
}
