//! The CSS parser: css-syntax-3 section 5 ("Parsing").
//!
//! Design choices (logged: chosen, why, rejected):
//!
//! - **Component values are a flat token list plus a skip table.** Which
//!   tokens form a function or a simple block doesn't depend on where the
//!   parser is: a `{`, `[`, `(` or function token always runs to the first
//!   matching closer that isn't inside something opened after it, or to the
//!   end of input (5.5.10 to 5.5.12). So one pass with a stack works out, for
//!   every token, the index just past the component value it starts
//!   ([`TokenList`]). A component value is then a view (list, index), its
//!   children are the tokens up to its closer, and "consume a component
//!   value" is one table lookup. Why: no tree is built or copied, nothing is
//!   recursive (so deeply nested input can't overflow the stack, in parsing
//!   or in drop), and preludes and values are just index ranges. Rejected: a
//!   tree of `Vec`s (copies every value, recursive drop).
//! - **Rules live in an arena** ([`Parsed::rules`]) and are built by an
//!   explicit stack of open blocks, not by recursion, for the same reason.
//! - **Declarations are tried early-exit.** In a block's contents the spec
//!   tries "consume a declaration" and, if it returns nothing, rewinds and
//!   consumes a qualified rule from the same place (5.5.5). Done literally,
//!   `a:b{a:b{a:b{...` re-reads every inner block once per level: quadratic.
//!   When the attempt is going to be rewound anyway, it stops as soon as
//!   failure is certain: no ident, no colon, a `{}`-block after other
//!   values, or more than `! important` after a `{}`-block, in a
//!   declaration that isn't a custom property (the implementation note of
//!   5.5.5 lists these). Each top-level token of a block is then read at most
//!   twice, and nested blocks are skipped through the table, so parsing is
//!   linear. Entry points that don't rewind run the literal algorithm.
//! - **Validity is a hook** ([`Validity`]). The spec leaves "valid in the
//!   current context" to the specs that define rules and properties; the
//!   default accepts everything, which is what a generic CSS syntax parser
//!   (and the conformance suites) do.

use crate::tokenizer::{Spanned, Token, TokenError, Tokenizer};
use optimal_wtf8::Wtf8;
use std::ops::Range;

/// Marks an opener that reaches the end of input without its closer.
const UNCLOSED: u32 = 1 << 31;

/// The tokens of an input, without comments, with the component-value
/// structure: for each token, where the component value it starts ends.
#[derive(Clone, Debug, Default)]
pub struct TokenList<'a> {
    tokens: Vec<Spanned<'a>>,
    /// Index just past the component value starting at each token, with
    /// [`UNCLOSED`] set on openers that run to the end of their run of
    /// tokens without a closer.
    next: Vec<u32>,
}

/// What kind of simple block (5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// `{}`-block
    Curly,
    /// `[]`-block
    Square,
    /// `()`-block
    Paren,
}

fn closer_of(t: &Token<'_>) -> Option<fn(&Token<'_>) -> bool> {
    match t {
        Token::OpenCurly => Some(|t| matches!(t, Token::CloseCurly)),
        Token::OpenSquare => Some(|t| matches!(t, Token::CloseSquare)),
        Token::OpenParen | Token::Function(_) => Some(|t| matches!(t, Token::CloseParen)),
        _ => None,
    }
}

impl<'a> TokenList<'a> {
    /// Append `tokens` (comments already removed) as their own run: their
    /// component values end at the end of the run at the latest. Returns the
    /// index range they occupy.
    fn push_run(&mut self, tokens: impl IntoIterator<Item = Spanned<'a>>) -> Range<u32> {
        let first = self.tokens.len();
        self.tokens.extend(tokens);
        let end = self.tokens.len();
        assert!(end < UNCLOSED as usize, "too many tokens");
        self.next.resize(end, 0);
        // Openers waiting for their closer, innermost last.
        let mut open: Vec<usize> = Vec::new();
        for i in first..end {
            self.next[i] = i as u32 + 1;
            if let Some(&top) = open.last()
                && closer_of(&self.tokens[top].token).expect("only openers are pushed")(
                    &self.tokens[i].token,
                )
            {
                open.pop();
                self.next[top] = i as u32 + 1;
            } else if closer_of(&self.tokens[i].token).is_some() {
                open.push(i);
            }
        }
        for i in open {
            self.next[i] = end as u32 | UNCLOSED;
        }
        first as u32..end as u32
    }

