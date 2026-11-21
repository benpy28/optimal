//! The input stream: decoded text waiting to be tokenized.
//!
//! Text arrives in chunks (from the network, or from `document.write`). This
//! module normalizes newlines as it arrives (CR LF and lone CR become LF,
//! including a CR LF split across two chunks), joins a surrogate pair split
//! across two chunks, and keeps a byte cursor for the tokenizer.
//!
//! Consumed text is dropped when the buffer is compacted, so memory stays
//! bounded by the unconsumed part of the stream, not the whole document.
//! Positions for error reports are tracked as absolute offsets and turned into
//! line and column numbers only when an error is actually reported.

use crate::error::Position;
use optimal_wtf8::{Wtf8, Wtf8Buf, decode_at};

/// Compact once at least this many consumed bytes have built up and they are
/// at least half of the buffer.
const COMPACT_MIN: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) struct Input {
    /// Unconsumed text plus some already-consumed text before `pos`.
    buf: Wtf8Buf,
    /// Cursor into `buf`: the next input character starts here.
    pub(crate) pos: usize,
    /// Absolute offset of `buf[0]` in the whole (normalized) stream.
    base: u64,
    /// No more text will arrive.
    pub(crate) ended: bool,
    /// The last chunk ended with CR, so a leading LF in the next one is part of
    /// a CR LF pair and must be dropped.
    pending_cr: bool,
    /// Line and column at absolute offset `anchor.0`, for turning offsets into
    /// positions. Only moves forward, except after compaction where it is
    /// re-based.
    anchor: (u64, Position),
    /// Line and column of `buf[0]`.
    base_pos: Position,
}

impl Input {
    pub(crate) fn new() -> Self {
        let start = Position { line: 1, column: 1 };
        Input {
            buf: Wtf8Buf::new(),
            pos: 0,
            base: 0,
            ended: false,
            pending_cr: false,
            anchor: (0, start),
            base_pos: start,
        }
    }

    /// Append a chunk of text.
    pub(crate) fn push(&mut self, text: Wtf8<'_>) {
        assert!(!self.ended, "input pushed after end");
        self.maybe_compact();
        let bytes = text.as_bytes();
        if bytes.is_empty() {
            return;
        }
        let mut start = 0;
        if self.pending_cr {
            self.pending_cr = false;
            if bytes.first() == Some(&b'\n') {
                start = 1;
            }
        }
        while let Some(off) = bytes[start..].iter().position(|&b| b == b'\r') {
            let cr = start + off;
            self.push_raw(text.slice(start, cr));
            self.buf.push_ascii(b'\n');
            match bytes.get(cr + 1) {
                None => {
                    self.pending_cr = true;
                    start = cr + 1;
                }
                Some(b'\n') => start = cr + 2,
                Some(_) => start = cr + 1,
            }
        }
        self.push_raw(text.slice(start, bytes.len()));
    }

    fn push_raw(&mut self, piece: Wtf8<'_>) {
        // `push_wtf8` joins a high surrogate at the end of the buffer with a
        // low surrogate starting the piece. That is only allowed while the high
        // surrogate is unconsumed, which the tokenizer guarantees by never
        // consuming a trailing high surrogate before input has ended (see
        // `is_trailing_high_surrogate`).
        self.buf.push_wtf8(piece);
    }

    /// Mark the end of the stream.
    pub(crate) fn end(&mut self) {
        self.ended = true;
        self.pending_cr = false;
    }

    #[inline]
    pub(crate) fn bytes(&self) -> &[u8] {
        self.buf.as_bytes()
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }

    /// Absolute offset of buffer index `i`.
    #[inline]
    pub(crate) fn abs(&self, i: usize) -> u64 {
        self.base + i as u64
    }

    /// Decode the code point at buffer index `i` (which must be in bounds).
    #[inline]
    pub(crate) fn decode(&self, i: usize) -> (u32, usize) {
        decode_at(self.buf.as_bytes(), i)
    }

    /// The text `start..end` of the buffer.
    #[inline]
    pub(crate) fn slice(&self, start: usize, end: usize) -> Wtf8<'_> {
        self.buf.as_wtf8().slice(start, end)
    }

    /// Does the buffer end with a high surrogate starting at or after `i`?
    /// Such a surrogate must wait for more input: the next chunk may start
    /// with its low half.
    #[inline]
    pub(crate) fn is_trailing_high_surrogate(&self, i: usize) -> bool {
        let b = self.buf.as_bytes();
        !self.ended && b.len() == i + 3 && b[i] == 0xED && (0xA0..=0xAF).contains(&b[i + 1])
    }

    /// Drop consumed text when enough has built up.
    fn maybe_compact(&mut self) {
        if self.pos >= COMPACT_MIN && self.pos * 2 >= self.buf.len() {
            let keep = self.pos;
            // Move the position anchor to the new start so later lookups never
            // need the dropped text.
            let p = self.position_of(self.abs(keep));
            self.anchor = (self.abs(keep), p);
            self.base_pos = p;
            let rest = Wtf8Buf::from(self.buf.as_wtf8().slice(keep, self.buf.len()));
            self.buf = rest;
            self.base += keep as u64;
            self.pos = 0;
        }
    }

    /// Line and column of absolute offset `off`, which must not be before the
    /// start of the buffer or after its end.
    pub(crate) fn position_of(&mut self, off: u64) -> Position {
        let (mut a_off, mut a_pos) = self.anchor;
        if off < a_off {
            // Errors are reported in input order almost always; when one is
            // not, recount from the start of the buffer.
            debug_assert!(off >= self.base);
            a_off = self.base;
            a_pos = self.anchor_at_base();
        }
        let start = (a_off - self.base) as usize;
        let end = (off - self.base) as usize;
        let mut pos = a_pos;
        let bytes = self.buf.as_bytes();
        let mut i = start;
        while i < end {
            let b = bytes[i];
            if b == b'\n' {
                pos.line += 1;
                pos.column = 1;
                i += 1;
            } else if b < 0x80 {
                pos.column += 1;
                i += 1;
            } else {
                let (cp, n) = decode_at(bytes, i);
                pos.column += if cp >= 0x10000 { 2 } else { 1 };
                i += n;
            }
        }
        self.anchor = (off, pos);
        pos
    }

    /// Position of the first byte of the buffer.
    fn anchor_at_base(&self) -> Position {
        self.base_pos
    }
}
