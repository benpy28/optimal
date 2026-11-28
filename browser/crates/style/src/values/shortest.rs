//! Serialization rules of particular properties and productions: the
//! "shorter representation" cssom-1 "serialize a CSS value" asks for ("if component values
//! can be omitted or replaced with a shorter representation without
//! changing the meaning of the value, omit/replace them"), where a spec
//! says what is equivalent, and the rules specs give for their own
//! values. Each rule cites its source.
//!
//! A rule is a function from a matched value to its serialized component
//! values, attached to the top node of a production or a property's
//! grammar; [`super::specified`] runs it in place of the generic walk.

use std::collections::HashMap;

use super::Compiled;
use super::specified::{SpecKind, Specified, items};
use super::syntax::NodeId;

/// A serialization rule: the component values a matched value
/// serializes as.
pub type Rule = fn(&Specified, &Compiled) -> Vec<String>;

/// The rules, by production name or `'property'` name.
const RULES: &[(&str, Rule)] = &[
    ("'display'", display),
    ("baseline-position", baseline_position),
    ("opacity-value", opacity_value),
    ("position", position),
    ("bg-position", position),
    ("bg-size", bg_size),
    ("repeat-style", repeat_style),
    ("radial-gradient-syntax", radial_gradient),
    ("linear-gradient-syntax", linear_gradient),
    ("font-family-name", font_family_name),
    ("feature-tag-value", feature_tag_value),
    ("'font-language-override'", font_language_override),
    ("symbols()", symbols),
    ("image-set-option", image_set_option),
    ("url-set", image_set_option),
];

/// The rules of a compiled grammar, by the top node they apply to.
/// `property` gives the top node of a property's grammar by name.
pub fn rules(
    productions: &HashMap<&'static str, NodeId>,
    property: impl Fn(&str) -> Option<NodeId>,
) -> HashMap<NodeId, Rule> {
    let mut out = HashMap::new();
    for &(name, rule) in RULES {
        let top = match name.strip_prefix('\'').and_then(|n| n.strip_suffix('\'')) {
            Some(prop) => property(prop),
            None => productions.get(name).copied(),
        };
        if let Some(top) = top {
            out.insert(top, rule);
        }
    }
    out
}

/// The keyword a value is, if it is one (through references and
/// alternatives).
fn keyword(s: &Specified) -> Option<&'static str> {
    match &s.kind {
        SpecKind::Keyword(k) => Some(k),
        SpecKind::Inner(inner) | SpecKind::Alternative(_, inner) => keyword(inner),
        SpecKind::List(c) if c.len() == 1 => keyword(&c[0].1),
        _ => None,
    }
}

/// Every keyword in a value, in order (functions excluded).
fn keywords(s: &Specified, out: &mut Vec<&'static str>) {
    match &s.kind {
        SpecKind::Keyword(k) => out.push(k),
        SpecKind::Inner(inner) | SpecKind::Alternative(_, inner) => keywords(inner, out),
        SpecKind::List(c) => c.iter().for_each(|(_, c)| keywords(c, out)),
        _ => {}
    }
}

/// The leaves of a value (component values, commas excluded), in order.
fn leaves<'s>(s: &'s Specified, out: &mut Vec<&'s Specified>) {
    match &s.kind {
        SpecKind::Inner(inner) | SpecKind::Alternative(_, inner) => leaves(inner, out),
        SpecKind::List(c) => c.iter().for_each(|(_, c)| leaves(c, out)),
        SpecKind::Comma => {}
        _ => out.push(s),
    }
}

/// css-display-4 #the-display-properties: "serialization of equivalent
/// 'display' values uses the “Short 'display'” column" of the table
/// there. A missing outer type is `block` (`inline` for `ruby`), a
/// missing inner type `flow` (#outer-role, #inner-model); for list items
/// the same defaults (#list-items), as the table's `list-item` and
/// `inline list-item` rows show.
fn display(s: &Specified, g: &Compiled) -> Vec<String> {
    let mut kw = Vec::new();
    keywords(s, &mut kw);
    let has = |k: &str| kw.contains(&k);
    let outside = ["block", "inline", "run-in"].into_iter().find(|k| has(k));
    let inside = ["flow", "flow-root", "table", "flex", "grid", "ruby"]
        .into_iter()
        .find(|k| has(k));
    let list_item = has("list-item");
    if outside.is_none() && inside.is_none() && !list_item {
        // <display-internal>, <display-box>, <display-legacy>.
        return items(s, g);
    }
    let inside = inside.unwrap_or("flow");
    let outside = outside.unwrap_or(if inside == "ruby" { "inline" } else { "block" });
    let words: Vec<&str> = if list_item {
        let mut w = Vec::new();
        if outside != "block" {
            w.push(outside);
        }
        if inside != "flow" {
            w.push(inside);
        }
        w.push("list-item");
        w
    } else {
        match (outside, inside) {
            ("block", "flow") => vec!["block"],
            ("block", "flow-root") => vec!["flow-root"],
            ("inline", "flow") => vec!["inline"],
            ("inline", "flow-root") => vec!["inline-block"],
            ("run-in", "flow") => vec!["run-in"],
            ("block", "flex") => vec!["flex"],
            ("inline", "flex") => vec!["inline-flex"],
            ("block", "grid") => vec!["grid"],
            ("inline", "grid") => vec!["inline-grid"],
            ("inline", "ruby") => vec!["ruby"],
            ("block", "table") => vec!["table"],
            ("inline", "table") => vec!["inline-table"],
            (o, i) => vec![o, i],
        }
    };
    words.into_iter().map(str::to_string).collect()
}