    /// The number of tokens.
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether there are no tokens.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// The token at `i`, with its source range.
    pub fn get(&self, i: u32) -> &Spanned<'a> {
        &self.tokens[i as usize]
    }

    /// Index just past the component value that starts at `i`.
    fn skip(&self, i: u32) -> u32 {
        self.next[i as usize] & !UNCLOSED
    }

    /// The component value starting at token `i`.
    pub fn value(&self, i: u32) -> ComponentValue<'_, 'a> {
        ComponentValue { list: self, i }
    }

    /// The component values starting at the tokens in `range`, which must
    /// begin at a component value and end where one ends.
    pub fn values(&self, range: Range<u32>) -> Values<'_, 'a> {
        Values {
            list: self,
            at: range.start,
            end: range.end,
        }
    }
}

/// A component value (5.4): a preserved token, a function or a simple block.
/// A view into a [`TokenList`].
#[derive(Clone, Copy, Debug)]
pub struct ComponentValue<'t, 'a> {
    list: &'t TokenList<'a>,
    i: u32,
}

/// What a [`ComponentValue`] is.
#[derive(Debug)]
pub enum Kind<'t, 'a> {
    /// A preserved token (any token but an opener).
    Token(&'t Token<'a>),
    /// A function: its name and its contents.
    Function(&'t str, Values<'t, 'a>),
    /// A simple block: its kind and its contents.
    Block(BlockKind, Values<'t, 'a>),
}

impl<'t, 'a> ComponentValue<'t, 'a> {
    /// The index of its first token.
    pub fn index(self) -> u32 {
        self.i
    }

    /// Its first token.
    pub fn token(self) -> &'t Token<'a> {
        &self.list.tokens[self.i as usize].token
    }

    /// What it is.
    pub fn kind(self) -> Kind<'t, 'a> {
        let contents = || self.list.values(self.i + 1..self.contents_end());
        match self.token() {
            Token::Function(name) => Kind::Function(name, contents()),
            Token::OpenCurly => Kind::Block(BlockKind::Curly, contents()),
            Token::OpenSquare => Kind::Block(BlockKind::Square, contents()),
            Token::OpenParen => Kind::Block(BlockKind::Paren, contents()),
            t => Kind::Token(t),
        }
    }

    /// For a function or block, whether its closer was in the input (it
    /// isn't when the input ends first).
    pub fn is_closed(self) -> bool {
        self.list.next[self.i as usize] & UNCLOSED == 0
    }

    /// Where its contents end: before the closer, or at the end.
    fn contents_end(self) -> u32 {
        let end = self.list.skip(self.i);
        if self.is_closed() && closer_of(self.token()).is_some() {
            end - 1
        } else {
            end
        }
    }

    /// The source byte range it covers (comments inside included).
    pub fn span(self) -> Range<usize> {
        let last = self.list.skip(self.i) - 1;
        self.list.tokens[self.i as usize].start..self.list.tokens[last as usize].end
    }
}

/// A sequence of sibling component values.
#[derive(Clone, Copy, Debug)]
pub struct Values<'t, 'a> {
    list: &'t TokenList<'a>,
    at: u32,
    end: u32,
}

impl<'t, 'a> Values<'t, 'a> {
    /// The token index range they cover.
    pub fn range(self) -> Range<u32> {
        self.at..self.end
    }

    /// Whether there are none.
    pub fn is_empty(self) -> bool {
        self.at == self.end
    }
}

impl<'t, 'a> Iterator for Values<'t, 'a> {
    type Item = ComponentValue<'t, 'a>;

    fn next(&mut self) -> Option<ComponentValue<'t, 'a>> {
        if self.at >= self.end {
            return None;
        }
        let v = self.list.value(self.at);
        self.at = self.list.skip(self.at);
        Some(v)
    }
}

/// Index of a rule in [`Parsed::rules`].
pub type RuleId = u32;

/// A declaration (5.4).
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    /// Index of the ident token that gives its name.
    pub name: u32,
    /// Its value: component values, `!important` and trailing whitespace
    /// removed.
    pub value: Range<u32>,
    /// The important flag.
    pub important: bool,
    /// For custom properties, the source text of the value (5.5.6 step 8),
    /// as a byte range.
    pub original_text: Option<Range<usize>>,
}

/// An item of a block's contents (5.5.5): a rule, or a run of declarations.
#[derive(Clone, Debug, PartialEq)]
pub enum BlockItem {
    /// A rule.
    Rule(RuleId),
    /// Declarations not separated by a rule.
    Declarations(Vec<Declaration>),
}

/// A rule (5.4).
#[derive(Clone, Debug, PartialEq)]
pub enum Rule {
    /// An at-rule: the index of its at-keyword token, its prelude, and for
    /// block at-rules, the block's contents.
    At {
        /// Index of the at-keyword token.
        name: u32,
        /// The prelude.
        prelude: Range<u32>,
        /// The block's contents, for block at-rules.
        block: Option<Vec<BlockItem>>,
    },
    /// A qualified rule: prelude, declarations, and child rules (where later
    /// runs of declarations become nested declarations rules).
    Qualified {
        /// The prelude.
        prelude: Range<u32>,
        /// The declarations before the first child rule.
        declarations: Vec<Declaration>,
        /// Child rules.
        children: Vec<RuleId>,
    },
    /// A nested declarations rule (made from declarations after a child
    /// rule in a qualified rule's block).
    NestedDeclarations(Vec<Declaration>),
}

