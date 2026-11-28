//! Hand-worked cases. Each names the css-syntax-3 step it checks and works
//! the expected result out from the spec text in a comment.

use crate::{
    ErrorKind, HashKind, Next, Number, Sign, StreamTokenizer, Token, TokenError, Tokenizer,
};
use optimal_wtf8::{Wtf8, Wtf8Buf};
use std::borrow::Cow;

fn tokens_of(
    src: Wtf8<'_>,
    unicode_ranges: bool,
) -> (Vec<(Token<'_>, usize, usize)>, Vec<TokenError>) {
    let mut t = Tokenizer::new(src);
    let mut out = Vec::new();
    while let Some(s) = t.next_token_with(unicode_ranges) {
        out.push((s.token, s.start, s.end));
    }
    (out, t.errors().to_vec())
}

/// Just the tokens of `src`, with unicode ranges off.
fn toks(src: &str) -> Vec<Token<'_>> {
    tokens_of(Wtf8::from_str(src), false)
        .0
        .into_iter()
        .map(|t| t.0)
        .collect()
}

fn errs(src: &str) -> Vec<(ErrorKind, usize)> {
    tokens_of(Wtf8::from_str(src), false)
        .1
        .into_iter()
        .map(|e| (e.kind, e.offset))
        .collect()
}

fn ident(s: &str) -> Token<'_> {
    Token::Ident(Cow::Borrowed(s))
}

fn num(value: f64, is_integer: bool, sign: Option<Sign>) -> Token<'static> {
    Token::Number(Number {
        value,
        is_integer,
        sign,
    })
}

fn first_number(src: &str) -> Number {
    match &toks(src)[0] {
        Token::Number(n) | Token::Dimension(n, _) => *n,
        other => panic!("{src:?}: {other:?}"),
    }
}

#[test]
fn numbers_4_3_13() {
    // "+.5": sign "+", no integer digits, ".5" sets type "number".
    assert_eq!(toks("+.5"), [num(0.5, false, Some(Sign::Plus))]);
    // "-0" keeps its sign: the value is negative zero.
    let n = first_number("-0");
    assert!(n.value == 0.0 && n.value.is_sign_negative() && n.is_integer);
    // "1e3": the exponent makes it type "number" even though it is whole.
    assert_eq!(toks("1e3"), [num(1000.0, false, None)]);
    assert_eq!(toks("1E+3"), [num(1000.0, false, None)]);
    assert_eq!(toks("1e-3"), [num(0.001, false, None)]);
    // "1e" has no digit after "e", so the number is 1 and "e" is a unit.
    assert_eq!(
        toks("1e"),
        [Token::Dimension(
            Number {
                value: 1.0,
                is_integer: true,
                sign: None
            },
            "e".into()
        )]
    );
    // "1e+" : "e" followed by "+" then no digit: still a unit "e", then "+".
    assert_eq!(
        toks("1e+"),
        [
            Token::Dimension(
                Number {
                    value: 1.0,
                    is_integer: true,
                    sign: None
                },
                "e".into()
            ),
            Token::Delim('+')
        ]
    );
    // "1e+x": "e+x" isn't an exponent; would "e+x" start an ident? "e" is an
    // ident-start code point, so yes: unit "e", then "+" is a delim and "x" an
    // ident.
    assert_eq!(toks("1e+x").len(), 3);
    // "1." : "." not followed by a digit, so the number is 1 and "." a delim.
    assert_eq!(toks("1."), [num(1.0, true, None), Token::Delim('.')]);
    // ".5.5" is two numbers: the second "." starts a number again.
    assert_eq!(toks(".5.5"), [num(0.5, false, None), num(0.5, false, None)]);
    // "1-2": "-2" starts a number, so two numbers (not a dimension "-2").
    assert_eq!(
        toks("1-2"),
        [num(1.0, true, None), num(-2.0, true, Some(Sign::Minus))]
    );
    // "1-x": "-x" would start an ident, so a dimension with unit "-x".
    assert_eq!(
        toks("1-x"),
        [Token::Dimension(
            Number {
                value: 1.0,
                is_integer: true,
                sign: None
            },
            "-x".into()
        )]
    );
    // "50%": a percentage keeps value and sign, no type flag.
    assert_eq!(
        toks("-50%"),
        [Token::Percentage {
            value: -50.0,
            sign: Some(Sign::Minus)
        }]
    );
}