/// css-align-3 #baseline-values: `baseline` "computes to first
/// baseline", and "because they are equivalent, and baseline is shorter,
/// the CSSOM serializes first baseline as baseline".
fn baseline_position(s: &Specified, _: &Compiled) -> Vec<String> {
    let mut kw = Vec::new();
    keywords(s, &mut kw);
    if kw.contains(&"last") {
        vec!["last".to_string(), "baseline".to_string()]
    } else {
        vec!["baseline".to_string()]
    }
}

/// css-color-4 #serializing-opacity-values: "If the specified value for an opacity value matches
/// a literal `<percentage-token>` (i.e. does not use calc()) it should be
/// serialized as the equivalent `<number>` (0% maps to 0, 100% maps to
/// 1) value."
fn opacity_value(s: &Specified, g: &Compiled) -> Vec<String> {
    let mut l = Vec::new();
    leaves(s, &mut l);
    match l.as_slice() {
        [one] => match one.kind {
            SpecKind::Percentage(p) => vec![super::serialize::number(p / 100.0)],
            _ => items(s, g),
        },
        _ => items(s, g),
    }
}

/// css-values-4 #position-serialization (and css-backgrounds-3
/// #bg-position-serialization for `<bg-position>`): with one component,
/// "the implied center keyword is added, and a 2-component value is
/// serialized"; otherwise components serialize
/// horizontal first, which the canonical order of the grammar's `&&`
/// already gives.
fn position(s: &Specified, g: &Compiled) -> Vec<String> {
    let mut l = Vec::new();
    leaves(s, &mut l);
    let out = items(s, g);
    if l.len() != 1 {
        return out;
    }
    match keyword(l[0]) {
        Some("top" | "bottom") => vec!["center".to_string(), out[0].clone()],
        _ => vec![out[0].clone(), "center".to_string()],
    }
}

/// css-backgrounds-3 #bg-size-serialization: "always serialize as two values, even when
/// the second value is auto".
fn bg_size(s: &Specified, g: &Compiled) -> Vec<String> {
    let mut out = items(s, g);
    if out.len() == 1 && out[0] != "cover" && out[0] != "contain" {
        out.push("auto".to_string());
    }
    out
}

/// css-backgrounds-3 #background-repeat: `repeat-x` "computes to repeat
/// no-repeat", `repeat-y` to `no-repeat repeat`, and each of `repeat`,
/// `space`, `round` and `no-repeat` alone computes to itself twice, so
/// those pairs have the shorter forms.
fn repeat_style(s: &Specified, g: &Compiled) -> Vec<String> {
    let out = items(s, g);
    match out.as_slice() {
        [a, b] if a == "repeat" && b == "no-repeat" => vec!["repeat-x".to_string()],
        [a, b] if a == "no-repeat" && b == "repeat" => vec!["repeat-y".to_string()],
        [a, b] if a == b => vec![a.clone()],
        _ => out,
    }
}

/// A condition on a list's other children.
type Others<'a> = &'a dyn Fn(&[(usize, Specified)]) -> bool;

/// Removes, from every list in `s`, the children that are the keyword
/// `k`, when `when` holds for the list's other children.
fn drop_keyword(s: &mut Specified, k: &str, when: Others<'_>) {
    match &mut s.kind {
        SpecKind::Inner(inner) | SpecKind::Alternative(_, inner) | SpecKind::Nested(_, inner) => {
            drop_keyword(inner, k, when)
        }
        SpecKind::List(children) => {
            if let Some(i) = children.iter().position(|(_, c)| keyword(c) == Some(k)) {
                let others: Vec<(usize, Specified)> = children
                    .iter()
                    .enumerate()
                    .filter(|&(j, _)| j != i)
                    .map(|(_, c)| c.clone())
                    .collect();
                if when(&others) {
                    children.remove(i);
                }
            }
            for (_, c) in children.iter_mut() {
                drop_keyword(c, k, when);
            }
        }
        _ => {}
    }
}

/// Whether a value is a length (one component that isn't a keyword).
fn is_length(s: &Specified) -> bool {
    let mut l = Vec::new();
    leaves(s, &mut l);
    l.len() == 1 && keyword(l[0]).is_none()
}

