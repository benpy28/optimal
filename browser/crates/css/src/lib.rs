//! CSS for the Optimal browser engine, written from CSS Syntax Module Level 3
//! (W3C Candidate Recommendation Draft, csswg-drafts commit 7027ab6).
//!
//! This first stage is the tokenizer (css-syntax-3 section 4): it turns style
//! sheet text into the tokens the parser, selectors and property grammars are
//! built on.
//!
//! - [`Tokenizer`] tokenizes text that is all available at once (a `<style>`
//!   element's text, a `style` attribute, a fetched sheet). Token values
//!   borrow from the input whenever they need no unescaping.
//! - [`StreamTokenizer`] takes text in chunks as it arrives and gives exactly
//!   the same tokens, errors and offsets, however the text is split.
//!
//! Both run the same scanner, which applies the input preprocessing of
//! section 3.3 as it reads, so token offsets are offsets into the text as
//! given (before CR LF and form feeds are turned into LF).
#![forbid(unsafe_code)]

mod stream;
mod tokenizer;

pub use stream::{Next, StreamTokenizer};
pub use tokenizer::{
    ErrorKind, HashKind, Number, Sign, Spanned, Token, TokenError, Tokenizer, is_ident_code_point,
    is_ident_start_code_point, is_non_ascii_ident_code_point,
};

#[cfg(test)]
mod tests;
