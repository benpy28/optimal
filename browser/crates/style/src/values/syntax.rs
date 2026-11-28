//! The CSS value definition syntax (css-values-4 2): reading a grammar,
//! as the specs write it, into nodes the matcher ([`super::matcher`]) runs.
//!
//! A grammar is read into a shared arena ([`Grammar`]). Names it refers to
//! (`<foo>`, `<foo()>`, `<'property'>`) are recorded as unresolved
//! references and resolved once every production is read
//! ([`super::compile`]), so productions can refer to each other in any
//! order.
//!
//! Precedence, strongest first (2.2): juxtaposition, `&&`, `||`, `|`.
//! Multipliers (2.3) bind to the component before them, and can be stacked
//! as the spec allows (`+#`, `#?`, `{A}?`, `{A,B}?`): each applies to the
//! result of the one before. A function is written as its name followed
//! directly by `(`, and its arguments are implicitly grouped (2.6). A `(`
//! that doesn't follow a name is a `()`-block, as in `( <calc-sum> )`.

use super::types::{Bounds, Builtin};

/// An index into [`Grammar::nodes`].
pub type NodeId = u32;

/// One node of a grammar.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// A keyword: an identifier, matched ASCII case-insensitively (2.1).
    Keyword(&'static str),
    /// A delimiter other than a comma: `/`, `:`, `;`, or a quoted one such
    /// as `'+'`.
    Delim(char),
    /// A comma written in the grammar: implicitly omissible when the
    /// items on one side of it are all omitted (2.1, "Commas").
    Comma,
    /// A basic data type, with its bracketed range (2.1, 5.1).
    Type(Builtin, Bounds),
    /// A reference to a production or a property's grammar, by name, as
    /// read. Resolved into [`Node::Ref`] by the compiler.
    Name(&'static str, Bounds),
    /// A resolved reference: the node the name stands for, and the range
    /// written on the reference (applied to the basic data types the
    /// referenced production reaches directly; see the compiler).
    Ref(NodeId),
    /// A functional notation: its name (matched ASCII case-insensitively)
    /// and its arguments.
    Function(&'static str, NodeId),
    /// A `()`-block and what it holds.
    Block(NodeId),
    /// Juxtaposition: all, in order.
    Seq(Box<[NodeId]>),
    /// `&&`: all, in any order.
    All(Box<[NodeId]>),
    /// `||`: one or more, in any order.
    Any(Box<[NodeId]>),
    /// `|`: exactly one.
    One(Box<[NodeId]>),
    /// A multiplier: between `min` and `max` (unbounded if `None`)
    /// repetitions, separated by commas if `hash` (`#`).
    Repeat {
        /// What repeats.
        node: NodeId,
        /// The least number of repetitions.
        min: u32,
        /// The greatest, if bounded.
        max: Option<u32>,
        /// Whether repetitions are comma-separated (`#`).
        hash: bool,
    },
    /// `!`: the group must produce at least one component value.
    Required(NodeId),
    /// Matches nothing: a feature this engine doesn't support yet, so a
    /// value that would need it is invalid, as in a browser without it.
    Never,
}

/// An arena of grammar nodes.
#[derive(Clone, Debug, Default)]
pub struct Grammar {
    /// The nodes.
    pub nodes: Vec<Node>,
}

/// Why a grammar couldn't be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    /// The grammar text.
    pub grammar: &'static str,
    /// Byte offset of the problem.
    pub at: usize,
    /// What was wrong.
    pub what: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok {
    Word(&'static str),
    Function(&'static str),
    Angle(&'static str, Bounds),
    Delim(char),
    Comma,
    Open,
    Close,
    CloseBang,
    OpenParen,
    CloseParen,
    Bar,
    DoubleBar,
    DoubleAmp,
    Star,
    Plus,
    Question,
    Hash,
    Braces(u32, Option<u32>),
}

fn tokens(text: &'static str) -> Result<Vec<(usize, Tok)>, SyntaxError> {
    let err = |at, what| SyntaxError {
        grammar: text,
        at,
        what,
    };
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        let tok = match c {
            b' ' | b'\t' | b'\n' => {
                i += 1;
                continue;
            }
            b'[' => {
                i += 1;
                Tok::Open
            }
            b']' => {
                i += 1;
                if b.get(i) == Some(&b'!') {
                    i += 1;
                    Tok::CloseBang
                } else {
                    Tok::Close
                }
            }
            b'(' => {
                i += 1;
                Tok::OpenParen
            }
            b')' => {
                i += 1;
                Tok::CloseParen
            }
            b'|' if b.get(i + 1) == Some(&b'|') => {
                i += 2;
                Tok::DoubleBar
            }
            b'|' => {
                i += 1;
                Tok::Bar
            }
            b'&' if b.get(i + 1) == Some(&b'&') => {
                i += 2;
                Tok::DoubleAmp
            }
            b',' => {
                i += 1;
                Tok::Comma
            }
            b'/' | b':' | b';' => {
                i += 1;
                Tok::Delim(c as char)
            }
            b'*' => {
                i += 1;
                Tok::Star
            }
            b'+' => {
                i += 1;
                Tok::Plus
            }
            b'?' => {
                i += 1;
                Tok::Question
            }
            b'#' => {
                i += 1;
                Tok::Hash
            }
            b'\'' => {
                // A quoted delimiter: one character between quotes.
                let rest = &text[i + 1..];
                let ch = rest.chars().next().ok_or(err(i, "unterminated quote"))?;
                let len = ch.len_utf8();
                if rest.as_bytes().get(len) != Some(&b'\'') {
                    return Err(err(i, "a quoted delimiter is one character"));
                }
                i += len + 2;
                Tok::Delim(ch)
            }
            b'{' => {
                let end = text[i..].find('}').ok_or(err(i, "unterminated {"))? + i;
                let inner = &text[i + 1..end];
                let num = |s: &str| s.trim().parse::<u32>().map_err(|_| err(i, "bad count"));
                let (min, max) = match inner.split_once(',') {
                    None => {
                        let n = num(inner)?;
                        (n, Some(n))
                    }
                    Some((a, b)) if b.trim().is_empty() => (num(a)?, None),
                    Some((a, b)) => (num(a)?, Some(num(b)?)),
                };
                if max.is_some_and(|m| m < min) {
                    return Err(err(i, "a count range runs backwards"));
                }
                i = end + 1;
                Tok::Braces(min, max)
            }
            b'<' => {
                let end = text[i..].find('>').ok_or(err(i, "unterminated <"))? + i;
                let inner = &text[i + 1..end];
                i = end + 1;
                let (name, range) = match inner.split_once(' ') {
                    None => (inner, Bounds::ANY),
                    Some((n, r)) => (n, Bounds::parse(r.trim()).ok_or(err(start, "bad range"))?),
                };
                if name.is_empty() {
                    return Err(err(start, "empty <>"));
                }
                Tok::Angle(name, range)
            }
            c if c.is_ascii_alphabetic() || c == b'-' => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-') {
                    i += 1;
                }
                let word = &text[start..i];
                if b.get(i) == Some(&b'(') {
                    i += 1;
                    Tok::Function(word)
                } else {
                    Tok::Word(word)
                }
            }
            _ => return Err(err(i, "unexpected character")),
        };
        out.push((start, tok));
    }
    Ok(out)
}

struct Reader<'g> {
    text: &'static str,
    toks: Vec<(usize, Tok)>,
    at: usize,
    grammar: &'g mut Grammar,
}

impl Reader<'_> {
    fn peek(&self) -> Option<Tok> {
        self.toks.get(self.at).map(|t| t.1)
    }

    fn err(&self, what: &'static str) -> SyntaxError {
        SyntaxError {
            grammar: self.text,
            at: self.toks.get(self.at).map_or(self.text.len(), |t| t.0),
            what,
        }
    }

    fn push(&mut self, n: Node) -> NodeId {
        self.grammar.nodes.push(n);
        (self.grammar.nodes.len() - 1) as NodeId
    }

    /// `|`-separated alternatives.
    fn one(&mut self) -> Result<NodeId, SyntaxError> {
        let mut items = vec![self.any()?];
        while self.peek() == Some(Tok::Bar) {
            self.at += 1;
            items.push(self.any()?);
        }
        Ok(self.group(items, Node::One))
    }

    fn any(&mut self) -> Result<NodeId, SyntaxError> {
        let mut items = vec![self.all()?];
        while self.peek() == Some(Tok::DoubleBar) {
            self.at += 1;
            items.push(self.all()?);
        }
        Ok(self.group(items, Node::Any))
    }

    fn all(&mut self) -> Result<NodeId, SyntaxError> {
        let mut items = vec![self.seq()?];
        while self.peek() == Some(Tok::DoubleAmp) {
            self.at += 1;
            items.push(self.seq()?);
        }
        Ok(self.group(items, Node::All))
    }

    fn seq(&mut self) -> Result<NodeId, SyntaxError> {
        let mut items = Vec::new();
        while let Some(t) = self.peek() {
            if matches!(
                t,
                Tok::Bar
                    | Tok::DoubleBar
                    | Tok::DoubleAmp
                    | Tok::Close
                    | Tok::CloseBang
                    | Tok::CloseParen
            ) {
                break;
            }
            items.push(self.multiplied()?);
        }
        if items.is_empty() {
            return Err(self.err("expected a component"));
        }
        Ok(self.group(items, Node::Seq))
    }

    fn group(&mut self, mut items: Vec<NodeId>, make: fn(Box<[NodeId]>) -> Node) -> NodeId {
        if items.len() == 1 {
            items.pop().expect("one item")
        } else {
            self.push(make(items.into()))
        }
    }

    fn multiplied(&mut self) -> Result<NodeId, SyntaxError> {
        let mut node = self.component()?;
        let mut last: Option<Tok> = None;
        while let Some(t) = self.peek() {
            let (min, max, hash) = match t {
                Tok::Star => (0, None, false),
                Tok::Plus => (1, None, false),
                Tok::Question => (0, Some(1), false),
                Tok::Hash => (1, None, true),
                Tok::Braces(a, b) => {
                    // `{A,B}` after `#` sets the count of that `#` (2.3).
                    if last == Some(Tok::Hash) {
                        self.at += 1;
                        let Node::Repeat { node: inner, .. } = self.grammar.nodes[node as usize]
                        else {
                            unreachable!("a # makes a Repeat")
                        };
                        self.grammar.nodes[node as usize] = Node::Repeat {
                            node: inner,
                            min: a,
                            max: b,
                            hash: true,
                        };
                        last = Some(t);
                        continue;
                    }
                    (a, b, false)
                }
                _ => break,
            };
            // Only the stacks the spec lists: `+#`, `#?`, `{A}?`, `{A,B}?`.
            let allowed = matches!(
                (last, t),
                (None, _)
                    | (Some(Tok::Plus), Tok::Hash)
                    | (Some(Tok::Hash | Tok::Braces(..)), Tok::Question)
            );
            if !allowed {
                return Err(self.err("multipliers stacked in a way the spec doesn't allow"));
            }
            self.at += 1;
            node = self.push(Node::Repeat {
                node,
                min,
                max,
                hash,
            });
            last = Some(t);
        }
        Ok(node)
    }

    fn component(&mut self) -> Result<NodeId, SyntaxError> {
        let Some(t) = self.peek() else {
            return Err(self.err("expected a component"));
        };
        self.at += 1;
        Ok(match t {
            Tok::Word(w) => self.push(Node::Keyword(w)),
            Tok::Delim(c) => self.push(Node::Delim(c)),
            Tok::Comma => self.push(Node::Comma),
            Tok::Angle(name, range) => self.push(Node::Name(name, range)),
            Tok::Open => {
                let inner = self.one()?;
                match self.peek() {
                    Some(Tok::Close) => {
                        self.at += 1;
                        inner
                    }
                    Some(Tok::CloseBang) => {
                        self.at += 1;
                        self.push(Node::Required(inner))
                    }
                    _ => return Err(self.err("expected ]")),
                }
            }
            Tok::Function(name) => {
                // A function may have no arguments: `name()`.
                let args = if self.peek() == Some(Tok::CloseParen) {
                    self.push(Node::Seq(Box::new([])))
                } else {
                    self.one()?
                };
                if self.peek() != Some(Tok::CloseParen) {
                    return Err(self.err("expected )"));
                }
                self.at += 1;
                self.push(Node::Function(name, args))
            }
            Tok::OpenParen => {
                let inner = self.one()?;
                if self.peek() != Some(Tok::CloseParen) {
                    return Err(self.err("expected )"));
                }
                self.at += 1;
                self.push(Node::Block(inner))
            }
            _ => {
                self.at -= 1;
                return Err(self.err("expected a component"));
            }
        })
    }
}

impl Grammar {
    /// Reads `text` into the arena and returns its top node. References
    /// stay unresolved ([`Node::Name`]).
    pub fn read(&mut self, text: &'static str) -> Result<NodeId, SyntaxError> {
        let toks = tokens(text)?;
        let mut r = Reader {
            text,
            toks,
            at: 0,
            grammar: self,
        };
        let top = r.one()?;
        if r.at != r.toks.len() {
            return Err(r.err("unexpected text after the grammar"));
        }
        Ok(top)
    }
}