/// The parse errors the parser itself reports (5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParserErrorKind {
    /// 5.5.3: the input ended in a qualified rule's prelude.
    EofInRule,
    /// 5.5.3: a `;` ended a nested qualified rule's prelude.
    SemicolonInRule,
    /// 5.5.3: a `}` in a qualified rule's prelude.
    CloseCurlyInRule,
    /// 5.5.7: a `}` in a list of component values outside a block.
    StrayCloseCurly,
}

/// A parse error: from the tokenizer or the parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParseError {
    /// The tokenizer's.
    Token(TokenError),
    /// The parser's, at a byte offset (the token's start, or the end of
    /// input).
    Parser(ParserErrorKind, usize),
}

/// The spec's "valid in the current context" checks, for the specs that
/// define rules and properties. Each gets the parse so far and the rule or
/// declaration, plus whether it is nested in a block. The default methods
/// accept everything.
pub trait Validity {
    /// Is this at-rule valid here?
    fn at_rule(&mut self, _parsed: &Parsed<'_>, _rule: &Rule, _nested: bool) -> bool {
        true
    }
    /// Is this qualified rule valid here?
    fn qualified_rule(&mut self, _parsed: &Parsed<'_>, _rule: &Rule, _nested: bool) -> bool {
        true
    }
    /// Is this declaration valid here?
    fn declaration(&mut self, _parsed: &Parsed<'_>, _decl: &Declaration, _nested: bool) -> bool {
        true
    }
}

/// A [`Validity`] that accepts everything: a generic syntax-level parse.
#[derive(Clone, Copy, Debug, Default)]
pub struct AcceptAll;

impl Validity for AcceptAll {}

/// The result of a parse: the tokens, every rule (in an arena), the errors.
#[derive(Clone, Debug, Default)]
pub struct Parsed<'a> {
    src: &'a [u8],
    /// The tokens and component-value structure.
    pub tokens: TokenList<'a>,
    /// Every rule made, by [`RuleId`]. Rules that were made and then dropped
    /// (invalid, or inside a discarded at-rule) may be here too; only
    /// reachable ones are part of the result.
    pub rules: Vec<Rule>,
    /// Tokenizer errors, then parser errors, each in input order.
    pub errors: Vec<ParseError>,
}

impl<'a> Parsed<'a> {
    fn new(src: Wtf8<'a>) -> Self {
        let mut t = Tokenizer::new(src);
        let mut tokens = TokenList::default();
        let toks: Vec<Spanned<'a>> = std::iter::from_fn(|| t.next_token())
            .filter(|s| s.token != Token::Comment)
            .collect();
        tokens.push_run(toks);
        let errors = t.errors().iter().map(|&e| ParseError::Token(e)).collect();
        Parsed {
            src: src.as_bytes(),
            tokens,
            rules: Vec::new(),
            errors,
        }
    }

    /// The name of an at-rule or a declaration, from its token index.
    pub fn name(&self, i: u32) -> &str {
        match &self.tokens.get(i).token {
            Token::AtKeyword(s) | Token::Ident(s) => s,
            other => panic!("not a name token: {other:?}"),
        }
    }

    /// The component values in `range`.
    pub fn values(&self, range: Range<u32>) -> Values<'_, 'a> {
        self.tokens.values(range)
    }

    /// The source text a byte range covers.
    pub fn source(&self, range: Range<usize>) -> Wtf8<'a> {
        Wtf8::from_bytes(&self.src[range]).expect("ranges fall on code point boundaries")
    }
}

/// The state of an at-rule or qualified rule whose block is being read.
struct Open {
    /// For at-rules, the at-keyword index; `None` for qualified rules.
    at_name: Option<u32>,
    prelude: Range<u32>,
    nested: bool,
    /// Where the block's contents end (its closer, or the end of input).
    limit: u32,
    /// Where to continue after the block.
    after: u32,
    items: Vec<BlockItem>,
    decls: Vec<Declaration>,
    /// The block is consumed only to be dropped (5.5.3, a custom-property
    /// look-alike outside a block).
    discard: bool,
}

/// What a rule-consuming step produced, as in 5.5.3 and 5.5.4.
enum Made {
    Rule(RuleId),
    InvalidRule,
    Nothing,
}

/// What the stylesheet or block-contents loop does with a finished rule.
#[derive(Clone, Copy, PartialEq)]
enum Parent {
    /// 5.5.1: append rules, drop everything else.
    Sheet,
    /// The rule is the result of the entry point (parse a rule).
    Single,
    /// The rule is consumed and dropped (an at-rule in a list of
    /// declarations).
    Discard,
}

