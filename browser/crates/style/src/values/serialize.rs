//! The serializations of single component values (cssom-1 "serialize a
//! CSS component value", and #common-serializing-idioms).
//!
//! The identifier and string idioms are the same algorithms the selectors
//! crate writes selectors with; they are repeated here (on a `String`)
//! rather than shared, so this crate's serialization doesn't depend on
//! that crate's formatting internals.

/// A `<number>`: "a base-ten number using digits 0-9 in the shortest form
/// possible, using "." to separate decimals (if any), rounding the value
/// if necessary to not produce more than 6 decimals, preceded by "-" if
/// it is negative", without scientific notation. A value exactly halfway
/// between two 6-decimal values rounds towards +∞, as css-color-4 asks of
/// its components ("rounded towards +∞, not truncated"); a value that
/// rounds to zero is not negative. Only for finite values.
pub fn number(v: f64) -> String {
    debug_assert!(v.is_finite());
    // 60 decimals hold every digit that matters: a finite f64 at least
    // 5e-7 has its digits past the 60th far below one unit in its last
    // place, so the text is exact as far as deciding the 7th digit's tail.
    let text = format!("{:.60}", v.abs());
    let (int, frac) = text.split_once('.').expect("a decimal point");
    let (kept, tail) = frac.split_at(6);
    let tail = tail.as_bytes();
    let half_or_more = tail[0] >= b'5';
    let exactly_half = tail[0] == b'5' && tail[1..].iter().all(|&d| d == b'0');
    let up = half_or_more && !(exactly_half && v < 0.0);
    let mut digits: Vec<u8> = int.bytes().chain(kept.bytes()).collect();
    if up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let split = digits.len() - 6;
    let mut s = String::from_utf8(digits[..split].to_vec()).expect("ASCII digits");
    let decimals = std::str::from_utf8(&digits[split..]).expect("ASCII digits");
    let decimals = decimals.trim_end_matches('0');
    if !decimals.is_empty() {
        s.push('.');
        s.push_str(decimals);
    }
    if v < 0.0 && s.bytes().any(|d| d != b'0' && d != b'.') {
        s.insert(0, '-');
    }
    s
}

/// "Escape a character as code point".
fn code_point(out: &mut String, c: char) {
    out.push_str(&format!("\\{:x} ", c as u32));
}

/// "Serialize an identifier".
pub fn identifier(out: &mut String, s: &str) {
    let mut chars = s.chars();
    let first = chars.next();
    let second = chars.next();
    for (i, c) in s.chars().enumerate() {
        match c {
            '\0' => out.push('\u{FFFD}'),
            '\u{1}'..='\u{1f}' | '\u{7f}' => code_point(out, c),
            '0'..='9' if i == 0 => code_point(out, c),
            '0'..='9' if i == 1 && first == Some('-') => code_point(out, c),
            '-' if i == 0 && second.is_none() => {
                out.push('\\');
                out.push(c);
            }
            c if optimal_css::is_non_ascii_ident_code_point(c)
                || c == '-'
                || c == '_'
                || c.is_ascii_alphanumeric() =>
            {
                out.push(c)
            }
            c => {
                out.push('\\');
                out.push(c);
            }
        }
    }
}

/// "Serialize a string".
pub fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\0' => out.push('\u{FFFD}'),
            '\u{1}'..='\u{1f}' | '\u{7f}' => code_point(out, c),
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// "Serialize a URL": `url(`, the URL serialized as a string, `)`.
pub fn url(out: &mut String, s: &str) {
    out.push_str("url(");
    string(out, s);
    out.push(')');
}