#[test]
fn numbers_round_once_correctly() {
    // The value is the exact decimal rounded once to the nearest f64.
    assert_eq!(first_number("0.1").value, 0.1);
    // 2^53 + 1 lies exactly halfway between two f64s: ties go to even.
    assert_eq!(first_number("9007199254740993").value, 9007199254740992.0);
    // Exponent and digits combine before rounding: 123.753e-2 = 1.23753.
    assert_eq!(first_number("123.753e-2").value, 1.23753);
    // Smallest normal f64, written with more digits than f64 holds.
    assert_eq!(
        first_number("2.2250738585072014e-308").value,
        f64::MIN_POSITIVE
    );
    // Out of range: infinity and zero here; clamping is for value parsers.
    assert_eq!(first_number("1e400").value, f64::INFINITY);
    assert_eq!(first_number("-1e400").value, f64::NEG_INFINITY);
    assert_eq!(first_number("1e-400").value, 0.0);
    // A huge exponent with many digits is still read in one pass.
    assert_eq!(
        first_number(&format!("1e{}", "9".repeat(1000))).value,
        f64::INFINITY
    );
}

#[test]
fn escapes_4_3_7() {
    // "\41 B": hex 41 is "A", and one whitespace after the hex is eaten.
    assert_eq!(toks("\\41 B"), [Token::Ident(Cow::Owned("AB".into()))]);
    // CR LF after the hex is one whitespace after preprocessing.
    assert_eq!(toks("\\41\r\nB"), [Token::Ident(Cow::Owned("AB".into()))]);
    // Zero, a surrogate, or above U+10FFFF give U+FFFD.
    assert_eq!(toks("\\0"), [ident("\u{FFFD}")]);
    assert_eq!(toks("\\d800"), [ident("\u{FFFD}")]);
    assert_eq!(toks("\\110000"), [ident("\u{FFFD}")]);
    assert_eq!(toks("\\10FFFF"), [ident("\u{10FFFF}")]);
    // At most 6 hex digits: "\1234567" is U+123456 (above the max: FFFD)
    // and then "7" continues the ident.
    assert_eq!(toks("\\1234567"), [ident("\u{FFFD}7")]);
    // "\000041" is six digits: U+0041; "1" after it is ident text.
    assert_eq!(toks("\\0000411"), [ident("A1")]);
    // A non-hex code point escapes itself.
    assert_eq!(toks("\\.a"), [ident(".a")]);
    // "a\" then EOF: "\" + EOF is a valid escape (4.3.8 only rejects a
    // newline), and consuming it at EOF is a parse error giving U+FFFD.
    assert_eq!(toks("a\\"), [ident("a\u{FFFD}")]);
    assert_eq!(errs("a\\"), [(ErrorKind::EofInEscape, 2)]);
}

#[test]
fn reverse_solidus_4_3_1() {
    // "\" then newline isn't a valid escape: parse error, delim "\".
    assert_eq!(toks("\\\n"), [Token::Delim('\\'), Token::Whitespace]);
    assert_eq!(errs("\\\n"), [(ErrorKind::InvalidEscape, 0)]);
}

#[test]
fn strings_4_3_5() {
    // An escaped newline is dropped from the value.
    assert_eq!(toks("'a\\\nb'"), [Token::String(Cow::Owned("ab".into()))]);
    // CR LF after "\" is one newline, also dropped.
    assert_eq!(toks("'a\\\r\nb'"), [Token::String(Cow::Owned("ab".into()))]);
    // An unescaped newline: parse error, bad string, the newline reconsumed.
    assert_eq!(
        toks("'a\nb"),
        [Token::BadString, Token::Whitespace, ident("b")]
    );
    assert_eq!(errs("'a\nb"), [(ErrorKind::NewlineInString, 2)]);
    // EOF: parse error, but the string is returned.
    assert_eq!(toks("\"abc"), [Token::String("abc".into())]);
    assert_eq!(errs("\"abc"), [(ErrorKind::EofInString, 4)]);
    // "\" then EOF does nothing (no escape error), then EOF ends the string.
    assert_eq!(toks("'\\"), [Token::String("".into())]);
    assert_eq!(errs("'\\"), [(ErrorKind::EofInString, 2)]);
    // The other quote is ordinary text.
    assert_eq!(toks("'\"'"), [Token::String("\"".into())]);
    assert_eq!(toks("\"'\""), [Token::String("'".into())]);
}

