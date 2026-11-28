//! Style for the Optimal browser engine: CSS Cascading and Inheritance
//! Level 5 (W3C Candidate Recommendation Draft, csswg-drafts commit
//! 7027ab6), with css-nesting-1 and css-namespaces-3 at the same commit.
//!
//! - [`StyleSet`] collects the style rules of a document's style sheets,
//!   each with its [`Origin`]: selectors (nested rules included),
//!   declarations, cascade layers ([`Layers`]) and namespaces (see
//!   [`collect`]). [`StyleAttribute`] parses a `style` attribute.
//! - [`StyleSet::prepare`] gives a [`Cascade`], which sorts the
//!   declarations that apply to each element ([`cascade`]).
//! - [`Session::style`] resolves those into each property's specified
//!   value: inheritance, initial values and the CSS-wide keywords
//!   ([`defaulting`]), for the longhands of the property table
//!   ([`property`]) and custom properties.
//!
//! Known gaps, each for a later step: `@import` doesn't load sheets; the
//! conditional group rules (`@media`, `@supports`, `@container`) and
//! `@scope` are dropped; values are not parsed against each property's
//! grammar (only the CSS-wide keyword rules are checked), so no computed
//! values; arbitrary substitution functions (`var()` and the rest) are not
//! substituted; the property table has no logical properties, grid,
//! animation or SVG properties yet; no shadow trees (one encapsulation
//! context), no animations or transitions, no presentational hints, no
//! pseudo-element styles.
#![forbid(unsafe_code)]

pub mod cascade;
pub mod collect;
pub mod defaulting;
pub mod layer;
pub mod property;
pub mod values;

pub use cascade::{ATTACHED_BLOCK, Cascade, Cascaded, Entry, Level, Priority, Session, Source};
pub use collect::{
    Block, Decl, MAX_RULE_NESTING, Origin, Sheet, SheetId, StyleAttribute, StyleRule, StyleSet,
};
pub use defaulting::{Outcome, Style, Value, resolve};
pub use layer::{LayerId, Layers};
pub use property::{Classified, Keyword, PropertyValidity, Target};
