//! The value grammars (`src/values/table.rs` and each property's `value`
//! in `src/property.rs`) checked against the spec text they copy, at
//! csswg-drafts commit 7027ab6 (fetched into `browser/build/csswg-propdefs`
//! by `scripts/fetch-test-suites.sh`; `CSSWG_PROPDEFS` overrides the path),
//! with the units and color keyword tables of `src/values/types.rs`.
//!
//! Spec text is compared after removing markup: comments, tags, the
//! `<<type>>` and `''value''` autolinks, character references, and runs of
//! white space. A production is found in a grammar production block
//! (`<pre class=prod>`, `<xmp class=prod>` or `<span class=prod>`) as
//! `<name> = grammar` or `name() = grammar`, up to the next definition.

use optimal_style::property::{GRAMMAR_DEVIATIONS, LONGHANDS, SHORTHANDS, Why};
use optimal_style::values::table::{Def, PRODUCTIONS};
use optimal_style::values::types::{
    Base, DEPRECATED_SYSTEM_COLORS, NAMED_COLORS, SYSTEM_COLORS, UNITS,
};
use optimal_style::values::{compile, syntax::Node};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

fn root() -> PathBuf {
    std::env::var_os("CSSWG_PROPDEFS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/csswg-propdefs")
        })
}

fn source(spec: &str) -> String {
    let path = root().join(spec).join("Overview.bs");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} (run browser/scripts/fetch-test-suites.sh)",
            path.display()
        )
    });
    // Comments go first: some hold old versions of productions.
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("<!--") {
        out.push_str(&rest[..at]);
        rest = match rest[at..].find("-->") {
            Some(end) => &rest[at + end + 3..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Spec text without markup, white space collapsed.
fn clean(text: &str) -> String {
    // `<<x>>` autolinks keep their angle brackets.
    let text = text.replace("<<", "\u{1}").replace(">>", "\u{2}");
    let mut s = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<'
            && chars
                .peek()
                .is_some_and(|n| n.is_ascii_alphabetic() || *n == '/')
        {
            // A tag: skip to its end, minding quoted attribute values.
            let mut quote: Option<char> = None;
            for t in chars.by_ref() {
                match (quote, t) {
                    (Some(q), t) if t == q => quote = None,
                    (Some(_), _) => {}
                    (None, '"' | '\'') => quote = Some(t),
                    (None, '>') => break,
                    _ => {}
                }
            }
            continue;
        }
        s.push(c);
    }
    let s = s.replace('\u{1}', "<").replace('\u{2}', ">");
    // `''value''` and `''property/value''` autolinks.
    let mut t = String::new();
    let mut rest = s.as_str();
    while let Some(at) = rest.find("''") {
        t.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        match after.find("''") {
            Some(end) if !after[..end].contains('\n') => {
                let inner = &after[..end];
                let inner = match inner.rsplit_once('/') {
                    Some((pre, v)) if !pre.contains(' ') && !pre.is_empty() => v,
                    _ => inner,
                };
                t.push_str(inner);
                rest = &after[end + 2..];
            }
            _ => {
                t.push_str("''");
                rest = after;
            }
        }
    }
    t.push_str(rest);
    // `[=term=]` and `[=term|text=]` autolinks, and `name!!type` link
    // disambiguators.
    let mut u = String::new();
    let mut rest = t.as_str();
    while let Some(at) = rest.find("[=") {
        u.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        match after.find("=]") {
            Some(end) => {
                let inner = &after[..end];
                u.push_str(inner.rsplit('|').next().unwrap_or(inner));
                rest = &after[end + 2..];
            }
            None => {
                u.push_str("[=");
                rest = after;
            }
        }
    }
    u.push_str(rest);
    let t = u.replace("!!property", "").replace("!!descriptor", "");
    let t = t
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&ndash;", "–")
        .replace("&mdash;", "—")
        .replace("&amp;", "&");
    // `'property'` autolinks (single quotes) stay as written: grammars
    // use `<'property'>`, which has the same quotes.
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every production defined in a spec's grammar production blocks: name
/// (`color`, `rgb()`) to each definition's cleaned text.
fn productions(spec: &str) -> HashMap<String, Vec<String>> {
    let text = source(spec);
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut rest = text.as_str();
    loop {
        let next = ["pre", "xmp", "span"]
            .into_iter()
            .filter_map(|t| {
                let mut from = 0;
                while let Some(i) = rest[from..].find(&format!("<{t} ")) {
                    let at = from + i;
                    let end = rest[at..].find('>')? + at;
                    let attrs = &rest[at..end];
                    if attrs.contains("prod") && attrs.contains("class") {
                        return Some((at, end, t));
                    }
                    from = end;
                }
                None
            })
            .min();
        let Some((_, end, tag)) = next else { break };
        let body_start = end + 1;
        let close = format!("</{tag}>");
        let body_end = rest[body_start..].find(&close).expect("a block ends") + body_start;
        let body = clean(&rest[body_start..body_end]);
        rest = &rest[body_end..];
        // Split at each `<name> =` or `name() =`.
        let marks: Vec<(usize, usize, String)> = definition_marks(&body);
        for (i, (start, after, name)) in marks.iter().enumerate() {
            let _ = start;
            let end = marks.get(i + 1).map_or(body.len(), |m| m.0);
            out.entry(name.clone())
                .or_default()
                .push(body[*after..end].trim().to_string());
        }
    }
    out
}

/// Where definitions start in a cleaned block: (start, start of the
/// grammar, name).
fn definition_marks(body: &str) -> Vec<(usize, usize, String)> {
    let b = body.as_bytes();
    let mut out = Vec::new();
    for (eq, _) in body.match_indices('=') {
        let before = body[..eq].trim_end();
        let name_end = before.len();
        let (name_start, name) = if before.ends_with('>') {
            let Some(open) = before.rfind('<') else {
                continue;
            };
            (open, before[open + 1..name_end - 1].to_string())
        } else if before.ends_with("()") {
            let start = before[..name_end - 2]
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .map_or(0, |i| i + 1);
            (start, before[start..].to_string())
        } else {
            continue;
        };
        if name.contains(' ') || name.is_empty() {
            continue;
        }
        let _ = b;
        out.push((name_start, eq + 1, name));
    }
    out
}

/// Every non-partial property definition table of a spec: name to Value.
fn propdef_values(spec: &str) -> HashMap<String, String> {
    let text = source(spec);
    let mut out = HashMap::new();
    let mut rest = text.as_str();
    loop {
        let next = ["pre", "xmp"]
            .into_iter()
            .filter_map(|t| rest.find(&format!("<{t} class=")).map(|i| (i, t)))
            .min();
        let Some((at, tag)) = next else { break };
        let after = &rest[at + tag.len() + 8..];
        let tag_end = after.find('>').expect("a tag ends");
        let attrs = &after[..tag_end];
        rest = &after[tag_end + 1..];
        let class = attrs.trim_start_matches(['"', '\'']);
        if !class.starts_with("propdef") {
            continue;
        }
        let close = format!("</{tag}>");
        let end = rest.find(&close).expect("a table ends");
        let body = &rest[..end];
        rest = &rest[end..];
        if attrs.contains("partial") {
            continue;
        }
        let mut fields: Vec<(String, String)> = Vec::new();
        for line in body.lines() {
            match line.trim().split_once(':') {
                Some((k, v))
                    if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic() || c == ' ') =>
                {
                    fields.push((k.trim().to_string(), v.trim().to_string()));
                }
                _ => {
                    if let Some(last) = fields.last_mut() {
                        last.1.push(' ');
                        last.1.push_str(line.trim());
                    }
                }
            }
        }
        let get = |k: &str| fields.iter().find(|f| f.0 == k).map(|f| f.1.clone());
        let (Some(names), Some(value)) = (get("Name"), get("Value")) else {
            continue;
        };
        for n in names.split(',') {
            out.insert(n.trim().to_string(), clean(&value));
        }
    }
    out
}

fn norm(g: &str) -> String {
    g.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Default)]
struct Specs {
    prods: HashMap<String, HashMap<String, Vec<String>>>,
    texts: HashMap<String, String>,
    values: HashMap<String, HashMap<String, String>>,
}

impl Specs {
    fn prods(&mut self, spec: &str) -> &HashMap<String, Vec<String>> {
        self.prods
            .entry(spec.to_string())
            .or_insert_with(|| productions(spec))
    }
    fn text(&mut self, spec: &str) -> &str {
        self.texts
            .entry(spec.to_string())
            .or_insert_with(|| clean(&source(spec)))
    }
    fn values(&mut self, spec: &str) -> &HashMap<String, String> {
        self.values
            .entry(spec.to_string())
            .or_insert_with(|| propdef_values(spec))
    }
}

#[test]
fn productions_match_the_specs() {
    let mut specs = Specs::default();
    let mut errors = Vec::new();
    for p in PRODUCTIONS {
        let mut check_prod = |specs: &mut Specs, text: &str| {
            let defs = specs.prods(p.spec).get(p.name).cloned().unwrap_or_default();
            if !defs.iter().any(|d| norm(d) == norm(text)) {
                errors.push(format!(
                    "{} ({}): table has {:?}, spec has {:?}",
                    p.name, p.spec, text, defs
                ));
            }
        };
        match p.def {
            Def::Prod(g) => check_prod(&mut specs, g),
            Def::Rule {
                spec,
                quote_in,
                quote,
                ..
            } => {
                check_prod(&mut specs, spec);
                if !specs.text(quote_in).contains(&norm(quote)) {
                    errors.push(format!("{}: quote not in {quote_in}: {quote:?}", p.name));
                }
            }
            Def::Prose(_, quote) | Def::Builtin(_, quote) => {
                if !specs.text(p.spec).contains(&norm(quote)) {
                    errors.push(format!("{}: quote not in {}: {quote:?}", p.name, p.spec));
                }
            }
            Def::Unsupported(_) => {
                let name = format!("<{}>", p.name);
                if !specs.text(p.spec).contains(&name) {
                    errors.push(format!("{}: {} doesn't mention {name}", p.name, p.spec));
                }
            }
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn productions_are_sorted_unique_and_used() {
    for w in PRODUCTIONS.windows(2) {
        assert!(w[0].name < w[1].name, "{} before {}", w[0].name, w[1].name);
    }
    let g = compile().expect("the grammars compile");
    // Every production is reachable from some property's grammar.
    let mut seen = HashSet::new();
    let mut todo: Vec<u32> = g.longhands.iter().chain(&g.shorthands).copied().collect();
    while let Some(n) = todo.pop() {
        if !seen.insert(n) {
            continue;
        }
        match &g.nodes[n as usize] {
            Node::Ref(c) | Node::Function(_, c) | Node::Block(c) | Node::Required(c) => {
                todo.push(*c)
            }
            Node::Repeat { node, .. } => todo.push(*node),
            Node::Seq(c) | Node::All(c) | Node::Any(c) | Node::One(c) => todo.extend(c.iter()),
            _ => {}
        }
    }
    for (name, top) in &g.productions {
        assert!(seen.contains(top), "production {name} is not used");
    }
}

#[test]
fn property_grammars_match_the_specs() {
    let mut specs = Specs::default();
    let mut errors = Vec::new();
    let props = LONGHANDS
        .iter()
        .map(|l| (l.name, l.value, l.spec))
        .chain(SHORTHANDS.iter().map(|s| (s.name, s.value, s.spec)));
    for (name, value, spec) in props {
        let Some(def) = specs.values(spec).get(name).cloned() else {
            errors.push(format!("{name}: no definition table in {spec}"));
            continue;
        };
        match GRAMMAR_DEVIATIONS.iter().find(|d| d.property == name) {
            None => {
                if norm(&def) != norm(value) {
                    errors.push(format!("{name}: table has {value:?}, {spec} has {def:?}"));
                }
            }
            Some(d) => {
                if norm(&def) != norm(d.spec) {
                    errors.push(format!(
                        "{name}: deviation copies {:?}, {spec} has {def:?}",
                        d.spec
                    ));
                }
                if norm(&def) == norm(value) {
                    errors.push(format!("{name}: listed as a deviation but equals the spec"));
                }
                if let Why::Rule { quote_in, quote } = d.why
                    && !specs.text(quote_in).contains(&norm(quote))
                {
                    errors.push(format!("{name}: quote not in {quote_in}: {quote:?}"));
                }
            }
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn units_match_css_values_4() {
    // Each unit is defined (a <dfn>) in the section of its type.
    let text = source("css-values-4");
    let section = |id: &str| {
        let start = text.find(&format!("id=\"{id}\"")).expect("a section");
        let end = text[start..]
            .find("\n<h2")
            .map_or(text.len(), |e| start + e);
        &text[start..end]
    };
    let dfns = |s: &str| -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(at) = rest.find("<dfn") {
            let after = &rest[at..];
            let open_end = after.find('>').expect("a tag ends");
            let inner = &after[open_end + 1..];
            let close = inner.find("</dfn>").expect("a dfn ends");
            let word = clean(&inner[..close]);
            if !word.contains(' ') && !word.contains('<') && !word.is_empty() {
                out.push(word);
            }
            rest = &inner[close..];
        }
        out
    };
    let lengths = dfns(section("lengths"));
    let other = dfns(section("other-units"));
    let mut seen = HashSet::new();
    for u in UNITS {
        let list = if u.base == Base::Length {
            &lengths
        } else {
            &other
        };
        assert!(
            list.contains(&u.name.to_string()),
            "unit {} not in css-values-4",
            u.name
        );
        assert!(
            seen.insert(u.name.to_ascii_lowercase()),
            "unit {} twice",
            u.name
        );
    }
    // And every unit the spec defines there is in the table.
    let units: HashSet<&str> = UNITS.iter().map(|u| u.name).collect();
    for w in lengths.iter().chain(&other) {
        let is_unit_like = w.chars().all(|c| c.is_ascii_alphabetic()) && w.len() <= 5;
        if is_unit_like && !["anchored", "none"].contains(&w.as_str()) {
            assert!(units.contains(w.as_str()), "css-values-4 defines unit {w}");
        }
    }
}

#[test]
fn color_keywords_match_css_color_4() {
    let text = source("css-color-4");
    // The named color table: name, then its hex value.
    let mut named = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("<th scope=row><dfn>") {
        let after = &rest[at + 19..];
        let end = after.find("</dfn>").expect("a dfn ends");
        let name = after[..end].to_string();
        let hex_at = after.find("<td>#").expect("a hex value") + 5;
        let hex = &after[hex_at..hex_at + 6];
        let rgb = [0, 2, 4].map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"));
        named.push((name, rgb));
        rest = &after[end..];
    }
    let ours: Vec<(String, [u8; 3])> = NAMED_COLORS
        .iter()
        .map(|(n, c)| (n.to_string(), *c))
        .collect();
    assert_eq!(ours, named);
    // System colors: the <dt><dfn> entries of the system color list and of
    // the deprecated list, with what each deprecated one computes to.
    let list = |id: &str| -> Vec<(String, Option<String>)> {
        let start = text.find(id).expect("a list");
        let body = &text[start..];
        let body = &body[..body.find("</dl>").expect("a list ends")];
        let mut out = Vec::new();
        for part in body.split("<dt><dfn>").skip(1) {
            let name = part[..part.find("</dfn>").expect("ends")].to_string();
            let to = part.find("Computes to the color of ''").map(|i| {
                let r = &part[i + 27..];
                r[..r.find("''").expect("ends")].to_string()
            });
            out.push((name, to));
        }
        out
    };
    let system: Vec<String> = list("id=\"system-color-values\"")
        .into_iter()
        .map(|x| x.0)
        .collect();
    assert_eq!(SYSTEM_COLORS, system.as_slice());
    let deprecated = list("dfn-for=\"<color>, <deprecated-color>\"");
    let ours: Vec<(String, Option<String>)> = DEPRECATED_SYSTEM_COLORS
        .iter()
        .map(|(a, b)| (a.to_string(), Some(b.to_string())))
        .collect();
    assert_eq!(ours, deprecated);
}