struct Parser<'p, 'a, V> {
    p: &'p mut Parsed<'a>,
    v: &'p mut V,
    /// Token index of the next token.
    i: u32,
    /// End of the tokens being parsed.
    end: u32,
}

impl<'p, 'a, V: Validity> Parser<'p, 'a, V> {
    fn token(&self, i: u32) -> Option<&Token<'a>> {
        (i < self.end).then(|| &self.p.tokens.get(i).token)
    }

    fn next(&self) -> Option<&Token<'a>> {
        self.token(self.i)
    }

    fn error(&mut self, kind: ParserErrorKind, at: u32) {
        let offset = if at < self.end {
            self.p.tokens.get(at).start
        } else {
            self.p.src.len()
        };
        self.p.errors.push(ParseError::Parser(kind, offset));
    }

    fn discard_whitespace(&mut self) {
        while matches!(self.next(), Some(Token::Whitespace)) {
            self.i += 1;
        }
    }

    /// 5.5.8: the value starting at `i` and the index after it.
    fn skip(&self, i: u32) -> u32 {
        self.p.tokens.skip(i)
    }

    /// 5.5.7 Consume a list of component values, stopping before `limit`
    /// (the end of the enclosing block or input). Returns the end index of
    /// the list.
    fn component_values(&mut self, stop_semicolon: bool, nested: bool, limit: u32) -> u32 {
        loop {
            let Some(t) = self.token(self.i).filter(|_| self.i < limit) else {
                return self.i;
            };
            match t {
                Token::Semicolon if stop_semicolon => return self.i,
                Token::CloseCurly => {
                    // In a block, the `}` at this level is its closer, which
                    // is the limit; so a `}` seen here is outside any block
                    // (5.5.7: with nested false, a parse error, consumed).
                    debug_assert!(!nested, "a block's closer is its limit");
                    self.error(ParserErrorKind::StrayCloseCurly, self.i);
                    self.i += 1;
                }
                _ => self.i = self.skip(self.i),
            }
        }
    }

    /// 5.5.6 Consume the remnants of a bad declaration, stopping before a
    /// `;` (every caller discards it next, as the spec's version does).
    fn bad_declaration_remnants(&mut self, nested: bool, limit: u32) {
        while let Some(t) = self.token(self.i).filter(|_| self.i < limit) {
            if let Token::Semicolon = t {
                return;
            }
            // A `}` here is outside any block (see component_values), and
            // is consumed like any other token.
            debug_assert!(!nested || !matches!(t, Token::CloseCurly));
            self.i = self.skip(self.i);
        }
    }

    /// 5.5.6 Consume a declaration. With `speculative`, the caller rewinds
    /// on failure, so this may stop as soon as failure is certain and leave
    /// the position anywhere.
    fn declaration(&mut self, nested: bool, speculative: bool, limit: u32) -> Option<Declaration> {
        // Step 1.
        let name = self.i;
        // Callers only start a declaration before the limit.
        debug_assert!(name < limit);
        let Some(Token::Ident(name_text)) = self.token(name) else {
            if !speculative {
                self.bad_declaration_remnants(nested, limit);
            }
            return None;
        };
        // "Custom property name string": css-variables-2 section 2 makes a
        // custom property name any ident starting with "--" except "--"
        // itself (reserved); css-syntax-3 doesn't define the term further.
        let custom = name_text.starts_with("--") && name_text.len() > 2;
        let unicode_range = name_text.eq_ignore_ascii_case("unicode-range");
        self.i += 1;
        // Steps 2 to 4.
        self.discard_whitespace();
        if !matches!(self.token(self.i), Some(Token::Colon)) {
            if !speculative {
                self.bad_declaration_remnants(nested, limit);
            }
            return None;
        }
        self.i += 1;
        self.discard_whitespace();
        // Step 5, watching for a certain failure of step 8's {}-block rule.
        let start = self.i;
        let list_end = if speculative && !custom {
            debug_assert!(nested, "only block contents rewind");
            self.declaration_value_early_exit(limit)?
        } else {
            self.component_values(true, nested, limit)
        };
        // Steps 6 and 7.
        let (end, important) = self.trim_important(start, list_end);
        let mut decl = Declaration {
            name,
            value: start..end,
            important,
            original_text: None,
        };
        // Step 8.
        if custom {
            decl.original_text = Some(self.byte_range(start..end));
        } else if self.has_curly_block_and_more(start..end) {
            return None;
        } else if unicode_range {
            decl.value = self.retokenize_unicode_range(start..list_end);
        }
        // Step 9.
        self.v.declaration(self.p, &decl, nested).then_some(decl)
    }

    /// Step 5 of 5.5.6 for a speculative, non-custom declaration: the same
    /// list, but `None` as soon as step 8 is sure to reject it (a `{}`-block
    /// after any non-whitespace value, or any value after a `{}`-block other
    /// than a final `! important`).
    fn declaration_value_early_exit(&mut self, limit: u32) -> Option<u32> {
        let mut before_block = 0u32;
        let mut seen_block = false;
        let mut after_block = 0u32;
        loop {
            let Some(t) = self.token(self.i).filter(|_| self.i < limit) else {
                return Some(self.i);
            };
            match t {
                Token::Semicolon => return Some(self.i),
                Token::Whitespace => {}
                Token::OpenCurly => {
                    if seen_block || before_block > 0 {
                        return None;
                    }
                    seen_block = true;
                }
                _ if seen_block => {
                    after_block += 1;
                    if after_block > 2 {
                        return None;
                    }
                }
                _ => before_block += 1,
            }
            self.i = self.skip(self.i);
        }
    }

    /// Steps 6 and 7 of 5.5.6 on the value `start..end`: the new end after
    /// removing a final `! important` and trailing whitespace, and whether
    /// it was there.
    fn trim_important(&self, start: u32, end: u32) -> (u32, bool) {
        // The last three non-whitespace values, walking forwards (the skip
        // table only goes forwards); linear in the value's length.
        let (mut third, mut second, mut last) = (None, None, None);
        let mut i = start;
        while i < end {
            if !matches!(self.p.tokens.get(i).token, Token::Whitespace) {
                (third, second, last) = (second, last, Some(i));
            }
            i = self.skip(i);
        }
        let important = matches!(
            (second.map(|i| &self.p.tokens.get(i).token), last.map(|i| &self.p.tokens.get(i).token)),
            (Some(Token::Delim('!')), Some(Token::Ident(s))) if s.eq_ignore_ascii_case("important")
        );
        // Removing "!" and "important" leaves only whitespace after the value
        // before them, and step 7 removes that too.
        let kept = if important { third } else { last };
        (kept.map_or(start, |k| self.skip(k)), important)
    }

    /// Step 8's check: a top-level `{}`-block and any other non-whitespace
    /// value.
    fn has_curly_block_and_more(&self, range: Range<u32>) -> bool {
        let mut block = false;
        let mut other = false;
        for v in self.p.tokens.values(range) {
            match v.token() {
                Token::OpenCurly => {
                    other |= block;
                    block = true;
                }
                Token::Whitespace => {}
                _ => other = true,
            }
        }
        block && other
    }

    fn byte_range(&self, range: Range<u32>) -> Range<usize> {
        if range.is_empty() {
            let at = if range.start < self.end {
                self.p.tokens.get(range.start).start
            } else {
                self.p.src.len()
            };
            return at..at;
        }
        let last = range.end - 1;
        self.p.tokens.get(range.start).start..self.p.tokens.get(last).end
    }

    /// 5.5.13 Consume the value of a unicode-range descriptor, from the
    /// source text of the tokens in `range` (everything step 5 consumed,
    /// which per the text includes any `!important`).
    fn retokenize_unicode_range(&mut self, range: Range<u32>) -> Range<u32> {
        let bytes = self.byte_range(range);
        let base = bytes.start;
        let text = Wtf8::from_bytes(&self.p.src[bytes]).expect("token boundaries");
        let mut t = Tokenizer::new(text);
        let toks: Vec<Spanned<'a>> = std::iter::from_fn(|| t.next_token_with(true))
            .filter(|s| s.token != Token::Comment)
            .map(|s| Spanned {
                token: s.token.into_owned(),
                start: s.start + base,
                end: s.end + base,
            })
            .collect();
        // Errors were reported by the first tokenization of the same text.
        self.p.tokens.push_run(toks)
    }

    /// 5.5.3 and 5.5.4: start consuming a rule at the next token (an
    /// at-keyword for an at-rule). Returns the rule if it has no block, or
    /// the open block to read.
    fn start_rule(&mut self, nested: bool, stop_semicolon: bool, limit: u32) -> Result<Made, Open> {
        if let Some(Token::AtKeyword(_)) = self.next() {
            let name = self.i;
            self.i += 1;
            let start = self.i;
            loop {
                let Some(t) = self.token(self.i).filter(|_| self.i < limit) else {
                    let rule = Rule::At {
                        name,
                        prelude: start..self.i,
                        block: None,
                    };
                    return Ok(self.finish_at(rule, nested));
                };
                match t {
                    Token::Semicolon => {
                        let rule = Rule::At {
                            name,
                            prelude: start..self.i,
                            block: None,
                        };
                        self.i += 1;
                        return Ok(self.finish_at(rule, nested));
                    }
                    Token::OpenCurly => return Err(self.open_block(Some(name), start, nested)),
                    _ => {
                        // A `}` before the limit is outside any block (see
                        // component_values) and goes into the prelude; in a
                        // block, reaching the limit (its closer) ends the
                        // rule above, as 5.5.2's `}` branch does.
                        debug_assert!(!(nested && matches!(t, Token::CloseCurly)));
                        self.i = self.skip(self.i);
                    }
                }
            }
        }
        let start = self.i;
        loop {
            let Some(t) = self.token(self.i).filter(|_| self.i < limit) else {
                // The limit is the end of input, or the `}` closing the
                // enclosing block (then nested is true, and that `}` is what
                // ends the prelude).
                let kind = if self.i < self.end {
                    ParserErrorKind::CloseCurlyInRule
                } else {
                    ParserErrorKind::EofInRule
                };
                self.error(kind, self.i);
                return Ok(Made::Nothing);
            };
            match t {
                Token::Semicolon if stop_semicolon => {
                    self.error(ParserErrorKind::SemicolonInRule, self.i);
                    return Ok(Made::Nothing);
                }
                Token::CloseCurly => {
                    // Outside any block (see component_values); in a block,
                    // the closer is the limit, handled above.
                    debug_assert!(!nested);
                    self.error(ParserErrorKind::CloseCurlyInRule, self.i);
                    self.i += 1;
                }
                Token::OpenCurly => {
                    if self.looks_like_custom_property(start..self.i) {
                        if nested {
                            self.bad_declaration_remnants(true, limit);
                            return Ok(Made::Nothing);
                        }
                        // "Consume a block" and return nothing: the block's
                        // contents are still parsed (their parse errors and
                        // validity checks happen), then dropped.
                        let mut open = self.open_block(None, start, nested);
                        open.discard = true;
                        return Err(open);
                    }
                    return Err(self.open_block(None, start, nested));
                }
                _ => self.i = self.skip(self.i),
            }
        }
    }

    /// 5.5.3: are the first two non-whitespace values an ident starting
    /// with "--" and a colon?
    fn looks_like_custom_property(&self, prelude: Range<u32>) -> bool {
        let mut it = self
            .p
            .tokens
            .values(prelude)
            .filter(|v| !matches!(v.token(), Token::Whitespace));
        matches!(it.next().map(|v| v.token()), Some(Token::Ident(s)) if s.starts_with("--"))
            && matches!(it.next().map(|v| v.token()), Some(Token::Colon))
    }

    /// The next token is a `{`: open its block (5.5.4 consume a block).
    fn open_block(&mut self, at_name: Option<u32>, prelude_start: u32, nested: bool) -> Open {
        let open = self.i;
        let after = self.skip(open);
        let closed = self.p.tokens.value(open).is_closed();
        let limit = if closed { after - 1 } else { after };
        self.i = open + 1;
        Open {
            at_name,
            prelude: prelude_start..open,
            nested,
            limit,
            after,
            items: Vec::new(),
            decls: Vec::new(),
            discard: false,
        }
    }

    fn push_rule(&mut self, rule: Rule) -> RuleId {
        let id = self.p.rules.len() as RuleId;
        self.p.rules.push(rule);
        id
    }

    fn finish_at(&mut self, rule: Rule, nested: bool) -> Made {
        if self.v.at_rule(self.p, &rule, nested) {
            Made::Rule(self.push_rule(rule))
        } else {
            Made::Nothing
        }
    }

    /// The block of `open` is fully read: make its rule.
    fn close(&mut self, mut open: Open) -> Made {
        if !open.decls.is_empty() {
            open.items
                .push(BlockItem::Declarations(std::mem::take(&mut open.decls)));
        }
        self.i = open.after;
        if open.discard {
            return Made::Nothing;
        }
        match open.at_name {
            Some(name) => {
                let rule = Rule::At {
                    name,
                    prelude: open.prelude,
                    block: Some(open.items),
                };
                self.finish_at(rule, open.nested)
            }
            None => {
                let mut items = open.items.into_iter().peekable();
                let declarations = match items.next_if(|i| matches!(i, BlockItem::Declarations(_)))
                {
                    Some(BlockItem::Declarations(d)) => d,
                    _ => Vec::new(),
                };
                let mut children = Vec::new();
                for item in items {
                    children.push(match item {
                        BlockItem::Rule(r) => r,
                        BlockItem::Declarations(d) => self.push_rule(Rule::NestedDeclarations(d)),
                    });
                }
                let rule = Rule::Qualified {
                    prelude: open.prelude,
                    declarations,
                    children,
                };
                if self.v.qualified_rule(self.p, &rule, open.nested) {
                    Made::Rule(self.push_rule(rule))
                } else {
                    Made::InvalidRule
                }
            }
        }
    }

    /// Run the rule machine: the stylesheet loop (5.5.1) or a single rule,
    /// with every block's contents (5.5.5) read on an explicit stack.
    /// Returns the rules the top level produced.
    fn run(&mut self, top: Parent) -> Vec<RuleId> {
        let mut out = Vec::new();
        let mut stack: Vec<Open> = Vec::new();
        let mut started = false;
        loop {
            // A rule just finished at the current nesting level, if any.
            let made;
            if let Some(open) = stack.last_mut() {
                let limit = open.limit;
                if self.i >= limit {
                    let open = stack.pop().expect("non-empty");
                    made = self.close(open);
                } else {
                    // 5.5.5, one step.
                    match self.next().expect("before the limit") {
                        Token::Whitespace | Token::Semicolon => {
                            self.i += 1;
                            continue;
                        }
                        Token::AtKeyword(_) => {
                            let open = stack.last_mut().expect("non-empty");
                            if !open.decls.is_empty() {
                                let d = std::mem::take(&mut open.decls);
                                open.items.push(BlockItem::Declarations(d));
                            }
                            match self.start_rule(true, false, limit) {
                                Ok(m) => made = m,
                                Err(o) => {
                                    stack.push(o);
                                    continue;
                                }
                            }
                        }
                        _ => {
                            let mark = self.i;
                            if let Some(d) = self.declaration(true, true, limit) {
                                stack.last_mut().expect("non-empty").decls.push(d);
                                continue;
                            }
                            self.i = mark;
                            match self.start_rule(true, true, limit) {
                                Ok(m) => made = m,
                                Err(o) => {
                                    stack.push(o);
                                    continue;
                                }
                            }
                        }
                    }
                }
            } else {
                // The top level.
                match top {
                    Parent::Sheet => match self.next() {
                        None => return out,
                        Some(Token::Whitespace | Token::Cdo | Token::Cdc) => {
                            self.i += 1;
                            continue;
                        }
                        Some(_) => match self.start_rule(false, false, self.end) {
                            Ok(m) => made = m,
                            Err(o) => {
                                stack.push(o);
                                continue;
                            }
                        },
                    },
                    Parent::Single | Parent::Discard => {
                        if started {
                            return out;
                        }
                        started = true;
                        match self.start_rule(false, false, self.end) {
                            Ok(m) => made = m,
                            Err(o) => {
                                stack.push(o);
                                continue;
                            }
                        }
                    }
                }
            }
            // Hand the finished rule to its parent.
            if let Some(parent) = stack.last_mut() {
                match made {
                    Made::Nothing => {}
                    Made::InvalidRule | Made::Rule(_) => {
                        if !parent.decls.is_empty() {
                            let d = std::mem::take(&mut parent.decls);
                            parent.items.push(BlockItem::Declarations(d));
                        }
                        if let Made::Rule(r) = made {
                            parent.items.push(BlockItem::Rule(r));
                        }
                    }
                }
            } else {
                // For parse a rule (Parent::Single), an invalid rule error
                // leaves `out` empty, which the caller reports as a syntax
                // error.
                if let (Parent::Sheet | Parent::Single, Made::Rule(r)) = (top, made) {
                    out.push(r);
                }
                if top != Parent::Sheet {
                    return out;
                }
            }
        }
    }
}

