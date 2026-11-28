//! Selectors for the Optimal browser engine, written from Selectors Level 4
//! (W3C Working Draft, csswg-drafts commit 7027ab6), with the
//! pseudo-elements of css-pseudo-4, the shadow tree selectors of
//! css-shadow-1 and the nesting selector of css-nesting-1 at the same
//! commit, and cssom-1's serialization.
//!
//! - [`parse_selector`] and [`parse_relative_selector`] are the "parse a
//!   selector" and "parse a relative selector" hooks (17.1, 17.2), from
//!   text; [`parse_selector_list`] and [`parse_relative_selector_list`] take
//!   component values from the CSS parser, such as a style rule's prelude.
//! - The result is a [`SelectorList`] (see [`ast`] for its shape), which
//!   serializes with `Display` and gives each selector's
//!   [`Specificity`].
//!
//! - [`matches`] and [`query_all`] match a list against an element or a
//!   subtree, through the [`Element`] trait (see [`matching`]).
#![forbid(unsafe_code)]

pub mod ast;
mod lang;
pub mod matching;
mod parse;
mod serialize;
mod specificity;

pub use ast::*;
pub use parse::{
    MAX_NESTING, Namespaces, SelectorError, parse_relative_selector, parse_relative_selector_list,
    parse_selector, parse_selector_list,
};
pub use lang::{lang_matches, well_formed as language_tag_well_formed};
pub use matching::{Direction, Element, MatchContext, Scope, matches, query_all};
pub use specificity::Specificity;
