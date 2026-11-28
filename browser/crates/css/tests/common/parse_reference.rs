//! A literal model of css-syntax-3 parsing (section 5), for differential
//! tests. It follows the 2026 text step by step on a token stream with
//! "mark" and "restore a mark", builds a nested tree with recursion, and
//! always runs "consume a declaration" in full before rewinding. It is slow
//! (quadratic on some inputs) and only meant for small ones. It shares no
//! parsing code with the crate, only the tokenizer.
//!
//! Results are written as JSON in a shape [`crate::common::parse_json`] also
//! produces from the crate's output, so the two can be compared exactly.

use optimal_css::{Spanned, Token, Tokenizer};
use optimal_wtf8::Wtf8;
use serde_json::{Value, json};

/// Which constructs the "valid in the current context" checks reject, so
/// the invalid-rule paths get exercised: declarations named `x`, qualified
/// rules whose prelude starts with the ident `bad`, at-rules named `bad`.
pub fn decl_valid(name: &str) -> bool {
    name != "x"
}
pub fn qualified_valid(first_prelude_ident: Option<&str>) -> bool {
    first_prelude_ident != Some("bad")
}
pub fn at_valid(name: &str) -> bool {
    name != "bad"
}

struct Stream {
    tokens: Vec<Spanned<'static>>,
    index: usize,
    marks: Vec<usize>,
    src_len: usize,
    errors: Vec<Value>,
    source: Vec<u8>,
    /// End of the stream: tokens past it are re-read unicode-range values.
    end: usize,
}

/// A component value of the model.
#[derive(Clone, Debug)]
enum Cv {
    Token(usize),
    Function {
        name: String,
        at: usize,
        value: Vec<Cv>,
        closed: bool,
    },
    Block {
        at: usize,
        value: Vec<Cv>,
        closed: bool,
    },
}

#[derive(Clone, Debug)]
struct Decl {
    name: String,
    value: Vec<Cv>,
    important: bool,
    original: Option<(usize, usize)>,
}

#[derive(Clone, Debug)]
enum Item {
    Rule(RuleM),
    Decls(Vec<Decl>),
}

#[derive(Clone, Debug)]
enum RuleM {
    At {
        name: String,
        prelude: Vec<Cv>,
        block: Option<Vec<Item>>,
    },
    Qualified {
        prelude: Vec<Cv>,
        decls: Vec<Decl>,
        children: Vec<RuleM>,
    },
    Nested(Vec<Decl>),
}

enum QrResult {
    Nothing,
    Invalid,
    Rule(RuleM),
}

fn tok_name<'t>(t: &'t Token<'_>) -> Option<&'t str> {
    match t {
        Token::Ident(s) => Some(s),
        _ => None,
    }
}