#[test]
fn urls_4_3_4_and_4_3_6() {
    assert_eq!(toks("url( a )"), [Token::Url("a".into())]);
    // A quote after "url(" (and optional whitespace) makes a function.
    assert_eq!(toks("url(\"x\")")[0], Token::Function("url".into()));
    assert_eq!(toks("URL( 'x')")[0], Token::Function("URL".into()));
    // An escaped "url" name still counts: the check is on the value.
    assert_eq!(toks("u\\rl(x)"), [Token::Url("x".into())]);
    // Whitespace then more text: bad URL, no parse error (4.3.6 has none
    // for this case).
    assert_eq!(toks("url(a b)"), [Token::BadUrl]);
    assert_eq!(errs("url(a b)"), []);
    // A quote inside: parse error and bad URL; the rest up to ")" is eaten.
    assert_eq!(toks("url(a\"b)c"), [Token::BadUrl, ident("c")]);
    assert_eq!(errs("url(a\"b)c"), [(ErrorKind::UnexpectedCharInUrl, 5)]);
    // Non-printable code points too.
    assert_eq!(errs("url(\u{7f})"), [(ErrorKind::UnexpectedCharInUrl, 4)]);
    // An escaped ")" is part of the URL.
    assert_eq!(toks("url(a\\)b)"), [Token::Url(Cow::Owned("a)b".into()))]);
    // In a bad URL's remnants an escaped ")" doesn't end it either.
    assert_eq!(toks("url(a b\\)c)d"), [Token::BadUrl, ident("d")]);
    // "\" newline in a URL: parse error, bad URL.
    assert_eq!(toks("url(\\\n)"), [Token::BadUrl]);
    assert_eq!(errs("url(\\\n)"), [(ErrorKind::InvalidEscapeInUrl, 4)]);
    // EOF inside and after trailing whitespace are both parse errors.
    assert_eq!(errs("url(a"), [(ErrorKind::EofInUrl, 5)]);
    assert_eq!(errs("url(a  "), [(ErrorKind::EofInUrl, 7)]);
    assert_eq!(toks("url(a  "), [Token::Url("a".into())]);
}

#[test]
fn hash_at_and_cdo_cdc_4_3_1() {
    // "#-": "-" is an ident code point, so a hash; "-" then EOF wouldn't
    // start an ident, so the type is unrestricted.
    assert_eq!(
        toks("#-"),
        [Token::Hash("-".into(), HashKind::Unrestricted)]
    );
    assert_eq!(toks("#-a"), [Token::Hash("-a".into(), HashKind::Id)]);
    assert_eq!(
        toks("#1"),
        [Token::Hash("1".into(), HashKind::Unrestricted)]
    );
    assert_eq!(
        toks("#\\31"),
        [Token::Hash(Cow::Owned("1".into()), HashKind::Id)]
    );
    assert_eq!(toks("# "), [Token::Delim('#'), Token::Whitespace]);
    assert_eq!(toks("@-x"), [Token::AtKeyword("-x".into())]);
    assert_eq!(toks("@1"), [Token::Delim('@'), num(1.0, true, None)]);
    assert_eq!(toks("<!--"), [Token::Cdo]);
    assert_eq!(
        toks("<!-"),
        [Token::Delim('<'), Token::Delim('!'), Token::Delim('-')]
    );
    // "-->" is checked before "--" as an ident start.
    assert_eq!(toks("-->"), [Token::Cdc]);
    assert_eq!(toks("--a"), [ident("--a")]);
    assert_eq!(toks("--"), [ident("--")]);
    assert_eq!(toks("-\\41"), [Token::Ident(Cow::Owned("-A".into()))]);
    assert_eq!(toks("-"), [Token::Delim('-')]);
}

