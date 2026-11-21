//! The Optimal browser engine's HTML parser, written from the WHATWG HTML
//! standard ("13.2 Parsing HTML documents").
//!
//! Status: the tokenizer passes every html5lib-tests tokenizer test, and the
//! tree builder ([`tree_builder`]) every runnable WPT tree construction test,
//! under any chunking of the input. Parsing stays linear on hostile input.

#![forbid(unsafe_code)]

pub mod entities;
pub mod error;
mod input;
mod open_stack;
pub mod tokenizer;
pub mod tree_builder;

pub use error::{ParseError, ParseErrorCode, Position};
pub use optimal_wtf8::{Wtf8, Wtf8Buf};
pub use tokenizer::{
    Attribute, Doctype, InitialState, RunResult, SinkResult, Tag, TagKind, Token, TokenSink,
    Tokenizer,
};
