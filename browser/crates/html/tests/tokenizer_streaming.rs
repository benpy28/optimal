//! Tokenizer behaviour the html5lib suite does not reach: streaming edge
//! cases, buffer compaction, pausing, and hostile inputs.
//!
//! The reference model for streaming is the tokenizer itself fed the whole
//! input at once: however input is split into chunks, tokens and parse errors
//! (with positions) must be identical.

use optimal_html::{
    InitialState, ParseError, RunResult, SinkResult, Tag, TagKind, Token, TokenSink, Tokenizer,
};
use optimal_wtf8::{Wtf8, Wtf8Buf};
use proptest::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
enum T {
    Doctype(String),
    Start(Wtf8Buf, Vec<(Wtf8Buf, Wtf8Buf)>, bool),
    End(Wtf8Buf),
    Comment(Wtf8Buf),
    Pi(Wtf8Buf, Wtf8Buf),
    Chars(Wtf8Buf),
}

#[derive(Default)]
struct Collect {
    tokens: Vec<T>,
    errors: Vec<ParseError>,
    pause_on_end_tag: Option<&'static str>,
    eof: bool,
}

impl TokenSink for Collect {
    fn process(&mut self, token: Token<'_>) -> SinkResult {
        match token {
            Token::Doctype(d) => self.tokens.push(T::Doctype(format!("{d:?}"))),
            Token::Tag(Tag {
                kind: TagKind::Start,
                name,
                self_closing,
                attributes,
            }) => self.tokens.push(T::Start(
                name.clone(),
                attributes
                    .iter()
                    .map(|a| (a.name.clone(), a.value.clone()))
                    .collect(),
                *self_closing,
            )),
            Token::Tag(t) => {
                self.tokens.push(T::End(t.name.clone()));
                if self.pause_on_end_tag.is_some_and(|n| t.name == n) {
                    return SinkResult::Pause;
                }
            }
            Token::Comment(c) => self.tokens.push(T::Comment(c.into())),
            Token::ProcessingInstruction { target, data } => {
                self.tokens.push(T::Pi(target.into(), data.into()))
            }
            Token::Characters(c) => {
                if let Some(T::Chars(prev)) = self.tokens.last_mut() {
                    prev.push_wtf8(c);
                } else {
                    self.tokens.push(T::Chars(c.into()));
                }
            }
            Token::Eof => self.eof = true,
        }
        SinkResult::Continue
    }
    fn parse_error(&mut self, e: ParseError) {
        self.errors.push(e);
    }
    fn wants_errors(&self) -> bool {
        true
    }
}

