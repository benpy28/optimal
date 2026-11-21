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
