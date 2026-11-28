//! Tokenizing text that arrives in chunks (a style sheet still downloading).
//!
//! The scanner gives up on a token whenever it would have to look past the
//! end of the text so far ([`NeedMore`](crate::tokenizer::NeedMore)), and the
//! token is scanned again from its start once more text is here. So a token is
//! only produced from text that later chunks cannot change, and the result is
//! the same however the input is split.
//!
//! Design choice (logged): retry from the token start instead of a resumable
//! state machine. Why: one scanner for whole and chunked input, so the two
//! cannot disagree. A retry is only made once the unscanned text has at least
//! doubled (or the input has ended), so one long token arriving in many small
//! chunks is scanned a bounded number of times its length in total, not once
//! per chunk. Rejected: per-state resumption (a second copy of every
//! algorithm's control flow).

use crate::tokenizer::{Scanner, Spanned, TokenError};
use optimal_wtf8::{Wtf8, Wtf8Buf};

/// Drop consumed text once at least this many bytes of it have built up and
/// they are at least half the buffer.
const COMPACT_MIN: usize = 64 * 1024;

/// What [`StreamTokenizer::next_token`] found.
#[derive(Debug, PartialEq)]
pub enum Next<'a> {
    /// A token. Offsets count from the start of the whole stream.
    Token(Spanned<'a>),
    /// The next token needs more text: push a chunk, or finish the input.
    NeedInput,
    /// The input has ended and every token has been returned.
    End,
}

/// Tokenizes text pushed in chunks. Gives the same tokens, errors and
/// offsets as [`Tokenizer`](crate::Tokenizer) on the whole text, for any
/// split. Memory is bounded by the text not yet tokenized (plus at most as
/// much again, or 64 KiB, of consumed text awaiting compaction).
#[derive(Debug, Default)]
pub struct StreamTokenizer {
    /// Text not yet tokenized, after some already tokenized text before `pos`.
    pub(crate) buf: Wtf8Buf,
    /// Where the next token starts in `buf`.
    pos: usize,
    /// Stream offset of `buf[0]`.
    base: usize,
    /// No more text will be pushed.
    ended: bool,
    /// Don't scan again until the stream is at least this long, unless it
    /// has ended: the last scan from `pos` ran out of text.
    retry_at: usize,
    errors: Vec<TokenError>,
    /// Scans started, for the tests of the retry policy.
    #[cfg(test)]
    pub(crate) scans: usize,
}

impl StreamTokenizer {
    /// A tokenizer with no text yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a chunk. A surrogate pair split between two chunks is joined.
    ///
    /// # Panics
    /// If called after [`finish`](Self::finish).
    pub fn push(&mut self, chunk: Wtf8<'_>) {
        assert!(!self.ended, "text pushed after the end of input");
        if self.pos >= COMPACT_MIN && self.pos * 2 >= self.buf.len() {
            let rest = self.buf.as_wtf8().slice(self.pos, self.buf.len());
            self.buf = Wtf8Buf::from(rest);
            self.base += self.pos;
            self.pos = 0;
        }
        self.buf.push_wtf8(chunk);
    }

    /// Mark the end of input.
    pub fn finish(&mut self) {
        self.ended = true;
    }

    /// The next token. `unicode_ranges` is the "unicode ranges allowed" flag
    /// of css-syntax-3 4.3.1.
    pub fn next_token(&mut self, unicode_ranges: bool) -> Next<'_> {
        let len = self.base + self.buf.len();
        if !self.ended && len < self.retry_at {
            return Next::NeedInput;
        }
        #[cfg(test)]
        {
            self.scans += 1;
        }
        let errors_before = self.errors.len();
        let src = self.buf.as_bytes();
        let mut scanner = Scanner::new(src, self.pos, self.ended, self.base, &mut self.errors);
        match scanner.token(unicode_ranges) {
            Ok(Some(token)) => {
                let end = scanner.pos;
                let start = self.base + self.pos;
                self.pos = end;
                Next::Token(Spanned {
                    token,
                    start,
                    end: self.base + end,
                })
            }
            Ok(None) => Next::End,
            Err(_) => {
                self.errors.truncate(errors_before);
                // Retry once the unscanned text has doubled. (When nothing is
                // unscanned, that is right away: the scan costs nothing.)
                let unscanned = src.len() - self.pos;
                self.retry_at = len + unscanned;
                Next::NeedInput
            }
        }
    }

    /// The parse errors found so far, in input order. Offsets count from the
    /// start of the stream.
    pub fn errors(&self) -> &[TokenError] {
        &self.errors
    }
}
