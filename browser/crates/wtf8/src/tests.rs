//! Tests for WTF-8. The reference model is UTF-16: every operation must give
//! the same result as doing it on `Vec<u16>` and converting.

use super::*;
use proptest::prelude::*;

/// Hand-worked: U+1F600 is D83D DE00 in UTF-16 and F0 9F 98 80 in UTF-8.
#[test]
fn pair_split_across_pushes_joins() {
    let mut s = Wtf8Buf::new();
    s.push_code_point(0xD83D);
    // A lone high surrogate is ED A0 BD: 0xD83D = 1101 1000 0011 1101,
    // so 1110_1101, 10_100000, 10_111101.
    assert_eq!(s.as_bytes(), &[0xED, 0xA0, 0xBD]);
    assert_eq!(s.as_str(), None);
    s.push_code_point(0xDE00);
    assert_eq!(s.as_bytes(), "😀".as_bytes());
    assert_eq!(s.as_str(), Some("😀"));
}

#[test]
fn pair_split_across_wtf8_concat_joins() {
    let mut a = Wtf8Buf::from_utf16(&[0x61, 0xD83D]);
    let b = Wtf8Buf::from_utf16(&[0xDE00, 0x62]);
    a.push_wtf8(b.as_wtf8());
    assert_eq!(a.as_str(), Some("a😀b"));
}

#[test]
fn low_then_high_stays_two_lone_surrogates() {
    let s = Wtf8Buf::from_utf16(&[0xDE00, 0xD83D]);
    assert_eq!(s.len(), 6);
    assert!(validate(s.as_bytes()));
    assert_eq!(
        s.as_wtf8().encode_utf16().collect::<Vec<_>>(),
        [0xDE00, 0xD83D]
    );
    assert_eq!(s.to_string(), "\u{FFFD}\u{FFFD}");
}

#[test]
fn validate_rejects_bad_input() {
    assert!(validate(b"plain ascii"));
    assert!(validate("ünïcödé 😀".as_bytes()));
    assert!(!validate(&[0xC0, 0x80])); // overlong NUL
    assert!(!validate(&[0xE0, 0x80, 0x80])); // overlong
    assert!(!validate(&[0xF4, 0x90, 0x80, 0x80])); // above U+10FFFF
    assert!(!validate(&[0xFF]));
    assert!(!validate(&[0xE2, 0x82])); // truncated
    assert!(!validate(&[0x80])); // stray continuation
    // An encoded pair must be joined, so this spelling is invalid.
    assert!(!validate(&[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]));
    assert!(validate(&[0xED, 0xB8, 0x80, 0xED, 0xA0, 0xBD]));
}

#[test]
fn display_and_debug() {
    let s = Wtf8Buf::from_utf16(&[0x41, 0xD800, 0x42]);
    assert_eq!(s.to_string(), "A\u{FFFD}B");
    assert_eq!(format!("{s:?}"), "\"A\\u{D800}B\"");
    assert_eq!(s.clone().into_string_lossy(), "A\u{FFFD}B");
}

#[test]
fn utf16_len_counts_units() {
    assert_eq!(Wtf8::from_str("").utf16_len(), 0);
    assert_eq!(Wtf8::from_str("aé€😀").utf16_len(), 1 + 1 + 1 + 2);
    assert_eq!(Wtf8Buf::from_utf16(&[0xD800]).as_wtf8().utf16_len(), 1);
}

#[test]
fn slice_refuses_to_split_code_points() {
    let s = Wtf8::from_str("a😀");
    assert_eq!(s.slice(1, 5), "😀");
    assert!(std::panic::catch_unwind(|| s.slice(2, 5)).is_err());
}

#[test]
#[should_panic(expected = "code point out of range")]
fn push_rejects_out_of_range() {
    Wtf8Buf::new().push_code_point(0x110000);
}