/// Why a single-item entry point gave no result (5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxError {
    /// Nothing but whitespace.
    Empty,
    /// The item is missing or invalid.
    Invalid,
    /// A valid item, followed by more than whitespace.
    ExtraInput,
}

fn parse<'a, V: Validity, R>(
    src: Wtf8<'a>,
    v: &mut V,
    f: impl FnOnce(&mut Parser<'_, 'a, V>) -> R,
) -> (Parsed<'a>, R) {
    let mut p = Parsed::new(src);
    let end = p.tokens.len() as u32;
    let r = {
        let mut parser = Parser {
            p: &mut p,
            v,
            i: 0,
            end,
        };
        f(&mut parser)
    };
    (p, r)
}

/// 5.3.3 Parse a stylesheet (and 5.3.4, a stylesheet's contents: the same
/// for text input). Returns the top-level rules.
pub fn parse_stylesheet<'a>(src: Wtf8<'a>, v: &mut impl Validity) -> (Parsed<'a>, Vec<RuleId>) {
    parse(src, v, |p| p.run(Parent::Sheet))
}

/// 5.3.5 Parse a list of declarations.
pub fn parse_declaration_list<'a>(
    src: Wtf8<'a>,
    v: &mut impl Validity,
) -> (Parsed<'a>, Vec<Declaration>) {
    parse(src, v, |p| {
        let mut decls = Vec::new();
        loop {
            p.discard_whitespace();
            match p.next() {
                None => return decls,
                Some(Token::Semicolon) => p.i += 1,
                Some(Token::AtKeyword(_)) => {
                    p.run(Parent::Discard);
                }
                Some(_) => {
                    let end = p.end;
                    if let Some(d) = p.declaration(false, false, end) {
                        decls.push(d);
                    }
                }
            }
        }
    })
}