/// css-images-3 #radial-gradient-syntax and #serialization ("omitting components when possible without
/// changing the meaning"): the shape is `ellipse` "if omitted", unless
/// the size is a single length, which makes it a circle ("If
/// `<radial-shape>` is omitted, the ending shape defaults to a circle if
/// the `<radial-size>` is a single `<length>`, and to an ellipse
/// otherwise"); the size "defaults to farthest-corner" if omitted.
fn radial_gradient(s: &Specified, g: &Compiled) -> Vec<String> {
    let mut s = s.clone();
    if let SpecKind::List(parts) = &mut s.kind
        && let Some((_, prelude)) = parts.first_mut()
    {
        drop_keyword(prelude, "farthest-corner", &|_| true);
        drop_keyword(prelude, "ellipse", &|_| true);
        // `circle` with a single length: the length alone says circle.
        drop_keyword(prelude, "circle", &|others| {
            others.len() == 1 && is_length(&others[0].1)
        });
    }
    items(&s, g)
}

/// css-images-3 #linear-gradient-syntax: the first argument "may be
/// omitted; if so, it defaults to to bottom", and the first and last
/// color stops default to 0% and 100% (#color-stop-fixup, "If the first
/// color stop does not have a position, set its position to 0%", and
/// likewise 100% for the last); #serialization's own example omits both.
fn linear_gradient(s: &Specified, g: &Compiled) -> Vec<String> {
    let out = items(s, g);
    let mut text: Vec<String> = out;
    // The direction: `to bottom` or `180deg` (the same angle).
    let first_comma = text.iter().position(|t| t == ",");
    if let Some(c) = first_comma {
        let prelude = &text[..c];
        if prelude == ["to", "bottom"] || prelude == ["180deg"] {
            text.drain(..=c);
        }
    }
    text
}

/// A string that is a sequence of identifiers separated by single
/// spaces, none of them a keyword a family name can't be, serializes
/// shorter as those identifiers (cssom-1 "serialize a CSS value";
/// css-fonts-4 #font-family-name-syntax: names "must either be given
/// quoted as `<string>`s, or unquoted as a sequence of one or more
/// `<custom-ident>`"). Names a keyword could be misread as stay strings.
fn font_family_name(s: &Specified, g: &Compiled) -> Vec<String> {
    let mut l = Vec::new();
    leaves(s, &mut l);
    if let [one] = l.as_slice()
        && let SpecKind::String(name) = &one.kind
    {
        let words: Vec<&str> = name.split(' ').collect();
        let plain = words.iter().all(|w| {
            let mut text = String::new();
            super::serialize::identifier(&mut text, w);
            !w.is_empty()
                && text == *w
                && !super::table::GENERIC_FAMILIES
                    .iter()
                    .chain(super::types::NOT_CUSTOM_IDENT)
                    .any(|k| k.eq_ignore_ascii_case(w))
        });
        if plain {
            return words.into_iter().map(str::to_string).collect();
        }
    }
    items(s, g)
}

/// css-fonts-4 #font-feature-settings-prop: "A value of on is synonymous with 1 and off is
/// synonymous with 0. If the value is omitted, a value of 1 is assumed."
/// So `1` and `on` are left out, and `off` is written `0`.
fn feature_tag_value(s: &Specified, g: &Compiled) -> Vec<String> {
    let out = items(s, g);
    match out.as_slice() {
        [tag, v] if v == "1" || v == "on" => vec![tag.clone()],
        [tag, v] if v == "off" => vec![tag.clone(), "0".to_string()],
        _ => out,
    }
}

/// css-fonts-4 #font-language-override-prop: "If the string is shorter than four characters, it
/// is padded at the end with space (U+0020) characters such that the
/// length is 4, before being matched", so trailing spaces mean nothing.
fn font_language_override(s: &Specified, g: &Compiled) -> Vec<String> {
    let mut l = Vec::new();
    leaves(s, &mut l);
    if let [one] = l.as_slice()
        && let SpecKind::String(tag) = &one.kind
    {
        let mut text = String::new();
        super::serialize::string(&mut text, tag.trim_end_matches(' '));
        return vec![text];
    }
    items(s, g)
}

/// css-counter-styles-3 #symbols-function: the algorithm uses the system
/// given, "or symbolic if the system was omitted".
fn symbols(s: &Specified, g: &Compiled) -> Vec<String> {
    let mut s = s.clone();
    drop_keyword(&mut s, "symbolic", &|_| true);
    items(&s, g)
}

/// css-images-4 #image-set-notation: "Each `<string>` inside image-set() represents a
/// `<url>`", so it serializes as one. Applies to an option
/// (`[ <image> | <string> ] ...`) or, for `<url-set>`, to each option of
/// its `image-set()`.
fn image_set_option(s: &Specified, g: &Compiled) -> Vec<String> {
    let mut s = s.clone();
    match &mut s.kind {
        SpecKind::Nested(_, inner) => {
            if let SpecKind::List(options) = &mut inner.kind {
                options.iter_mut().for_each(|(_, o)| string_to_url(o));
            }
        }
        _ => string_to_url(&mut s),
    }
    items(&s, g)
}

/// Makes the first component of an option a URL if it is a string.
fn string_to_url(option: &mut Specified) {
    let SpecKind::List(parts) = &mut option.kind else {
        return;
    };
    let Some((_, first)) = parts.first_mut() else {
        return;
    };
    if let SpecKind::Alternative(_, chosen) = &mut first.kind
        && let SpecKind::String(text) = &mut chosen.kind
    {
        chosen.kind = SpecKind::Url(std::mem::take(text));
    }
}