fn tokenize_chunks(chunks: &[Wtf8<'_>], state: InitialState) -> Collect {
    let mut t = Tokenizer::new();
    t.set_state(state);
    let mut sink = Collect::default();
    for c in chunks {
        t.feed(*c);
        assert_eq!(t.run(&mut sink), RunResult::NeedInput);
    }
    t.end();
    assert_eq!(t.run(&mut sink), RunResult::Done);
    assert!(sink.eof);
    sink
}

fn whole(s: &str) -> Collect {
    tokenize_chunks(&[Wtf8::from_str(s)], InitialState::Data)
}

/// Tokenize script data after a `<script>` start tag.
fn script(s: &str) -> Vec<T> {
    let mut t = Tokenizer::new();
    t.set_state(InitialState::ScriptData);
    t.set_last_start_tag(Some("script"));
    let mut sink = Collect::default();
    t.feed(Wtf8::from_str(s));
    t.end();
    assert_eq!(t.run(&mut sink), RunResult::Done);
    sink.tokens
}

fn chars(s: &str) -> T {
    T::Chars(Wtf8Buf::from(s))
}

/// Hand-worked from 13.2.5.26 and 13.2.5.31: whitespace, like "/" and ">",
/// ends the "script" word that switches into and out of the double-escaped
/// state. Inside `<!--<script ...`, a `</script>` is text; after the
/// matching `</script ` it ends the script again.
#[test]
fn whitespace_ends_the_double_escape_words() {
    for ws in [" ", "\t", "\n", "\x0C"] {
        // Into double-escaped: the first </script> is text, the second
        // (after leaving with "</script>" + ws) is a real end tag.
        let input = format!("<!--<script{ws}</script>-->");
        assert_eq!(
            script(&input),
            vec![chars(&input)],
            "start word ended by {ws:?}"
        );
        let input = format!("<!--<script>x</script{ws}y</script>z");
        let text = format!("<!--<script>x</script{ws}y");
        assert_eq!(
            script(&input),
            vec![chars(&text), T::End(Wtf8Buf::from("script")), chars("z")],
            "end word ended by {ws:?}"
        );
    }
}

/// Hand-worked from 13.2.5.24: "</" followed by a non-letter in escaped
/// script data is text, and the escape continues.
#[test]
fn escaped_end_tag_open_needs_a_letter() {
    assert_eq!(
        script("<!--</1</script>"),
        vec![chars("<!--</1"), T::End(Wtf8Buf::from("script"))]
    );
}

/// Hand-worked: "a\r" + "\nb" is one CR LF pair, so one LF and no extra line.
#[test]
fn cr_lf_split_across_chunks_is_one_newline() {
    let got = tokenize_chunks(
        &[Wtf8::from_str("a\r"), Wtf8::from_str("\nb\0")],
        InitialState::Data,
    );
    assert_eq!(got.tokens, vec![T::Chars(Wtf8Buf::from("a\nb\0"))]);
    // The NUL is on line 2, column 2.
    assert_eq!(got.errors.len(), 1);
    assert_eq!(
        (got.errors[0].position.line, got.errors[0].position.column),
        (2, 2)
    );
}

#[test]
fn trailing_cr_at_end_of_input_is_a_newline() {
    let got = tokenize_chunks(&[Wtf8::from_str("a\r")], InitialState::Data);
    assert_eq!(got.tokens, vec![T::Chars(Wtf8Buf::from("a\n"))]);
}

#[test]
fn empty_chunk_after_cr_keeps_the_pair() {
    let got = tokenize_chunks(
        &[
            Wtf8::from_str("a\r"),
            Wtf8::from_str(""),
            Wtf8::from_str("\nb"),
        ],
        InitialState::Data,
    );
    assert_eq!(got.tokens, vec![T::Chars(Wtf8Buf::from("a\nb"))]);
}

/// A surrogate pair split across two `document.write` calls joins into one
/// character, with no surrogate errors.
#[test]
fn surrogate_pair_split_across_chunks_joins() {
    let hi = Wtf8Buf::from_utf16(&[0x61, 0xD83D]);
    let lo = Wtf8Buf::from_utf16(&[0xDE00, 0x62]);
    let got = tokenize_chunks(&[hi.as_wtf8(), lo.as_wtf8()], InitialState::Data);
    assert_eq!(got.tokens, vec![T::Chars(Wtf8Buf::from("a😀b"))]);
    assert!(got.errors.is_empty(), "{:?}", got.errors);
}

#[test]
fn lone_high_surrogate_at_end_is_reported() {
    let hi = Wtf8Buf::from_utf16(&[0x61, 0xD83D]);
    let got = tokenize_chunks(&[hi.as_wtf8()], InitialState::Data);
    assert_eq!(got.tokens, vec![T::Chars(hi.clone())]);
    assert_eq!(got.errors.len(), 1);
    assert_eq!(got.errors[0].code.as_str(), "surrogate-in-input-stream");
    assert_eq!(got.errors[0].position.column, 2);
}

/// Columns count UTF-16 units: "😀" is two, so the NUL after it is column 3.
#[test]
fn columns_count_utf16_units() {
    let got = whole("😀\0");
    assert_eq!(got.errors[0].position.column, 3);
    let got = whole("é\0");
    assert_eq!(got.errors[0].position.column, 2);
}

/// Pausing (a script must run) and resuming loses nothing.
#[test]
fn pause_and_resume() {
    let src = "<script>a</script>b<p>c";
    let mut t = Tokenizer::new();
    let mut sink = Collect {
        pause_on_end_tag: Some("script"),
        ..Default::default()
    };
    t.feed_str(src);
    t.end();
    assert_eq!(t.run(&mut sink), RunResult::Paused);
    assert_eq!(sink.tokens.last(), Some(&T::End(Wtf8Buf::from("script"))));
    assert_eq!(t.run(&mut sink), RunResult::Done);
    let reference = whole(src);
    assert_eq!(sink.tokens, reference.tokens);
}

/// Feeding a large document in small chunks forces the input buffer to
/// compact (drop consumed text) many times. Positions of errors far into the
/// document must still be exact, and memory must stay bounded.
#[test]
fn compaction_keeps_positions_exact() {
    let mut src = String::new();
    for i in 0..20_000 {
        src.push_str(&format!("<p class=x{i}>line {i} é😀</p>\n"));
        if i % 997 == 0 {
            src.push('\0');
            src.push_str("&notit; &#0; <!a>\r\n");
        }
    }
    let reference = whole(&src);
    assert!(reference.errors.len() > 60);
    let bytes = src.as_bytes();
    let w = Wtf8::from_str(&src);
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut size = 1;
    while start < bytes.len() {
        let mut end = (start + size).min(bytes.len());
        while !w.is_boundary(end) {
            end += 1;
        }
        chunks.push(w.slice(start, end));
        start = end;
        size = size * 7 % 4093 + 1;
    }
    let got = tokenize_chunks(&chunks, InitialState::Data);
    assert_eq!(got.errors, reference.errors);
    assert_eq!(got.tokens, reference.tokens);
}

/// 100 000 attributes, half of them duplicates. Must stay linear time (the
/// test has a generous time limit) and drop exactly the duplicates.
#[test]
fn many_attributes_stay_linear() {
    let mut src = String::from("<a");
    for i in 0..50_000 {
        src.push_str(&format!(" a{i}=1"));
    }
    for i in 0..50_000 {
        src.push_str(&format!(" a{i}=2"));
    }
    src.push('>');
    let start = std::time::Instant::now();
    let got = whole(&src);
    let elapsed = start.elapsed();
    match &got.tokens[0] {
        T::Start(_, attrs, _) => {
            assert_eq!(attrs.len(), 50_000);
            assert!(attrs.iter().all(|(_, v)| *v == "1"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(got.errors.len(), 50_000);
    assert!(elapsed.as_secs() < 20, "took {elapsed:?}");
}

#[test]
fn huge_numeric_reference_does_not_overflow() {
    let got = whole("&#99999999999999999999999999; &#x10FFFF; &#x110000;");
    assert_eq!(
        got.tokens,
        vec![T::Chars(Wtf8Buf::from("\u{FFFD} \u{10FFFF} \u{FFFD}"))]
    );
}

#[test]
fn deeply_nested_markup_is_flat_for_the_tokenizer() {
    let src = "<div>".repeat(200_000);
    let got = whole(&src);
    assert_eq!(got.tokens.len(), 200_000);
}

fn any_state() -> impl Strategy<Value = InitialState> {
    prop_oneof![
        Just(InitialState::Data),
        Just(InitialState::Rcdata),
        Just(InitialState::Rawtext),
        Just(InitialState::ScriptData),
        Just(InitialState::Plaintext),
        Just(InitialState::CdataSection),
    ]
}

/// HTML-ish text: lots of markup characters, plus some awkward code points.
fn htmlish() -> impl Strategy<Value = Vec<u16>> {
    let piece = prop_oneof![
        Just("<".to_owned()),
        Just(">".to_owned()),
        Just("</".to_owned()),
        Just("<!--".to_owned()),
        Just("-->".to_owned()),
        Just("<!DOCTYPE ".to_owned()),
        Just("PUBLIC".to_owned()),
        Just("SYSTEM".to_owned()),
        Just("<![CDATA[".to_owned()),
        Just("]]>".to_owned()),
        Just("<?".to_owned()),
        Just("?>".to_owned()),
        Just("&".to_owned()),
        Just("&amp".to_owned()),
        Just("&notin;".to_owned()),
        Just("&#x".to_owned()),
        Just("&#".to_owned()),
        Just(";".to_owned()),
        Just("=".to_owned()),
        Just("\"".to_owned()),
        Just("'".to_owned()),
        Just("/".to_owned()),
        Just(" ".to_owned()),
        Just("\r".to_owned()),
        Just("\r\n".to_owned()),
        Just("\0".to_owned()),
        Just("\u{b}".to_owned()),
        Just("\u{FFFF}".to_owned()),
        Just("😀".to_owned()),
        Just("script".to_owned()),
        Just("title".to_owned()),
        Just("<script>".to_owned()),
        Just("</script>".to_owned()),
        Just("<!--<script>".to_owned()),
        "[a-zA-Z0-9]{1,4}",
    ];
    prop::collection::vec(
        prop_oneof![
            9 => piece.prop_map(|s| s.encode_utf16().collect::<Vec<u16>>()),
            1 => (0xD800u16..0xE000).prop_map(|u| vec![u]),
        ],
        0..30,
    )
    .prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(2048)
    ))]

    /// However the input is chunked, tokens and errors are the same.
    #[test]
    fn chunking_never_changes_the_result(
        units in htmlish(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..12),
        state in any_state(),
    ) {
        let input = Wtf8Buf::from_utf16(&units);
        let w = input.as_wtf8();
        let reference = tokenize_chunks(&[w], state);
        // Cut in UTF-16 unit space, so surrogate pairs get split too.
        let mut cut_units: Vec<usize> = cuts.iter().map(|i| i.index(units.len() + 1)).collect();
        cut_units.sort();
        cut_units.dedup();
        let mut pieces: Vec<Wtf8Buf> = Vec::new();
        let mut prev = 0;
        for &c in cut_units.iter().chain(std::iter::once(&units.len())) {
            pieces.push(Wtf8Buf::from_utf16(&units[prev..c]));
            prev = c;
        }
        let views: Vec<Wtf8<'_>> = pieces.iter().map(|p| p.as_wtf8()).collect();
        let got = tokenize_chunks(&views, state);
        prop_assert_eq!(&got.tokens, &reference.tokens);
        prop_assert_eq!(&got.errors, &reference.errors);
    }

    /// Error positions never go past the end of the input and are reported
    /// in input order.
    #[test]
    fn error_positions_are_ordered_and_in_range(units in htmlish(), state in any_state()) {
        let input = Wtf8Buf::from_utf16(&units);
        let got = tokenize_chunks(&[input.as_wtf8()], state);
        let lines = 1 + units.iter().filter(|&&u| u == b'\n' as u16 || u == b'\r' as u16).count() as u64;
        for w in got.errors.windows(2) {
            prop_assert!(w[0].position <= w[1].position, "{:?}", got.errors);
        }
        for e in &got.errors {
            prop_assert!(e.position.line >= 1 && e.position.line <= lines);
            prop_assert!(e.position.column >= 1);
        }
    }
}
