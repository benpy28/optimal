//! The property table (`src/property.rs`) checked against the property
//! definition tables of the specs it cites, at csswg-drafts commit 7027ab6
//! (fetched into `browser/build/csswg-propdefs` by
//! `scripts/fetch-test-suites.sh`; `CSSWG_PROPDEFS` overrides the path).
//!
//! A definition table is a `<pre class=propdef>` (or `<xmp
//! class="propdef">` in css2) of `Field: value` lines; `Name` may list
//! several properties. Tables marked `partial` add to another spec's
//! definition and are skipped. Initial values are compared after removing
//! markup: links, and the `''value''` and `''property/value''` autolinks.

use optimal_style::property::{
    ALIASES, Keyword, LONGHANDS, NOT_RESET_BY_ALL, SHORTHANDS, Target, longhand, longhands, target,
};
use std::collections::HashMap;
use std::path::PathBuf;

fn root() -> PathBuf {
    std::env::var_os("CSSWG_PROPDEFS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/csswg-propdefs")
        })
}

type Def = HashMap<String, String>;

/// Every non-partial definition table of a spec, by property name.
fn defs(spec: &str) -> HashMap<String, Def> {
    let path = root().join(spec).join("Overview.bs");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} (run browser/scripts/fetch-test-suites.sh)",
            path.display()
        )
    });
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
        let mut def = Def::new();
        for line in body.lines() {
            if let Some((k, v)) = line.trim().split_once(':')
                && !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphabetic() || c == ' ')
            {
                def.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        let names = def.get("Name").cloned().unwrap_or_default();
        for n in names.split(',') {
            out.insert(n.trim().to_string(), def.clone());
        }
    }
    out
}

/// A definition table's value without markup.
fn clean(v: &str) -> String {
    let mut s = String::new();
    let mut in_tag = false;
    for c in v.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => s.push(c),
            _ => {}
        }
    }
    // ''value'' and ''property/value''
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(a) = rest.find("''") {
        out.push_str(&rest[..a]);
        let inner = &rest[a + 2..];
        let b = inner.find("''").expect("closed autolink");
        let link = &inner[..b];
        out.push_str(link.rsplit_once('/').map_or(link, |(_, v)| v));
        rest = &inner[b + 2..];
    }
    out.push_str(rest);
    out.trim().to_string()
}

#[test]
fn longhands_match_their_definition_tables() {
    let mut cache: HashMap<&str, HashMap<String, Def>> = HashMap::new();
    for l in LONGHANDS {
        let defs = cache.entry(l.spec).or_insert_with(|| defs(l.spec));
        let def = defs
            .get(l.name)
            .unwrap_or_else(|| panic!("{}: no definition table in {}", l.name, l.spec));
        let inherited = match def["Inherited"].as_str() {
            "yes" => true,
            "no" => false,
            other => panic!("{}: Inherited: {other}", l.name),
        };
        assert_eq!(l.inherited, inherited, "{} inherited", l.name);
        let initial = clean(&def["Initial"]);
        if initial == "depends on user agent" {
            // The engine's choice (see the property module's docs).
            assert_eq!((l.name, l.initial), ("font-family", "serif"));
        } else {
            assert_eq!(l.initial, initial, "{} initial", l.name);
        }
    }
}

#[test]
fn shorthands_have_definition_tables_and_known_longhands() {
    let mut cache: HashMap<&str, HashMap<String, Def>> = HashMap::new();
    for s in SHORTHANDS {
        let defs = cache.entry(s.spec).or_insert_with(|| defs(s.spec));
        assert!(
            defs.contains_key(s.name),
            "{}: no table in {}",
            s.name,
            s.spec
        );
        let mut seen = Vec::new();
        for l in s.longhands {
            assert!(longhand(l).is_some(), "{}: {l} is not in the table", s.name);
            assert!(!seen.contains(l), "{}: {l} twice", s.name);
            seen.push(*l);
        }
    }
    for (old, new, spec) in ALIASES {
        let defs = cache.entry(spec).or_insert_with(|| defs(spec));
        assert!(longhand(new).is_some(), "alias {old} -> {new}");
        // The old name has no table of its own.
        assert!(!defs.contains_key(*old), "{old} has a table");
    }
    // `all` is a shorthand of css-cascade-5.
    assert!(defs("css-cascade-5").contains_key("all"));
}

#[test]
fn names_are_sorted_lowercase_and_unique() {
    let names = |v: Vec<&str>| {
        for w in v.windows(2) {
            assert!(w[0] < w[1], "{} before {}", w[0], w[1]);
        }
        for n in v {
            assert_eq!(n, n.to_ascii_lowercase());
        }
    };
    names(LONGHANDS.iter().map(|l| l.name).collect());
    names(SHORTHANDS.iter().map(|s| s.name).collect());
    names(ALIASES.iter().map(|a| a.0).collect());
    // No name is two things.
    for s in SHORTHANDS {
        assert!(longhand(s.name).is_none(), "{} is both", s.name);
    }
    for a in ALIASES {
        assert!(target(a.0) == target(a.1), "{} is an alias", a.0);
        assert!(longhand(a.0).is_none());
    }
}

#[test]
fn targets_and_expansion() {
    assert_eq!(
        target("color"),
        Some(Target::Longhand(longhand("color").unwrap()))
    );
    assert_eq!(target("word-wrap"), target("overflow-wrap"));
    assert_eq!(target("font-stretch"), target("font-width"));
    assert_eq!(target("all"), Some(Target::All));
    assert_eq!(target("--x"), Some(Target::Custom));
    assert_eq!(target("--"), Some(Target::Custom));
    assert_eq!(target("nope"), None);
    assert_eq!(target("-webkit-box"), None);
    assert_eq!(target("-"), None);
    let names = |t| -> Vec<&str> { longhands(t).map(|l| LONGHANDS[l as usize].name).collect() };
    assert_eq!(names(target("color").unwrap()), ["color"]);
    assert_eq!(
        names(target("margin").unwrap()),
        ["margin-top", "margin-right", "margin-bottom", "margin-left"]
    );
    assert!(names(Target::Custom).is_empty());
    let all = names(Target::All);
    assert_eq!(all.len(), LONGHANDS.len() - 2);
    for n in NOT_RESET_BY_ALL {
        assert!(!all.contains(n));
    }
    assert!(all.contains(&"display") && all.contains(&"writing-mode"));
}

#[test]
fn keywords() {
    assert_eq!(Keyword::parse("initial"), Some(Keyword::Initial));
    assert_eq!(Keyword::parse("INHERIT"), Some(Keyword::Inherit));
    assert_eq!(Keyword::parse("Unset"), Some(Keyword::Unset));
    assert_eq!(Keyword::parse("revert"), Some(Keyword::Revert));
    assert_eq!(Keyword::parse("revert-LAYER"), Some(Keyword::RevertLayer));
    assert_eq!(Keyword::parse("revert-rule"), Some(Keyword::RevertRule));
    // `default` is reserved, not a CSS-wide keyword.
    assert_eq!(Keyword::parse("default"), None);
    assert_eq!(Keyword::parse("revert-"), None);
    assert_eq!(Keyword::parse("initia"), None);
}
