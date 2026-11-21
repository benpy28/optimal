//! The HTML tokenizer (WHATWG HTML, "13.2.5 Tokenization").
//!
//! This is the state machine from the standard, state for state, with the
//! standard's names. Each state's comment quotes which characters it handles;
//! the behaviour is exactly the standard's, including every parse error.
//!
//! Design notes (each logged as: chosen, why, rejected):
//! - Push-driven and resumable: text is pushed in chunks with
//!   [`Tokenizer::feed`], and [`Tokenizer::run`] stops with
//!   [`RunResult::NeedInput`] whenever it needs to see more text, without
//!   consuming anything. Why: network streaming and `document.write` both need
//!   it. Rejected: requiring the whole document up front.
//! - Tokens borrow the tokenizer's reusable buffers ([`Token`] has a lifetime),
//!   so no allocation happens per token once buffers are warm. Rejected: owned
//!   tokens (one allocation per tag).
//! - Runs of characters are emitted as one [`Token::Characters`], never one
//!   token per character. Text-heavy states copy whole runs of ordinary bytes
//!   at a time.
//! - Text is WTF-8 so lone surrogates (possible through `document.write`)
//!   survive exactly. See `optimal-wtf8`.
//! - Parse errors are only located (line and column) when the sink asks for
//!   them, so normal page loads pay nothing for error reporting.

use crate::entities::{Expansion, Matcher};
use crate::error::{ParseError, ParseErrorCode as E};
use crate::input::Input;
use optimal_wtf8::{Wtf8, Wtf8Buf};

/// The end of the input, as a pseudo code point.
const EOF: u32 = u32::MAX;

/// The tokenizer states the tree builder (or a test) may switch to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialState {
    /// Data state.
    Data,
    /// PLAINTEXT state.
    Plaintext,
    /// RCDATA state.
    Rcdata,
    /// RAWTEXT state.
    Rawtext,
    /// Script data state.
    ScriptData,
    /// CDATA section state.
    CdataSection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Data,
    Rcdata,
    Rawtext,
    ScriptData,
    Plaintext,
    TagOpen,
    EndTagOpen,
    TagName,
    RcdataLessThanSign,
    RcdataEndTagOpen,
    RcdataEndTagName,
    RawtextLessThanSign,
    RawtextEndTagOpen,
    RawtextEndTagName,
    ScriptDataLessThanSign,
    ScriptDataEndTagOpen,
    ScriptDataEndTagName,
    ScriptDataEscapeStart,
    ScriptDataEscapeStartDash,
    ScriptDataEscaped,
    ScriptDataEscapedDash,
    ScriptDataEscapedDashDash,
    ScriptDataEscapedLessThanSign,
    ScriptDataEscapedEndTagOpen,
    ScriptDataEscapedEndTagName,
    ScriptDataDoubleEscapeStart,
    ScriptDataDoubleEscaped,
    ScriptDataDoubleEscapedDash,
    ScriptDataDoubleEscapedDashDash,
    ScriptDataDoubleEscapedLessThanSign,
    ScriptDataDoubleEscapeEnd,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    AttributeValueDoubleQuoted,
    AttributeValueSingleQuoted,
    AttributeValueUnquoted,
    AfterAttributeValueQuoted,
    SelfClosingStartTag,
    BogusComment,
    MarkupDeclarationOpen,
    CommentStart,
    CommentStartDash,
    Comment,
    CommentLessThanSign,
    CommentLessThanSignBang,
    CommentLessThanSignBangDash,
    CommentLessThanSignBangDashDash,
    CommentEndDash,
    CommentEnd,
    CommentEndBang,
    Doctype,
    BeforeDoctypeName,
    DoctypeName,
    AfterDoctypeName,
    AfterDoctypePublicKeyword,
    BeforeDoctypePublicIdentifier,
    DoctypePublicIdentifierDoubleQuoted,
    DoctypePublicIdentifierSingleQuoted,
    AfterDoctypePublicIdentifier,
    BetweenDoctypePublicAndSystemIdentifiers,
    AfterDoctypeSystemKeyword,
    BeforeDoctypeSystemIdentifier,
    DoctypeSystemIdentifierDoubleQuoted,
    DoctypeSystemIdentifierSingleQuoted,
    AfterDoctypeSystemIdentifier,
    BogusDoctype,
    CdataSection,
    CdataSectionBracket,
    CdataSectionEnd,
    ProcessingInstructionOpen,
    ProcessingInstructionTarget,
    AfterProcessingInstructionTarget,
    ProcessingInstructionData,
    ProcessingInstructionQuestionable,
    CharacterReference,
    NamedCharacterReference,
    AmbiguousAmpersand,
    NumericCharacterReference,
    HexadecimalCharacterReferenceStart,
    HexadecimalCharacterReference,
    DecimalCharacterReference,
    NumericCharacterReferenceEnd,
    /// The end-of-file token has been emitted.
    Finished,
}

impl From<InitialState> for State {
    fn from(s: InitialState) -> State {
        match s {
            InitialState::Data => State::Data,
            InitialState::Plaintext => State::Plaintext,
            InitialState::Rcdata => State::Rcdata,
            InitialState::Rawtext => State::Rawtext,
            InitialState::ScriptData => State::ScriptData,
            InitialState::CdataSection => State::CdataSection,
        }
    }
}

/// One attribute of a tag.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attribute {
    /// Name, ASCII-lowercased.
    pub name: Wtf8Buf,
    /// Value, with character references decoded.
    pub value: Wtf8Buf,
}

/// Start or end tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagKind {
    /// `<name ...>`
    Start,
    /// `</name ...>`
    End,
}

/// A start or end tag token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    /// Start or end.
    pub kind: TagKind,
    /// Tag name, ASCII-lowercased.
    pub name: Wtf8Buf,
    /// The self-closing flag (`<br/>`).
    pub self_closing: bool,
    /// Attributes in source order, duplicates already removed.
    pub attributes: Vec<Attribute>,
}

/// A DOCTYPE token. `None` means "missing", which the standard distinguishes
/// from the empty string.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Doctype {
    /// The name, ASCII-lowercased.
    pub name: Option<Wtf8Buf>,
    /// The public identifier.
    pub public_id: Option<Wtf8Buf>,
    /// The system identifier.
    pub system_id: Option<Wtf8Buf>,
    /// The force-quirks flag.
    pub force_quirks: bool,
}