/// 5.3.6 Parse a rule.
pub fn parse_rule<'a>(
    src: Wtf8<'a>,
    v: &mut impl Validity,
) -> (Parsed<'a>, Result<RuleId, SyntaxError>) {
    parse(src, v, |p| {
        p.discard_whitespace();
        if p.next().is_none() {
            return Err(SyntaxError::Empty);
        }
        let rule = p.run(Parent::Single).pop().ok_or(SyntaxError::Invalid)?;
        p.discard_whitespace();
        if p.next().is_none() {
            Ok(rule)
        } else {
            Err(SyntaxError::ExtraInput)
        }
    })
}

/// 5.3.7 Parse a declaration.
pub fn parse_declaration<'a>(
    src: Wtf8<'a>,
    v: &mut impl Validity,
) -> (Parsed<'a>, Result<Declaration, SyntaxError>) {
    parse(src, v, |p| {
        p.discard_whitespace();
        if p.next().is_none() {
            return Err(SyntaxError::Empty);
        }
        let end = p.end;
        let d = p
            .declaration(false, false, end)
            .ok_or(SyntaxError::Invalid)?;
        p.discard_whitespace();
        if p.next().is_none() {
            Ok(d)
        } else {
            Err(SyntaxError::ExtraInput)
        }
    })
}

