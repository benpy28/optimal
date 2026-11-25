//! WTF-8 strings for the Optimal browser engine.
//!
//! Web text is a sequence of UTF-16 code units, and it may hold *lone
//! surrogates* (for example `"\uD800"` in JavaScript, or a `document.write`
//! call that splits a surrogate pair in two). Rust's `str` cannot hold those,
//! so an engine built on `str` must either replace them (changing the page) or
//! store text as UTF-16 (doubling memory for most pages and losing fast byte
//! scanning).
//!
//! [WTF-8](https://simonsapin.github.io/wtf-8/) keeps the best of both: it is
//! UTF-8, plus lone surrogates encoded the same way UTF-8 would encode any
//! other code point in U+D800..=U+DFFF. A string without lone surrogates is
//! byte-for-byte the same as UTF-8, so ASCII delimiters can be found by
//! scanning bytes and text converts to `&str` for free.
//!
//! The one rule that keeps the encoding unique: a high surrogate followed by a
//! low surrogate is never stored as two 3-byte sequences. [`Wtf8Buf`] joins
//! them into the 4-byte encoding of the supplementary code point when they
//! meet, which is exactly what UTF-16 concatenation does.
//!
//! Design choice (logged): WTF-8 over UTF-16 or Latin-1/UTF-16 dual strings,
//! because it round-trips every web string exactly, stays as small as UTF-8,
//! and lets the parser scan bytes. Rejected: `String` (lossy for lone
//! surrogates) and UTF-16 (twice the memory for typical pages).

#![forbid(unsafe_code)]

use core::fmt;

/// A Unicode code point, including surrogates (U+D800..=U+DFFF), which `char`
/// cannot represent. Always `<= 0x10FFFF`.
pub type CodePoint = u32;

/// The largest code point.
pub const MAX_CODE_POINT: CodePoint = 0x10FFFF;

/// Is `cp` a surrogate (U+D800..=U+DFFF)?
#[inline]
pub const fn is_surrogate(cp: CodePoint) -> bool {
    cp >= 0xD800 && cp <= 0xDFFF
}

#[inline]
const fn is_high_surrogate(cp: CodePoint) -> bool {
    cp >= 0xD800 && cp <= 0xDBFF
}

#[inline]
const fn is_low_surrogate(cp: CodePoint) -> bool {
    cp >= 0xDC00 && cp <= 0xDFFF
}

/// Number of bytes the generalized UTF-8 encoding of `cp` takes.
#[inline]
pub const fn encoded_len(cp: CodePoint) -> usize {
    if cp < 0x80 {
        1
    } else if cp < 0x800 {
        2
    } else if cp < 0x10000 {
        3
    } else {
        4
    }
}

/// Encode `cp` (U+0080 or above, surrogates included) as generalized UTF-8.
/// Returns the buffer and the number of bytes used. ASCII never comes here:
/// every caller pushes it as a single byte first.
///
/// The parts are added rather than OR-ed: they never share a bit, so the
/// result is the same, and a slip in either operand changes the bytes.
#[inline]
fn encode(cp: CodePoint) -> ([u8; 4], usize) {
    debug_assert!((0x80..=MAX_CODE_POINT).contains(&cp));
    let mut b = [0u8; 4];
    let n = encoded_len(cp);
    match n {
        2 => {
            b[0] = 0xC0 + (cp >> 6) as u8;
            b[1] = 0x80 + (cp & 0x3F) as u8;
        }
        3 => {
            b[0] = 0xE0 + (cp >> 12) as u8;
            b[1] = 0x80 + ((cp >> 6) & 0x3F) as u8;
            b[2] = 0x80 + (cp & 0x3F) as u8;
        }
        _ => {
            b[0] = 0xF0 + (cp >> 18) as u8;
            b[1] = 0x80 + ((cp >> 12) & 0x3F) as u8;
            b[2] = 0x80 + ((cp >> 6) & 0x3F) as u8;
            b[3] = 0x80 + (cp & 0x3F) as u8;
        }
    }
    (b, n)
}

/// Decode the code point that starts at `bytes[i]`, returning it and its
/// encoded length.
///
/// `bytes` must be well-formed WTF-8 and `i` must be on a code point boundary
/// and in bounds; both hold for every byte slice this crate hands out.
#[inline]
pub fn decode_at(bytes: &[u8], i: usize) -> (CodePoint, usize) {
    let b0 = bytes[i] as u32;
    if b0 <= 0x7F {
        (b0, 1)
    } else if b0 < 0xE0 {
        (((b0 & 0x1F) << 6) + (bytes[i + 1] as u32 & 0x3F), 2)
    } else if b0 < 0xF0 {
        (
            ((b0 & 0x0F) << 12)
                + ((bytes[i + 1] as u32 & 0x3F) << 6)
                + (bytes[i + 2] as u32 & 0x3F),
            3,
        )
    } else {
        (
            ((b0 & 0x07) << 18)
                + ((bytes[i + 1] as u32 & 0x3F) << 12)
                + ((bytes[i + 2] as u32 & 0x3F) << 6)
                + (bytes[i + 3] as u32 & 0x3F),
            4,
        )
    }
}

