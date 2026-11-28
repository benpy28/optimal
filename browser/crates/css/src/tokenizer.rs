//! The CSS tokenizer: css-syntax-3 section 4 ("Tokenization"), with the
//! input preprocessing of section 3.3 applied while reading.
//!
//! Design choices (logged: chosen, why, rejected):
//!
//! - **Preprocess while scanning**, not as a separate pass. Each read maps
//!   CR, CR LF and FF to one LF, and NUL and surrogates to U+FFFD, as it
//!   decodes. Why: token offsets then point into the text as given (what
//!   developer tools and error reports need), and no second copy of the sheet
//!   is made. Rejected: a filtered copy plus an offset map (twice the memory).
//! - **Values borrow** from the input when they are a plain run of the source
//!   (no escapes, no preprocessed code points), and are only copied
//!   otherwise. Why: most identifiers, strings and URLs in real sheets need
//!   no unescaping. Values are always valid Unicode, because preprocessing
//!   and escapes both turn surrogates into U+FFFD, so they are `str`.
//! - **Comments are tokens** ([`Token::Comment`]) instead of being dropped
//!   inside "consume a token". The spec's "consume comments" step is a loop
//!   that only ever skips comments, so returning each one as a token and
//!   letting the caller skip it gives the same token sequence otherwise, and
//!   keeps every byte of the input covered by exactly one token (needed for
//!   faithful serialization and source positions).
//! - **Numbers are `f64`, correctly rounded** from the exact decimal value the
//!   spec describes (number part times ten to the exponent), computed in one
//!   rounding from the source digits. The type flag ("integer" or "number")
//!   and the sign character are kept, as the spec requires. Values too large
//!   for `f64` become infinity here; range clamping is the job of the value
//!   parsers (css-values-4 section 5). Rejected: computing the number part
//!   and the power of ten separately (two roundings, wrong low bits).
//! - **One scanner for whole and chunked input.** The scanner is told whether
//!   the input has ended. Any look past the end of input that has not ended
//!   gives up on the token ([`NeedMore`]) and the stream tokenizer retries it
//!   once more text has arrived, so a token is only ever produced from text
//!   that more input cannot change.

use optimal_wtf8::{Wtf8, decode_at};
use std::borrow::Cow;

/// The sign character a numeric token was written with (css-syntax-3 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    /// `+`
    Plus,
    /// `-`
    Minus,
}

/// The type flag of a hash token (css-syntax-3 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashKind {
    /// What follows `#` is an ident sequence, so it can be an ID selector.
    Id,
    /// Anything else, such as `#123`.
    Unrestricted,
}

/// The numeric part of a `<number-token>` or `<dimension-token>`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Number {
    /// The value, correctly rounded to `f64` (infinity if out of range).
    pub value: f64,
    /// The type flag: `true` for "integer", `false` for "number".
    pub is_integer: bool,
    /// The sign character, if one was written.
    pub sign: Option<Sign>,
}