/// Hand-worked encodings at every length boundary, byte for byte.
#[test]
fn encoding_boundaries_hand_worked() {
    let cases: &[(CodePoint, &[u8])] = &[
        (0x00, &[0x00]),
        (0x7F, &[0x7F]),
        (0x80, &[0xC2, 0x80]),
        (0x7FF, &[0xDF, 0xBF]),
        (0x800, &[0xE0, 0xA0, 0x80]),
        (0xD7FF, &[0xED, 0x9F, 0xBF]),
        (0xD800, &[0xED, 0xA0, 0x80]),
        (0xDBFF, &[0xED, 0xAF, 0xBF]),
        (0xDC00, &[0xED, 0xB0, 0x80]),
        (0xDFFF, &[0xED, 0xBF, 0xBF]),
        (0xFFFF, &[0xEF, 0xBF, 0xBF]),
        (0x10000, &[0xF0, 0x90, 0x80, 0x80]),
        (0x1F600, &[0xF0, 0x9F, 0x98, 0x80]),
        (0x10FFFF, &[0xF4, 0x8F, 0xBF, 0xBF]),
    ];
    for &(cp, bytes) in cases {
        assert_eq!(encoded_len(cp), bytes.len(), "encoded_len({cp:#X})");
        let mut s = Wtf8Buf::new();
        s.push_code_point(cp);
        assert_eq!(s.as_bytes(), bytes, "encoding of {cp:#X}");
        assert_eq!(decode_at(bytes, 0), (cp, bytes.len()), "decoding {cp:#X}");
        assert!(validate(bytes), "{cp:#X} is valid");
    }
}

/// Every code point, surrogates included, against the standard library for
/// scalar values and the generalized UTF-8 formula for surrogates. This covers
/// each length boundary exactly, which random cases almost never hit.
#[test]
fn every_code_point_encodes_and_decodes() {
    let mut s = Wtf8Buf::new();
    let mut units = Vec::new();
    for cp in 0..=MAX_CODE_POINT {
        s.clear();
        s.push_code_point(cp);
        let bytes = s.as_bytes();
        match char::from_u32(cp) {
            Some(c) => {
                let mut tmp = [0u8; 4];
                assert_eq!(bytes, c.encode_utf8(&mut tmp).as_bytes(), "{cp:#X}");
                units.clear();
                units.extend(c.encode_utf16(&mut [0u16; 2]).iter().copied());
            }
            None => {
                assert_eq!(
                    bytes,
                    [
                        0xED,
                        0xA0 + ((cp >> 6) & 0x1F) as u8,
                        0x80 + (cp & 0x3F) as u8
                    ],
                    "{cp:#X}"
                );
                units.clear();
                units.push(cp as u16);
            }
        }
        assert_eq!(encoded_len(cp), bytes.len(), "{cp:#X}");
        assert_eq!(decode_at(bytes, 0), (cp, bytes.len()), "{cp:#X}");
        assert!(validate(bytes), "{cp:#X}");
        let w = s.as_wtf8();
        assert_eq!(w.encode_utf16().collect::<Vec<_>>(), units, "{cp:#X}");
        assert_eq!(w.utf16_len(), units.len(), "{cp:#X}");
    }
}

/// Hand-worked checks of the small accessors and conversions, so each one is
/// pinned by a test of its own rather than only exercised in passing.
#[test]
fn accessors_and_conversions() {
    let lone = Wtf8Buf::from_utf16(&[0x61, 0xD800, 0x62]);
    let w = lone.as_wtf8();
    assert_eq!(w.len(), 5);
    assert_eq!(lone.len(), 5);
    assert!(!w.is_empty() && !lone.is_empty());
    assert!(Wtf8::EMPTY.is_empty() && Wtf8Buf::new().is_empty());
    assert_eq!(Wtf8::EMPTY.len(), 0);
    assert_eq!(w.to_string_lossy(), "a\u{FFFD}b");
    assert_eq!(lone.clone().into_string_lossy(), "a\u{FFFD}b");
    assert_eq!(Wtf8Buf::from("x\u{e9}").into_string_lossy(), "x\u{e9}");
    assert_eq!(w.as_str(), None);
    assert_eq!(Wtf8::from_str("h\u{e9}").as_str(), Some("h\u{e9}"));

    assert_eq!(Wtf8::from_bytes(b"ab"), Some(Wtf8::from_str("ab")));
    assert_eq!(Wtf8::from_bytes(lone.as_bytes()), Some(w));
    assert_eq!(Wtf8::from_bytes(&[0xFF]), None);

    assert!(Wtf8::from_str("DiV").eq_ignore_ascii_case("div"));
    assert!(!Wtf8::from_str("div").eq_ignore_ascii_case("dvi"));

    // Equality with str, both owned and borrowed, both ways round.
    assert!(Wtf8::from_str("ab") == *"ab");
    assert!(Wtf8::from_str("ab") != *"ac");
    assert!(Wtf8::from_str("ab") == "ab");
    assert!(Wtf8::from_str("ab") != "abc");
    let ab = Wtf8Buf::from("ab");
    assert!(ab == *"ab");
    assert!(ab != *"ba");
    assert!(ab == "ab");
    assert!(ab != "a");

    // Every constructor keeps the text.
    assert_eq!(Wtf8Buf::from("q\u{e9}").as_bytes(), "q\u{e9}".as_bytes());
    assert_eq!(
        Wtf8Buf::from(String::from("q\u{e9}")).as_bytes(),
        "q\u{e9}".as_bytes()
    );
    assert_eq!(Wtf8Buf::from(w).as_bytes(), lone.as_bytes());
}