/// If `bytes` ends with an encoded high surrogate, return it.
#[inline]
fn trailing_high_surrogate(bytes: &[u8]) -> Option<CodePoint> {
    let n = bytes.len();
    if n >= 3 && bytes[n - 3] == 0xED && (0xA0..=0xAF).contains(&bytes[n - 2]) {
        Some(decode_at(bytes, n - 3).0)
    } else {
        None
    }
}

/// If `bytes` starts with an encoded low surrogate, return it.
#[inline]
fn leading_low_surrogate(bytes: &[u8]) -> Option<CodePoint> {
    if bytes.len() >= 3 && bytes[0] == 0xED && (0xB0..=0xBF).contains(&bytes[1]) {
        Some(decode_at(bytes, 0).0)
    } else {
        None
    }
}

#[inline]
fn combine(high: CodePoint, low: CodePoint) -> CodePoint {
    0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
}

/// Check that `bytes` is well-formed WTF-8: generalized UTF-8 with shortest
/// encodings, no code point above U+10FFFF, and no high surrogate directly
/// followed by a low surrogate.
pub fn validate(bytes: &[u8]) -> bool {
    let mut i = 0;
    let mut prev_high = false;
    while i < bytes.len() {
        let b0 = bytes[i];
        let (cp, n) = if b0 < 0x80 {
            (b0 as u32, 1)
        } else {
            let n = match b0 {
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => return false,
            };
            if i + n > bytes.len() || !bytes[i + 1..i + n].iter().all(|&b| b & 0xC0 == 0x80) {
                return false;
            }
            let (cp, _) = decode_at(bytes, i);
            // Reject overlong encodings and values past U+10FFFF.
            if encoded_len(cp) != n || cp > MAX_CODE_POINT {
                return false;
            }
            (cp, n)
        };
        if prev_high && is_low_surrogate(cp) {
            return false;
        }
        prev_high = is_high_surrogate(cp);
        i += n;
    }
    true
}

/// A borrowed, well-formed WTF-8 string.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Wtf8<'a> {
    bytes: &'a [u8],
}

impl<'a> Wtf8<'a> {
    /// The empty string.
    pub const EMPTY: Wtf8<'static> = Wtf8 { bytes: &[] };

    /// View a `str` as WTF-8 (always valid, no copy).
    #[inline]
    pub const fn from_str(s: &'a str) -> Self {
        Wtf8 {
            bytes: s.as_bytes(),
        }
    }

    /// Check and wrap bytes. Returns `None` if they are not well-formed WTF-8.
    pub fn from_bytes(bytes: &'a [u8]) -> Option<Self> {
        validate(bytes).then_some(Wtf8 { bytes })
    }

    /// The encoded bytes.
    #[inline]
    pub const fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Length in bytes.
    #[inline]
    pub const fn len(self) -> usize {
        self.bytes.len()
    }