/// A token, borrowing the tokenizer's buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token<'a> {
    /// A DOCTYPE.
    Doctype(&'a Doctype),
    /// A start or end tag.
    Tag(&'a Tag),
    /// A comment, with its data.
    Comment(Wtf8<'a>),
    /// A processing instruction (`<?target data>`).
    ProcessingInstruction {
        /// Target name.
        target: Wtf8<'a>,
        /// Data after the target.
        data: Wtf8<'a>,
    },
    /// One or more character tokens, in order. Never empty. Adjacent runs are
    /// not guaranteed to be merged.
    Characters(Wtf8<'a>),
    /// The end of the input.
    Eof,
}

/// What the consumer of tokens wants the tokenizer to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkResult {
    /// Keep going.
    Continue,
    /// Switch to another state (the tree builder does this after `<title>`,
    /// `<script>` and others), then keep going.
    SwitchTo(InitialState),
    /// Stop: the parser pause flag is set (a script must run). `run` returns
    /// [`RunResult::Paused`] and can be called again later.
    Pause,
}

/// The consumer of tokens: the tree builder, or a test harness.
pub trait TokenSink {
    /// Handle one token.
    fn process(&mut self, token: Token<'_>) -> SinkResult;

    /// Handle a parse error. Only called if [`TokenSink::wants_errors`]
    /// returns true.
    fn parse_error(&mut self, _error: ParseError) {}

    /// Should parse errors be located and reported? Locating costs a little,
    /// so page loads can leave this off.
    fn wants_errors(&self) -> bool {
        false
    }

    /// Is there an adjusted current node that is not an element in the HTML
    /// namespace? Decides whether `<![CDATA[` starts a CDATA section.
    fn adjusted_current_node_is_foreign(&self) -> bool {
        false
    }
}

/// Why [`Tokenizer::run`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunResult {
    /// All pushed text was tokenized, or more text is needed to decide what
    /// the next characters mean. Push more with [`Tokenizer::feed`], or call
    /// [`Tokenizer::end`], then run again.
    NeedInput,
    /// The sink asked to pause.
    Paused,
    /// The end-of-file token was emitted. Nothing more will happen.
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Continue,
    NeedInput,
    Paused,
}

/// Byte classes for the bulk-copy fast paths in text states. `true` means the
/// byte needs the full per-character treatment: delimiters, NUL, CR (never
/// present after normalization, kept for safety), controls that are input
/// stream errors, and every non-ASCII lead byte.
const fn special_table(extra: &[u8]) -> [bool; 256] {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        t[i] = b >= 0x80 || (b < 0x20 && b != b'\t' && b != b'\n' && b != 0x0C) || b == 0x7F;
        i += 1;
    }
    let mut j = 0;
    while j < extra.len() {
        t[extra[j] as usize] = true;
        j += 1;
    }
    t
}

static DATA_SPECIAL: [bool; 256] = special_table(b"&<");
static RAWTEXT_SPECIAL: [bool; 256] = special_table(b"<");
static PLAINTEXT_SPECIAL: [bool; 256] = special_table(b"");
static ATTR_DQ_SPECIAL: [bool; 256] = special_table(b"&\"");
static ATTR_SQ_SPECIAL: [bool; 256] = special_table(b"&'");
static COMMENT_SPECIAL: [bool; 256] = special_table(b"<-");
static TAG_NAME_SPECIAL: [bool; 256] = {
    let mut t = special_table(b"\t\n\x0C />");
    let mut c = b'A';
    while c <= b'Z' {
        t[c as usize] = true;
        c += 1;
    }
    t
};

#[inline]
fn is_ascii_whitespace(c: u32) -> bool {
    matches!(c, 0x09 | 0x0A | 0x0C | 0x20)
}

#[inline]
fn is_ascii_alpha(c: u32) -> bool {
    matches!(c, 0x41..=0x5A | 0x61..=0x7A)
}

#[inline]
fn is_ascii_upper(c: u32) -> bool {
    matches!(c, 0x41..=0x5A)
}

#[inline]
fn is_ascii_digit(c: u32) -> bool {
    matches!(c, 0x30..=0x39)
}

#[inline]
fn is_ascii_alphanumeric(c: u32) -> bool {
    is_ascii_alpha(c) || is_ascii_digit(c)
}

#[inline]
fn is_ascii_hex_digit(c: u32) -> bool {
    is_ascii_digit(c) || matches!(c, 0x41..=0x46 | 0x61..=0x66)
}

#[inline]
fn to_lower(c: u32) -> u32 {
    if is_ascii_upper(c) { c + 0x20 } else { c }
}

/// Noncharacters: U+FDD0..=U+FDEF and the last two code points of each plane.
#[inline]
fn is_noncharacter(c: u32) -> bool {
    (0xFDD0..=0xFDEF).contains(&c) || (c & 0xFFFE == 0xFFFE && c <= 0x10FFFF)
}

/// C0 controls and U+007F..=U+009F.
#[inline]
fn is_control(c: u32) -> bool {
    c <= 0x1F || (0x7F..=0x9F).contains(&c)
}

/// The Windows-1252 replacements for numeric references 0x80..=0x9F.
fn c1_replacement(c: u32) -> Option<u32> {
    Some(match c {
        0x80 => 0x20AC,
        0x82 => 0x201A,
        0x83 => 0x0192,
        0x84 => 0x201E,
        0x85 => 0x2026,
        0x86 => 0x2020,
        0x87 => 0x2021,
        0x88 => 0x02C6,
        0x89 => 0x2030,
        0x8A => 0x0160,
        0x8B => 0x2039,
        0x8C => 0x0152,
        0x8E => 0x017D,
        0x91 => 0x2018,
        0x92 => 0x2019,
        0x93 => 0x201C,
        0x94 => 0x201D,
        0x95 => 0x2022,
        0x96 => 0x2013,
        0x97 => 0x2014,
        0x98 => 0x02DC,
        0x99 => 0x2122,
        0x9A => 0x0161,
        0x9B => 0x203A,
        0x9C => 0x0153,
        0x9E => 0x017E,
        0x9F => 0x0178,
        _ => return None,
    })
}

/// Duplicate attribute checks switch from a linear scan to a hash set above
/// this many attributes, so a tag with 100 000 attributes costs O(n), not
/// O(n^2).
const DUP_SCAN_LIMIT: usize = 16;

/// The HTML tokenizer.
pub struct Tokenizer {
    input: Input,
    state: State,
    return_state: State,
    /// Byte length of the current input character (0 for EOF), so it can be
    /// reconsumed.
    cur_len: usize,
    /// Input before this buffer index has been checked for input stream
    /// errors (controls, noncharacters, surrogates).
    checked: usize,
    report_errors: bool,
    /// Errors not yet reported, each with the length the pending text had
    /// when it was found, so text and errors reach the sink in input order
    /// however the input was chunked.
    pending_errors: Vec<(ParseError, usize)>,

    /// Character tokens waiting to be emitted as one run.
    text: Wtf8Buf,
    temp: Wtf8Buf,
    char_ref_code: u32,
    matcher: Matcher,

    tag: Tag,
    attr_name: Wtf8Buf,
    attr_value: Wtf8Buf,
    /// An attribute is being built (name or value).
    attr_open: bool,
    /// The attribute being built repeats an earlier name and will be dropped.
    attr_duplicate: bool,
    attr_names: std::collections::HashSet<Wtf8Buf>,
    last_start_tag: Option<Wtf8Buf>,

    comment: Wtf8Buf,
    pi_target: Wtf8Buf,
    doctype: Doctype,
}

impl Default for Tokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Tokenizer {
    /// A tokenizer in the data state with no input yet.
    pub fn new() -> Self {
        Tokenizer {
            input: Input::new(),
            state: State::Data,
            return_state: State::Data,
            cur_len: 0,
            checked: 0,
            report_errors: false,
            pending_errors: Vec::new(),
            text: Wtf8Buf::new(),
            temp: Wtf8Buf::new(),
            char_ref_code: 0,
            matcher: Matcher::new(),
            tag: Tag {
                kind: TagKind::Start,
                name: Wtf8Buf::new(),
                self_closing: false,
                attributes: Vec::new(),
            },
            attr_name: Wtf8Buf::new(),
            attr_value: Wtf8Buf::new(),
            attr_open: false,
            attr_duplicate: false,
            attr_names: Default::default(),
            last_start_tag: None,
            comment: Wtf8Buf::new(),
            pi_target: Wtf8Buf::new(),
            doctype: Doctype::default(),
        }
    }

    /// Switch state. The tree builder uses this (through [`SinkResult`]) and
    /// fragment parsing uses it to set the starting state.
    pub fn set_state(&mut self, state: InitialState) {
        self.state = state.into();
    }

    /// Set "the tag name of the last start tag to have been emitted", used to
    /// decide which end tags are appropriate. Fragment parsing and tests set
    /// this; normally the tokenizer tracks it itself.
    pub fn set_last_start_tag(&mut self, name: Option<&str>) {
        self.last_start_tag = name.map(Wtf8Buf::from);
    }

    /// Push more text.
    ///
    /// # Panics
    /// If called after [`Tokenizer::end`].
    pub fn feed(&mut self, text: Wtf8<'_>) {
        // Compaction may move the buffer, so keep index-based state relative
        // to the cursor across the push.
        let checked_ahead = self.checked.saturating_sub(self.input.pos);
        self.cur_len = 0;
        self.input.push(text);
        self.checked = self.input.pos + checked_ahead;
    }

    /// Push more text from a `str`.
    pub fn feed_str(&mut self, text: &str) {
        self.feed(Wtf8::from_str(text));
    }

    /// Mark the end of the input. The next `run` will reach EOF.
    pub fn end(&mut self) {
        self.input.end();
    }

    /// Tokenize as far as the pushed input allows, handing each token to
    /// `sink`.
    pub fn run<S: TokenSink>(&mut self, sink: &mut S) -> RunResult {
        self.report_errors = sink.wants_errors();
        loop {
            if self.state == State::Finished {
                return RunResult::Done;
            }
            match self.step(sink) {
                Step::Continue => {}
                Step::NeedInput => {
                    // Text that is complete is emitted now rather than held
                    // until the next token, so streaming shows text promptly.
                    if self.flush_text(sink) == Step::Paused {
                        return RunResult::Paused;
                    }
                    return RunResult::NeedInput;
                }
                Step::Paused => return RunResult::Paused,
            }
        }
    }

    // ---- Input ---------------------------------------------------------

    /// Consume the next input character. `None` means more input is needed;
    /// nothing is consumed then.
    #[inline]
    fn next(&mut self) -> Option<u32> {
        let pos = self.input.pos;
        if pos >= self.input.len() {
            if self.input.ended {
                self.cur_len = 0;
                return Some(EOF);
            }
            return None;
        }
        let b = self.input.bytes()[pos];
        if b < 0x80 {
            self.input.pos = pos + 1;
            self.cur_len = 1;
            if pos >= self.checked {
                self.checked = pos + 1;
                if self.report_errors
                    && is_control(b as u32)
                    && !is_ascii_whitespace(b as u32)
                    && b != 0
                {
                    self.error_at(E::ControlCharacterInInputStream, pos);
                }
            }
            return Some(b as u32);
        }
        if self.input.is_trailing_high_surrogate(pos) {
            return None;
        }
        let (c, n) = self.input.decode(pos);
        self.input.pos = pos + n;
        self.cur_len = n;
        if pos >= self.checked {
            self.checked = pos + n;
            if self.report_errors {
                self.check_input_char(c, pos);
            }
        }
        Some(c)
    }

    fn check_input_char(&mut self, c: u32, at: usize) {
        if optimal_wtf8::is_surrogate(c) {
            self.error_at(E::SurrogateInInputStream, at);
        } else if is_noncharacter(c) {
            self.error_at(E::NoncharacterInInputStream, at);
        } else if is_control(c) && !is_ascii_whitespace(c) && c != 0 {
            self.error_at(E::ControlCharacterInInputStream, at);
        }
    }

    /// Check the next (unconsumed) input character for input stream errors,
    /// as if it had been consumed, without consuming it.
    fn check_next_char(&mut self) {
        let pos = self.input.pos;
        if pos < self.input.len()
            && pos >= self.checked
            && !self.input.is_trailing_high_surrogate(pos)
        {
            let (c, n) = self.input.decode(pos);
            self.checked = pos + n;
            if self.report_errors {
                self.check_input_char(c, pos);
            }
        }
    }

    /// Put the current input character back.
    #[inline]
    fn reconsume(&mut self) {
        self.input.pos -= self.cur_len;
        self.cur_len = 0;
    }

    /// Does the unconsumed input start with `lit`, comparing ASCII
    /// case-insensitively if `ci`? `None`: more input is needed to know.
    fn lookahead(&self, lit: &[u8], ci: bool) -> Option<bool> {
        let rest = &self.input.bytes()[self.input.pos..];
        let n = rest.len().min(lit.len());
        let same = if ci {
            rest[..n].eq_ignore_ascii_case(&lit[..n])
        } else {
            rest[..n] == lit[..n]
        };
        if !same {
            return Some(false);
        }
        if n < lit.len() {
            return if self.input.ended { Some(false) } else { None };
        }
        Some(true)
    }

    /// Consume `n` ASCII bytes that `lookahead` has already matched.
    fn consume_ascii(&mut self, n: usize) {
        let pos = self.input.pos;
        // Matched literals are letters, digits and punctuation: never input
        // stream errors, so only the checked mark needs moving.
        self.input.pos = pos + n;
        self.checked = self.checked.max(pos + n);
        self.cur_len = 1;
    }

    /// Copy a run of ordinary bytes starting at the cursor into `out`, stopping
    /// before the first byte `special` marks. Returns whether anything was
    /// copied. Ordinary bytes are printable ASCII, tab, LF and FF, so they
    /// never carry input stream errors.
    #[inline]
    fn bulk(
        input: &mut Input,
        checked: &mut usize,
        special: &[bool; 256],
        out: &mut Wtf8Buf,
    ) -> bool {
        let pos = input.pos;
        let rest = &input.bytes()[pos..];
        let n = rest
            .iter()
            .position(|&b| special[b as usize])
            .unwrap_or(rest.len());
        if n == 0 {
            return false;
        }
        out.push_wtf8(input.slice(pos, pos + n));
        input.pos = pos + n;
        *checked = (*checked).max(pos + n);
        true
    }

    // ---- Errors --------------------------------------------------------

    /// Report a parse error at the current input character (or at the end,
    /// for EOF).
    #[inline]
    fn error(&mut self, code: E) {
        if self.report_errors {
            let at = self.input.pos - self.cur_len;
            self.error_at(code, at);
        }
    }

    /// Report a parse error at the next (unconsumed) input character. Used by
    /// states that look ahead without consuming.
    #[inline]
    fn error_next(&mut self, code: E) {
        if self.report_errors {
            let at = self.input.pos;
            self.error_at(code, at);
        }
    }

    fn error_at(&mut self, code: E, at: usize) {
        if !self.report_errors {
            return;
        }
        let abs = self.input.abs(at);
        let position = self.input.position_of(abs);
        let offset = self.text.len();
        self.pending_errors
            .push((ParseError { code, position }, offset));
    }

    // ---- Emitting ------------------------------------------------------

    #[inline]
    fn emit_char(&mut self, c: u32) {
        self.text.push_code_point(c);
    }

    /// Send the pending text, split at the points where errors were found,
    /// with each error between the text before and after it.
    fn flush_text<S: TokenSink>(&mut self, sink: &mut S) -> Step {
        let mut sent = 0;
        let mut step = Step::Continue;
        let errors = std::mem::take(&mut self.pending_errors);
        for (e, mut offset) in errors {
            // A lone low surrogate appended after a lone high one joins it
            // into one code point (as the UTF-16 text would), which can put
            // an offset inside that pair; report the error before the pair.
            while !self.text.as_wtf8().is_boundary(offset) {
                offset -= 1;
            }
            if offset > sent {
                let r = sink.process(Token::Characters(self.text.as_wtf8().slice(sent, offset)));
                sent = offset;
                step = self.apply(r);
            }
            sink.parse_error(e);
        }
        if self.text.len() > sent {
            let r = sink.process(Token::Characters(
                self.text.as_wtf8().slice(sent, self.text.len()),
            ));
            step = self.apply(r);
        }
        self.text.clear();
        step
    }

    fn flush_errors<S: TokenSink>(&mut self, sink: &mut S) {
        debug_assert!(self.text.is_empty(), "text is flushed before other tokens");
        for (e, _) in self.pending_errors.drain(..) {
            sink.parse_error(e);
        }
    }

    fn apply(&mut self, r: SinkResult) -> Step {
        match r {
            SinkResult::Continue => Step::Continue,
            SinkResult::SwitchTo(s) => {
                self.state = s.into();
                Step::Continue
            }
            SinkResult::Pause => Step::Paused,
        }
    }

    fn emit<S: TokenSink>(&mut self, sink: &mut S, token: TokenRef) -> Step {
        if self.flush_text(sink) == Step::Paused {
            // The pending token must not be lost: the sink paused on the text
            // before it. Pausing only happens on end tags (scripts), which
            // are never preceded by an unflushed run here, because end tags
            // are emitted from states entered after the text was flushed by
            // this very function. Assert it rather than assume it.
            debug_assert!(false, "sink paused on a character token");
        }
        let r = match token {
            TokenRef::Tag => {
                self.finish_attribute();
                if self.tag.kind == TagKind::End {
                    if !self.tag.attributes.is_empty() {
                        self.error(E::EndTagWithAttributes);
                    }
                    if self.tag.self_closing {
                        self.error(E::EndTagWithTrailingSolidus);
                    }
                } else {
                    self.last_start_tag = Some(self.tag.name.clone());
                }
                self.flush_errors(sink);
                sink.process(Token::Tag(&self.tag))
            }
            TokenRef::Comment => {
                self.flush_errors(sink);
                sink.process(Token::Comment(self.comment.as_wtf8()))
            }
            TokenRef::Doctype => {
                self.flush_errors(sink);
                sink.process(Token::Doctype(&self.doctype))
            }
            TokenRef::ProcessingInstruction => {
                self.flush_errors(sink);
                sink.process(Token::ProcessingInstruction {
                    target: self.pi_target.as_wtf8(),
                    data: self.comment.as_wtf8(),
                })
            }
            TokenRef::Eof => {
                self.flush_errors(sink);
                self.state = State::Finished;
                sink.process(Token::Eof);
                return Step::Continue;
            }
        };
        self.apply(r)
    }

    fn emit_eof<S: TokenSink>(&mut self, sink: &mut S) -> Step {
        self.emit(sink, TokenRef::Eof)
    }

    // ---- Tags ------------------------------------------------------------

    fn new_tag(&mut self, kind: TagKind) {
        self.tag.kind = kind;
        self.tag.name.clear();
        self.tag.self_closing = false;
        self.tag.attributes.clear();
        self.attr_open = false;
        self.attr_names.clear();
    }

    fn start_attribute(&mut self) {
        self.finish_attribute();
        self.attr_open = true;
        self.attr_duplicate = false;
        self.attr_name.clear();
        self.attr_value.clear();
    }

    /// Run when leaving the attribute name state: a repeated name is a parse
    /// error and the attribute (with its value) is dropped.
    fn check_duplicate_attribute(&mut self) {
        let attrs = &self.tag.attributes;
        let dup = if attrs.len() <= DUP_SCAN_LIMIT {
            attrs.iter().any(|a| a.name == self.attr_name)
        } else {
            if self.attr_names.is_empty() {
                self.attr_names.extend(attrs.iter().map(|a| a.name.clone()));
            }
            self.attr_names.contains(&self.attr_name)
        };
        if dup {
            self.error(E::DuplicateAttribute);
            self.attr_duplicate = true;
        }
    }

    fn finish_attribute(&mut self) {
        if !self.attr_open {
            return;
        }
        self.attr_open = false;
        if self.attr_duplicate {
            return;
        }
        let a = Attribute {
            name: self.attr_name.take(),
            value: self.attr_value.take(),
        };
        if !self.attr_names.is_empty() || self.tag.attributes.len() >= DUP_SCAN_LIMIT {
            if self.attr_names.is_empty() {
                self.attr_names
                    .extend(self.tag.attributes.iter().map(|a| a.name.clone()));
            }
            self.attr_names.insert(a.name.clone());
        }
        self.tag.attributes.push(a);
    }

    fn is_appropriate_end_tag(&self) -> bool {
        self.tag.kind == TagKind::End
            && self
                .last_start_tag
                .as_ref()
                .is_some_and(|n| *n == self.tag.name)
    }

    // ---- Character references ---------------------------------------------

    fn char_ref_in_attribute(&self) -> bool {
        matches!(
            self.return_state,
            State::AttributeValueDoubleQuoted
                | State::AttributeValueSingleQuoted
                | State::AttributeValueUnquoted
        )
    }

    fn flush_char_ref(&mut self) {
        if self.char_ref_in_attribute() {
            self.attr_value.push_wtf8(self.temp.as_wtf8());
        } else {
            self.text.push_wtf8(self.temp.as_wtf8());
        }
    }

    fn new_doctype(&mut self) {
        self.doctype = Doctype::default();
    }

    fn doctype_name(&mut self) -> &mut Wtf8Buf {
        self.doctype.name.get_or_insert_with(Wtf8Buf::new)
    }

    // ---- The state machine --------------------------------------------------

    #[allow(clippy::cognitive_complexity)]
    fn step<S: TokenSink>(&mut self, sink: &mut S) -> Step {
        macro_rules! next {
            () => {
                match self.next() {
                    Some(c) => c,
                    None => return Step::NeedInput,
                }
            };
        }
        macro_rules! go {
            ($s:ident) => {{
                self.state = State::$s;
                return Step::Continue;
            }};
        }
        macro_rules! reconsume_in {
            ($s:ident) => {{
                self.reconsume();
                self.state = State::$s;
                return Step::Continue;
            }};
        }
        macro_rules! emit_and_go {
            ($tok:ident, $s:ident) => {{
                self.state = State::$s;
                return self.emit(sink, TokenRef::$tok);
            }};
        }

        match self.state {
            State::Finished => Step::Continue,

            // 13.2.5.1
            State::Data => loop {
                if Self::bulk(
                    &mut self.input,
                    &mut self.checked,
                    &DATA_SPECIAL,
                    &mut self.text,
                ) {
                    continue;
                }
                match next!() {
                    0x26 => {
                        self.return_state = State::Data;
                        go!(CharacterReference)
                    }
                    0x3C => go!(TagOpen),
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.emit_char(0);
                    }
                    EOF => return self.emit_eof(sink),
                    c => self.emit_char(c),
                }
            },

            // 13.2.5.2
            State::Rcdata => loop {
                if Self::bulk(
                    &mut self.input,
                    &mut self.checked,
                    &DATA_SPECIAL,
                    &mut self.text,
                ) {
                    continue;
                }
                match next!() {
                    0x26 => {
                        self.return_state = State::Rcdata;
                        go!(CharacterReference)
                    }
                    0x3C => go!(RcdataLessThanSign),
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.emit_char(0xFFFD);
                    }
                    EOF => return self.emit_eof(sink),
                    c => self.emit_char(c),
                }
            },

            // 13.2.5.3
            State::Rawtext => loop {
                if Self::bulk(
                    &mut self.input,
                    &mut self.checked,
                    &RAWTEXT_SPECIAL,
                    &mut self.text,
                ) {
                    continue;
                }
                match next!() {
                    0x3C => go!(RawtextLessThanSign),
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.emit_char(0xFFFD);
                    }
                    EOF => return self.emit_eof(sink),
                    c => self.emit_char(c),
                }
            },

            // 13.2.5.4
            State::ScriptData => loop {
                if Self::bulk(
                    &mut self.input,
                    &mut self.checked,
                    &RAWTEXT_SPECIAL,
                    &mut self.text,
                ) {
                    continue;
                }
                match next!() {
                    0x3C => go!(ScriptDataLessThanSign),
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.emit_char(0xFFFD);
                    }
                    EOF => return self.emit_eof(sink),
                    c => self.emit_char(c),
                }
            },

            // 13.2.5.5
            State::Plaintext => loop {
                if Self::bulk(
                    &mut self.input,
                    &mut self.checked,
                    &PLAINTEXT_SPECIAL,
                    &mut self.text,
                ) {
                    continue;
                }
                match next!() {
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.emit_char(0xFFFD);
                    }
                    EOF => return self.emit_eof(sink),
                    c => self.emit_char(c),
                }
            },

            // 13.2.5.6
            State::TagOpen => match next!() {
                0x21 => go!(MarkupDeclarationOpen),
                0x2F => go!(EndTagOpen),
                c if is_ascii_alpha(c) => {
                    self.new_tag(TagKind::Start);
                    reconsume_in!(TagName)
                }
                0x3F => {
                    self.temp.clear();
                    go!(ProcessingInstructionOpen)
                }
                EOF => {
                    self.error(E::EofBeforeTagName);
                    self.emit_char(0x3C);
                    self.emit_eof(sink)
                }
                _ => {
                    self.error(E::InvalidFirstCharacterOfTagName);
                    self.emit_char(0x3C);
                    reconsume_in!(Data)
                }
            },

            // 13.2.5.7
            State::EndTagOpen => match next!() {
                c if is_ascii_alpha(c) => {
                    self.new_tag(TagKind::End);
                    reconsume_in!(TagName)
                }
                0x3E => {
                    self.error(E::MissingEndTagName);
                    go!(Data)
                }
                EOF => {
                    self.error(E::EofBeforeTagName);
                    self.emit_char(0x3C);
                    self.emit_char(0x2F);
                    self.emit_eof(sink)
                }
                _ => {
                    self.error(E::InvalidFirstCharacterOfTagName);
                    self.comment.clear();
                    reconsume_in!(BogusComment)
                }
            },

            // 13.2.5.8
            State::TagName => loop {
                if Self::bulk(
                    &mut self.input,
                    &mut self.checked,
                    &TAG_NAME_SPECIAL,
                    &mut self.tag.name,
                ) {
                    continue;
                }
                match next!() {
                    c if is_ascii_whitespace(c) => go!(BeforeAttributeName),
                    0x2F => go!(SelfClosingStartTag),
                    0x3E => emit_and_go!(Tag, Data),
                    c if is_ascii_upper(c) => self.tag.name.push_code_point(c + 0x20),
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.tag.name.push_code_point(0xFFFD);
                    }
                    EOF => {
                        self.error(E::EofInTag);
                        return self.emit_eof(sink);
                    }
                    c => self.tag.name.push_code_point(c),
                }
            },

            // 13.2.5.9
            State::RcdataLessThanSign => match next!() {
                0x2F => {
                    self.temp.clear();
                    go!(RcdataEndTagOpen)
                }
                _ => {
                    self.emit_char(0x3C);
                    reconsume_in!(Rcdata)
                }
            },

            // 13.2.5.10
            State::RcdataEndTagOpen => match next!() {
                c if is_ascii_alpha(c) => {
                    self.new_tag(TagKind::End);
                    reconsume_in!(RcdataEndTagName)
                }
                _ => {
                    self.emit_char(0x3C);
                    self.emit_char(0x2F);
                    reconsume_in!(Rcdata)
                }
            },

            // 13.2.5.11
            State::RcdataEndTagName => self.end_tag_name_state(sink, State::Rcdata),

            // 13.2.5.12
            State::RawtextLessThanSign => match next!() {
                0x2F => {
                    self.temp.clear();
                    go!(RawtextEndTagOpen)
                }
                _ => {
                    self.emit_char(0x3C);
                    reconsume_in!(Rawtext)
                }
            },

            // 13.2.5.13
            State::RawtextEndTagOpen => match next!() {
                c if is_ascii_alpha(c) => {
                    self.new_tag(TagKind::End);
                    reconsume_in!(RawtextEndTagName)
                }
                _ => {
                    self.emit_char(0x3C);
                    self.emit_char(0x2F);
                    reconsume_in!(Rawtext)
                }
            },

            // 13.2.5.14
            State::RawtextEndTagName => self.end_tag_name_state(sink, State::Rawtext),

            // 13.2.5.15
            State::ScriptDataLessThanSign => match next!() {
                0x2F => {
                    self.temp.clear();
                    go!(ScriptDataEndTagOpen)
                }
                0x21 => {
                    self.emit_char(0x3C);
                    self.emit_char(0x21);
                    go!(ScriptDataEscapeStart)
                }
                _ => {
                    self.emit_char(0x3C);
                    reconsume_in!(ScriptData)
                }
            },

            // 13.2.5.16
            State::ScriptDataEndTagOpen => match next!() {
                c if is_ascii_alpha(c) => {
                    self.new_tag(TagKind::End);
                    reconsume_in!(ScriptDataEndTagName)
                }
                _ => {
                    self.emit_char(0x3C);
                    self.emit_char(0x2F);
                    reconsume_in!(ScriptData)
                }
            },

            // 13.2.5.17
            State::ScriptDataEndTagName => self.end_tag_name_state(sink, State::ScriptData),

            // 13.2.5.18
            State::ScriptDataEscapeStart => match next!() {
                0x2D => {
                    self.emit_char(0x2D);
                    go!(ScriptDataEscapeStartDash)
                }
                _ => reconsume_in!(ScriptData),
            },

            // 13.2.5.19
            State::ScriptDataEscapeStartDash => match next!() {
                0x2D => {
                    self.emit_char(0x2D);
                    go!(ScriptDataEscapedDashDash)
                }
                _ => reconsume_in!(ScriptData),
            },

            // 13.2.5.20
            State::ScriptDataEscaped => match next!() {
                0x2D => {
                    self.emit_char(0x2D);
                    go!(ScriptDataEscapedDash)
                }
                0x3C => go!(ScriptDataEscapedLessThanSign),
                0x00 => {
                    self.error(E::UnexpectedNullCharacter);
                    self.emit_char(0xFFFD);
                    Step::Continue
                }
                EOF => {
                    self.error(E::EofInScriptHtmlCommentLikeText);
                    self.emit_eof(sink)
                }
                c => {
                    self.emit_char(c);
                    Step::Continue
                }
            },

            // 13.2.5.21
            State::ScriptDataEscapedDash => match next!() {
                0x2D => {
                    self.emit_char(0x2D);
                    go!(ScriptDataEscapedDashDash)
                }
                0x3C => go!(ScriptDataEscapedLessThanSign),
                0x00 => {
                    self.error(E::UnexpectedNullCharacter);
                    self.emit_char(0xFFFD);
                    go!(ScriptDataEscaped)
                }
                EOF => {
                    self.error(E::EofInScriptHtmlCommentLikeText);
                    self.emit_eof(sink)
                }
                c => {
                    self.emit_char(c);
                    go!(ScriptDataEscaped)
                }
            },

            // 13.2.5.22
            State::ScriptDataEscapedDashDash => match next!() {
                0x2D => {
                    self.emit_char(0x2D);
                    Step::Continue
                }
                0x3C => go!(ScriptDataEscapedLessThanSign),
                0x3E => {
                    self.emit_char(0x3E);
                    go!(ScriptData)
                }
                0x00 => {
                    self.error(E::UnexpectedNullCharacter);
                    self.emit_char(0xFFFD);
                    go!(ScriptDataEscaped)
                }
                EOF => {
                    self.error(E::EofInScriptHtmlCommentLikeText);
                    self.emit_eof(sink)
                }
                c => {
                    self.emit_char(c);
                    go!(ScriptDataEscaped)
                }
            },

            // 13.2.5.23
            State::ScriptDataEscapedLessThanSign => match next!() {
                0x2F => {
                    self.temp.clear();
                    go!(ScriptDataEscapedEndTagOpen)
                }
                c if is_ascii_alpha(c) => {
                    self.temp.clear();
                    self.emit_char(0x3C);
                    reconsume_in!(ScriptDataDoubleEscapeStart)
                }
                _ => {
                    self.emit_char(0x3C);
                    reconsume_in!(ScriptDataEscaped)
                }
            },

            // 13.2.5.24
            State::ScriptDataEscapedEndTagOpen => match next!() {
                c if is_ascii_alpha(c) => {
                    self.new_tag(TagKind::End);
                    reconsume_in!(ScriptDataEscapedEndTagName)
                }
                _ => {
                    self.emit_char(0x3C);
                    self.emit_char(0x2F);
                    reconsume_in!(ScriptDataEscaped)
                }
            },

            // 13.2.5.25
            State::ScriptDataEscapedEndTagName => {
                self.end_tag_name_state(sink, State::ScriptDataEscaped)
            }

            // 13.2.5.26
            State::ScriptDataDoubleEscapeStart => match next!() {
                c if is_ascii_whitespace(c) || c == 0x2F || c == 0x3E => {
                    self.emit_char(c);
                    if self.temp == "script" {
                        go!(ScriptDataDoubleEscaped)
                    } else {
                        go!(ScriptDataEscaped)
                    }
                }
                c if is_ascii_alpha(c) => {
                    self.temp.push_code_point(to_lower(c));
                    self.emit_char(c);
                    Step::Continue
                }
                _ => reconsume_in!(ScriptDataEscaped),
            },

            // 13.2.5.27
            State::ScriptDataDoubleEscaped => match next!() {
                0x2D => {
                    self.emit_char(0x2D);
                    go!(ScriptDataDoubleEscapedDash)
                }
                0x3C => {
                    self.emit_char(0x3C);
                    go!(ScriptDataDoubleEscapedLessThanSign)
                }
                0x00 => {
                    self.error(E::UnexpectedNullCharacter);
                    self.emit_char(0xFFFD);
                    Step::Continue
                }
                EOF => {
                    self.error(E::EofInScriptHtmlCommentLikeText);
                    self.emit_eof(sink)
                }
                c => {
                    self.emit_char(c);
                    Step::Continue
                }
            },

            // 13.2.5.28
            State::ScriptDataDoubleEscapedDash => match next!() {
                0x2D => {
                    self.emit_char(0x2D);
                    go!(ScriptDataDoubleEscapedDashDash)
                }
                0x3C => {
                    self.emit_char(0x3C);
                    go!(ScriptDataDoubleEscapedLessThanSign)
                }
                0x00 => {
                    self.error(E::UnexpectedNullCharacter);
                    self.emit_char(0xFFFD);
                    go!(ScriptDataDoubleEscaped)
                }
                EOF => {
                    self.error(E::EofInScriptHtmlCommentLikeText);
                    self.emit_eof(sink)
                }
                c => {
                    self.emit_char(c);
                    go!(ScriptDataDoubleEscaped)
                }
            },

            // 13.2.5.29
            State::ScriptDataDoubleEscapedDashDash => match next!() {
                0x2D => {
                    self.emit_char(0x2D);
                    Step::Continue
                }
                0x3C => {
                    self.emit_char(0x3C);
                    go!(ScriptDataDoubleEscapedLessThanSign)
                }
                0x3E => {
                    self.emit_char(0x3E);
                    go!(ScriptData)
                }
                0x00 => {
                    self.error(E::UnexpectedNullCharacter);
                    self.emit_char(0xFFFD);
                    go!(ScriptDataDoubleEscaped)
                }
                EOF => {
                    self.error(E::EofInScriptHtmlCommentLikeText);
                    self.emit_eof(sink)
                }
                c => {
                    self.emit_char(c);
                    go!(ScriptDataDoubleEscaped)
                }
            },

            // 13.2.5.30
            State::ScriptDataDoubleEscapedLessThanSign => match next!() {
                0x2F => {
                    self.temp.clear();
                    self.emit_char(0x2F);
                    go!(ScriptDataDoubleEscapeEnd)
                }
                _ => reconsume_in!(ScriptDataDoubleEscaped),
            },

            // 13.2.5.31
            State::ScriptDataDoubleEscapeEnd => match next!() {
                c if is_ascii_whitespace(c) || c == 0x2F || c == 0x3E => {
                    self.emit_char(c);
                    if self.temp == "script" {
                        go!(ScriptDataEscaped)
                    } else {
                        go!(ScriptDataDoubleEscaped)
                    }
                }
                c if is_ascii_alpha(c) => {
                    self.temp.push_code_point(to_lower(c));
                    self.emit_char(c);
                    Step::Continue
                }
                _ => reconsume_in!(ScriptDataDoubleEscaped),
            },

            // 13.2.5.32
            State::BeforeAttributeName => match next!() {
                c if is_ascii_whitespace(c) => Step::Continue,
                0x2F | 0x3E | EOF => reconsume_in!(AfterAttributeName),
                0x3D => {
                    self.error(E::UnexpectedEqualsSignBeforeAttributeName);
                    self.start_attribute();
                    self.attr_name.push_code_point(0x3D);
                    go!(AttributeName)
                }
                _ => {
                    self.start_attribute();
                    reconsume_in!(AttributeName)
                }
            },

            // 13.2.5.33
            State::AttributeName => loop {
                match next!() {
                    c if is_ascii_whitespace(c) || c == 0x2F || c == 0x3E || c == EOF => {
                        self.reconsume();
                        self.check_duplicate_attribute();
                        go!(AfterAttributeName)
                    }
                    0x3D => {
                        self.check_duplicate_attribute();
                        go!(BeforeAttributeValue)
                    }
                    c if is_ascii_upper(c) => self.attr_name.push_code_point(c + 0x20),
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.attr_name.push_code_point(0xFFFD);
                    }
                    c @ (0x22 | 0x27 | 0x3C) => {
                        self.error(E::UnexpectedCharacterInAttributeName);
                        self.attr_name.push_code_point(c);
                    }
                    c => self.attr_name.push_code_point(c),
                }
            },

            // 13.2.5.34
            State::AfterAttributeName => match next!() {
                c if is_ascii_whitespace(c) => Step::Continue,
                0x2F => go!(SelfClosingStartTag),
                0x3D => go!(BeforeAttributeValue),
                0x3E => emit_and_go!(Tag, Data),
                EOF => {
                    self.error(E::EofInTag);
                    self.emit_eof(sink)
                }
                _ => {
                    self.start_attribute();
                    reconsume_in!(AttributeName)
                }
            },

            // 13.2.5.35
            State::BeforeAttributeValue => match next!() {
                c if is_ascii_whitespace(c) => Step::Continue,
                0x22 => go!(AttributeValueDoubleQuoted),
                0x27 => go!(AttributeValueSingleQuoted),
                0x3E => {
                    self.error(E::MissingAttributeValue);
                    emit_and_go!(Tag, Data)
                }
                _ => reconsume_in!(AttributeValueUnquoted),
            },

            // 13.2.5.36
            State::AttributeValueDoubleQuoted => loop {
                if Self::bulk(
                    &mut self.input,
                    &mut self.checked,
                    &ATTR_DQ_SPECIAL,
                    &mut self.attr_value,
                ) {
                    continue;
                }
                match next!() {
                    0x22 => go!(AfterAttributeValueQuoted),
                    0x26 => {
                        self.return_state = State::AttributeValueDoubleQuoted;
                        go!(CharacterReference)
                    }
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.attr_value.push_code_point(0xFFFD);
                    }
                    EOF => {
                        self.error(E::EofInTag);
                        return self.emit_eof(sink);
                    }
                    c => self.attr_value.push_code_point(c),
                }
            },

            // 13.2.5.37
            State::AttributeValueSingleQuoted => loop {
                if Self::bulk(
                    &mut self.input,
                    &mut self.checked,
                    &ATTR_SQ_SPECIAL,
                    &mut self.attr_value,
                ) {
                    continue;
                }
                match next!() {
                    0x27 => go!(AfterAttributeValueQuoted),
                    0x26 => {
                        self.return_state = State::AttributeValueSingleQuoted;
                        go!(CharacterReference)
                    }
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.attr_value.push_code_point(0xFFFD);
                    }
                    EOF => {
                        self.error(E::EofInTag);
                        return self.emit_eof(sink);
                    }
                    c => self.attr_value.push_code_point(c),
                }
            },

            // 13.2.5.38
            State::AttributeValueUnquoted => loop {
                match next!() {
                    c if is_ascii_whitespace(c) => go!(BeforeAttributeName),
                    0x26 => {
                        self.return_state = State::AttributeValueUnquoted;
                        go!(CharacterReference)
                    }
                    0x3E => emit_and_go!(Tag, Data),
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.attr_value.push_code_point(0xFFFD);
                    }
                    c @ (0x22 | 0x27 | 0x3C | 0x3D | 0x60) => {
                        self.error(E::UnexpectedCharacterInUnquotedAttributeValue);
                        self.attr_value.push_code_point(c);
                    }
                    EOF => {
                        self.error(E::EofInTag);
                        return self.emit_eof(sink);
                    }
                    c => self.attr_value.push_code_point(c),
                }
            },

            // 13.2.5.39
            State::AfterAttributeValueQuoted => match next!() {
                c if is_ascii_whitespace(c) => go!(BeforeAttributeName),
                0x2F => go!(SelfClosingStartTag),
                0x3E => emit_and_go!(Tag, Data),
                EOF => {
                    self.error(E::EofInTag);
                    self.emit_eof(sink)
                }
                _ => {
                    self.error(E::MissingWhitespaceBetweenAttributes);
                    reconsume_in!(BeforeAttributeName)
                }
            },

            // 13.2.5.40
            State::SelfClosingStartTag => match next!() {
                0x3E => {
                    self.tag.self_closing = true;
                    emit_and_go!(Tag, Data)
                }
                EOF => {
                    self.error(E::EofInTag);
                    self.emit_eof(sink)
                }
                _ => {
                    self.error(E::UnexpectedSolidusInTag);
                    reconsume_in!(BeforeAttributeName)
                }
            },

            // 13.2.5.41
            State::BogusComment => loop {
                match next!() {
                    0x3E => emit_and_go!(Comment, Data),
                    EOF => {
                        let s = self.emit(sink, TokenRef::Comment);
                        if s == Step::Paused {
                            return s;
                        }
                        return self.emit_eof(sink);
                    }
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.comment.push_code_point(0xFFFD);
                    }
                    c => self.comment.push_code_point(c),
                }
            },

            // 13.2.5.42
            State::MarkupDeclarationOpen => {
                match self.lookahead(b"--", false) {
                    None => return Step::NeedInput,
                    Some(true) => {
                        self.consume_ascii(2);
                        self.comment.clear();
                        go!(CommentStart)
                    }
                    Some(false) => {}
                }
                match self.lookahead(b"DOCTYPE", true) {
                    None => return Step::NeedInput,
                    Some(true) => {
                        self.consume_ascii(7);
                        go!(Doctype)
                    }
                    Some(false) => {}
                }
                match self.lookahead(b"[CDATA[", false) {
                    None => return Step::NeedInput,
                    Some(true) => {
                        self.consume_ascii(7);
                        if sink.adjusted_current_node_is_foreign() {
                            go!(CdataSection)
                        }
                        self.error(E::CdataInHtmlContent);
                        self.comment.clear();
                        self.comment.push_str("[CDATA[");
                        go!(BogusComment)
                    }
                    Some(false) => {}
                }
                // The lookahead has examined the next input character, so any
                // input stream error it carries is reported first.
                self.check_next_char();
                self.error_next(E::IncorrectlyOpenedComment);
                self.comment.clear();
                go!(BogusComment)
            }

            // 13.2.5.43
            State::CommentStart => match next!() {
                0x2D => go!(CommentStartDash),
                0x3E => {
                    self.error(E::AbruptClosingOfEmptyComment);
                    emit_and_go!(Comment, Data)
                }
                _ => reconsume_in!(Comment),
            },

            // 13.2.5.44
            State::CommentStartDash => match next!() {
                0x2D => go!(CommentEnd),
                0x3E => {
                    self.error(E::AbruptClosingOfEmptyComment);
                    emit_and_go!(Comment, Data)
                }
                EOF => {
                    self.error(E::EofInComment);
                    self.emit_comment_then_eof(sink)
                }
                _ => {
                    self.comment.push_ascii(b'-');
                    reconsume_in!(Comment)
                }
            },

            // 13.2.5.45
            State::Comment => loop {
                if Self::bulk(
                    &mut self.input,
                    &mut self.checked,
                    &COMMENT_SPECIAL,
                    &mut self.comment,
                ) {
                    continue;
                }
                match next!() {
                    0x3C => {
                        self.comment.push_ascii(b'<');
                        go!(CommentLessThanSign)
                    }
                    0x2D => go!(CommentEndDash),
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.comment.push_code_point(0xFFFD);
                    }
                    EOF => {
                        self.error(E::EofInComment);
                        return self.emit_comment_then_eof(sink);
                    }
                    c => self.comment.push_code_point(c),
                }
            },

            // 13.2.5.46
            State::CommentLessThanSign => match next!() {
                0x21 => {
                    self.comment.push_ascii(b'!');
                    go!(CommentLessThanSignBang)
                }
                0x3C => {
                    self.comment.push_ascii(b'<');
                    Step::Continue
                }
                _ => reconsume_in!(Comment),
            },

            // 13.2.5.47
            State::CommentLessThanSignBang => match next!() {
                0x2D => go!(CommentLessThanSignBangDash),
                _ => reconsume_in!(Comment),
            },

            // 13.2.5.48
            State::CommentLessThanSignBangDash => match next!() {
                0x2D => go!(CommentLessThanSignBangDashDash),
                _ => reconsume_in!(CommentEndDash),
            },

            // 13.2.5.49
            State::CommentLessThanSignBangDashDash => match next!() {
                0x3E | EOF => reconsume_in!(CommentEnd),
                _ => {
                    self.error(E::NestedComment);
                    reconsume_in!(CommentEnd)
                }
            },

            // 13.2.5.50
            State::CommentEndDash => match next!() {
                0x2D => go!(CommentEnd),
                EOF => {
                    self.error(E::EofInComment);
                    self.emit_comment_then_eof(sink)
                }
                _ => {
                    self.comment.push_ascii(b'-');
                    reconsume_in!(Comment)
                }
            },

            // 13.2.5.51
            State::CommentEnd => match next!() {
                0x3E => emit_and_go!(Comment, Data),
                0x21 => go!(CommentEndBang),
                0x2D => {
                    self.comment.push_ascii(b'-');
                    Step::Continue
                }
                EOF => {
                    self.error(E::EofInComment);
                    self.emit_comment_then_eof(sink)
                }
                _ => {
                    self.comment.push_str("--");
                    reconsume_in!(Comment)
                }
            },

            // 13.2.5.52
            State::CommentEndBang => match next!() {
                0x2D => {
                    self.comment.push_str("--!");
                    go!(CommentEndDash)
                }
                0x3E => {
                    self.error(E::IncorrectlyClosedComment);
                    emit_and_go!(Comment, Data)
                }
                EOF => {
                    self.error(E::EofInComment);
                    self.emit_comment_then_eof(sink)
                }
                _ => {
                    self.comment.push_str("--!");
                    reconsume_in!(Comment)
                }
            },

            // 13.2.5.53
            State::Doctype => match next!() {
                c if is_ascii_whitespace(c) => go!(BeforeDoctypeName),
                0x3E => reconsume_in!(BeforeDoctypeName),
                EOF => {
                    self.error(E::EofInDoctype);
                    self.new_doctype();
                    self.doctype.force_quirks = true;
                    self.emit_doctype_then_eof(sink)
                }
                _ => {
                    self.error(E::MissingWhitespaceBeforeDoctypeName);
                    reconsume_in!(BeforeDoctypeName)
                }
            },

            // 13.2.5.54
            State::BeforeDoctypeName => match next!() {
                c if is_ascii_whitespace(c) => Step::Continue,
                c if is_ascii_upper(c) => {
                    self.new_doctype();
                    self.doctype_name().push_code_point(c + 0x20);
                    go!(DoctypeName)
                }
                0x00 => {
                    self.error(E::UnexpectedNullCharacter);
                    self.new_doctype();
                    self.doctype_name().push_code_point(0xFFFD);
                    go!(DoctypeName)
                }
                0x3E => {
                    self.error(E::MissingDoctypeName);
                    self.new_doctype();
                    self.doctype.force_quirks = true;
                    emit_and_go!(Doctype, Data)
                }
                EOF => {
                    self.error(E::EofInDoctype);
                    self.new_doctype();
                    self.doctype.force_quirks = true;
                    self.emit_doctype_then_eof(sink)
                }
                c => {
                    self.new_doctype();
                    self.doctype_name().push_code_point(c);
                    go!(DoctypeName)
                }
            },

            // 13.2.5.55
            State::DoctypeName => loop {
                match next!() {
                    c if is_ascii_whitespace(c) => go!(AfterDoctypeName),
                    0x3E => emit_and_go!(Doctype, Data),
                    c if is_ascii_upper(c) => self.doctype_name().push_code_point(c + 0x20),
                    0x00 => {
                        self.error(E::UnexpectedNullCharacter);
                        self.doctype_name().push_code_point(0xFFFD);
                    }
                    EOF => {
                        self.error(E::EofInDoctype);
                        self.doctype.force_quirks = true;
                        return self.emit_doctype_then_eof(sink);
                    }
                    c => self.doctype_name().push_code_point(c),
                }
            },

            // 13.2.5.56
            State::AfterDoctypeName => match next!() {
                c if is_ascii_whitespace(c) => Step::Continue,
                0x3E => emit_and_go!(Doctype, Data),
                EOF => {
                    self.error(E::EofInDoctype);
                    self.doctype.force_quirks = true;
                    self.emit_doctype_then_eof(sink)
                }
                _ => {
                    // "the six characters starting from the current input
                    // character": put it back and look ahead.
                    self.reconsume();
                    match self.lookahead(b"PUBLIC", true) {
                        None => return Step::NeedInput,
                        Some(true) => {
                            self.consume_ascii(6);
                            go!(AfterDoctypePublicKeyword)
                        }
                        Some(false) => {}
                    }
                    match self.lookahead(b"SYSTEM", true) {
                        None => return Step::NeedInput,
                        Some(true) => {
                            self.consume_ascii(6);
                            go!(AfterDoctypeSystemKeyword)
                        }
                        Some(false) => {}
                    }
                    self.error_next(E::InvalidCharacterSequenceAfterDoctypeName);
                    self.doctype.force_quirks = true;
                    go!(BogusDoctype)
                }
            },

            // 13.2.5.57
            State::AfterDoctypePublicKeyword => match next!() {
                c if is_ascii_whitespace(c) => go!(BeforeDoctypePublicIdentifier),
                0x22 => {
                    self.error(E::MissingWhitespaceAfterDoctypePublicKeyword);
                    self.doctype.public_id = Some(Wtf8Buf::new());
                    go!(DoctypePublicIdentifierDoubleQuoted)
                }
                0x27 => {
                    self.error(E::MissingWhitespaceAfterDoctypePublicKeyword);
                    self.doctype.public_id = Some(Wtf8Buf::new());
                    go!(DoctypePublicIdentifierSingleQuoted)
                }
                0x3E => {
                    self.error(E::MissingDoctypePublicIdentifier);
                    self.doctype.force_quirks = true;
                    emit_and_go!(Doctype, Data)
                }
                EOF => {
                    self.error(E::EofInDoctype);
                    self.doctype.force_quirks = true;
                    self.emit_doctype_then_eof(sink)
                }
                _ => {
                    self.error(E::MissingQuoteBeforeDoctypePublicIdentifier);
                    self.doctype.force_quirks = true;
                    reconsume_in!(BogusDoctype)
                }
            },

            // 13.2.5.58
            State::BeforeDoctypePublicIdentifier => match next!() {
                c if is_ascii_whitespace(c) => Step::Continue,
                0x22 => {
                    self.doctype.public_id = Some(Wtf8Buf::new());
                    go!(DoctypePublicIdentifierDoubleQuoted)
                }
                0x27 => {
                    self.doctype.public_id = Some(Wtf8Buf::new());
                    go!(DoctypePublicIdentifierSingleQuoted)
                }
                0x3E => {
                    self.error(E::MissingDoctypePublicIdentifier);
                    self.doctype.force_quirks = true;
                    emit_and_go!(Doctype, Data)
                }
                EOF => {
                    self.error(E::EofInDoctype);
                    self.doctype.force_quirks = true;
                    self.emit_doctype_then_eof(sink)
                }
                _ => {
                    self.error(E::MissingQuoteBeforeDoctypePublicIdentifier);
                    self.doctype.force_quirks = true;
                    reconsume_in!(BogusDoctype)
                }
            },

            // 13.2.5.59 and 13.2.5.60
            State::DoctypePublicIdentifierDoubleQuoted
            | State::DoctypePublicIdentifierSingleQuoted => {
                let quote = if self.state == State::DoctypePublicIdentifierDoubleQuoted {
                    0x22
                } else {
                    0x27
                };
                loop {
                    match next!() {
                        c if c == quote => go!(AfterDoctypePublicIdentifier),
                        0x00 => {
                            self.error(E::UnexpectedNullCharacter);
                            self.public_id().push_code_point(0xFFFD);
                        }
                        0x3E => {
                            self.error(E::AbruptDoctypePublicIdentifier);
                            self.doctype.force_quirks = true;
                            emit_and_go!(Doctype, Data)
                        }
                        EOF => {
                            self.error(E::EofInDoctype);
                            self.doctype.force_quirks = true;
                            return self.emit_doctype_then_eof(sink);
                        }
                        c => self.public_id().push_code_point(c),
                    }
                }
            }

            // 13.2.5.61
            State::AfterDoctypePublicIdentifier => match next!() {
                c if is_ascii_whitespace(c) => go!(BetweenDoctypePublicAndSystemIdentifiers),
                0x3E => emit_and_go!(Doctype, Data),
                0x22 => {
                    self.error(E::MissingWhitespaceBetweenDoctypePublicAndSystemIdentifiers);
                    self.doctype.system_id = Some(Wtf8Buf::new());
                    go!(DoctypeSystemIdentifierDoubleQuoted)
                }
                0x27 => {
                    self.error(E::MissingWhitespaceBetweenDoctypePublicAndSystemIdentifiers);
                    self.doctype.system_id = Some(Wtf8Buf::new());
                    go!(DoctypeSystemIdentifierSingleQuoted)
                }
                EOF => {
                    self.error(E::EofInDoctype);
                    self.doctype.force_quirks = true;
                    self.emit_doctype_then_eof(sink)
                }
                _ => {
                    self.error(E::MissingQuoteBeforeDoctypeSystemIdentifier);
                    self.doctype.force_quirks = true;
                    reconsume_in!(BogusDoctype)
                }
            },

            // 13.2.5.62
            State::BetweenDoctypePublicAndSystemIdentifiers => match next!() {
                c if is_ascii_whitespace(c) => Step::Continue,
                0x3E => emit_and_go!(Doctype, Data),
                0x22 => {
                    self.doctype.system_id = Some(Wtf8Buf::new());
                    go!(DoctypeSystemIdentifierDoubleQuoted)
                }
                0x27 => {
                    self.doctype.system_id = Some(Wtf8Buf::new());
                    go!(DoctypeSystemIdentifierSingleQuoted)
                }
                EOF => {
                    self.error(E::EofInDoctype);
                    self.doctype.force_quirks = true;
                    self.emit_doctype_then_eof(sink)
                }
                _ => {
                    self.error(E::MissingQuoteBeforeDoctypeSystemIdentifier);
                    self.doctype.force_quirks = true;
                    reconsume_in!(BogusDoctype)
                }
            },

            // 13.2.5.63
            State::AfterDoctypeSystemKeyword => match next!() {
                c if is_ascii_whitespace(c) => go!(BeforeDoctypeSystemIdentifier),
                0x22 => {
                    self.error(E::MissingWhitespaceAfterDoctypeSystemKeyword);
                    self.doctype.system_id = Some(Wtf8Buf::new());
                    go!(DoctypeSystemIdentifierDoubleQuoted)
                }
                0x27 => {
                    self.error(E::MissingWhitespaceAfterDoctypeSystemKeyword);
                    self.doctype.system_id = Some(Wtf8Buf::new());
                    go!(DoctypeSystemIdentifierSingleQuoted)
                }
                0x3E => {
                    self.error(E::MissingDoctypeSystemIdentifier);
                    self.doctype.force_quirks = true;
                    emit_and_go!(Doctype, Data)
                }
                EOF => {
                    self.error(E::EofInDoctype);
                    self.doctype.force_quirks = true;
                    self.emit_doctype_then_eof(sink)
                }
                _ => {
                    self.error(E::MissingQuoteBeforeDoctypeSystemIdentifier);
                    self.doctype.force_quirks = true;
                    reconsume_in!(BogusDoctype)
                }
            },

            // 13.2.5.64
            State::BeforeDoctypeSystemIdentifier => match next!() {
                c if is_ascii_whitespace(c) => Step::Continue,
                0x22 => {
                    self.doctype.system_id = Some(Wtf8Buf::new());
                    go!(DoctypeSystemIdentifierDoubleQuoted)
                }
                0x27 => {
                    self.doctype.system_id = Some(Wtf8Buf::new());
                    go!(DoctypeSystemIdentifierSingleQuoted)
                }
                0x3E => {
                    self.error(E::MissingDoctypeSystemIdentifier);
                    self.doctype.force_quirks = true;
                    emit_and_go!(Doctype, Data)
                }
                EOF => {
                    self.error(E::EofInDoctype);
                    self.doctype.force_quirks = true;
                    self.emit_doctype_then_eof(sink)
                }
                _ => {
                    self.error(E::MissingQuoteBeforeDoctypeSystemIdentifier);
                    self.doctype.force_quirks = true;
                    reconsume_in!(BogusDoctype)
                }
            },

            // 13.2.5.65 and 13.2.5.66
            State::DoctypeSystemIdentifierDoubleQuoted
            | State::DoctypeSystemIdentifierSingleQuoted => {
                let quote = if self.state == State::DoctypeSystemIdentifierDoubleQuoted {
                    0x22
                } else {
                    0x27
                };
                loop {
                    match next!() {
                        c if c == quote => go!(AfterDoctypeSystemIdentifier),
                        0x00 => {
                            self.error(E::UnexpectedNullCharacter);
                            self.system_id().push_code_point(0xFFFD);
                        }
                        0x3E => {
                            self.error(E::AbruptDoctypeSystemIdentifier);
                            self.doctype.force_quirks = true;
                            emit_and_go!(Doctype, Data)
                        }
                        EOF => {
                            self.error(E::EofInDoctype);
                            self.doctype.force_quirks = true;
                            return self.emit_doctype_then_eof(sink);
                        }
                        c => self.system_id().push_code_point(c),
                    }
                }
            }

            // 13.2.5.67
            State::AfterDoctypeSystemIdentifier => match next!() {
                c if is_ascii_whitespace(c) => Step::Continue,
                0x3E => emit_and_go!(Doctype, Data),
                EOF => {
                    self.error(E::EofInDoctype);
                    self.doctype.force_quirks = true;
                    self.emit_doctype_then_eof(sink)
                }
                _ => {
                    // Does *not* set force-quirks.
                    self.error(E::UnexpectedCharacterAfterDoctypeSystemIdentifier);
                    reconsume_in!(BogusDoctype)
                }
            },

            // 13.2.5.68
            State::BogusDoctype => loop {
                match next!() {
                    0x3E => emit_and_go!(Doctype, Data),
                    0x00 => self.error(E::UnexpectedNullCharacter),
                    EOF => return self.emit_doctype_then_eof(sink),
                    _ => {}
                }
            },

            // 13.2.5.69
            State::CdataSection => loop {
                match next!() {
                    0x5D => go!(CdataSectionBracket),
                    EOF => {
                        self.error(E::EofInCdata);
                        return self.emit_eof(sink);
                    }
                    c => self.emit_char(c),
                }
            },

            // 13.2.5.70
            State::CdataSectionBracket => match next!() {
                0x5D => go!(CdataSectionEnd),
                _ => {
                    self.emit_char(0x5D);
                    reconsume_in!(CdataSection)
                }
            },

            // 13.2.5.71
            State::CdataSectionEnd => match next!() {
                0x5D => {
                    self.emit_char(0x5D);
                    Step::Continue
                }
                0x3E => go!(Data),
                _ => {
                    self.emit_char(0x5D);
                    self.emit_char(0x5D);
                    reconsume_in!(CdataSection)
                }
            },

            // Processing instruction open state
            State::ProcessingInstructionOpen => match next!() {
                c if is_ascii_alpha(c) || c == 0x5F => reconsume_in!(ProcessingInstructionTarget),
                EOF => {
                    self.error(E::EofInProcessingInstruction);
                    self.emit_eof(sink)
                }
                _ => {
                    self.error(E::InvalidFirstCharacterOfProcessingInstructionTarget);
                    self.temp_to_comment();
                    reconsume_in!(BogusComment)
                }
            },

            // Processing instruction target state
            State::ProcessingInstructionTarget => loop {
                match next!() {
                    c @ (0x09 | 0x0A | 0x0C | 0x20 | 0x3F | 0x3E) => {
                        let _ = c;
                        let t = self.temp.as_wtf8();
                        if t.eq_ignore_ascii_case("xml") || t.eq_ignore_ascii_case("xml-stylesheet")
                        {
                            self.error(E::DisallowedProcessingInstructionTarget);
                            self.temp_to_comment();
                            reconsume_in!(BogusComment)
                        }
                        self.pi_target.clear();
                        self.pi_target.push_wtf8(self.temp.as_wtf8());
                        self.comment.clear();
                        reconsume_in!(AfterProcessingInstructionTarget)
                    }
                    c if is_ascii_alphanumeric(c) || c == 0x2D || c == 0x5F => {
                        self.temp.push_code_point(c)
                    }
                    EOF => {
                        self.error(E::EofInProcessingInstruction);
                        return self.emit_eof(sink);
                    }
                    _ => {
                        self.error(E::InvalidProcessingInstructionTarget);
                        self.temp_to_comment();
                        reconsume_in!(BogusComment)
                    }
                }
            },

            // After processing instruction target state
            State::AfterProcessingInstructionTarget => match next!() {
                c if is_ascii_whitespace(c) => Step::Continue,
                _ => reconsume_in!(ProcessingInstructionData),
            },

            // Processing instruction data state
            State::ProcessingInstructionData => loop {
                match next!() {
                    0x3F => go!(ProcessingInstructionQuestionable),
                    0x3E => emit_and_go!(ProcessingInstruction, Data),
                    EOF => {
                        self.error(E::EofInProcessingInstruction);
                        return self.emit_eof(sink);
                    }
                    c => self.comment.push_code_point(c),
                }
            },

            // Processing instruction questionable state
            State::ProcessingInstructionQuestionable => match next!() {
                0x3E => emit_and_go!(ProcessingInstruction, Data),
                EOF => {
                    self.error(E::EofInProcessingInstruction);
                    self.emit_eof(sink)
                }
                _ => {
                    self.comment.push_ascii(b'?');
                    reconsume_in!(ProcessingInstructionData)
                }
            },

            // 13.2.5.72
            State::CharacterReference => {
                self.temp.clear();
                self.temp.push_ascii(b'&');
                match next!() {
                    c if is_ascii_alphanumeric(c) => {
                        self.matcher = Matcher::new();
                        reconsume_in!(NamedCharacterReference)
                    }
                    0x23 => {
                        self.temp.push_ascii(b'#');
                        go!(NumericCharacterReference)
                    }
                    _ => {
                        self.flush_char_ref();
                        self.state = self.return_state;
                        self.reconsume();
                        Step::Continue
                    }
                }
            }

            // 13.2.5.73
            State::NamedCharacterReference => self.named_character_reference(),

            // 13.2.5.74
            State::AmbiguousAmpersand => match next!() {
                c if is_ascii_alphanumeric(c) => {
                    if self.char_ref_in_attribute() {
                        self.attr_value.push_code_point(c);
                    } else {
                        self.emit_char(c);
                    }
                    Step::Continue
                }
                0x3B => {
                    self.error(E::UnknownNamedCharacterReference);
                    self.state = self.return_state;
                    self.reconsume();
                    Step::Continue
                }
                _ => {
                    self.state = self.return_state;
                    self.reconsume();
                    Step::Continue
                }
            },

            // 13.2.5.75
            State::NumericCharacterReference => {
                self.char_ref_code = 0;
                match next!() {
                    c @ (0x78 | 0x58) => {
                        self.temp.push_code_point(c);
                        go!(HexadecimalCharacterReferenceStart)
                    }
                    c if is_ascii_digit(c) => reconsume_in!(DecimalCharacterReference),
                    _ => {
                        self.error(E::AbsenceOfDigitsInNumericCharacterReference);
                        self.flush_char_ref();
                        self.state = self.return_state;
                        self.reconsume();
                        Step::Continue
                    }
                }
            }

            // 13.2.5.76
            State::HexadecimalCharacterReferenceStart => match next!() {
                c if is_ascii_hex_digit(c) => reconsume_in!(HexadecimalCharacterReference),
                _ => {
                    self.error(E::AbsenceOfDigitsInNumericCharacterReference);
                    self.flush_char_ref();
                    self.state = self.return_state;
                    self.reconsume();
                    Step::Continue
                }
            },

            // 13.2.5.78
            State::HexadecimalCharacterReference => loop {
                match next!() {
                    c if is_ascii_digit(c) => self.add_digit(16, c - 0x30),
                    c @ 0x41..=0x46 => self.add_digit(16, c - 0x37),
                    c @ 0x61..=0x66 => self.add_digit(16, c - 0x57),
                    0x3B => go!(NumericCharacterReferenceEnd),
                    _ => {
                        self.error(E::MissingSemicolonAfterCharacterReference);
                        reconsume_in!(NumericCharacterReferenceEnd)
                    }
                }
            },

            // 13.2.5.79
            State::DecimalCharacterReference => loop {
                match next!() {
                    c if is_ascii_digit(c) => self.add_digit(10, c - 0x30),
                    0x3B => go!(NumericCharacterReferenceEnd),
                    _ => {
                        self.error(E::MissingSemicolonAfterCharacterReference);
                        reconsume_in!(NumericCharacterReferenceEnd)
                    }
                }
            },

            // 13.2.5.80
            State::NumericCharacterReferenceEnd => {
                let mut c = self.char_ref_code;
                if c == 0 {
                    self.error_next(E::NullCharacterReference);
                    c = 0xFFFD;
                } else if c > 0x10FFFF {
                    self.error_next(E::CharacterReferenceOutsideUnicodeRange);
                    c = 0xFFFD;
                } else if optimal_wtf8::is_surrogate(c) {
                    self.error_next(E::SurrogateCharacterReference);
                    c = 0xFFFD;
                } else if is_noncharacter(c) {
                    self.error_next(E::NoncharacterCharacterReference);
                } else if c == 0x0D || (is_control(c) && !is_ascii_whitespace(c)) {
                    self.error_next(E::ControlCharacterReference);
                    if let Some(r) = c1_replacement(c) {
                        c = r;
                    }
                }
                self.temp.clear();
                self.temp.push_code_point(c);
                self.flush_char_ref();
                self.state = self.return_state;
                Step::Continue
            }
        }
    }

    /// The shared body of the RCDATA, RAWTEXT, script data and script data
    /// escaped "end tag name" states (13.2.5.11, .14, .17, .25). `text_state`
    /// is the state to fall back to.
    fn end_tag_name_state<S: TokenSink>(&mut self, sink: &mut S, text_state: State) -> Step {
        loop {
            let c = match self.next() {
                Some(c) => c,
                None => return Step::NeedInput,
            };
            match c {
                c if is_ascii_whitespace(c) && self.is_appropriate_end_tag() => {
                    self.state = State::BeforeAttributeName;
                    return Step::Continue;
                }
                0x2F if self.is_appropriate_end_tag() => {
                    self.state = State::SelfClosingStartTag;
                    return Step::Continue;
                }
                0x3E if self.is_appropriate_end_tag() => {
                    self.state = State::Data;
                    return self.emit(sink, TokenRef::Tag);
                }
                c if is_ascii_alpha(c) => {
                    self.tag.name.push_code_point(to_lower(c));
                    self.temp.push_code_point(c);
                }
                _ => {
                    self.emit_char(0x3C);
                    self.emit_char(0x2F);
                    self.text.push_wtf8(self.temp.as_wtf8());
                    self.reconsume();
                    self.state = text_state;
                    return Step::Continue;
                }
            }
        }
    }

    /// 13.2.5.73 Named character reference state.
    fn named_character_reference(&mut self) -> Step {
        // Look ahead for the longest name without consuming, so that running
        // out of input part-way leaves everything in place.
        let bytes = self.input.bytes();
        let start = self.input.pos;
        let mut m = Matcher::new();
        let mut i = start;
        loop {
            match bytes.get(i) {
                Some(&b) => {
                    if !m.feed(b) {
                        break;
                    }
                    i += 1;
                }
                None => {
                    if self.input.ended {
                        break;
                    }
                    return Step::NeedInput;
                }
            }
        }
        match m.best() {
            Some((len, Expansion { first, second })) => {
                let last_is_semicolon = bytes[start + len - 1] == b';';
                let next = bytes.get(start + len).copied();
                if next.is_none()
                    && !self.input.ended
                    && !last_is_semicolon
                    && self.char_ref_in_attribute()
                {
                    // The historical rule below needs the next character.
                    return Step::NeedInput;
                }
                let name = self.input.slice(start, start + len);
                self.temp.push_wtf8(name);
                self.consume_ascii(len);
                if self.char_ref_in_attribute()
                    && !last_is_semicolon
                    && next.is_some_and(|b| b == b'=' || b.is_ascii_alphanumeric())
                {
                    self.flush_char_ref();
                    self.state = self.return_state;
                    return Step::Continue;
                }
                if !last_is_semicolon {
                    // Reported at the character after the name, where the
                    // missing semicolon should have been.
                    self.error_next(E::MissingSemicolonAfterCharacterReference);
                }
                self.temp.clear();
                self.temp.push_code_point(first);
                if second != 0 {
                    self.temp.push_code_point(second);
                }
                self.flush_char_ref();
                self.state = self.return_state;
                Step::Continue
            }
            None => {
                self.flush_char_ref();
                self.state = State::AmbiguousAmpersand;
                Step::Continue
            }
        }
    }

    fn add_digit(&mut self, base: u32, d: u32) {
        // Saturate above the Unicode range: the value only matters as "too
        // big" once it passes U+10FFFF, and this keeps it from overflowing on
        // inputs like "&#99999999999999999999;".
        self.char_ref_code = self
            .char_ref_code
            .saturating_mul(base)
            .saturating_add(d)
            .min(0x110000);
    }

    fn temp_to_comment(&mut self) {
        self.comment.clear();
        self.comment.push_ascii(b'?');
        self.comment.push_wtf8(self.temp.as_wtf8());
    }

    fn public_id(&mut self) -> &mut Wtf8Buf {
        self.doctype.public_id.get_or_insert_with(Wtf8Buf::new)
    }

    fn system_id(&mut self) -> &mut Wtf8Buf {
        self.doctype.system_id.get_or_insert_with(Wtf8Buf::new)
    }

    fn emit_comment_then_eof<S: TokenSink>(&mut self, sink: &mut S) -> Step {
        if self.emit(sink, TokenRef::Comment) == Step::Paused {
            return Step::Paused;
        }
        self.emit_eof(sink)
    }

    fn emit_doctype_then_eof<S: TokenSink>(&mut self, sink: &mut S) -> Step {
        if self.emit(sink, TokenRef::Doctype) == Step::Paused {
            return Step::Paused;
        }
        self.emit_eof(sink)
    }
}

/// Which buffered token to emit.
#[derive(Clone, Copy)]
enum TokenRef {
    Tag,
    Comment,
    Doctype,
    ProcessingInstruction,
    Eof,
}
