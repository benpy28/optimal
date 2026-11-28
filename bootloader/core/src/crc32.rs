//! CRC-32 (IEEE 802.3, reflected, polynomial 0xEDB88320), as GPT headers and
//! partition arrays use (UEFI 2.10, 5.3.2) and as the BIOS stage uses to check
//! its own image.

const fn make_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = make_table();

/// Running CRC: start with `Crc32::new()`, feed bytes, read `finish()`.
#[derive(Clone, Copy)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub const fn new() -> Self {
        Crc32(0xffff_ffff)
    }
    pub fn update(&mut self, data: &[u8]) {
        let mut c = self.0;
        for &b in data {
            c = TABLE[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }
    pub fn finish(self) -> u32 {
        !self.0
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    /// Bit-at-a-time reference, straight from the definition.
    fn reference(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    (c >> 1) ^ 0xEDB8_8320
                } else {
                    c >> 1
                };
            }
        }
        !c
    }

    #[test]
    fn check_values() {
        // The standard check value for CRC-32/ISO-HDLC.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        // One zero byte: worked by hand from the table, TABLE[0xff] ^ 0x00ffffff, inverted.
        assert_eq!(crc32(&[0]), 0xD202_EF8D);
    }

    #[test]
    fn matches_reference_and_streams() {
        let mut rng = crate::testrng::Rng::new(7);
        for len in 0..300 {
            let data: std::vec::Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            assert_eq!(crc32(&data), reference(&data));
            let cut = rng.below(len as u64 + 1) as usize;
            let mut c = Crc32::new();
            c.update(&data[..cut]);
            c.update(&data[cut..]);
            assert_eq!(c.finish(), crc32(&data));
        }
    }
}
