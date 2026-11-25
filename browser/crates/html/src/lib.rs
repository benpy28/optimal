//! The Optimal browser engine's HTML parser, written from the WHATWG HTML
//! standard ("13.2 Parsing HTML documents").
//!
//! Status: the tokenizer is complete and checked against html5lib-tests. The
//! tree builder comes next.

#![forbid(unsafe_code)]

pub mod entities;
pub mod error;
mod input;
pub mod tokenizer;

pub use error::{ParseError, ParseErrorCode, Position};
pub use optimal_wtf8::{Wtf8, Wtf8Buf};
pub use tokenizer::{
    Attribute, Doctype, InitialState, RunResult, SinkResult, Tag, TagKind, Token, TokenSink,
    Tokenizer,
};