/// A CSS token (css-syntax-3 section 4). The end of input is not a token:
/// the tokenizers return `None` there.
#[derive(Clone, Debug, PartialEq)]
pub enum Token<'a> {
    /// `<ident-token>`
    Ident(Cow<'a, str>),
    /// `<function-token>`: the name, without the `(`.
    Function(Cow<'a, str>),
    /// `<at-keyword-token>`: the name, without the `@`.
    AtKeyword(Cow<'a, str>),
    /// `<hash-token>`: the value, without the `#`, and the type flag.
    Hash(Cow<'a, str>, HashKind),
    /// `<string-token>`: the value, without quotes, escapes resolved.
    String(Cow<'a, str>),
    /// `<bad-string-token>`
    BadString,
    /// `<url-token>`: the unquoted URL, escapes resolved.
    Url(Cow<'a, str>),
    /// `<bad-url-token>`
    BadUrl,
    /// `<delim-token>`
    Delim(char),
    /// `<number-token>`
    Number(Number),
    /// `<percentage-token>`: value and sign (no type flag, per the spec).
    Percentage {
        /// The value, correctly rounded to `f64`.
        value: f64,
        /// The sign character, if one was written.
        sign: Option<Sign>,
    },
    /// `<dimension-token>`: the number and its unit.
    Dimension(Number, Cow<'a, str>),
    /// `<unicode-range-token>`: an inclusive range (empty if `end < start`).
    /// Only produced when the caller allows unicode ranges.
    UnicodeRange {
        /// First code point of the range.
        start: u32,
        /// Last code point of the range.
        end: u32,
    },
    /// `<whitespace-token>`
    Whitespace,
    /// `<CDO-token>`: `<!--`
    Cdo,
    /// `<CDC-token>`: `-->`
    Cdc,
    /// `<colon-token>`
    Colon,
    /// `<semicolon-token>`
    Semicolon,
    /// `<comma-token>`
    Comma,
    /// `<[-token>`
    OpenSquare,
    /// `<]-token>`
    CloseSquare,
    /// `<(-token>`
    OpenParen,
    /// `<)-token>`
    CloseParen,
    /// `<{-token>`
    OpenCurly,
    /// `<}-token>`
    CloseCurly,
    /// A comment (`/* ... */`, or unterminated up to the end of input).
    /// Not a token in the spec, which skips comments; see the module notes.
    Comment,
}

impl Token<'_> {
    /// The same token with every value copied, so it no longer borrows.
    pub fn into_owned(self) -> Token<'static> {
        fn own(s: Cow<'_, str>) -> Cow<'static, str> {
            Cow::Owned(s.into_owned())
        }
        match self {
            Token::Ident(s) => Token::Ident(own(s)),
            Token::Function(s) => Token::Function(own(s)),
            Token::AtKeyword(s) => Token::AtKeyword(own(s)),
            Token::Hash(s, k) => Token::Hash(own(s), k),
            Token::String(s) => Token::String(own(s)),
            Token::Url(s) => Token::Url(own(s)),
            Token::Dimension(n, s) => Token::Dimension(n, own(s)),
            Token::BadString => Token::BadString,
            Token::BadUrl => Token::BadUrl,
            Token::Delim(c) => Token::Delim(c),
            Token::Number(n) => Token::Number(n),
            Token::Percentage { value, sign } => Token::Percentage { value, sign },
            Token::UnicodeRange { start, end } => Token::UnicodeRange { start, end },
            Token::Whitespace => Token::Whitespace,
            Token::Cdo => Token::Cdo,
            Token::Cdc => Token::Cdc,
            Token::Colon => Token::Colon,
            Token::Semicolon => Token::Semicolon,
            Token::Comma => Token::Comma,
            Token::OpenSquare => Token::OpenSquare,
            Token::CloseSquare => Token::CloseSquare,
            Token::OpenParen => Token::OpenParen,
            Token::CloseParen => Token::CloseParen,
            Token::OpenCurly => Token::OpenCurly,
            Token::CloseCurly => Token::CloseCurly,
            Token::Comment => Token::Comment,
        }
    }
}

/// A token and the byte range of the input it was read from. Ranges of
/// consecutive tokens touch, so together they cover the input exactly.
#[derive(Clone, Debug, PartialEq)]
pub struct Spanned<'a> {
    /// The token.
    pub token: Token<'a>,
    /// Byte offset of the token's first byte in the input.
    pub start: usize,
    /// Byte offset just past the token's last byte.
    pub end: usize,
}

/// The parse errors the tokenizer can report (css-syntax-3 section 4.3).
/// The spec names no error codes; these say which step found the error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// 4.3.2: the input ended inside a comment.
    EofInComment,
    /// 4.3.5: the input ended inside a string.
    EofInString,
    /// 4.3.5: an unescaped newline inside a string (gives a bad string).
    NewlineInString,
    /// 4.3.6: the input ended inside an unquoted `url(`.
    EofInUrl,
    /// 4.3.6: `"`, `'`, `(` or a non-printable code point inside an unquoted
    /// `url(` (gives a bad URL).
    UnexpectedCharInUrl,
    /// 4.3.6: a `\` followed by a newline inside an unquoted `url(`.
    InvalidEscapeInUrl,
    /// 4.3.1: a `\` followed by a newline outside a string or URL.
    InvalidEscape,
    /// 4.3.7: the input ended right after a `\`.
    EofInEscape,
}

/// A parse error: what went wrong, and the byte offset where it was found
/// (the offending code point, or the end of input for end-of-input errors).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TokenError {
    /// What went wrong.
    pub kind: ErrorKind,
    /// Byte offset in the input.
    pub offset: usize,
}

/// The scanner looked past the end of input that has not ended yet, so it
/// cannot finish the token until more text arrives.
#[derive(Debug)]
pub(crate) struct NeedMore;

type R<T> = Result<T, NeedMore>;

/// A code point after preprocessing, the number of source bytes it was read
/// from, and whether those bytes are exactly its UTF-8 encoding (so a value
/// can keep borrowing the source across it).
#[derive(Clone, Copy, Debug)]
struct Cp {
    c: char,
    len: usize,
    raw: bool,
}

const REPLACEMENT: char = '\u{FFFD}';

/// css-syntax-3 4.2: non-ASCII ident code point.
pub fn is_non_ascii_ident_code_point(c: char) -> bool {
    matches!(c as u32,
        0xB7
        | 0xC0..=0xD6
        | 0xD8..=0xF6
        | 0xF8..=0x37D
        | 0x37F..=0x1FFF
        | 0x200C
        | 0x200D
        | 0x203F
        | 0x2040
        | 0x2070..=0x218F
        | 0x2C00..=0x2FEF
        | 0x3001..=0xD7FF
        | 0xF900..=0xFDCF
        | 0xFDF0..=0xFFFD
        | 0x10000..)
}

/// css-syntax-3 4.2: ident-start code point (a letter, `_`, or a non-ASCII
/// ident code point).
pub fn is_ident_start_code_point(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || is_non_ascii_ident_code_point(c)
}

/// css-syntax-3 4.2: ident code point (an ident-start code point, a digit,
/// or `-`).
pub fn is_ident_code_point(c: char) -> bool {
    is_ident_start_code_point(c) || c.is_ascii_digit() || c == '-'
}

fn is_whitespace(c: Option<char>) -> bool {
    matches!(c, Some('\n' | '\t' | ' '))
}

fn is_digit(c: Option<char>) -> bool {
    matches!(c, Some('0'..='9'))
}

fn is_hex(c: Option<char>) -> bool {
    matches!(c, Some('0'..='9' | 'a'..='f' | 'A'..='F'))
}

fn hex_value(c: char) -> u32 {
    c.to_digit(16).expect("checked hex digit")
}

/// 4.2: non-printable code point. (U+0000 never reaches the tokenizer: it is
/// preprocessed to U+FFFD.)
fn is_non_printable(c: char) -> bool {
    matches!(c, '\0'..='\u{8}' | '\u{B}' | '\u{E}'..='\u{1F}' | '\u{7F}')
}

/// 4.3.8: do two code points form a valid escape?
fn valid_escape(first: Option<char>, second: Option<char>) -> bool {
    first == Some('\\') && second != Some('\n')
}

/// 4.3.9: would three code points start an ident sequence?
fn starts_ident(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
    match a {
        Some('-') => {
            matches!(b, Some(b) if is_ident_start_code_point(b) || b == '-') || valid_escape(b, c)
        }
        Some('\\') => valid_escape(a, b),
        Some(a) => is_ident_start_code_point(a),
        None => false,
    }
}

/// 4.3.10: would three code points start a number?
fn starts_number(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
    match a {
        Some('+' | '-') => is_digit(b) || (b == Some('.') && is_digit(c)),
        Some('.') => is_digit(b),
        a => is_digit(a),
    }
}

/// 4.3.11: would three code points start a unicode-range?
fn starts_unicode_range(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
    matches!(a, Some('U' | 'u')) && b == Some('+') && (c == Some('?') || is_hex(c))
}

/// A token value being built: a run of source bytes while it is a plain copy
/// of the source, then an owned string from the first code point that isn't.
struct Value {
    start: usize,
    end: usize,
    owned: Option<String>,
}

impl Value {
    fn new(at: usize) -> Self {
        Value {
            start: at,
            end: at,
            owned: None,
        }
    }

    fn owned(&mut self, src: &[u8]) -> &mut String {
        let (start, end) = (self.start, self.end);
        self.owned
            .get_or_insert_with(|| plain(src, start, end).to_owned())
    }

    /// Append `cp`, read from the source at `at`.
    fn push(&mut self, src: &[u8], cp: Cp, at: usize) {
        if cp.raw && self.owned.is_none() && at == self.end {
            self.end = at + cp.len;
        } else {
            self.owned(src).push(cp.c);
        }
    }

    /// Append a code point that is not a copy of the source (an escape).
    fn push_char(&mut self, src: &[u8], c: char) {
        self.owned(src).push(c);
    }

    fn finish(self, src: &[u8]) -> Cow<'_, str> {
        match self.owned {
            Some(s) => Cow::Owned(s),
            None => Cow::Borrowed(plain(src, self.start, self.end)),
        }
    }
}

/// A run of source bytes that holds only code points read raw (no
/// surrogates), so it is UTF-8.
fn plain(src: &[u8], start: usize, end: usize) -> &str {
    std::str::from_utf8(&src[start..end]).expect("a raw run is UTF-8")
}

/// The scanner: one token at a time from `src[pos..]`.
pub(crate) struct Scanner<'a, 'e> {
    src: &'a [u8],
    /// No more input will follow `src`.
    eof: bool,
    pub(crate) pos: usize,
    /// Added to every reported offset (where `src` starts in the stream).
    base: usize,
    errors: &'e mut Vec<TokenError>,
}

impl<'a, 'e> Scanner<'a, 'e> {
    pub(crate) fn new(
        src: &'a [u8],
        pos: usize,
        eof: bool,
        base: usize,
        errors: &'e mut Vec<TokenError>,
    ) -> Self {
        Scanner {
            src,
            eof,
            pos,
            base,
            errors,
        }
    }

    fn error(&mut self, kind: ErrorKind, at: usize) {
        self.errors.push(TokenError {
            kind,
            offset: self.base + at,
        });
    }

    /// The preprocessed code point whose source starts at byte `i`, or
    /// `None` at the end of input (3.3).
    fn at(&self, i: usize) -> R<Option<Cp>> {
        let s = self.src;
        if i >= s.len() {
            return if self.eof { Ok(None) } else { Err(NeedMore) };
        }
        let b = s[i];
        let lf = |len| Cp {
            c: '\n',
            len,
            raw: false,
        };
        if b < 0x80 {
            return Ok(Some(match b {
                b'\r' => match s.get(i + 1) {
                    Some(b'\n') => lf(2),
                    Some(_) => lf(1),
                    // A LF may still follow in the next chunk.
                    None if !self.eof => return Err(NeedMore),
                    None => lf(1),
                },
                0x0C => lf(1),
                0 => Cp {
                    c: REPLACEMENT,
                    len: 1,
                    raw: false,
                },
                _ => Cp {
                    c: b as char,
                    len: 1,
                    raw: true,
                },
            }));
        }
        let (cp, len) = decode_at(s, i);
        Ok(Some(match char::from_u32(cp) {
            Some(c) => Cp { c, len, raw: true },
            None => {
                // A high surrogate at the end of a chunk may yet pair with a
                // low one at the start of the next and become one code point.
                if cp < 0xDC00 && i + len == s.len() && !self.eof {
                    return Err(NeedMore);
                }
                Cp {
                    c: REPLACEMENT,
                    len,
                    raw: false,
                }
            }
        }))
    }

    /// The code point `k` places after the next input code point (`k = 0`
    /// is the next input code point itself), without consuming anything.
    fn nth(&self, k: usize) -> R<Option<char>> {
        let mut i = self.pos;
        for _ in 0..k {
            match self.at(i)? {
                Some(cp) => i += cp.len,
                None => return Ok(None),
            }
        }
        Ok(self.at(i)?.map(|cp| cp.c))
    }

    /// Consume the next input code point.
    fn bump(&mut self) -> R<Option<Cp>> {
        let cp = self.at(self.pos)?;
        if let Some(cp) = cp {
            self.pos += cp.len;
        }
        Ok(cp)
    }

    /// 4.3.1 Consume a token, preceded by 4.3.2 (one comment at a time).
    /// `Ok(None)` is the end of input.
    pub(crate) fn token(&mut self, unicode_ranges: bool) -> R<Option<Token<'a>>> {
        let start = self.pos;
        if self.nth(0)? == Some('/') && self.nth(1)? == Some('*') {
            return self.comment().map(Some);
        }
        let Some(cp) = self.bump()? else {
            return Ok(None);
        };
        let tok = match cp.c {
            '\n' | '\t' | ' ' => {
                while is_whitespace(self.nth(0)?) {
                    self.bump()?;
                }
                Token::Whitespace
            }
            '"' | '\'' => self.string(cp.c)?,
            '#' => {
                let (a, b) = (self.nth(0)?, self.nth(1)?);
                if matches!(a, Some(a) if is_ident_code_point(a)) || valid_escape(a, b) {
                    let kind = if starts_ident(a, b, self.nth(2)?) {
                        HashKind::Id
                    } else {
                        HashKind::Unrestricted
                    };
                    Token::Hash(self.ident_sequence()?, kind)
                } else {
                    Token::Delim('#')
                }
            }
            '(' => Token::OpenParen,
            ')' => Token::CloseParen,
            '+' | '.' => {
                if starts_number(Some(cp.c), self.nth(0)?, self.nth(1)?) {
                    self.pos = start;
                    self.numeric()?
                } else {
                    Token::Delim(cp.c)
                }
            }
            ',' => Token::Comma,
            '-' => {
                let (a, b) = (self.nth(0)?, self.nth(1)?);
                if starts_number(Some('-'), a, b) {
                    self.pos = start;
                    self.numeric()?
                } else if a == Some('-') && b == Some('>') {
                    self.pos += 2;
                    Token::Cdc
                } else if starts_ident(Some('-'), a, b) {
                    self.pos = start;
                    self.ident_like()?
                } else {
                    Token::Delim('-')
                }
            }
            ':' => Token::Colon,
            ';' => Token::Semicolon,
            '<' => {
                if self.nth(0)? == Some('!')
                    && self.nth(1)? == Some('-')
                    && self.nth(2)? == Some('-')
                {
                    self.pos += 3;
                    Token::Cdo
                } else {
                    Token::Delim('<')
                }
            }
            '@' => {
                if starts_ident(self.nth(0)?, self.nth(1)?, self.nth(2)?) {
                    Token::AtKeyword(self.ident_sequence()?)
                } else {
                    Token::Delim('@')
                }
            }
            '[' => Token::OpenSquare,
            '\\' => {
                if valid_escape(Some('\\'), self.nth(0)?) {
                    self.pos = start;
                    self.ident_like()?
                } else {
                    self.error(ErrorKind::InvalidEscape, start);
                    Token::Delim('\\')
                }
            }
            ']' => Token::CloseSquare,
            '{' => Token::OpenCurly,
            '}' => Token::CloseCurly,
            '0'..='9' => {
                self.pos = start;
                self.numeric()?
            }
            'U' | 'u'
                if unicode_ranges
                    && starts_unicode_range(Some(cp.c), self.nth(0)?, self.nth(1)?) =>
            {
                self.pos = start;
                self.unicode_range()?
            }
            c if is_ident_start_code_point(c) => {
                self.pos = start;
                self.ident_like()?
            }
            c => Token::Delim(c),
        };
        Ok(Some(tok))
    }

    /// 4.3.2, for one comment: the next two code points are `/*`.
    fn comment(&mut self) -> R<Token<'a>> {
        let body = self.pos + 2;
        match self.src[body..].windows(2).position(|w| w == b"*/") {
            Some(i) => self.pos = body + i + 2,
            None if self.eof => {
                self.pos = self.src.len();
                self.error(ErrorKind::EofInComment, self.pos);
            }
            None => return Err(NeedMore),
        }
        Ok(Token::Comment)
    }

    /// 4.3.3 Consume a numeric token.
    fn numeric(&mut self) -> R<Token<'a>> {
        let number = self.number()?;
        if starts_ident(self.nth(0)?, self.nth(1)?, self.nth(2)?) {
            let unit = self.ident_sequence()?;
            Ok(Token::Dimension(number, unit))
        } else if self.nth(0)? == Some('%') {
            self.pos += 1;
            Ok(Token::Percentage {
                value: number.value,
                sign: number.sign,
            })
        } else {
            Ok(Token::Number(number))
        }
    }

    /// 4.3.4 Consume an ident-like token.
    fn ident_like(&mut self) -> R<Token<'a>> {
        let name = self.ident_sequence()?;
        if self.nth(0)? != Some('(') {
            return Ok(Token::Ident(name));
        }
        self.pos += 1;
        if !name.eq_ignore_ascii_case("url") {
            return Ok(Token::Function(name));
        }
        while is_whitespace(self.nth(0)?) && is_whitespace(self.nth(1)?) {
            self.bump()?;
        }
        let a = self.nth(0)?;
        let quote = |c: Option<char>| matches!(c, Some('"' | '\''));
        if quote(a) || (is_whitespace(a) && quote(self.nth(1)?)) {
            Ok(Token::Function(name))
        } else {
            self.url()
        }
    }

    /// 4.3.5 Consume a string token, ending at `ending`.
    fn string(&mut self, ending: char) -> R<Token<'a>> {
        let mut value = Value::new(self.pos);
        loop {
            let at = self.pos;
            let Some(cp) = self.at(at)? else {
                self.error(ErrorKind::EofInString, at);
                return Ok(Token::String(value.finish(self.src)));
            };
            match cp.c {
                c if c == ending => {
                    self.pos += cp.len;
                    return Ok(Token::String(value.finish(self.src)));
                }
                '\n' => {
                    // Reconsumed: the newline is left for the next token.
                    self.error(ErrorKind::NewlineInString, at);
                    return Ok(Token::BadString);
                }
                '\\' => {
                    self.pos += 1;
                    match self.at(self.pos)? {
                        None => {}
                        Some(next) if next.c == '\n' => self.pos += next.len,
                        Some(_) => {
                            let c = self.escaped()?;
                            value.push_char(self.src, c);
                        }
                    }
                }
                _ => {
                    self.pos += cp.len;
                    value.push(self.src, cp, at);
                }
            }
        }
    }

    /// 4.3.6 Consume a url token (after `url(`).
    fn url(&mut self) -> R<Token<'a>> {
        while is_whitespace(self.nth(0)?) {
            self.bump()?;
        }
        let mut value = Value::new(self.pos);
        loop {
            let at = self.pos;
            let Some(cp) = self.bump()? else {
                self.error(ErrorKind::EofInUrl, at);
                return Ok(Token::Url(value.finish(self.src)));
            };
            match cp.c {
                ')' => return Ok(Token::Url(value.finish(self.src))),
                '\n' | '\t' | ' ' => {
                    while is_whitespace(self.nth(0)?) {
                        self.bump()?;
                    }
                    match self.nth(0)? {
                        Some(')') => {
                            self.pos += 1;
                            return Ok(Token::Url(value.finish(self.src)));
                        }
                        None => {
                            self.error(ErrorKind::EofInUrl, self.pos);
                            return Ok(Token::Url(value.finish(self.src)));
                        }
                        Some(_) => {
                            self.bad_url_remnants()?;
                            return Ok(Token::BadUrl);
                        }
                    }
                }
                '"' | '\'' | '(' => {
                    self.error(ErrorKind::UnexpectedCharInUrl, at);
                    self.bad_url_remnants()?;
                    return Ok(Token::BadUrl);
                }
                c if is_non_printable(c) => {
                    self.error(ErrorKind::UnexpectedCharInUrl, at);
                    self.bad_url_remnants()?;
                    return Ok(Token::BadUrl);
                }
                '\\' => {
                    if valid_escape(Some('\\'), self.nth(0)?) {
                        let c = self.escaped()?;
                        value.push_char(self.src, c);
                    } else {
                        self.error(ErrorKind::InvalidEscapeInUrl, at);
                        self.bad_url_remnants()?;
                        return Ok(Token::BadUrl);
                    }
                }
                _ => value.push(self.src, cp, at),
            }
        }
    }

    /// 4.3.7 Consume an escaped code point (the `\` is already consumed).
    fn escaped(&mut self) -> R<char> {
        let at = self.pos;
        let Some(cp) = self.bump()? else {
            self.error(ErrorKind::EofInEscape, at);
            return Ok(REPLACEMENT);
        };
        if !is_hex(Some(cp.c)) {
            return Ok(cp.c);
        }
        let mut value = hex_value(cp.c);
        for _ in 0..5 {
            match self.nth(0)? {
                Some(d) if is_hex(Some(d)) => {
                    value = value * 16 + hex_value(d);
                    self.pos += 1;
                }
                _ => break,
            }
        }
        if is_whitespace(self.nth(0)?) {
            self.bump()?;
        }
        Ok(match char::from_u32(value) {
            Some('\0') | None => REPLACEMENT,
            Some(c) => c,
        })
    }

    /// 4.3.12 Consume an ident sequence.
    fn ident_sequence(&mut self) -> R<Cow<'a, str>> {
        let mut value = Value::new(self.pos);
        loop {
            let at = self.pos;
            match self.at(at)? {
                Some(cp) if is_ident_code_point(cp.c) => {
                    self.pos += cp.len;
                    value.push(self.src, cp, at);
                }
                Some(cp)
                    if cp.c == '\\' && valid_escape(Some('\\'), self.at(at + 1)?.map(|n| n.c)) =>
                {
                    self.pos += 1;
                    let c = self.escaped()?;
                    value.push_char(self.src, c);
                }
                _ => return Ok(value.finish(self.src)),
            }
        }
    }

    /// 4.3.13 Consume a number.
    fn number(&mut self) -> R<Number> {
        let start = self.pos;
        let mut is_integer = true;
        let sign = match self.nth(0)? {
            Some('+') => Some(Sign::Plus),
            Some('-') => Some(Sign::Minus),
            _ => None,
        };
        if sign.is_some() {
            self.pos += 1;
        }
        self.digits()?;
        if self.nth(0)? == Some('.') && is_digit(self.nth(1)?) {
            self.pos += 1;
            self.digits()?;
            is_integer = false;
        }
        if matches!(self.nth(0)?, Some('e' | 'E')) {
            let b = self.nth(1)?;
            let skip = if is_digit(b) {
                1
            } else if matches!(b, Some('+' | '-')) && is_digit(self.nth(2)?) {
                2
            } else {
                0
            };
            if skip > 0 {
                self.pos += skip;
                self.digits()?;
                is_integer = false;
            }
        }
        // Every byte read is an ASCII sign, digit, '.' or 'e', in exactly the
        // syntax Rust's decimal parser takes, and it rounds once, correctly.
        let value = plain(self.src, start, self.pos)
            .parse::<f64>()
            .expect("a CSS number is a valid decimal literal");
        Ok(Number {
            value,
            is_integer,
            sign,
        })
    }

    fn digits(&mut self) -> R<()> {
        while is_digit(self.nth(0)?) {
            self.pos += 1;
        }
        Ok(())
    }

    /// 4.3.14 Consume a unicode-range token.
    fn unicode_range(&mut self) -> R<Token<'a>> {
        self.pos += 2;
        let mut digits = 0;
        let mut start = 0u32;
        while digits < 6 && is_hex(self.nth(0)?) {
            start = start * 16 + hex_value(self.nth(0)?.expect("checked"));
            self.pos += 1;
            digits += 1;
        }
        let mut questions = 0;
        while digits + questions < 6 && self.nth(0)? == Some('?') {
            self.pos += 1;
            questions += 1;
        }
        if questions > 0 {
            let scale = 16u32.pow(questions as u32);
            return Ok(Token::UnicodeRange {
                start: start * scale,
                end: start * scale + (scale - 1),
            });
        }
        if self.nth(0)? == Some('-') && is_hex(self.nth(1)?) {
            self.pos += 1;
            let mut end = 0u32;
            let mut digits = 0;
            while digits < 6 && is_hex(self.nth(0)?) {
                end = end * 16 + hex_value(self.nth(0)?.expect("checked"));
                self.pos += 1;
                digits += 1;
            }
            return Ok(Token::UnicodeRange { start, end });
        }
        Ok(Token::UnicodeRange { start, end: start })
    }

    /// 4.3.15 Consume the remnants of a bad url.
    fn bad_url_remnants(&mut self) -> R<()> {
        loop {
            match self.bump()? {
                None | Some(Cp { c: ')', .. }) => return Ok(()),
                Some(Cp { c: '\\', .. }) if valid_escape(Some('\\'), self.nth(0)?) => {
                    self.escaped()?;
                }
                Some(_) => {}
            }
        }
    }
}

