//! Serialization (cssom-1 section 5.2, "Serializing Selectors", with its
//! "serialize an identifier" and "serialize a string" idioms of 2.1).
//!
//! cssom-1 only spells out the arguments of `:lang()`, the `:nth-*()`
//! pseudo-classes and `:not()`. The other functional pseudo-classes and
//! pseudo-elements serialize their arguments the same way: selector
//! arguments as a comma-separated list of selectors, identifiers as
//! identifiers. Forgiving lists give their invalid items back as written
//! (see [`crate::Forgiving`]).

use crate::ast::*;
use core::fmt::{self, Display, Formatter, Write};

/// "Escape a character as code point".
fn code_point(f: &mut Formatter<'_>, c: char) -> fmt::Result {
    write!(f, "\\{:x} ", c as u32)
}

/// "Serialize an identifier".
pub(crate) fn ident(f: &mut Formatter<'_>, s: &str) -> fmt::Result {
    let mut chars = s.chars();
    let first = chars.next();
    let second = chars.next();
    for (i, c) in s.chars().enumerate() {
        match c {
            '\0' => f.write_char('\u{FFFD}')?,
            '\u{1}'..='\u{1f}' | '\u{7f}' => code_point(f, c)?,
            '0'..='9' if i == 0 => code_point(f, c)?,
            '0'..='9' if i == 1 && first == Some('-') => code_point(f, c)?,
            '-' if i == 0 && second.is_none() => {
                f.write_char('\\')?;
                f.write_char(c)?;
            }
            c if optimal_css::is_non_ascii_ident_code_point(c)
                || c == '-'
                || c == '_'
                || c.is_ascii_alphanumeric() =>
            {
                f.write_char(c)?
            }
            c => {
                f.write_char('\\')?;
                f.write_char(c)?;
            }
        }
    }
    Ok(())
}

/// "Serialize a string".
pub(crate) fn string(f: &mut Formatter<'_>, s: &str) -> fmt::Result {
    f.write_char('"')?;
    for c in s.chars() {
        match c {
            '\0' => f.write_char('\u{FFFD}')?,
            '\u{1}'..='\u{1f}' | '\u{7f}' => code_point(f, c)?,
            '"' | '\\' => {
                f.write_char('\\')?;
                f.write_char(c)?;
            }
            c => f.write_char(c)?,
        }
    }
    f.write_char('"')
}

/// Items joined by ", " ("serialize a comma-separated list").
fn comma_list<T: Display>(f: &mut Formatter<'_>, items: &[T]) -> fmt::Result {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        item.fmt(f)?;
    }
    Ok(())
}

/// "Serialize a group of selectors".
impl Display for SelectorList {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        comma_list(f, &self.0)
    }
}

/// "Serialize a selector".
impl Display for Selector {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.first.fmt(f)?;
        for (combinator, unit) in &self.rest {
            combinator.fmt(f)?;
            unit.fmt(f)?;
        }
        Ok(())
    }
}

impl Display for RelativeSelector {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.explicit {
            // The combinator with its spaces, minus the leading one.
            let c = self.combinator.to_string();
            f.write_str(&c[1..])?;
        }
        self.selector.fmt(f)
    }
}

/// A combinator between two units: one space, the combinator, and one more
/// space unless the combinator is whitespace.
impl Display for Combinator {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Combinator::Descendant => " ",
            Combinator::Child => " > ",
            Combinator::NextSibling => " + ",
            Combinator::SubsequentSibling => " ~ ",
        })
    }
}

impl Display for Unit {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        // Steps 1 and 2: a universal selector is written out only when it is
        // the compound's only simple selector, or when it needs a prefix to
        // keep its namespace.
        let alone = self.compound.len() == 1 && self.pseudos.is_empty();
        for simple in &self.compound {
            if let Simple::Universal { prefix: None, .. } = simple
                && !alone
            {
                continue;
            }
            simple.fmt(f)?;
        }
        for pseudo in &self.pseudos {
            pseudo.element.fmt(f)?;
            for class in &pseudo.classes {
                class.fmt(f)?;
            }
        }
        Ok(())
    }
}

/// "Serialize a simple selector".
impl Display for Simple {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Simple::Type { prefix, local, .. } => {
                if let Some(p) = prefix {
                    namespace_prefix(f, p)?;
                }
                ident(f, local)
            }
            Simple::Universal { prefix, .. } => {
                if let Some(p) = prefix {
                    namespace_prefix(f, p)?;
                }
                f.write_char('*')
            }
            Simple::Id(id) => {
                f.write_char('#')?;
                ident(f, id)
            }
            Simple::Class(class) => {
                f.write_char('.')?;
                ident(f, class)
            }
            Simple::Attribute(a) => a.fmt(f),
            Simple::PseudoClass(pc) => pc.fmt(f),
            Simple::Nesting => f.write_char('&'),
        }
    }
}