#[test]
fn unicode_ranges_4_3_14() {
    let ur = |s: &str| -> Vec<Token<'static>> {
        tokens_of(Wtf8::from_str(s), true)
            .0
            .into_iter()
            .map(|t| t.0.into_owned())
            .collect()
    };
    let r = |start, end| Token::UnicodeRange { start, end };
    assert_eq!(ur("U+26"), [r(0x26, 0x26)]);
    assert_eq!(ur("u+0-7F"), [r(0, 0x7F)]);
    // Question marks: 0s for the start, Fs for the end.
    assert_eq!(ur("U+4??"), [r(0x400, 0x4FF)]);
    assert_eq!(ur("U+??????"), [r(0, 0xFFFFFF)]);
    // At most 6 hex digits in all; a seventh starts a new token.
    assert_eq!(ur("U+1234567")[0], r(0x123456, 0x123456));
    assert_eq!(ur("U+12345??")[0], r(0x123450, 0x12345F));
    // After question marks there is no "-end".
    assert_eq!(ur("U+1?-5")[0], r(0x10, 0x1F));
    // "-" not followed by a hex digit isn't part of the range.
    assert_eq!(ur("U+5-")[0], r(5, 5));
    // An end before the start is kept as written: an empty range.
    assert_eq!(ur("U+9-1"), [r(9, 1)]);
    // The end takes at most 6 hex digits.
    assert_eq!(ur("U+0-1234567")[0], r(0, 0x123456));
    // "U+" without a hex digit or "?" isn't a unicode range.
    assert_eq!(ur("U+x")[0], ident("U"));
    // With the flag off (the normal case), it is an ident and a number.
    assert_eq!(
        toks("U+26"),
        [ident("U"), num(26.0, true, Some(Sign::Plus))]
    );
}

#[test]
fn preprocessing_3_3() {
    // CR LF is one LF: one whitespace token covering both bytes.
    let w = Wtf8::from_str("a\r\nb");
    assert_eq!(
        tokens_of(w, false).0,
        [
            (ident("a"), 0, 1),
            (Token::Whitespace, 1, 3),
            (ident("b"), 3, 4)
        ]
    );
    // Lone CR and FF are newlines too.
    assert_eq!(toks("'\r"), [Token::BadString, Token::Whitespace]);
    assert_eq!(toks("'\u{c}"), [Token::BadString, Token::Whitespace]);
    // NUL becomes U+FFFD, which is a non-ASCII ident code point.
    assert_eq!(toks("a\0"), [Token::Ident(Cow::Owned("a\u{FFFD}".into()))]);
    // So does a lone surrogate (only possible in WTF-8 input).
    let lone = Wtf8Buf::from_utf16(&[0x61, 0xD800, 0x62]);
    let (t, _) = tokens_of(lone.as_wtf8(), false);
    assert_eq!(t, [(Token::Ident(Cow::Owned("a\u{FFFD}b".into())), 0, 5)]);
}

#[test]
fn comments_4_3_2() {
    let w = Wtf8::from_str("/* x */a/**/");
    assert_eq!(
        tokens_of(w, false).0,
        [
            (Token::Comment, 0, 7),
            (ident("a"), 7, 8),
            (Token::Comment, 8, 12)
        ]
    );
    // "/*/" doesn't close: the "*" of "/*" can't be reused.
    assert_eq!(toks("/*/"), [Token::Comment]);
    assert_eq!(errs("/*/"), [(ErrorKind::EofInComment, 3)]);
    // A comment inside a string is string text.
    assert_eq!(toks("'/**/'"), [Token::String("/**/".into())]);
}