/// 5.3.8 Parse a component value. Returns the index of its first token.
pub fn parse_component_value(src: Wtf8<'_>) -> (Parsed<'_>, Result<u32, SyntaxError>) {
    parse(src, &mut AcceptAll, |p| {
        p.discard_whitespace();
        if p.next().is_none() {
            return Err(SyntaxError::Empty);
        }
        let v = p.i;
        p.i = p.skip(v);
        p.discard_whitespace();
        if p.next().is_none() {
            Ok(v)
        } else {
            Err(SyntaxError::ExtraInput)
        }
    })
}

/// 5.3.9 Parse a list of component values.
pub fn parse_component_values(src: Wtf8<'_>) -> (Parsed<'_>, Range<u32>) {
    parse(src, &mut AcceptAll, |p| {
        let end = p.end;
        let stop = p.component_values(false, false, end);
        0..stop
    })
}

/// 5.3.10 Parse a comma-separated list of component values.
pub fn parse_comma_separated_component_values(src: Wtf8<'_>) -> (Parsed<'_>, Vec<Range<u32>>) {
    parse(src, &mut AcceptAll, |p| {
        let mut groups = Vec::new();
        while p.next().is_some() {
            let start = p.i;
            let stop = p.component_values_until_comma();
            groups.push(start..stop);
            // Discard the comma.
            if p.next().is_some() {
                p.i += 1;
            }
        }
        groups
    })
}

