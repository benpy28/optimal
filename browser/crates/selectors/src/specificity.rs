//! Specificity (selectors-4 section 15).

use crate::ast::*;

/// A specificity (A, B, C): ID selectors; class, attribute and pseudo-class
/// selectors; type selectors and pseudo-elements. Ordered by A, then B,
/// then C. Each count saturates at `u32::MAX` instead of overflowing
/// (section 15 allows clamping).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Specificity {
    /// ID selectors.
    pub a: u32,
    /// Class selectors, attribute selectors and pseudo-classes.
    pub b: u32,
    /// Type selectors and pseudo-elements.
    pub c: u32,
}

impl Specificity {
    /// Zero.
    pub const ZERO: Specificity = Specificity { a: 0, b: 0, c: 0 };
    const ID: Specificity = Specificity { a: 1, b: 0, c: 0 };
    const CLASS: Specificity = Specificity { a: 0, b: 1, c: 0 };
    const TYPE: Specificity = Specificity { a: 0, b: 0, c: 1 };

    fn add(self, o: Specificity) -> Specificity {
        Specificity {
            a: self.a.saturating_add(o.a),
            b: self.b.saturating_add(o.b),
            c: self.c.saturating_add(o.c),
        }
    }
}

/// The largest specificity among `items` (zero for none): what `:is()`,
/// `:not()`, `:has()` and the `of S` of `:nth-child()` contribute.
fn max_of<'s>(items: impl Iterator<Item = &'s Selector>, nesting: Specificity) -> Specificity {
    items
        .map(|s| s.specificity(nesting))
        .max()
        .unwrap_or(Specificity::ZERO)
}

impl Selector {
    /// The selector's specificity. `nesting` is what each `&` counts for:
    /// the largest specificity in the parent style rule's selector list, or
    /// zero where there is none (css-nesting-1, "Nesting Selector").
    pub fn specificity(&self, nesting: Specificity) -> Specificity {
        std::iter::once(&self.first)
            .chain(self.rest.iter().map(|(_, u)| u))
            .fold(Specificity::ZERO, |acc, u| acc.add(u.specificity(nesting)))
    }
}

impl RelativeSelector {
    /// The specificity of the selector after the leading combinator.
    pub fn specificity(&self, nesting: Specificity) -> Specificity {
        self.selector.specificity(nesting)
    }
}

impl Unit {
    fn specificity(&self, nesting: Specificity) -> Specificity {
        let mut s = compound(&self.compound, nesting);
        for p in &self.pseudos {
            s = s.add(p.element.specificity(nesting));
            for c in &p.classes {
                s = s.add(c.specificity(nesting));
            }
        }
        s
    }
}

fn compound(simples: &[Simple], nesting: Specificity) -> Specificity {
    simples
        .iter()
        .fold(Specificity::ZERO, |acc, s| acc.add(s.specificity(nesting)))
}

impl Simple {
    fn specificity(&self, nesting: Specificity) -> Specificity {
        match self {
            Simple::Type { .. } => Specificity::TYPE,
            Simple::Universal { .. } => Specificity::ZERO,
            Simple::Id(_) => Specificity::ID,
            Simple::Class(_) | Simple::Attribute(_) => Specificity::CLASS,
            Simple::PseudoClass(pc) => pc.specificity(nesting),
            Simple::Nesting => nesting,
        }
    }
}

impl PseudoClass {
    fn specificity(&self, nesting: Specificity) -> Specificity {
        let valid = |f: &Forgiving| {
            max_of(
                f.0.iter().filter_map(|i| match i {
                    ForgivingItem::Valid(s) => Some(s),
                    // Invalid items count as zero (16.1).
                    ForgivingItem::Invalid(_) => None,
                }),
                nesting,
            )
        };
        match self {
            PseudoClass::Is(f) => valid(f),
            PseudoClass::Where(_) => Specificity::ZERO,
            PseudoClass::Not(list) => max_of(list.0.iter(), nesting),
            PseudoClass::Has(list) => max_of(list.iter().map(|r| &r.selector), nesting),
            PseudoClass::NthChild(_, of) | PseudoClass::NthLastChild(_, of) => Specificity::CLASS
                .add(
                    of.as_ref()
                        .map_or(Specificity::ZERO, |l| max_of(l.0.iter(), nesting)),
                ),
            // :host() and :host-context(): a pseudo-class plus the argument
            // (css-shadow-1).
            PseudoClass::Host(c) | PseudoClass::HostContext(c) => {
                Specificity::CLASS.add(compound(c, nesting))
            }
            _ => Specificity::CLASS,
        }
    }
}

impl PseudoElement {
    fn specificity(&self, nesting: Specificity) -> Specificity {
        match self {
            // ::slotted(): a pseudo-element plus the argument (css-shadow-1).
            PseudoElement::Slotted(c) => Specificity::TYPE.add(compound(c, nesting)),
            // ::highlight(*) is zero; with a name, like a type selector
            // (css-pseudo-4 3.1).
            PseudoElement::Highlight(None) => Specificity::ZERO,
            _ => Specificity::TYPE,
        }
    }
}