impl Stream {
    fn next(&self) -> Option<&Token<'static>> {
        (self.index < self.end).then(|| &self.tokens[self.index].token)
    }
    fn is(&self, f: impl Fn(&Token<'_>) -> bool) -> bool {
        self.next().is_some_and(f)
    }
    fn eof(&self) -> bool {
        self.next().is_none()
    }
    fn consume(&mut self) -> usize {
        let i = self.index;
        self.index += 1;
        i
    }
    fn discard(&mut self) {
        if !self.eof() {
            self.index += 1;
        }
    }
    fn mark(&mut self) {
        self.marks.push(self.index);
    }
    fn restore(&mut self) {
        self.index = self.marks.pop().unwrap();
    }
    fn discard_mark(&mut self) {
        self.marks.pop().unwrap();
    }
    fn discard_whitespace(&mut self) {
        while self.is(|t| matches!(t, Token::Whitespace)) {
            self.discard();
        }
    }
    fn error(&mut self, kind: &str) {
        let at = if self.index < self.end {
            self.tokens[self.index].start
        } else {
            self.src_len
        };
        self.errors.push(json!([kind, at]));
    }

    /// 5.5.1
    fn stylesheet_contents(&mut self) -> Vec<RuleM> {
        let mut rules = Vec::new();
        loop {
            match self.next() {
                Some(Token::Whitespace) => self.discard(),
                None => return rules,
                Some(Token::Cdo | Token::Cdc) => self.discard(),
                Some(Token::AtKeyword(_)) => {
                    if let Some(r) = self.at_rule(false) {
                        rules.push(r);
                    }
                }
                Some(_) => {
                    if let QrResult::Rule(r) = self.qualified_rule(false, false) {
                        rules.push(r);
                    }
                }
            }
        }
    }

    /// 5.5.2
    fn at_rule(&mut self, nested: bool) -> Option<RuleM> {
        let i = self.consume();
        let Token::AtKeyword(name) = &self.tokens[i].token else {
            unreachable!()
        };
        let name = name.to_string();
        let mut prelude = Vec::new();
        loop {
            match self.next() {
                Some(Token::Semicolon) | None => {
                    self.discard();
                    return at_valid(&name).then_some(RuleM::At {
                        name,
                        prelude,
                        block: None,
                    });
                }
                Some(Token::CloseCurly) => {
                    if nested {
                        return at_valid(&name).then_some(RuleM::At {
                            name,
                            prelude,
                            block: None,
                        });
                    }
                    let t = self.consume();
                    prelude.push(Cv::Token(t));
                }
                Some(Token::OpenCurly) => {
                    let block = self.block();
                    return at_valid(&name).then_some(RuleM::At {
                        name,
                        prelude,
                        block: Some(block),
                    });
                }
                Some(_) => prelude.push(self.component_value()),
            }
        }
    }

    /// 5.5.3
    fn qualified_rule(&mut self, stop_semicolon: bool, nested: bool) -> QrResult {
        let mut prelude: Vec<Cv> = Vec::new();
        loop {
            match self.next() {
                None => {
                    self.error("EofInRule");
                    return QrResult::Nothing;
                }
                Some(Token::Semicolon) if stop_semicolon => {
                    self.error("SemicolonInRule");
                    return QrResult::Nothing;
                }
                Some(Token::CloseCurly) => {
                    self.error("CloseCurlyInRule");
                    if nested {
                        return QrResult::Nothing;
                    }
                    let t = self.consume();
                    prelude.push(Cv::Token(t));
                }
                Some(Token::OpenCurly) => {
                    let non_ws: Vec<&Cv> = prelude
                        .iter()
                        .filter(|c| !matches!(c, Cv::Token(i) if matches!(self.tokens[*i].token, Token::Whitespace)))
                        .take(2)
                        .collect();
                    let custom = matches!(non_ws.first(), Some(Cv::Token(i)) if matches!(&self.tokens[*i].token, Token::Ident(s) if s.starts_with("--")))
                        && matches!(non_ws.get(1), Some(Cv::Token(i)) if matches!(self.tokens[*i].token, Token::Colon));
                    if custom {
                        if nested {
                            self.bad_declaration_remnants(true);
                        } else {
                            self.block();
                        }
                        return QrResult::Nothing;
                    }
                    let mut child_rules = self.block();
                    let mut decls = Vec::new();
                    if let Some(Item::Decls(_)) = child_rules.first() {
                        let Item::Decls(d) = child_rules.remove(0) else {
                            unreachable!()
                        };
                        decls = d;
                    }
                    let children = child_rules
                        .into_iter()
                        .map(|i| match i {
                            Item::Decls(d) => RuleM::Nested(d),
                            Item::Rule(r) => r,
                        })
                        .collect();
                    let first_ident = match prelude.first() {
                        Some(Cv::Token(i)) => tok_name(&self.tokens[*i].token).map(str::to_owned),
                        _ => None,
                    };
                    let rule = RuleM::Qualified {
                        prelude,
                        decls,
                        children,
                    };
                    return if qualified_valid(first_ident.as_deref()) {
                        QrResult::Rule(rule)
                    } else {
                        QrResult::Invalid
                    };
                }
                Some(_) => prelude.push(self.component_value()),
            }
        }
    }

    /// 5.5.4
    fn block(&mut self) -> Vec<Item> {
        assert!(self.is(|t| matches!(t, Token::OpenCurly)));
        self.discard();
        let rules = self.block_contents();
        self.discard();
        rules
    }

    /// 5.5.5
    fn block_contents(&mut self) -> Vec<Item> {
        let mut rules = Vec::new();
        let mut decls = Vec::new();
        let flush = |rules: &mut Vec<Item>, decls: &mut Vec<Decl>| {
            if !decls.is_empty() {
                rules.push(Item::Decls(std::mem::take(decls)));
            }
        };
        loop {
            match self.next() {
                Some(Token::Whitespace | Token::Semicolon) => self.discard(),
                None | Some(Token::CloseCurly) => {
                    flush(&mut rules, &mut decls);
                    return rules;
                }
                Some(Token::AtKeyword(_)) => {
                    flush(&mut rules, &mut decls);
                    if let Some(r) = self.at_rule(true) {
                        rules.push(Item::Rule(r));
                    }
                }
                Some(_) => {
                    self.mark();
                    if let Some(d) = self.declaration(true) {
                        decls.push(d);
                        self.discard_mark();
                    } else {
                        self.restore();
                        match self.qualified_rule(true, true) {
                            QrResult::Nothing => {}
                            QrResult::Invalid => flush(&mut rules, &mut decls),
                            QrResult::Rule(r) => {
                                flush(&mut rules, &mut decls);
                                rules.push(Item::Rule(r));
                            }
                        }
                    }
                }
            }
        }
    }

    /// 5.5.6
    fn declaration(&mut self, nested: bool) -> Option<Decl> {
        let name = match self.next() {
            Some(Token::Ident(s)) => {
                let s = s.to_string();
                self.consume();
                s
            }
            _ => {
                self.bad_declaration_remnants(nested);
                return None;
            }
        };
        self.discard_whitespace();
        if self.is(|t| matches!(t, Token::Colon)) {
            self.discard();
        } else {
            self.bad_declaration_remnants(nested);
            return None;
        }
        self.discard_whitespace();
        let list_start = self.index;
        let mut value = self.component_values(Some(Token::Semicolon), nested);
        let list_end = self.index;
        let ws = |s: &Self, c: &Cv| matches!(c, Cv::Token(i) if matches!(s.tokens[*i].token, Token::Whitespace));
        let non_ws: Vec<usize> = (0..value.len()).filter(|&k| !ws(self, &value[k])).collect();
        let mut important = false;
        if non_ws.len() >= 2 {
            let (a, b) = (non_ws[non_ws.len() - 2], non_ws[non_ws.len() - 1]);
            let bang = matches!(value[a], Cv::Token(i) if matches!(self.tokens[i].token, Token::Delim('!')));
            let imp = matches!(&value[b], Cv::Token(i) if matches!(&self.tokens[*i].token, Token::Ident(s) if s.eq_ignore_ascii_case("important")));
            if bang && imp {
                value.remove(b);
                value.remove(a);
                important = true;
            }
        }
        while value.last().is_some_and(|c| ws(self, c)) {
            value.pop();
        }
        let mut original = None;
        if name.starts_with("--") && name != "--" {
            original = Some(match (value.first(), value.last()) {
                (Some(f), Some(l)) => (self.cv_start(f), self.cv_end(l)),
                _ => {
                    let at = if list_start < self.end { self.tokens[list_start].start } else { self.src_len };
                    (at, at)
                }
            });
        } else if value.iter().any(|c| matches!(c, Cv::Block { at, .. } if matches!(self.tokens[*at].token, Token::OpenCurly)))
            && value.iter().filter(|c| !ws(self, c)).count() > 1
        {
            return None;
        } else if name.eq_ignore_ascii_case("unicode-range") {
            // 5.5.13 on the source text of everything step 5 consumed.
            let (from, to) = if list_start < list_end {
                (self.tokens[list_start].start, self.tokens[list_end - 1].end)
            } else {
                let at = if list_start < self.end { self.tokens[list_start].start } else { self.src_len };
                (at, at)
            };
            value = self.retokenize(from, to);
        }
        decl_valid(&name).then_some(Decl {
            name,
            value,
            important,
            original,
        })
    }

    fn retokenize(&mut self, from: usize, to: usize) -> Vec<Cv> {
        let text = self.source[from..to].to_vec();
        let w = Wtf8::from_bytes(&text).unwrap();
        let mut t = Tokenizer::new(w);
        let toks: Vec<Spanned<'static>> = std::iter::from_fn(|| t.next_token_with(true))
            .filter(|s| s.token != Token::Comment)
            .map(|s| Spanned {
                token: s.token.into_owned(),
                start: s.start + from,
                end: s.end + from,
            })
            .collect();
        // Consume a list of component values from a fresh stream.
        let n = toks.len();
        let mut sub = Stream {
            end: n,
            tokens: toks,
            index: 0,
            marks: Vec::new(),
            src_len: to,
            errors: Vec::new(),
            source: Vec::new(),
        };
        let vals = sub.component_values(None, false);
        // Splice the sub-stream's tokens onto ours so indices stay valid.
        let base = self.tokens.len();
        self.tokens.extend(sub.tokens);
        // Errors found again in re-read text are not reported twice (the
        // crate's choice, see parser.rs); the model drops them too.
        vals.into_iter().map(|c| shift(c, base)).collect()
    }

    fn cv_start(&self, c: &Cv) -> usize {
        match c {
            Cv::Token(i) | Cv::Function { at: i, .. } | Cv::Block { at: i, .. } => {
                self.tokens[*i].start
            }
        }
    }
    fn cv_end(&self, c: &Cv) -> usize {
        match c {
            Cv::Token(i) => self.tokens[*i].end,
            Cv::Function {
                at, value, closed, ..
            }
            | Cv::Block { at, value, closed } => {
                if *closed {
                    // The closer follows the last value (or the opener).
                    self.tokens[self.last_index(*at, value) + 1].end
                } else {
                    match value.last() {
                        Some(l) => self.cv_end(l),
                        None => self.tokens[*at].end,
                    }
                }
            }
        }
    }
    fn last_index(&self, at: usize, value: &[Cv]) -> usize {
        match value.last() {
            None => at,
            Some(Cv::Token(i)) => *i,
            Some(
                Cv::Function {
                    at, value, closed, ..
                }
                | Cv::Block { at, value, closed },
            ) => {
                let l = self.last_index(*at, value);
                if *closed { l + 1 } else { l }
            }
        }
    }

    /// 5.5.6, the remnants.
    fn bad_declaration_remnants(&mut self, nested: bool) {
        loop {
            match self.next() {
                None | Some(Token::Semicolon) => {
                    self.discard();
                    return;
                }
                Some(Token::CloseCurly) => {
                    if nested {
                        return;
                    }
                    self.discard();
                }
                Some(_) => {
                    self.component_value();
                }
            }
        }
    }

    /// 5.5.7
    fn component_values(&mut self, stop: Option<Token<'static>>, nested: bool) -> Vec<Cv> {
        let mut values = Vec::new();
        loop {
            match self.next() {
                None => return values,
                Some(t)
                    if stop.as_ref().is_some_and(|s| {
                        std::mem::discriminant(s) == std::mem::discriminant(t)
                    }) =>
                {
                    return values;
                }
                Some(Token::CloseCurly) => {
                    if nested {
                        return values;
                    }
                    self.error("StrayCloseCurly");
                    let t = self.consume();
                    values.push(Cv::Token(t));
                }
                Some(_) => values.push(self.component_value()),
            }
        }
    }

    /// 5.5.8
    fn component_value(&mut self) -> Cv {
        match self.next() {
            Some(Token::OpenCurly | Token::OpenSquare | Token::OpenParen) => self.simple_block(),
            Some(Token::Function(_)) => self.function(),
            _ => Cv::Token(self.consume()),
        }
    }

    /// 5.5.9
    fn simple_block(&mut self) -> Cv {
        let at = self.index;
        let ending = match self.next().unwrap() {
            Token::OpenCurly => Token::CloseCurly,
            Token::OpenSquare => Token::CloseSquare,
            _ => Token::CloseParen,
        };
        self.discard();
        let mut value = Vec::new();
        loop {
            match self.next() {
                None => {
                    return Cv::Block {
                        at,
                        value,
                        closed: false,
                    };
                }
                Some(t) if std::mem::discriminant(t) == std::mem::discriminant(&ending) => {
                    self.discard();
                    return Cv::Block {
                        at,
                        value,
                        closed: true,
                    };
                }
                Some(_) => value.push(self.component_value()),
            }
        }
    }

    /// 5.5.10
    fn function(&mut self) -> Cv {
        let at = self.consume();
        let Token::Function(name) = &self.tokens[at].token else {
            unreachable!()
        };
        let name = name.to_string();
        let mut value = Vec::new();
        loop {
            match self.next() {
                None => {
                    return Cv::Function {
                        name,
                        at,
                        value,
                        closed: false,
                    };
                }
                Some(Token::CloseParen) => {
                    self.discard();
                    return Cv::Function {
                        name,
                        at,
                        value,
                        closed: true,
                    };
                }
                Some(_) => value.push(self.component_value()),
            }
        }
    }
}