/// Tokenizes text that is all available at once.
///
/// Token values borrow from the input where they can. The end of input
/// gives `None`, after which every call gives `None` again.
///
/// ```
/// use optimal_css::{Token, Tokenizer};
/// use optimal_wtf8::Wtf8;
///
/// let mut t = Tokenizer::new(Wtf8::from_str("a{color:red}"));
/// let tokens: Vec<_> = std::iter::from_fn(|| t.next_token()).map(|s| s.token).collect();
/// assert_eq!(tokens[0], Token::Ident("a".into()));
/// assert_eq!(tokens.len(), 6);
/// ```
#[derive(Debug)]
pub struct Tokenizer<'a> {
    src: &'a [u8],
    pos: usize,
    errors: Vec<TokenError>,
}

impl<'a> Tokenizer<'a> {
    /// A tokenizer over the whole of `src`.
    pub fn new(src: Wtf8<'a>) -> Self {
        Tokenizer {
            src: src.as_bytes(),
            pos: 0,
            errors: Vec::new(),
        }
    }

    /// The next token, with unicode-range tokens off (the normal case).
    pub fn next_token(&mut self) -> Option<Spanned<'a>> {
        self.next_token_with(false)
    }

    /// The next token. `unicode_ranges` is the "unicode ranges allowed" flag
    /// of 4.3.1, which only the `unicode-range` descriptor turns on.
    pub fn next_token_with(&mut self, unicode_ranges: bool) -> Option<Spanned<'a>> {
        let start = self.pos;
        let mut scanner = Scanner::new(self.src, start, true, 0, &mut self.errors);
        let token = scanner
            .token(unicode_ranges)
            .expect("the whole input is here, so no token waits for more");
        let end = scanner.pos;
        self.pos = end;
        token.map(|token| Spanned { token, start, end })
    }

    /// Byte offset of the next token.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// The parse errors found so far, in input order.
    pub fn errors(&self) -> &[TokenError] {
        &self.errors
    }
}

impl<'a> Iterator for Tokenizer<'a> {
    type Item = Spanned<'a>;

    fn next(&mut self) -> Option<Spanned<'a>> {
        self.next_token()
    }
}
