//! Defaulting (css-cascade-5 #defaulting): from the cascade's sorted
//! declarations to each property's specified value.
//!
//! For each longhand in the table and each custom property:
//!
//! - if the cascade gives no value, an inherited property takes its
//!   parent's value (the root, its initial value) and any other property
//!   its initial value (#initial-values, #inheriting);
//! - `initial` gives the initial value; `inherit` the parent's value;
//!   `unset` acts as `inherit` for inherited properties and `initial` for
//!   the rest (#defaulting-keywords);
//! - `revert`, `revert-layer` and `revert-rule` roll the cascaded value
//!   back, and the declaration rolled back to is read in turn, so a chain
//!   of them works.
//!
//! Rolling back works on the list [`crate::Cascaded`] gives for the
//! property, from the winner down, as if some declarations were not
//! there:
//!
//! - `revert` in the author origin: as if no author declarations were
//!   there (it rolls back to the user origin); in the user origin, as if no
//!   author or user declarations were there; in the user-agent origin it
//!   is `unset` (#default);
//! - `revert-layer` in a normal declaration: as if its layer's normal
//!   declarations of its origin were not there (for a style attribute, the
//!   element-attached normal declarations); in an important one, as if
//!   nothing between its layer's normal and important levels were there
//!   (#revert-layer);
//! - `revert-layer` in an important element-attached declaration: as if
//!   no element-attached declarations were there, and no further (the
//!   spec's exception: "it only reverts the element-attached styles ...
//!   and not any of the intervening author-origin important rules");
//! - `revert-rule`: as if its style rule (or nested declarations rule)
//!   were not there; a style attribute is one rule (#revert-rule-keyword).
//!
//! Design choices (chosen, why, rejected):
//!
//! - **What is rolled back stays rolled back.** Each roll-back adds to the
//!   declarations treated as absent; none comes back. So the walk only
//!   moves down the list: origins and layers are contiguous runs of it
//!   (cut off below a floor [`Priority`]), and rules are a set of block
//!   ids. That gives the WPT results ("a chain of revert-rule works") and
//!   always ends. Rejected: rolling back each step from the full list,
//!   which loops between `revert-rule` and `revert-layer` (WPT's
//!   `revert-rule-cycle.tentative.html` asks for `unset` there; the spec
//!   text doesn't say so, see conformance/README.md).
//! - **Inheritance copies the parent's specified values** ([`Value`] with
//!   the number of levels up it came from), because computed values don't
//!   exist yet; for declarations without CSS-wide keywords, the computed
//!   value is a function of the specified value and the element, and
//!   value computation will replace these with computed values.
//! - **Arbitrary substitution functions are not resolved yet**, so a
//!   `var()` that would substitute a CSS-wide keyword isn't treated as
//!   one (css-values-5 #substitution: it is, after substitution).

use optimal_selectors::Element;

use crate::cascade::{Entry, Level, Priority, Session, Source};
use crate::collect::StyleAttribute;
use crate::property::{Keyword, LONGHANDS, longhand};
use optimal_selectors::Specificity;

/// Where a property's specified value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    /// The property's initial value (for a custom property, the
    /// guaranteed-invalid value; [`Style::custom`] leaves those out).
    Initial,
    /// A declaration's value (never a CSS-wide keyword), declared on the
    /// element `up` levels up: 0 for the element itself, 1 for its parent,
    /// and so on.
    Declared {
        /// The declaration.
        source: Source,
        /// How many levels up the element it is declared on is.
        up: u32,
    },
}

impl Value {
    fn inherited(self) -> Value {
        match self {
            Value::Initial => Value::Initial,
            Value::Declared { source, up } => Value::Declared { source, up: up + 1 },
        }
    }
}

/// An element's specified values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Style<'n> {
    /// Each longhand's, by [`LonghandId`].
    pub longhands: Box<[Value]>,
    /// The custom properties that have a value, sorted by name. Each is a
    /// [`Value::Declared`].
    pub custom: Vec<(&'n str, Value)>,
}

impl<'n> Style<'n> {
    /// The root element's parent's style, as far as inheritance sees it:
    /// initial values, no custom properties.
    pub fn initial() -> Style<'n> {
        Style {
            longhands: vec![Value::Initial; LONGHANDS.len()].into(),
            custom: Vec::new(),
        }
    }

    /// A longhand's value by name, `None` if it isn't in the table.
    pub fn get(&self, name: &str) -> Option<Value> {
        longhand(name).map(|l| self.longhands[l as usize])
    }

    /// A custom property's value, `None` if it is the guaranteed-invalid
    /// value.
    pub fn custom(&self, name: &str) -> Option<Value> {
        self.custom
            .binary_search_by(|(n, _)| (*n).cmp(name))
            .ok()
            .map(|i| self.custom[i].1)
    }
}