fn shift(c: Cv, by: usize) -> Cv {
    match c {
        Cv::Token(i) => Cv::Token(i + by),
        Cv::Function {
            name,
            at,
            value,
            closed,
        } => Cv::Function {
            name,
            at: at + by,
            value: value.into_iter().map(|c| shift(c, by)).collect(),
            closed,
        },
        Cv::Block { at, value, closed } => Cv::Block {
            at: at + by,
            value: value.into_iter().map(|c| shift(c, by)).collect(),
            closed,
        },
    }
}

/// The common JSON shape (see `parse_json` in mod.rs).
struct Out<'s> {
    s: &'s Stream,
}

impl Out<'_> {
    fn cv(&self, c: &Cv) -> Value {
        match c {
            Cv::Token(i) => {
                let t = &self.s.tokens[*i];
                json!(["t", format!("{:?}", t.token), t.start, t.end])
            }
            Cv::Function {
                name,
                value,
                closed,
                ..
            } => {
                json!(["f", name, self.cvs(value), closed])
            }
            Cv::Block { at, value, closed } => {
                json!([
                    "b",
                    format!("{:?}", self.s.tokens[*at].token),
                    self.cvs(value),
                    closed
                ])
            }
        }
    }
    fn cvs(&self, v: &[Cv]) -> Value {
        Value::Array(v.iter().map(|c| self.cv(c)).collect())
    }
    fn decl(&self, d: &Decl) -> Value {
        json!([
            "d",
            d.name,
            self.cvs(&d.value),
            d.important,
            d.original.map(|(a, b)| json!([a, b]))
        ])
    }
    fn decls(&self, d: &[Decl]) -> Value {
        Value::Array(d.iter().map(|d| self.decl(d)).collect())
    }
    fn rule(&self, r: &RuleM) -> Value {
        match r {
            RuleM::At {
                name,
                prelude,
                block,
            } => json!([
                "at",
                name,
                self.cvs(prelude),
                block
                    .as_ref()
                    .map(|b| Value::Array(b.iter().map(|i| self.item(i)).collect()))
            ]),
            RuleM::Qualified {
                prelude,
                decls,
                children,
            } => json!([
                "qr",
                self.cvs(prelude),
                self.decls(decls),
                Value::Array(children.iter().map(|c| self.rule(c)).collect())
            ]),
            RuleM::Nested(d) => json!(["nd", self.decls(d)]),
        }
    }
    fn item(&self, i: &Item) -> Value {
        match i {
            Item::Rule(r) => self.rule(r),
            Item::Decls(d) => json!(["decls", self.decls(d)]),
        }
    }
}

