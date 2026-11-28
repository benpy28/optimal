//! Language range matching for `:lang()` (selectors-4 7.2).
//!
//! An element's content language matches a language range when both are
//! well-formed (RFC 5646 section 2.1; for the range, after an initial `*`
//! is replaced with a language subtag) and the tag matches the range under
//! extended filtering (RFC 4647 section 3.3.2), ASCII case-insensitively.
//! `*` matches every tagged element (`und` included), and the empty range
//! matches only untagged ones.
//!
//! Not yet done: selectors-4 also asks for both to be *valid* (every subtag
//! registered) and for canonicalization to extlang form (RFC 5646 4.5)
//! before filtering. Both need the IANA Language Subtag Registry, which the
//! engine does not carry yet. Until then, a well-formed but unregistered tag
//! is treated as valid, and tags are compared as written.

/// Whether an element whose content language is `tag` (empty when the
/// language is not tagged) matches the language range `range`.
pub fn lang_matches(tag: &[u8], range: &[u8]) -> bool {
    if range.is_empty() {
        return tag.is_empty();
    }
    if tag.is_empty() || !well_formed(tag) {
        return false;
    }
    if range == b"*" {
        return true;
    }
    let well_formed_range = match range.strip_prefix(b"*-") {
        Some(rest) => {
            let mut t = b"zz-".to_vec();
            t.extend_from_slice(rest);
            well_formed(&t)
        }
        None => well_formed(range),
    };
    well_formed_range && extended_filter(tag, range)
}

/// RFC 4647 3.3.2, extended filtering, for one range and one tag.
fn extended_filter(tag: &[u8], range: &[u8]) -> bool {
    let mut r = range.split(|&b| b == b'-');
    let mut t = tag.split(|&b| b == b'-');
    // 2. The first subtags must match, or the range's must be `*`.
    let (Some(r0), Some(t0)) = (r.next(), t.next()) else {
        return false;
    };
    if r0 != b"*" && !r0.eq_ignore_ascii_case(t0) {
        return false;
    }
    let mut t_cur = t.next();
    // 3. Walk the rest of the range.
    for rs in r {
        // A. A wildcard matches zero or more subtags: skip it.
        if rs == b"*" {
            continue;
        }
        loop {
            match t_cur {
                // B. No tag subtags left: no match.
                None => return false,
                // C. Equal: both move on.
                Some(ts) if ts.eq_ignore_ascii_case(rs) => {
                    t_cur = t.next();
                    break;
                }
                // D. A singleton in the tag stops the search.
                Some(ts) if ts.len() == 1 => return false,
                // E. Otherwise the tag moves on.
                Some(_) => t_cur = t.next(),
            }
        }
    }
    true
}

/// Whether `tag` is a well-formed language tag (RFC 5646 2.1's
/// `Language-Tag` production), ASCII case-insensitively.
pub fn well_formed(tag: &[u8]) -> bool {
    if GRANDFATHERED.iter().any(|g| g.as_bytes().eq_ignore_ascii_case(tag)) {
        return true;
    }
    let subtags: Vec<&[u8]> = tag.split(|&b| b == b'-').collect();
    if subtags.iter().any(|s| s.is_empty() || s.len() > 8 || !s.iter().all(u8::is_ascii_alphanumeric)) {
        return false;
    }
    let alpha = |s: &[u8]| s.iter().all(u8::is_ascii_alphabetic);
    let digit = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    let mut i = 0;
    let n = subtags.len();
    let private = |i: usize| {
        // privateuse = "x" 1*("-" (1*8alphanum))
        subtags[i].eq_ignore_ascii_case(b"x") && i + 1 < n
    };
    if private(0) {
        return true;
    }
    // language = 2*3ALPHA ["-" extlang] / 4ALPHA / 5*8ALPHA
    let lang = subtags[0];
    if !alpha(lang) || lang.len() < 2 {
        return false;
    }
    i += 1;
    if lang.len() <= 3 {
        // extlang = 3ALPHA *2("-" 3ALPHA)
        let mut ext = 0;
        while ext < 3 && i < n && subtags[i].len() == 3 && alpha(subtags[i]) {
            ext += 1;
            i += 1;
        }
    }
    // ["-" script]: 4ALPHA
    if i < n && subtags[i].len() == 4 && alpha(subtags[i]) {
        i += 1;
    }
    // ["-" region]: 2ALPHA / 3DIGIT
    if i < n && ((subtags[i].len() == 2 && alpha(subtags[i])) || (subtags[i].len() == 3 && digit(subtags[i]))) {
        i += 1;
    }
    // *("-" variant): 5*8alphanum / (DIGIT 3alphanum)
    while i < n && (subtags[i].len() >= 5 || (subtags[i].len() == 4 && subtags[i][0].is_ascii_digit())) {
        i += 1;
    }
    // *("-" extension): singleton 1*("-" (2*8alphanum))
    while i < n && subtags[i].len() == 1 && !subtags[i].eq_ignore_ascii_case(b"x") {
        let start = i;
        i += 1;
        while i < n && subtags[i].len() >= 2 {
            i += 1;
        }
        if i == start + 1 {
            return false;
        }
    }
    // ["-" privateuse]
    if i < n && private(i) {
        return true;
    }
    i == n
}

/// RFC 5646 2.1's `grandfathered` tags, irregular then regular.
const GRANDFATHERED: &[&str] = &[
    "en-GB-oed",
    "i-ami",
    "i-bnn",
    "i-default",
    "i-enochian",
    "i-hak",
    "i-klingon",
    "i-lux",
    "i-mingo",
    "i-navajo",
    "i-pwn",
    "i-tao",
    "i-tay",
    "i-tsu",
    "sgn-BE-FR",
    "sgn-BE-NL",
    "sgn-CH-DE",
    "art-lojban",
    "cel-gaulish",
    "no-bok",
    "no-nyn",
    "zh-guoyu",
    "zh-hakka",
    "zh-min",
    "zh-min-nan",
    "zh-xiang",
];
