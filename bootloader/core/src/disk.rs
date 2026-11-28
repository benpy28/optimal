//! Block devices as the BIOS stage sees them: whole sectors by LBA, from a
//! disk the firmware reads for us. Everything above this (partition tables,
//! FAT) treats what comes back as untrusted bytes.

/// Largest sector size any parser here accepts.
pub const MAX_SECTOR: usize = 4096;

pub trait BlockDevice {
    /// Bytes per sector: 512, 1024, 2048 or 4096.
    fn sector_size(&self) -> usize;
    /// Total sectors, when the firmware reports it.
    fn sectors(&self) -> Option<u64>;
    /// Reads whole sectors starting at `lba` into `buf`, whose length is a
    /// multiple of the sector size. False on any error.
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool;
}

pub fn valid_sector_size(s: usize) -> bool {
    matches!(s, 512 | 1024 | 2048 | 4096)
}

/// A byte-addressed window `[start_lba, start_lba + sectors)` of a device:
/// one partition. Reads outside it fail rather than reach other partitions.
pub struct Window<'a, D: BlockDevice> {
    dev: &'a mut D,
    start_lba: u64,
    sectors: u64,
    ss: usize,
    scratch: [u8; MAX_SECTOR],
}

impl<'a, D: BlockDevice> Window<'a, D> {
    /// None if the device's sector size is not one we accept.
    pub fn new(dev: &'a mut D, start_lba: u64, sectors: u64) -> Option<Self> {
        let ss = dev.sector_size();
        if !valid_sector_size(ss) {
            return None;
        }
        Some(Window {
            dev,
            start_lba,
            sectors,
            ss,
            scratch: [0; MAX_SECTOR],
        })
    }

    /// Window length in bytes (saturating).
    pub fn len(&self) -> u64 {
        self.sectors.saturating_mul(self.ss as u64)
    }

    pub fn is_empty(&self) -> bool {
        self.sectors == 0
    }

    /// Reads `buf.len()` bytes at byte `offset` inside the window. Whole
    /// aligned sectors go straight into `buf`; partial ones through a
    /// scratch sector.
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        let Some(end) = offset.checked_add(buf.len() as u64) else {
            return false;
        };
        if end > self.len() {
            return false;
        }
        let ss = self.ss as u64;
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let lba = pos / ss;
            let within = (pos % ss) as usize;
            let left = buf.len() - done;
            if within == 0 && left >= self.ss {
                // As many whole sectors as fit.
                let n = left / self.ss * self.ss;
                if !self
                    .dev
                    .read(self.start_lba + lba, &mut buf[done..done + n])
                {
                    return false;
                }
                done += n;
            } else {
                let take = (self.ss - within).min(left);
                if !self
                    .dev
                    .read(self.start_lba + lba, &mut self.scratch[..self.ss])
                {
                    return false;
                }
                buf[done..done + take].copy_from_slice(&self.scratch[within..within + take]);
                done += take;
            }
        }
        true
    }
}

#[cfg(test)]
pub mod sim {
    //! In-memory devices for host tests. `Sparse` keeps only written
    //! sectors (others read as zero), so a FAT32 volume costs kilobytes.
    extern crate std;
    use super::BlockDevice;
    use std::collections::BTreeMap;
    use std::vec;
    use std::vec::Vec;

    pub struct Sparse {
        pub ss: usize,
        pub total: u64,
        pub data: BTreeMap<u64, Vec<u8>>,
        /// Sectors read, to bound work in tests.
        pub reads: u64,
        /// LBAs that fail to read (a damaged disk).
        pub bad: Vec<u64>,
    }

    impl Sparse {
        pub fn new(ss: usize, total: u64) -> Self {
            Sparse {
                ss,
                total,
                data: BTreeMap::new(),
                reads: 0,
                bad: Vec::new(),
            }
        }

        pub fn write(&mut self, byte_off: u64, bytes: &[u8]) {
            let ss = self.ss;
            let mut done = 0usize;
            while done < bytes.len() {
                let p = byte_off + done as u64;
                let within = (p % ss as u64) as usize;
                let take = (ss - within).min(bytes.len() - done);
                let s = self
                    .data
                    .entry(p / ss as u64)
                    .or_insert_with(|| vec![0; ss]);
                s[within..within + take].copy_from_slice(&bytes[done..done + take]);
                done += take;
            }
        }

        pub fn read_bytes(&self, byte_off: u64, len: usize) -> Vec<u8> {
            let ss = self.ss as u64;
            let mut out = Vec::with_capacity(len);
            while out.len() < len {
                let p = byte_off + out.len() as u64;
                let within = (p % ss) as usize;
                let take = (self.ss - within).min(len - out.len());
                match self.data.get(&(p / ss)) {
                    Some(s) => out.extend_from_slice(&s[within..within + take]),
                    None => out.resize(out.len() + take, 0),
                }
            }
            out
        }
    }

    impl BlockDevice for Sparse {
        fn sector_size(&self) -> usize {
            self.ss
        }
        fn sectors(&self) -> Option<u64> {
            Some(self.total)
        }
        fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
            assert!(buf.len().is_multiple_of(self.ss), "partial sector read");
            let n = (buf.len() / self.ss) as u64;
            if lba.checked_add(n).is_none_or(|e| e > self.total) {
                return false;
            }
            for i in 0..n {
                if self.bad.contains(&(lba + i)) {
                    return false;
                }
                self.reads += 1;
                let dst = &mut buf[i as usize * self.ss..(i as usize + 1) * self.ss];
                match self.data.get(&(lba + i)) {
                    Some(s) => dst.copy_from_slice(s),
                    None => dst.fill(0),
                }
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sim::Sparse;
    use super::*;
    extern crate std;
    use std::vec;

    #[test]
    fn window_reads_unaligned_ranges_and_stays_inside() {
        let mut d = Sparse::new(512, 100);
        let pattern: std::vec::Vec<u8> = (0..100 * 512).map(|i| (i * 7 + 3) as u8).collect();
        d.write(0, &pattern);
        let mut w = Window::new(&mut d, 10, 20).unwrap();
        assert_eq!(w.len(), 20 * 512);
        for (off, len) in [
            (0u64, 1usize),
            (511, 2),
            (100, 3000),
            (512, 1024),
            (0, 20 * 512),
            (20 * 512 - 1, 1),
        ] {
            let mut buf = vec![0u8; len];
            assert!(w.read_at(off, &mut buf), "{off} {len}");
            let base = 10 * 512 + off as usize;
            assert_eq!(&buf[..], &pattern[base..base + len]);
        }
        // One byte past the window, and offsets that would wrap.
        assert!(!w.read_at(20 * 512, &mut [0u8; 1]));
        assert!(!w.read_at(20 * 512 - 1, &mut [0u8; 2]));
        assert!(!w.read_at(u64::MAX, &mut [0u8; 2]));
    }

    #[test]
    fn window_refuses_odd_sector_sizes() {
        let mut d = Sparse::new(520, 10);
        assert!(Window::new(&mut d, 0, 10).is_none());
        for ss in [512, 1024, 2048, 4096] {
            let mut d = Sparse::new(ss, 4);
            assert!(Window::new(&mut d, 0, 4).is_some());
        }
    }

    #[test]
    fn window_passes_read_errors_up() {
        let mut d = Sparse::new(512, 10);
        d.bad.push(3);
        let mut w = Window::new(&mut d, 0, 10).unwrap();
        assert!(w.read_at(0, &mut [0u8; 1536]));
        assert!(!w.read_at(0, &mut [0u8; 2048]));
        assert!(!w.read_at(3 * 512 + 5, &mut [0u8; 1]));
    }
}
