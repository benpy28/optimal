//! A literal model of css-syntax-3 tokenization, for differential tests.
//!
//! It follows the spec text step by step on a preprocessed copy of the input
//! (3.3 applied as its own pass, with each code point remembering the source
//! offset it came from), with "consume" and "reconsume" as an index into that
//! copy. It is slow and simple on purpose and shares no code with the crate:
//! the production scanner preprocesses while reading, borrows values, and
//! can stop and retry at chunk ends, and every one of those must give exactly
//! what this gives.

use optimal_css::{
    ErrorKind, HashKind, Number, Sign, Spanned, Token, TokenError, is_ident_code_point,
    is_ident_start_code_point,
};
use optimal_wtf8::Wtf8;
use std::borrow::Cow;

struct Model {
    /// Preprocessed code points.
    cps: Vec<char>,
    /// Source byte offset of each code point, plus the source length.
    offs: Vec<usize>,
    /// Index of the next input code point.
    i: usize,
    unicode_ranges: bool,
    errors: Vec<TokenError>,
}

/// 3.3 Preprocessing, keeping each code point's source offset.
fn preprocess(src: Wtf8<'_>) -> (Vec<char>, Vec<usize>) {
    let mut raw = Vec::new();
    let mut off = 0;
    for cp in src.code_points() {
        raw.push((cp, off));
        off += optimal_wtf8::encoded_len(cp);
    }
    let mut cps = Vec::new();
    let mut offs = Vec::new();
    let mut k = 0;
    while k < raw.len() {
        let (cp, o) = raw[k];
        offs.push(o);
        if cp == 0x0D && raw.get(k + 1).map(|r| r.0) == Some(0x0A) {
            cps.push('\n');
            k += 2;
            continue;
        }
        cps.push(match cp {
            0x0D | 0x0C => '\n',
            0 => '\u{FFFD}',
            _ => char::from_u32(cp).unwrap_or('\u{FFFD}'),
        });
        k += 1;
    }
    offs.push(src.len());
    (cps, offs)
}

fn whitespace(c: Option<char>) -> bool {
    matches!(c, Some('\n') | Some('\t') | Some(' '))
}
fn digit(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_digit())
}
fn hex(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_hexdigit())
}
fn ident_start(c: Option<char>) -> bool {
    c.is_some_and(is_ident_start_code_point)
}
fn ident(c: Option<char>) -> bool {
    c.is_some_and(is_ident_code_point)
}
fn non_printable(c: Option<char>) -> bool {
    c.is_some_and(|c| {
        let v = c as u32;
        v <= 8 || v == 0xB || (0xE..=0x1F).contains(&v) || v == 0x7F
    })
}

/// 4.3.8
fn are_valid_escape(a: Option<char>, b: Option<char>) -> bool {
    if a != Some('\\') {
        return false;
    }
    b != Some('\n')
}

/// 4.3.9
fn would_start_ident(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
    if a == Some('-') {
        return ident_start(b) || b == Some('-') || are_valid_escape(b, c);
    }
    if ident_start(a) {
        return true;
    }
    if a == Some('\\') {
        return are_valid_escape(a, b);
    }
    false
}

/// 4.3.10
fn would_start_number(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
    if a == Some('+') || a == Some('-') {
        if digit(b) {
            return true;
        }
        return b == Some('.') && digit(c);
    }
    if a == Some('.') {
        return digit(b);
    }
    digit(a)
}

impl Model {
    fn peek(&self, k: usize) -> Option<char> {
        self.cps.get(self.i + k).copied()
    }
    fn consume(&mut self) -> Option<char> {
        let c = self.peek(0);
        // Consuming EOF is allowed and moves past the end.
        self.i += 1;
        c
    }
    fn current(&self) -> Option<char> {
        self.cps.get(self.i - 1).copied()
    }
    fn reconsume(&mut self) {
        self.i -= 1;
    }
    fn offset(&self, idx: usize) -> usize {
        self.offs[idx.min(self.cps.len())]
    }
    fn error(&mut self, kind: ErrorKind, idx: usize) {
        let offset = self.offset(idx);
        self.errors.push(TokenError { kind, offset });
    }