    /// Is the string empty?
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.bytes.is_empty()
    }

    /// The text as `&str`, if it holds no lone surrogates.
    #[inline]
    pub fn as_str(self) -> Option<&'a str> {
        // WTF-8 without surrogates is exactly UTF-8, and an encoded surrogate
        // is never valid UTF-8, so the standard check decides this.
        core::str::from_utf8(self.bytes).ok()
    }

    /// The text as a `String`, with each lone surrogate replaced by U+FFFD.
    pub fn to_string_lossy(self) -> String {
        self.to_string()
    }

    /// Iterate over the code points (surrogates included).
    #[inline]
    pub fn code_points(self) -> CodePoints<'a> {
        CodePoints {
            bytes: self.bytes,
            pos: 0,
        }
    }

    /// Iterate over the UTF-16 code units this string encodes.
    pub fn encode_utf16(self) -> EncodeUtf16<'a> {
        EncodeUtf16 {
            inner: self.code_points(),
            low: None,
        }
    }

    /// Length in UTF-16 code units (what JavaScript's `length` reports).
    pub fn utf16_len(self) -> usize {
        // Each 4-byte sequence is 2 units; every other code point is 1. Count
        // lead bytes, plus one more for each 4-byte lead.
        self.bytes
            .iter()
            .map(|&b| match b {
                0x80..=0xBF => 0,
                0xF0..=0xFF => 2,
                _ => 1,
            })
            .sum()
    }

    /// Compare with another string, ignoring ASCII case only.
    #[inline]
    pub fn eq_ignore_ascii_case(self, other: &str) -> bool {
        self.bytes.eq_ignore_ascii_case(other.as_bytes())
    }

    /// The bytes `start..end` as a string. Both ends must be code point
    /// boundaries; this panics otherwise, and never splits a code point.
    pub fn slice(self, start: usize, end: usize) -> Wtf8<'a> {
        assert!(self.is_boundary(start) && self.is_boundary(end) && start <= end);
        Wtf8 {
            bytes: &self.bytes[start..end],
        }
    }

    /// Is `i` the start of a code point (or the end of the string)?
    ///
    /// Slicing a 4-byte sequence in its middle would leave halves that look
    /// like surrogates; this crate never does that.
    #[inline]
    pub fn is_boundary(self, i: usize) -> bool {
        match self.bytes.get(i) {
            Some(&b) => b & 0xC0 != 0x80,
            None => i == self.bytes.len(),
        }
    }
}

impl fmt::Debug for Wtf8<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"")?;
        for cp in self.code_points() {
            match char::from_u32(cp) {
                Some(c) => {
                    for e in c.escape_debug() {
                        fmt::Write::write_char(f, e)?;
                    }
                }
                None => write!(f, "\\u{{{cp:X}}}")?,
            }
        }
        f.write_str("\"")
    }
}

impl fmt::Display for Wtf8<'_> {
    /// Writes the text with each lone surrogate replaced by U+FFFD.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(s) = self.as_str() {
            return f.write_str(s);
        }
        let mut start = 0;
        let mut i = 0;
        while i < self.bytes.len() {
            let (cp, n) = decode_at(self.bytes, i);
            if is_surrogate(cp) {
                // `start..i` holds no surrogates, so it is valid UTF-8.
                f.write_str(core::str::from_utf8(&self.bytes[start..i]).map_err(|_| fmt::Error)?)?;
                f.write_str("\u{FFFD}")?;
                start = i + n;
            }
            i += n;
        }
        f.write_str(core::str::from_utf8(&self.bytes[start..]).map_err(|_| fmt::Error)?)
    }
}

impl PartialEq<str> for Wtf8<'_> {
    fn eq(&self, other: &str) -> bool {
        self.bytes == other.as_bytes()
    }
}

impl PartialEq<&str> for Wtf8<'_> {
    fn eq(&self, other: &&str) -> bool {
        self.bytes == other.as_bytes()
    }
}

/// Iterator over code points. See [`Wtf8::code_points`].
#[derive(Clone)]
pub struct CodePoints<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Iterator for CodePoints<'_> {
    type Item = CodePoint;

    #[inline]
    fn next(&mut self) -> Option<CodePoint> {
        if self.pos >= self.bytes.len() {
            return None;
        }
        let (cp, n) = decode_at(self.bytes, self.pos);
        self.pos += n;
        Some(cp)
    }
}

/// Iterator over UTF-16 code units. See [`Wtf8::encode_utf16`].
#[derive(Clone)]
pub struct EncodeUtf16<'a> {
    inner: CodePoints<'a>,
    low: Option<u16>,
}

impl Iterator for EncodeUtf16<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<u16> {
        if let Some(low) = self.low.take() {
            return Some(low);
        }
        let cp = self.inner.next()?;
        if cp >= 0x10000 {
            let v = cp - 0x10000;
            self.low = Some(0xDC00 + (v & 0x3FF) as u16);
            Some(0xD800 + (v >> 10) as u16)
        } else {
            Some(cp as u16)
        }
    }
}

/// An owned, growable, well-formed WTF-8 string.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Wtf8Buf {
    bytes: Vec<u8>,
}

impl Wtf8Buf {
    /// An empty string. Does not allocate.
    #[inline]
    pub const fn new() -> Self {
        Wtf8Buf { bytes: Vec::new() }
    }

    /// An empty string with room for `cap` bytes.
    #[inline]
    pub fn with_capacity(cap: usize) -> Self {
        Wtf8Buf {
            bytes: Vec::with_capacity(cap),
        }
    }

    /// Decode UTF-16 (lone surrogates kept, pairs joined).
    pub fn from_utf16(units: &[u16]) -> Self {
        let mut s = Wtf8Buf::with_capacity(units.len());
        for &u in units {
            s.push_code_point(u as CodePoint);
        }
        s
    }