#[test]
fn buffer_growth_and_editing() {
    let mut s = Wtf8Buf::with_capacity(37);
    assert!(s.capacity() >= 37 && s.is_empty());
    s.reserve(500);
    assert!(s.capacity() >= 500);

    s.push_ascii(b'a');
    s.push_char('b');
    s.push_char('\u{e9}');
    s.push_char('\u{1F600}');
    s.push_str("cd");
    assert_eq!(s, "ab\u{e9}\u{1F600}cd");
    assert!(!s.is_empty());

    let cap = s.capacity();
    s.truncate(4); // after "ab" + U+00E9
    assert_eq!(s, "ab\u{e9}");
    s.truncate(99); // past the end: no change, as with Vec
    assert_eq!(s, "ab\u{e9}");

    let taken = s.take();
    assert_eq!(taken, "ab\u{e9}");
    assert!(taken.capacity() >= cap.min(4));
    assert!(s.is_empty());
    assert_eq!(s.capacity(), 0, "take leaves no allocation behind");

    s.push_str("xyz");
    s.clear();
    assert!(s.is_empty() && s.capacity() >= 3);
}

#[test]
#[should_panic]
fn truncate_refuses_to_split_a_code_point() {
    let mut s = Wtf8Buf::from("a\u{e9}");
    s.truncate(2);
}

#[test]
#[should_panic]
fn push_ascii_rejects_non_ascii() {
    Wtf8Buf::new().push_ascii(0x80);
}

#[test]
fn boundaries_hand_worked() {
    let s = Wtf8::from_str("a\u{e9}\u{1F600}");
    let expect = [true, true, false, true, false, false, false, true];
    for (i, &b) in expect.iter().enumerate() {
        assert_eq!(s.is_boundary(i), b, "offset {i}");
    }
    assert!(!s.is_boundary(8), "past the end is not a boundary");
    assert!(Wtf8::EMPTY.is_boundary(0));
    assert!(!Wtf8::EMPTY.is_boundary(1));
}

fn any_units() -> impl Strategy<Value = Vec<u16>> {
    // Bias towards surrogates and ASCII so pairs and lone halves both appear.
    prop::collection::vec(
        prop_oneof![
            0x00u16..0x80,
            0xD800u16..0xE000,
            any::<u16>(),
            Just(0xD83Du16),
            Just(0xDE00u16),
        ],
        0..40,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(4096)
    ))]

    #[test]
    fn utf16_round_trip(units in any_units()) {
        let s = Wtf8Buf::from_utf16(&units);
        prop_assert!(validate(s.as_bytes()));
        prop_assert_eq!(s.as_wtf8().encode_utf16().collect::<Vec<_>>(), units.clone());
        prop_assert_eq!(s.as_wtf8().utf16_len(), units.len());
        prop_assert_eq!(s.to_string(), String::from_utf16_lossy(&units));
        prop_assert_eq!(s.as_str().map(str::to_owned), String::from_utf16(&units).ok());
    }

    #[test]
    fn concat_matches_utf16_concat(a in any_units(), b in any_units()) {
        let mut s = Wtf8Buf::from_utf16(&a);
        s.push_wtf8(Wtf8Buf::from_utf16(&b).as_wtf8());
        let mut joined = a.clone();
        joined.extend_from_slice(&b);
        prop_assert_eq!(s, Wtf8Buf::from_utf16(&joined));
    }

    #[test]
    fn code_points_match_char_decoding(units in any_units()) {
        let s = Wtf8Buf::from_utf16(&units);
        let expected: Vec<u32> = char::decode_utf16(units.iter().copied())
            .map(|r| match r { Ok(c) => c as u32, Err(e) => e.unpaired_surrogate() as u32 })
            .collect();
        prop_assert_eq!(s.as_wtf8().code_points().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn validate_agrees_with_std_on_utf8(bytes in prop::collection::vec(any::<u8>(), 0..24)) {
        // On input without encoded surrogates, WTF-8 validity is UTF-8 validity.
        let has_surrogate_lead = bytes.windows(2).any(|w| w[0] == 0xED && w[1] >= 0xA0);
        if !has_surrogate_lead {
            prop_assert_eq!(validate(&bytes), core::str::from_utf8(&bytes).is_ok());
        }
    }
}