#[test]
fn values_borrow_unless_unescaped() {
    let t = toks("abc 'str' url(u) #h @k 1px");
    for tok in &t {
        let s = match tok {
            Token::Ident(s)
            | Token::String(s)
            | Token::Url(s)
            | Token::Hash(s, _)
            | Token::AtKeyword(s) => s,
            Token::Dimension(_, s) => s,
            _ => continue,
        };
        assert!(matches!(s, Cow::Borrowed(_)), "{tok:?} copied");
    }
    assert!(matches!(&toks("a\\62 c")[0], Token::Ident(Cow::Owned(s)) if s == "abc"));
    // A skipped escaped newline breaks the plain run.
    assert!(matches!(&toks("'a\\\nb'")[0], Token::String(Cow::Owned(s)) if s == "ab"));
}

#[test]
fn end_of_input_is_sticky() {
    let mut t = Tokenizer::new(Wtf8::from_str("ab c"));
    assert!(t.next().is_some());
    assert_eq!(t.position(), 2);
    assert!(t.next().is_some());
    assert!(t.next().is_some());
    assert_eq!(t.position(), 4);
    assert!(t.next().is_none());
    assert!(t.next().is_none());
}

fn drain(s: &mut StreamTokenizer) -> Vec<Token<'static>> {
    let mut out = Vec::new();
    while let Next::Token(t) = s.next_token(false) {
        out.push(t.token.into_owned());
    }
    out
}

#[test]
fn stream_joins_a_split_surrogate_pair() {
    // U+1F600 as two UTF-16 halves in two chunks: one ident code point, not
    // two U+FFFDs. The ident reads on past the lone high surrogate to the
    // chunk end and waits there; the retry reads the joined code point.
    let mut s = StreamTokenizer::new();
    s.push(Wtf8Buf::from_utf16(&[0x61, 0xD83D]).as_wtf8());
    assert_eq!(drain(&mut s), []);
    s.push(Wtf8Buf::from_utf16(&[0xDE00]).as_wtf8());
    s.finish();
    assert_eq!(drain(&mut s), [Token::Ident("a\u{1F600}".into())]);
}

#[test]
fn stream_waits_after_a_cr() {
    // A CR at a chunk end may be the first half of CR LF, so even the ident
    // before it waits (the scanner can't read the CR's length yet).
    let mut s = StreamTokenizer::new();
    s.push(Wtf8::from_str("a\r"));
    assert_eq!(drain(&mut s), []);
    s.push(Wtf8::from_str("\nb"));
    assert_eq!(
        s.next_token(false),
        Next::Token(crate::Spanned {
            token: ident("a"),
            start: 0,
            end: 1
        })
    );
    match s.next_token(false) {
        Next::Token(t) => assert_eq!((t.token, t.start, t.end), (Token::Whitespace, 1, 3)),
        other => panic!("{other:?}"),
    }
    s.finish();
    assert_eq!(drain(&mut s), [ident("b")]);
    // A lone CR at the very end is a newline once the input has ended.
    let mut s = StreamTokenizer::new();
    s.push(Wtf8::from_str("\r"));
    assert_eq!(drain(&mut s), []);
    s.finish();
    assert_eq!(drain(&mut s), [Token::Whitespace]);
}

#[test]
fn stream_retries_once_the_unscanned_text_doubles() {
    let mut s = StreamTokenizer::new();
    s.push(Wtf8::from_str("ab"));
    assert_eq!(s.next_token(false), Next::NeedInput);
    assert_eq!(s.scans, 1);
    // One more byte: 3 unscanned, less than twice 2. No scan.
    s.push(Wtf8::from_str("c"));
    assert_eq!(s.next_token(false), Next::NeedInput);
    assert_eq!(s.scans, 1);
    // Four unscanned: exactly twice. Scans, and the ident is complete.
    s.push(Wtf8::from_str(";"));
    assert_eq!(drain(&mut s), [ident("abc"), Token::Semicolon]);
    // Nothing unscanned: asking again scans (for free) and needs input.
    let before = s.scans;
    assert_eq!(s.next_token(false), Next::NeedInput);
    assert_eq!(s.scans, before + 1);
    // Finishing always scans.
    s.finish();
    assert_eq!(s.next_token(false), Next::End);
    // The threshold is unscanned text plus what has arrived, not a product:
    // 3 unscanned at length 3 means retry at length 6.
    let mut s = StreamTokenizer::new();
    s.push(Wtf8::from_str("abc"));
    assert_eq!(s.next_token(false), Next::NeedInput);
    s.push(Wtf8::from_str("de;"));
    assert_eq!(drain(&mut s), [ident("abcde"), Token::Semicolon]);
}