    /// Borrow as a [`Wtf8`] view.
    #[inline]
    pub fn as_wtf8(&self) -> Wtf8<'_> {
        Wtf8 { bytes: &self.bytes }
    }

    /// The encoded bytes.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Length in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Is the string empty?
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Bytes the buffer can hold without reallocating.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.bytes.capacity()
    }

    /// Remove everything, keeping the allocation.
    #[inline]
    pub fn clear(&mut self) {
        self.bytes.clear();
    }

    /// Reserve room for at least `additional` more bytes.
    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        self.bytes.reserve(additional);
    }

    /// Shorten to `len` bytes. `len` must be a code point boundary.
    pub fn truncate(&mut self, len: usize) {
        assert!(self.as_wtf8().is_boundary(len.min(self.len())));
        self.bytes.truncate(len);
    }

    /// Append an ASCII byte.
    #[inline]
    pub fn push_ascii(&mut self, b: u8) {
        assert!(b.is_ascii());
        self.bytes.push(b);
    }

    /// Append a `char`.
    #[inline]
    pub fn push_char(&mut self, c: char) {
        if c.is_ascii() {
            self.bytes.push(c as u8);
        } else {
            let mut tmp = [0u8; 4];
            self.bytes
                .extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
        }
    }

    /// Append any code point, surrogates included. A low surrogate that
    /// follows a high surrogate joins it into one supplementary code point,
    /// just as two UTF-16 units would.
    ///
    /// # Panics
    /// If `cp > 0x10FFFF`.
    #[inline]
    pub fn push_code_point(&mut self, cp: CodePoint) {
        assert!(cp <= MAX_CODE_POINT, "code point out of range");
        if cp < 0x80 {
            self.bytes.push(cp as u8);
            return;
        }
        if is_low_surrogate(cp)
            && let Some(high) = trailing_high_surrogate(&self.bytes)
        {
            let n = self.bytes.len();
            self.bytes.truncate(n - 3);
            let (b, len) = encode(combine(high, cp));
            self.bytes.extend_from_slice(&b[..len]);
            return;
        }
        let (b, len) = encode(cp);
        self.bytes.extend_from_slice(&b[..len]);
    }

    /// Append a `str`.
    #[inline]
    pub fn push_str(&mut self, s: &str) {
        self.bytes.extend_from_slice(s.as_bytes());
    }

    /// Append WTF-8 text, joining a surrogate pair split across the boundary.
    pub fn push_wtf8(&mut self, s: Wtf8<'_>) {
        let bytes = s.as_bytes();
        if let Some(low) = leading_low_surrogate(bytes)
            && trailing_high_surrogate(&self.bytes).is_some()
        {
            self.push_code_point(low);
            self.bytes.extend_from_slice(&bytes[3..]);
        } else {
            self.bytes.extend_from_slice(bytes);
        }
    }

    /// The text as `&str`, if it holds no lone surrogates.
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        self.as_wtf8().as_str()
    }

    /// Convert to a `String`, replacing each lone surrogate with U+FFFD.
    pub fn into_string_lossy(self) -> String {
        match String::from_utf8(self.bytes) {
            Ok(s) => s,
            Err(e) => Wtf8 {
                bytes: e.as_bytes(),
            }
            .to_string(),
        }
    }

    /// Take the bytes out, leaving an empty string with no allocation.
    #[inline]
    pub fn take(&mut self) -> Wtf8Buf {
        core::mem::take(self)
    }
}

impl From<&str> for Wtf8Buf {
    fn from(s: &str) -> Self {
        Wtf8Buf {
            bytes: s.as_bytes().to_vec(),
        }
    }
}

impl From<String> for Wtf8Buf {
    fn from(s: String) -> Self {
        Wtf8Buf {
            bytes: s.into_bytes(),
        }
    }
}

impl From<Wtf8<'_>> for Wtf8Buf {
    fn from(s: Wtf8<'_>) -> Self {
        Wtf8Buf {
            bytes: s.bytes.to_vec(),
        }
    }
}

impl fmt::Debug for Wtf8Buf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.as_wtf8(), f)
    }
}

impl fmt::Display for Wtf8Buf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.as_wtf8(), f)
    }
}

impl PartialEq<str> for Wtf8Buf {
    fn eq(&self, other: &str) -> bool {
        self.bytes == other.as_bytes()
    }
}

impl PartialEq<&str> for Wtf8Buf {
    fn eq(&self, other: &&str) -> bool {
        self.bytes == other.as_bytes()
    }
}

#[cfg(test)]
mod tests;
