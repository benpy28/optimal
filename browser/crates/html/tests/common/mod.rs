//! Shared test helpers: the html5lib tree dump format.

#![allow(dead_code)]

use optimal_dom::{Document, Namespace, NodeData, NodeId};
use optimal_html::tree_builder::Parsed;
use std::fmt::Write as _;

fn utf16_key(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn dump_node(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    let _ = write!(out, "| {pad}");
    match doc.data(id) {
        NodeData::Document | NodeData::DocumentFragment => unreachable!(),
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => {
            if public_id.as_wtf8().as_bytes().is_empty()
                && system_id.as_wtf8().as_bytes().is_empty()
            {
                let _ = writeln!(out, "<!DOCTYPE {name}>");
            } else {
                let _ = writeln!(out, "<!DOCTYPE {name} \"{public_id}\" \"{system_id}\">");
            }
        }
        NodeData::Text(t) => {
            let _ = writeln!(out, "\"{t}\"");
        }
        NodeData::Comment(t) => {
            let _ = writeln!(out, "<!-- {t} -->");
        }
        NodeData::ProcessingInstruction { target, data } => {
            let _ = writeln!(out, "<?{target} {data}?>");
        }
        NodeData::Element(e) => {
            let ns = match e.namespace {
                Namespace::Html => "",
                Namespace::Svg => "svg ",
                Namespace::MathMl => "math ",
                other => panic!("element in namespace {other:?}"),
            };
            let _ = writeln!(out, "<{ns}{}>", doc.name_text(e.local));
            let mut attrs: Vec<(String, String)> = e
                .attributes
                .iter()
                .map(|a| {
                    let ns = match a.namespace {
                        Namespace::None => "",
                        Namespace::XLink => "xlink ",
                        Namespace::Xml => "xml ",
                        Namespace::Xmlns => "xmlns ",
                        other => panic!("attribute in namespace {other:?}"),
                    };
                    (
                        format!("{ns}{}", doc.name_text(a.local)),
                        a.value.to_string(),
                    )
                })
                .collect();
            attrs.sort_by_key(|(n, _)| utf16_key(n));
            for (n, v) in attrs {
                let _ = writeln!(out, "| {pad}  {n}=\"{v}\"");
            }
            if let Some(c) = e.template_contents {
                let _ = writeln!(out, "| {pad}  content");
                for k in doc.children(c) {
                    dump_node(doc, k, depth + 2, out);
                }
            }
        }
    }
    if !matches!(doc.data(id), NodeData::Element(_)) {
        return;
    }
    for k in doc.children(id) {
        dump_node(doc, k, depth + 1, out);
    }
}

pub fn dump(p: &Parsed) -> String {
    let root = p.fragment.unwrap_or(Document::ROOT);
    let mut out = String::new();
    for k in p.document.children(root) {
        dump_node(&p.document, k, 0, &mut out);
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}
