//! Specified values as text, for tests: a declaration's value as written,
//! or the property table's initial value.

#![allow(dead_code)]

use super::tree::Tree;
use optimal_style::property::LONGHANDS;
use optimal_style::{Origin, Source, Style, StyleAttribute, StyleSet, Value};
use optimal_wtf8::Wtf8;

/// The text of a declaration's value.
pub fn value_text(
    set: &StyleSet<'_>,
    styles: &[Option<StyleAttribute<'_>>],
    el: usize,
    source: Source,
) -> String {
    let (p, v) = match source {
        Source::Rule(d) => {
            let d = &set.decls()[d as usize];
            (&set.sheets()[d.sheet as usize].parsed, set.values(d))
        }
        Source::Attribute(a) => {
            let st = styles[el].as_ref().unwrap();
            (&st.parsed, st.values(a as usize))
        }
    };
    let spans: Vec<_> = v.map(|c| c.span()).collect();
    match (spans.first(), spans.last()) {
        (Some(f), Some(l)) => p.source(f.start..l.end).to_string_lossy(),
        _ => String::new(),
    }
}

/// The specified value's text of `prop` on each element (`None` for a
/// custom property with the guaranteed-invalid value), with `sheets` of
/// their origins.
pub fn specified(sheets: &[(&str, Origin)], t: &Tree, prop: &str) -> Vec<Option<String>> {
    let mut set = StyleSet::new();
    for (s, o) in sheets {
        set.add_sheet(Wtf8::from_str(s), *o);
    }
    let styles: Vec<_> = (0..t.nodes.len())
        .map(|i| {
            t.attr(i, "style")
                .map(|s| StyleAttribute::parse(Wtf8::from_str(s)))
        })
        .collect();
    let c = set.prepare();
    c.with_session(false, |s| {
        let mut out: Vec<Style<'_>> = Vec::new();
        #[allow(clippy::needless_range_loop)]
        for i in 0..t.nodes.len() {
            let initial = Style::initial();
            let parent = t.nodes[i].parent.map_or(&initial, |p| &out[p]);
            let st = s.style(&t.el(i), i as u32, styles[i].as_ref(), parent);
            out.push(st);
        }
        (0..t.nodes.len())
            .map(|i| {
                let v = if prop.starts_with("--") {
                    out[i].custom(prop)?
                } else {
                    out[i].get(prop).expect("a longhand in the table")
                };
                Some(match v {
                    Value::Initial => LONGHANDS
                        .iter()
                        .find(|l| l.name == prop)
                        .unwrap()
                        .initial
                        .to_string(),
                    Value::Declared { source, element } => {
                        value_text(&set, &styles, element as usize, source)
                    }
                })
            })
            .collect()
    })
}