/// What a property's list of declarations resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A declaration's value.
    Declared(Entry),
    /// The initial value.
    Initial,
    /// The inherited value.
    Inherit,
    /// Inherited or initial as the property is: `unset`, or no declaration
    /// left after rolling back.
    Unset,
}

/// The lowest priority of a level: every declaration of that level is at
/// least this.
fn lowest(level: Level, layer: u32) -> Priority {
    Priority {
        level,
        attached: false,
        layer,
        specificity: Specificity::ZERO,
        order: 0,
    }
}

/// The normal level of an important level's origin.
fn normal(level: Level) -> Level {
    match level {
        Level::ImportantAuthor => Level::Author,
        Level::ImportantUser => Level::User,
        Level::ImportantUserAgent => Level::UserAgent,
        l => l,
    }
}

/// Resolves a property's declarations, from the winner down (as
/// [`crate::Cascaded::get`] gives them), to what its value comes from.
pub fn resolve(entries: &[Entry]) -> Outcome {
    // Only declarations below the floor are still there.
    let mut floor: Option<Priority> = None;
    // The rules rolled back by revert-rule.
    let mut gone: Vec<u32> = Vec::new();
    for e in entries {
        let p = e.priority;
        if floor.is_some_and(|f| p >= f) || gone.contains(&e.block) {
            continue;
        }
        let Some(k) = e.keyword else {
            return Outcome::Declared(*e);
        };
        match k {
            Keyword::Initial => return Outcome::Initial,
            Keyword::Inherit => return Outcome::Inherit,
            Keyword::Unset => return Outcome::Unset,
            Keyword::Revert => match normal(p.level) {
                Level::UserAgent => return Outcome::Unset,
                l => floor = Some(lowest(l, 0)),
            },
            // Element-attached declarations are one layer, and for an
            // important one only they are rolled back: what is left of
            // them is the normal ones, below every important rule.
            Keyword::RevertLayer if p.attached => gone.push(e.block),
            Keyword::RevertLayer => {
                floor = Some(if p.level == normal(p.level) {
                    lowest(p.level, p.layer)
                } else {
                    // The important layer key counts down from the end.
                    lowest(normal(p.level), u32::MAX - p.layer)
                });
            }
            Keyword::RevertRule => gone.push(e.block),
        }
    }
    Outcome::Unset
}

impl<'c, 's: 'c, 'a, E: Element> Session<'c, 's, 'a, E> {
    /// The specified values of `element`, given its `style` attribute and
    /// its parent's style ([`Style::initial`] for the root element).
    pub fn style<'n>(
        &mut self,
        element: &E,
        style: Option<&'n StyleAttribute<'_>>,
        parent: &Style<'n>,
    ) -> Style<'n>
    where
        'c: 'n,
    {
        let cascaded = self.cascade(element, style);
        let mut longhands: Box<[Value]> = LONGHANDS
            .iter()
            .zip(parent.longhands.iter())
            .map(|(l, &v)| if l.inherited { v.inherited() } else { Value::Initial })
            .collect();
        let mut custom: Vec<(&'n str, Value)> = Vec::new();
        let mut inherited = parent.custom.iter().peekable();
        for &(name, ref entries) in &cascaded.properties {
            let outcome = resolve(entries);
            if name.starts_with("--") {
                while let Some(&&(n, v)) = inherited.peek()
                    && n < name
                {
                    custom.push((n, v.inherited()));
                    inherited.next();
                }
                let parent_value = match inherited.peek() {
                    Some(&&(n, v)) if n == name => {
                        inherited.next();
                        Some(v.inherited())
                    }
                    _ => None,
                };
                // Custom properties are inherited; their initial value is
                // the guaranteed-invalid value.
                let v = match outcome {
                    Outcome::Declared(e) => Some(Value::Declared {
                        source: e.source,
                        up: 0,
                    }),
                    Outcome::Initial => None,
                    Outcome::Inherit | Outcome::Unset => parent_value,
                };
                if let Some(v) = v {
                    custom.push((name, v));
                }
                continue;
            }
            let id = longhand(name).expect("the cascade lists longhands by name") as usize;
            let inherit = || parent.longhands[id].inherited();
            longhands[id] = match outcome {
                Outcome::Declared(e) => Value::Declared {
                    source: e.source,
                    up: 0,
                },
                Outcome::Initial => Value::Initial,
                Outcome::Inherit => inherit(),
                Outcome::Unset if LONGHANDS[id].inherited => inherit(),
                Outcome::Unset => Value::Initial,
            };
        }
        custom.extend(inherited.map(|&(n, v)| (n, v.inherited())));
        Style { longhands, custom }
    }
}