/// A namespace prefix and its `|`: `*` and the empty prefix as they are,
/// names as identifiers.
fn namespace_prefix(f: &mut Formatter<'_>, p: &str) -> fmt::Result {
    if p != "*" {
        ident(f, p)?;
    } else {
        f.write_char('*')?;
    }
    f.write_char('|')
}

impl Display for Attribute {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_char('[')?;
        if let Some(p) = &self.prefix {
            namespace_prefix(f, p)?;
        }
        ident(f, &self.local)?;
        if let Some((matcher, value)) = &self.test {
            f.write_str(match matcher {
                AttrMatcher::Equals => "=",
                AttrMatcher::Includes => "~=",
                AttrMatcher::DashMatch => "|=",
                AttrMatcher::Prefix => "^=",
                AttrMatcher::Suffix => "$=",
                AttrMatcher::Substring => "*=",
            })?;
            string(f, value)?;
        }
        match self.case {
            Some(AttrCase::Insensitive) => f.write_str(" i")?,
            Some(AttrCase::Sensitive) => f.write_str(" s")?,
            None => {}
        }
        f.write_char(']')
    }
}

impl Display for Forgiving {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        comma_list(f, &self.0)
    }
}

impl Display for ForgivingItem {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ForgivingItem::Valid(sel) => sel.fmt(f),
            ForgivingItem::Invalid(text) => f.write_str(text),
        }
    }
}

fn compound(f: &mut Formatter<'_>, simples: &[Simple]) -> fmt::Result {
    Unit {
        compound: simples.to_vec(),
        pseudos: Vec::new(),
    }
    .fmt(f)
}

impl Display for PseudoClass {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_char(':')?;
        let nth = |f: &mut Formatter<'_>,
                   name: &str,
                   anb: &optimal_css::AnB,
                   of: Option<&SelectorList>| {
            write!(f, "{name}({anb}")?;
            if let Some(list) = of {
                write!(f, " of {list}")?;
            }
            f.write_char(')')
        };
        match self {
            PseudoClass::Keyword(k) => f.write_str(k.name()),
            PseudoClass::Is(list) => write!(f, "is({list})"),
            PseudoClass::Where(list) => write!(f, "where({list})"),
            PseudoClass::Not(list) => write!(f, "not({list})"),
            PseudoClass::Has(list) => {
                f.write_str("has(")?;
                comma_list(f, list)?;
                f.write_char(')')
            }
            PseudoClass::NthChild(anb, of) => nth(f, "nth-child", anb, of.as_ref()),
            PseudoClass::NthLastChild(anb, of) => nth(f, "nth-last-child", anb, of.as_ref()),
            PseudoClass::NthOfType(anb) => nth(f, "nth-of-type", anb, None),
            PseudoClass::NthLastOfType(anb) => nth(f, "nth-last-of-type", anb, None),
            PseudoClass::Lang(ranges) => {
                f.write_str("lang(")?;
                for (i, r) in ranges.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    string(f, r)?;
                }
                f.write_char(')')
            }
            PseudoClass::Dir(d) => {
                f.write_str("dir(")?;
                ident(f, d)?;
                f.write_char(')')
            }
            PseudoClass::State(s) => {
                f.write_str("state(")?;
                ident(f, s)?;
                f.write_char(')')
            }
            PseudoClass::Host(c) => {
                f.write_str("host(")?;
                compound(f, c)?;
                f.write_char(')')
            }
            PseudoClass::HostContext(c) => {
                f.write_str("host-context(")?;
                compound(f, c)?;
                f.write_char(')')
            }
        }
    }
}

impl Display for PseudoElement {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("::")?;
        match self {
            PseudoElement::Keyword(k) => f.write_str(k.name()),
            PseudoElement::UnknownWebkit(name) => ident(f, name),
            PseudoElement::Highlight(None) => f.write_str("highlight(*)"),
            PseudoElement::Highlight(Some(name)) => {
                f.write_str("highlight(")?;
                ident(f, name)?;
                f.write_char(')')
            }
            PseudoElement::Part(names) => {
                f.write_str("part(")?;
                for (i, n) in names.iter().enumerate() {
                    if i > 0 {
                        f.write_char(' ')?;
                    }
                    ident(f, n)?;
                }
                f.write_char(')')
            }
            PseudoElement::Slotted(c) => {
                f.write_str("slotted(")?;
                compound(f, c)?;
                f.write_char(')')
            }
        }
    }
}