/// Which entry point to run.
#[derive(Clone, Copy, Debug)]
pub enum Entry {
    Stylesheet,
    DeclarationList,
    Rule,
    Declaration,
    ComponentValue,
    ComponentValues,
    CommaSeparated,
}

impl Stream {
    fn new(src: Wtf8<'_>) -> Self {
        let mut t = Tokenizer::new(src);
        let tokens: Vec<Spanned<'static>> = std::iter::from_fn(|| t.next_token())
            .filter(|s| s.token != Token::Comment)
            .map(|s| Spanned {
                token: s.token.into_owned(),
                start: s.start,
                end: s.end,
            })
            .collect();
        let end = tokens.len();
        Stream {
            end,
            tokens,
            index: 0,
            marks: Vec::new(),
            src_len: src.len(),
            errors: Vec::new(),
            source: src.as_bytes().to_vec(),
        }
    }
}

/// Run `entry` on `src` with the model: the result and the parser errors.
pub fn parse(src: Wtf8<'_>, entry: Entry) -> (Value, Value) {
    let mut s = Stream::new(src);
    let result = run(&mut s, entry);
    (result, Value::Array(s.errors.clone()))
}

fn run(s: &mut Stream, entry: Entry) -> Value {
    match entry {
        Entry::Stylesheet => {
            let rules = s.stylesheet_contents();
            let o = Out { s };
            Value::Array(rules.iter().map(|r| o.rule(r)).collect())
        }
        Entry::DeclarationList => {
            let mut decls = Vec::new();
            while !s.eof() {
                s.discard_whitespace();
                match s.next() {
                    None => {}
                    Some(Token::Semicolon) => s.discard(),
                    Some(Token::AtKeyword(_)) => {
                        s.at_rule(false);
                    }
                    Some(_) => {
                        if let Some(d) = s.declaration(false) {
                            decls.push(d);
                        }
                    }
                }
            }
            Out { s }.decls(&decls)
        }
        Entry::Rule => {
            s.discard_whitespace();
            if s.eof() {
                return json!(["error", "empty"]);
            }
            let rule = if s.is(|t| matches!(t, Token::AtKeyword(_))) {
                s.at_rule(false)
            } else {
                match s.qualified_rule(false, false) {
                    QrResult::Rule(r) => Some(r),
                    _ => None,
                }
            };
            let Some(rule) = rule else {
                return json!(["error", "invalid"]);
            };
            s.discard_whitespace();
            if s.eof() {
                Out { s }.rule(&rule)
            } else {
                json!(["error", "extra-input"])
            }
        }
        Entry::Declaration => {
            s.discard_whitespace();
            if s.eof() {
                return json!(["error", "empty"]);
            }
            let Some(d) = s.declaration(false) else {
                return json!(["error", "invalid"]);
            };
            s.discard_whitespace();
            if s.eof() {
                Out { s }.decl(&d)
            } else {
                json!(["error", "extra-input"])
            }
        }
        Entry::ComponentValue => {
            s.discard_whitespace();
            if s.eof() {
                return json!(["error", "empty"]);
            }
            let v = s.component_value();
            s.discard_whitespace();
            if s.eof() {
                Out { s }.cv(&v)
            } else {
                json!(["error", "extra-input"])
            }
        }
        Entry::ComponentValues => {
            let v = s.component_values(None, false);
            Out { s }.cvs(&v)
        }
        Entry::CommaSeparated => {
            let mut groups = Vec::new();
            while !s.eof() {
                groups.push(s.component_values(Some(Token::Comma), false));
                s.discard();
            }
            let o = Out { s };
            Value::Array(groups.iter().map(|g| o.cvs(g)).collect())
        }
    }
}
