//! The An+B microsyntax (css-syntax-3 section 6): recognizing
//! `<an+b>` in component values (6.2) and serializing it (9.2).
//!
//! A and B are clamped to `i32`. css-values-4 lets a UA clamp an integer
//! outside the range it supports to that range (section 5.1, "Integers");
//! `i32` is the range Optimal supports here, and the values are worked out
//! in `f64` first so that, for example, the negation in `-n- <integer>`
//! saturates at the far end instead of overflowing.

use core::fmt;

use crate::parser::{ComponentValue, Values};
use crate::tokenizer::{Number, Token};

/// An `<an+b>` value: the step A and the offset B. It represents the
/// (A*n + B)th elements of a list, for every integer n of 0 or more, with
/// the first element at index 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AnB {
    /// The step.
    pub a: i32,
    /// The offset.
    pub b: i32,
}

impl AnB {
    /// Whether the element at `index` (1 for the first) is one of the
    /// elements this value represents: whether A*n + B = index for some
    /// integer n of 0 or more.
    pub fn matches(self, index: u32) -> bool {
        let (a, d) = (i64::from(self.a), i64::from(index) - i64::from(self.b));
        if a == 0 {
            d == 0
        } else {
            d % a == 0 && d / a >= 0
        }
    }
}

/// The serialization of 9.2 ("serialize an `<an+b>` value").
impl fmt::Display for AnB {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let AnB { a, b } = *self;
        if a == 0 {
            return write!(f, "{b}");
        }
        match a {
            1 => f.write_str("n")?,
            -1 => f.write_str("-n")?,
            _ => write!(f, "{a}n")?,
        }
        if b > 0 {
            write!(f, "+{b}")
        } else if b < 0 {
            write!(f, "{b}")
        } else {
            Ok(())
        }
    }
}

/// Parses `values` as an `<an+b>`, with any whitespace before and after it.
/// `None` if they don't match the grammar.
pub fn parse_anb(values: Values<'_, '_>) -> Option<AnB> {
    let (anb, mut rest) = parse_anb_prefix(values)?;
    rest.all(|v| is_whitespace(v)).then_some(anb)
}

/// Parses the longest `<an+b>` at the start of `values` (after any
/// whitespace), for grammars where more follows it, such as
/// `:nth-child(<an+b> of <selector-list>)`. Returns the value and the
/// component values after it; whitespace after it is left in the rest.
/// `None` if no prefix matches the grammar.
pub fn parse_anb_prefix<'t, 'a>(values: Values<'t, 'a>) -> Option<(AnB, Values<'t, 'a>)> {
    let mut rest = values;
    // A function or block never matches: its first token is a
    // function-token or an opener, which no arm below takes.
    let first = next_non_whitespace(&mut rest)?;
    let anb = |a: f64, b: f64| AnB {
        a: a as i32,
        b: b as i32,
    };
    match first.token() {
        Token::Ident(v) if v.eq_ignore_ascii_case("odd") => Some((anb(2.0, 1.0), rest)),
        Token::Ident(v) if v.eq_ignore_ascii_case("even") => Some((anb(2.0, 0.0), rest)),
        // <integer>
        Token::Number(n) if n.is_integer => Some((anb(0.0, n.value), rest)),
        // <n-dimension>, <ndash-dimension>, <ndashdigit-dimension>
        Token::Dimension(n, unit) if n.is_integer => after_n(n.value, unit, rest),
        // '+'? n..., with no whitespace after the '+' (the † note).
        Token::Delim('+') => {
            let next = rest.next()?;
            match next.token() {
                Token::Ident(v) => after_n(1.0, v, rest),
                _ => None,
            }
        }
        Token::Ident(v) => match v.as_bytes().first() {
            Some(b'-') => after_n(-1.0, &v[1..], rest),
            _ => after_n(1.0, v, rest),
        },
        _ => None,
    }
}

/// Finishes a clause whose A part was `a` followed by `unit`, an ident or
/// dimension unit that must start with "n" (the sign of `-n` already taken
/// into `a`).
fn after_n<'t, 'a>(a: f64, unit: &str, mut rest: Values<'t, 'a>) -> Option<(AnB, Values<'t, 'a>)> {
    let anb = |b: f64| AnB {
        a: a as i32,
        b: b as i32,
    };
    let (n, tail) = unit.split_at_checked(1)?;
    if !n.eq_ignore_ascii_case("n") {
        return None;
    }
    if tail.is_empty() {
        // `n`, then an optional B part.
        return Some(match b_part(rest) {
            Some((b, after)) => (anb(b), after),
            None => (anb(0.0), rest),
        });
    }
    let digits = tail.strip_prefix('-')?;
    if digits.is_empty() {
        // `n-` then a <signless-integer>.
        let int = next_non_whitespace(&mut rest)?;
        return match int.token() {
            Token::Number(Number {
                value,
                is_integer: true,
                sign: None,
            }) => Some((anb(-value), rest)),
            _ => None,
        };
    }
    // `n-` and one or more digits: B is the negative number they spell.
    if !digits.bytes().all(|d| d.is_ascii_digit()) {
        return None;
    }
    let value: f64 = digits.parse().ok()?;
    Some((anb(-value), rest))
}

/// The B part after an `n`: a <signed-integer>, or a '+' or '-' and a
/// <signless-integer>. Returns B and the values after it.
fn b_part<'t, 'a>(mut rest: Values<'t, 'a>) -> Option<(f64, Values<'t, 'a>)> {
    let v = next_non_whitespace(&mut rest)?;
    match v.token() {
        Token::Number(Number {
            value,
            is_integer: true,
            sign: Some(_),
        }) => Some((*value, rest)),
        Token::Delim(c @ ('+' | '-')) => {
            let int = next_non_whitespace(&mut rest)?;
            match int.token() {
                Token::Number(Number {
                    value,
                    is_integer: true,
                    sign: None,
                }) => Some((if *c == '-' { -value } else { *value }, rest)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn is_whitespace(v: ComponentValue<'_, '_>) -> bool {
    matches!(v.token(), Token::Whitespace)
}

fn next_non_whitespace<'t, 'a>(values: &mut Values<'t, 'a>) -> Option<ComponentValue<'t, 'a>> {
    values.find(|&v| !is_whitespace(v))
}