#[test]
fn stream_scans_a_long_token_logarithmically_often() {
    // 100 000 bytes of one string, one byte per chunk, asking after each:
    // with doubling that is about log2(100 000) = 17 scans, not 100 000.
    let mut s = StreamTokenizer::new();
    s.push(Wtf8::from_str("'"));
    for _ in 0..100_000 {
        s.push(Wtf8::from_str("a"));
        assert_eq!(s.next_token(false), Next::NeedInput);
    }
    assert!(s.scans <= 18, "{} scans", s.scans);
    s.push(Wtf8::from_str("'"));
    s.finish();
    match s.next_token(false) {
        Next::Token(t) => assert_eq!((t.start, t.end), (0, 100_002)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stream_drops_consumed_text() {
    // 200 KB of complete tokens, then the start of a long string. Once the
    // tokens are consumed, the next push must drop them: the buffer then
    // holds only the unfinished string and the new chunk.
    let mut s = StreamTokenizer::new();
    s.push(Wtf8::from_str(&"a ".repeat(100_000)));
    // The last whitespace waits: more whitespace could follow.
    assert_eq!(drain(&mut s).len(), 199_999);
    s.push(Wtf8::from_str(&format!("'{}", "x".repeat(1000))));
    assert_eq!(drain(&mut s), [Token::Whitespace]);
    s.push(Wtf8::from_str("x'"));
    assert!(s.buf.len() < 2000, "buffer still {} bytes", s.buf.len());
    // Offsets still count from the start of the stream.
    s.finish();
    let mut t = Vec::new();
    loop {
        match s.next_token(false) {
            Next::Token(x) => t.push((x.token.into_owned(), x.start, x.end)),
            Next::End => break,
            Next::NeedInput => panic!("ended"),
        }
    }
    assert_eq!(
        t,
        [(Token::String("x".repeat(1001).into()), 200_000, 201_003)]
    );
    assert_eq!(s.errors(), []);
    // Consumed text is dropped once it is half the buffer, even while much
    // unconsumed text remains: 200 000 consumed bytes before a 150 000 byte
    // unfinished string.
    let mut s = StreamTokenizer::new();
    s.push(Wtf8::from_str(&format!(
        "{}'{}",
        "a ".repeat(100_000),
        "x".repeat(150_000)
    )));
    assert_eq!(drain(&mut s).len(), 200_000);
    s.push(Wtf8::from_str("x"));
    assert!(s.buf.len() < 160_000, "buffer still {} bytes", s.buf.len());
    // Below 64 KiB consumed, nothing is dropped (copying would cost more
    // than it saves).
    let mut s = StreamTokenizer::new();
    s.push(Wtf8::from_str(&"a ".repeat(1000)));
    drain(&mut s);
    s.push(Wtf8::from_str("b"));
    assert_eq!(s.buf.len(), 2001);
}

#[test]
#[should_panic(expected = "after the end")]
fn stream_rejects_text_after_finish() {
    let mut s = StreamTokenizer::new();
    s.finish();
    s.push(Wtf8::from_str("a"));
}

#[test]
fn stream_errors_are_not_doubled_by_retries() {
    // The unterminated comment is scanned (and gives up) several times
    // before the end; its error is reported once, at the end.
    let mut s = StreamTokenizer::new();
    for c in ["/*", "a", "bc", "defg"] {
        s.push(Wtf8::from_str(c));
        assert_eq!(drain(&mut s), []);
    }
    s.finish();
    assert_eq!(drain(&mut s), [Token::Comment]);
    assert_eq!(
        s.errors(),
        [TokenError {
            kind: ErrorKind::EofInComment,
            offset: 9
        }]
    );
}