    /// 4.3.2, one comment per call. Returns whether one was consumed.
    fn consume_comment(&mut self) -> bool {
        if !(self.peek(0) == Some('/') && self.peek(1) == Some('*')) {
            return false;
        }
        self.i += 2;
        loop {
            if self.peek(0).is_none() {
                self.error(ErrorKind::EofInComment, self.i);
                return true;
            }
            if self.peek(0) == Some('*') && self.peek(1) == Some('/') {
                self.i += 2;
                return true;
            }
            self.i += 1;
        }
    }

    /// 4.3.1
    fn consume_token(&mut self) -> Option<Token<'static>> {
        if self.consume_comment() {
            return Some(Token::Comment);
        }
        let start = self.i;
        let c = self.consume();
        let tok = match c {
            None => {
                self.i = start;
                return None;
            }
            _ if whitespace(c) => {
                while whitespace(self.peek(0)) {
                    self.consume();
                }
                Token::Whitespace
            }
            Some('"') => self.consume_string(None),
            Some('#') => {
                if ident(self.peek(0)) || are_valid_escape(self.peek(0), self.peek(1)) {
                    let mut kind = HashKind::Unrestricted;
                    if would_start_ident(self.peek(0), self.peek(1), self.peek(2)) {
                        kind = HashKind::Id;
                    }
                    let v = self.consume_ident_sequence();
                    Token::Hash(Cow::Owned(v), kind)
                } else {
                    Token::Delim('#')
                }
            }
            Some('\'') => self.consume_string(None),
            Some('(') => Token::OpenParen,
            Some(')') => Token::CloseParen,
            Some('+') => {
                if would_start_number(self.current(), self.peek(0), self.peek(1)) {
                    self.reconsume();
                    self.consume_numeric()
                } else {
                    Token::Delim('+')
                }
            }
            Some(',') => Token::Comma,
            Some('-') => {
                if would_start_number(self.current(), self.peek(0), self.peek(1)) {
                    self.reconsume();
                    self.consume_numeric()
                } else if self.peek(0) == Some('-') && self.peek(1) == Some('>') {
                    self.consume();
                    self.consume();
                    Token::Cdc
                } else if would_start_ident(self.current(), self.peek(0), self.peek(1)) {
                    self.reconsume();
                    self.consume_ident_like()
                } else {
                    Token::Delim('-')
                }
            }
            Some('.') => {
                if would_start_number(self.current(), self.peek(0), self.peek(1)) {
                    self.reconsume();
                    self.consume_numeric()
                } else {
                    Token::Delim('.')
                }
            }
            Some(':') => Token::Colon,
            Some(';') => Token::Semicolon,
            Some('<') => {
                if self.peek(0) == Some('!')
                    && self.peek(1) == Some('-')
                    && self.peek(2) == Some('-')
                {
                    self.i += 3;
                    Token::Cdo
                } else {
                    Token::Delim('<')
                }
            }
            Some('@') => {
                if would_start_ident(self.peek(0), self.peek(1), self.peek(2)) {
                    Token::AtKeyword(Cow::Owned(self.consume_ident_sequence()))
                } else {
                    Token::Delim('@')
                }
            }
            Some('[') => Token::OpenSquare,
            Some('\\') => {
                if are_valid_escape(self.current(), self.peek(0)) {
                    self.reconsume();
                    self.consume_ident_like()
                } else {
                    self.error(ErrorKind::InvalidEscape, start);
                    Token::Delim('\\')
                }
            }
            Some(']') => Token::CloseSquare,
            Some('{') => Token::OpenCurly,
            Some('}') => Token::CloseCurly,
            _ if digit(c) => {
                self.reconsume();
                self.consume_numeric()
            }
            Some('U') | Some('u') => {
                if self.unicode_ranges
                    && self.peek(0) == Some('+')
                    && (self.peek(1) == Some('?') || hex(self.peek(1)))
                {
                    self.reconsume();
                    self.consume_unicode_range()
                } else {
                    self.reconsume();
                    self.consume_ident_like()
                }
            }
            _ if ident_start(c) => {
                self.reconsume();
                self.consume_ident_like()
            }
            Some(other) => Token::Delim(other),
        };
        Some(tok)
    }

    /// 4.3.3
    fn consume_numeric(&mut self) -> Token<'static> {
        let number = self.consume_number();
        if would_start_ident(self.peek(0), self.peek(1), self.peek(2)) {
            let unit = self.consume_ident_sequence();
            return Token::Dimension(number, Cow::Owned(unit));
        }
        if self.peek(0) == Some('%') {
            self.consume();
            return Token::Percentage {
                value: number.value,
                sign: number.sign,
            };
        }
        Token::Number(number)
    }

    /// 4.3.4
    fn consume_ident_like(&mut self) -> Token<'static> {
        let string = self.consume_ident_sequence();
        if string.eq_ignore_ascii_case("url") && self.peek(0) == Some('(') {
            self.consume();
            while whitespace(self.peek(0)) && whitespace(self.peek(1)) {
                self.consume();
            }
            let q = |c: Option<char>| c == Some('"') || c == Some('\'');
            if q(self.peek(0)) || (whitespace(self.peek(0)) && q(self.peek(1))) {
                return Token::Function(Cow::Owned(string));
            }
            return self.consume_url();
        }
        if self.peek(0) == Some('(') {
            self.consume();
            return Token::Function(Cow::Owned(string));
        }
        Token::Ident(Cow::Owned(string))
    }

    /// 4.3.5
    fn consume_string(&mut self, ending: Option<char>) -> Token<'static> {
        let ending = ending.or(self.current());
        let mut value = String::new();
        loop {
            let c = self.consume();
            if c == ending {
                return Token::String(Cow::Owned(value));
            }
            match c {
                None => {
                    self.error(ErrorKind::EofInString, self.i - 1);
                    self.i -= 1;
                    return Token::String(Cow::Owned(value));
                }
                Some('\n') => {
                    self.error(ErrorKind::NewlineInString, self.i - 1);
                    self.reconsume();
                    return Token::BadString;
                }
                Some('\\') => {
                    if self.peek(0).is_none() {
                        // do nothing
                    } else if self.peek(0) == Some('\n') {
                        self.consume();
                    } else {
                        value.push(self.consume_escaped());
                    }
                }
                Some(other) => value.push(other),
            }
        }
    }

    /// 4.3.6
    fn consume_url(&mut self) -> Token<'static> {
        let mut value = String::new();
        while whitespace(self.peek(0)) {
            self.consume();
        }
        loop {
            let c = self.consume();
            match c {
                Some(')') => return Token::Url(Cow::Owned(value)),
                None => {
                    self.i -= 1;
                    self.error(ErrorKind::EofInUrl, self.i);
                    return Token::Url(Cow::Owned(value));
                }
                _ if whitespace(c) => {
                    while whitespace(self.peek(0)) {
                        self.consume();
                    }
                    if self.peek(0) == Some(')') {
                        self.consume();
                        return Token::Url(Cow::Owned(value));
                    }
                    if self.peek(0).is_none() {
                        self.error(ErrorKind::EofInUrl, self.i);
                        return Token::Url(Cow::Owned(value));
                    }
                    self.consume_bad_url_remnants();
                    return Token::BadUrl;
                }
                Some('"') | Some('\'') | Some('(') => {
                    self.error(ErrorKind::UnexpectedCharInUrl, self.i - 1);
                    self.consume_bad_url_remnants();
                    return Token::BadUrl;
                }
                _ if non_printable(c) => {
                    self.error(ErrorKind::UnexpectedCharInUrl, self.i - 1);
                    self.consume_bad_url_remnants();
                    return Token::BadUrl;
                }
                Some('\\') => {
                    if are_valid_escape(self.current(), self.peek(0)) {
                        value.push(self.consume_escaped());
                    } else {
                        self.error(ErrorKind::InvalidEscapeInUrl, self.i - 1);
                        self.consume_bad_url_remnants();
                        return Token::BadUrl;
                    }
                }
                Some(other) => value.push(other),
            }
        }
    }

    /// 4.3.7
    fn consume_escaped(&mut self) -> char {
        let c = self.consume();
        if hex(c) {
            let mut digits = c.unwrap().to_string();
            while digits.len() < 6 && hex(self.peek(0)) {
                digits.push(self.consume().unwrap());
            }
            if whitespace(self.peek(0)) {
                self.consume();
            }
            let n = u32::from_str_radix(&digits, 16).unwrap();
            if n == 0 || (0xD800..=0xDFFF).contains(&n) || n > 0x10FFFF {
                return '\u{FFFD}';
            }
            return char::from_u32(n).unwrap();
        }
        match c {
            None => {
                self.i -= 1;
                self.error(ErrorKind::EofInEscape, self.i);
                '\u{FFFD}'
            }
            Some(other) => other,
        }
    }

    /// 4.3.12
    fn consume_ident_sequence(&mut self) -> String {
        let mut result = String::new();
        loop {
            let c = self.consume();
            if ident(c) {
                result.push(c.unwrap());
            } else if are_valid_escape(self.current(), self.peek(0)) {
                result.push(self.consume_escaped());
            } else {
                self.reconsume();
                return result;
            }
        }
    }

    /// 4.3.13
    fn consume_number(&mut self) -> Number {
        let mut is_integer = true;
        let mut number_part = String::new();
        let mut exponent_part = String::new();
        let mut sign = None;
        if self.peek(0) == Some('+') || self.peek(0) == Some('-') {
            let c = self.consume().unwrap();
            number_part.push(c);
            sign = Some(if c == '+' { Sign::Plus } else { Sign::Minus });
        }
        while digit(self.peek(0)) {
            number_part.push(self.consume().unwrap());
        }
        if self.peek(0) == Some('.') && digit(self.peek(1)) {
            number_part.push(self.consume().unwrap());
            while digit(self.peek(0)) {
                number_part.push(self.consume().unwrap());
            }
            is_integer = false;
        }
        let e = self.peek(0) == Some('e') || self.peek(0) == Some('E');
        let s1 = self.peek(1) == Some('+') || self.peek(1) == Some('-');
        if e && (digit(self.peek(1)) || (s1 && digit(self.peek(2)))) {
            self.consume();
            if self.peek(0) == Some('+') || self.peek(0) == Some('-') {
                exponent_part.push(self.consume().unwrap());
            }
            while digit(self.peek(0)) {
                exponent_part.push(self.consume().unwrap());
            }
            is_integer = false;
        }
        // number part * 10^exponent, rounded once.
        let text = if exponent_part.is_empty() {
            number_part
        } else {
            format!("{number_part}e{exponent_part}")
        };
        Number {
            value: text.parse().unwrap(),
            is_integer,
            sign,
        }
    }

    /// 4.3.14
    fn consume_unicode_range(&mut self) -> Token<'static> {
        self.consume();
        self.consume();
        let mut first = String::new();
        while first.len() < 6 && hex(self.peek(0)) {
            first.push(self.consume().unwrap());
        }
        while first.len() < 6 && self.peek(0) == Some('?') {
            first.push(self.consume().unwrap());
        }
        if first.contains('?') {
            let start = u32::from_str_radix(&first.replace('?', "0"), 16).unwrap();
            let end = u32::from_str_radix(&first.replace('?', "F"), 16).unwrap();
            return Token::UnicodeRange { start, end };
        }
        let start = u32::from_str_radix(&first, 16).unwrap();
        if self.peek(0) == Some('-') && hex(self.peek(1)) {
            self.consume();
            let mut second = String::new();
            while second.len() < 6 && hex(self.peek(0)) {
                second.push(self.consume().unwrap());
            }
            let end = u32::from_str_radix(&second, 16).unwrap();
            return Token::UnicodeRange { start, end };
        }
        Token::UnicodeRange { start, end: start }
    }

    /// 4.3.15
    fn consume_bad_url_remnants(&mut self) {
        loop {
            let c = self.consume();
            if c == Some(')') {
                return;
            }
            if c.is_none() {
                self.i -= 1;
                return;
            }
            if are_valid_escape(self.current(), self.peek(0)) {
                self.consume_escaped();
            }
        }
    }
}

/// Tokenize `src` with the literal model. `unicode_ranges` applies to every
/// token.
pub fn tokenize(src: Wtf8<'_>, unicode_ranges: bool) -> (Vec<Spanned<'static>>, Vec<TokenError>) {
    let (cps, offs) = preprocess(src);
    let mut m = Model {
        cps,
        offs,
        i: 0,
        unicode_ranges,
        errors: Vec::new(),
    };
    let mut out = Vec::new();
    loop {
        let start = m.offset(m.i);
        let Some(token) = m.consume_token() else {
            break;
        };
        out.push(Spanned {
            token,
            start,
            end: m.offset(m.i),
        });
    }
    (out, m.errors)
}