impl<V: Validity> Parser<'_, '_, V> {
    /// 5.5.7 with `<comma-token>` as the stop token, not nested.
    fn component_values_until_comma(&mut self) -> u32 {
        loop {
            match self.next() {
                None | Some(Token::Comma) => return self.i,
                Some(Token::CloseCurly) => {
                    self.error(ParserErrorKind::StrayCloseCurly, self.i);
                    self.i += 1;
                }
                Some(_) => self.i = self.skip(self.i),
            }
        }
    }
}

#[cfg(test)]
mod early_exit_tests {
    use super::*;

    /// Runs the early-exit value scan on all of `src` (or up to `limit`):
    /// its result and where it stopped.
    fn scan(src: &str, limit: Option<u32>) -> (Option<u32>, u32) {
        parse(Wtf8::from_str(src), &mut AcceptAll, |p| {
            let limit = limit.unwrap_or(p.end);
            let r = p.declaration_value_early_exit(limit);
            (r, p.i)
        })
        .1
    }

    #[test]
    fn stops_at_the_first_certain_failure() {
        // Tokens: { a } ws b ws c ws d. Three values after the block: the
        // third ("d", token 8) is where step 8 is sure to fail.
        assert_eq!(scan("{a} b c d", None), (None, 8));
        // Two after it may still be "! important".
        assert_eq!(scan("{a} ! important", None), (Some(7), 7));
        // A block after another value (token 2), or a second block (3).
        assert_eq!(scan("b {a}", None), (None, 2));
        assert_eq!(scan("{a}{b}", None), (None, 3));
        // No block: the whole list, to the end or the ";".
        assert_eq!(scan("b c", None), (Some(3), 3));
        assert_eq!(scan("b;c", None), (Some(1), 1));
        // It stops at the limit.
        assert_eq!(scan("b c", Some(1)), (Some(1), 1));
    }
}
