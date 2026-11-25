//! Namespaces and interned local names.
//!
//! Element and attribute names are compared constantly (the tree builder
//! alone checks names on almost every token), so they are interned: a
//! [`LocalName`] is a small integer, and comparing two is one instruction.
//! Names the engine knows about are static and have constants in [`local`];
//! any other name is interned per document by [`Interner`].
//!
//! Design choice (logged): per-document interning plus a static table.
//! Rejected: a global interner (needs locking or `unsafe`, and leaks names
//! across sites, which would also be a cross-site information channel).

use optimal_wtf8::{Wtf8, Wtf8Buf};
use std::collections::HashMap;
use std::sync::OnceLock;

/// A namespace an element or attribute can be in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Namespace {
    /// No namespace (most attributes).
    None,
    /// `http://www.w3.org/1999/xhtml`
    Html,
    /// `http://www.w3.org/1998/Math/MathML`
    MathMl,
    /// `http://www.w3.org/2000/svg`
    Svg,
    /// `http://www.w3.org/1999/xlink`
    XLink,
    /// `http://www.w3.org/XML/1998/namespace`
    Xml,
    /// `http://www.w3.org/2000/xmlns/`
    Xmlns,
}

impl Namespace {
    /// The namespace URL, or `""` for no namespace.
    pub const fn url(self) -> &'static str {
        match self {
            Namespace::None => "",
            Namespace::Html => "http://www.w3.org/1999/xhtml",
            Namespace::MathMl => "http://www.w3.org/1998/Math/MathML",
            Namespace::Svg => "http://www.w3.org/2000/svg",
            Namespace::XLink => "http://www.w3.org/1999/xlink",
            Namespace::Xml => "http://www.w3.org/XML/1998/namespace",
            Namespace::Xmlns => "http://www.w3.org/2000/xmlns/",
        }
    }
}

/// An interned local name. Static names (see [`local`]) are the same in every
/// document; other names are only meaningful with the [`Interner`] that made
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalName(pub(crate) u32);

/// How many static names there are. Static names have indices below this.
pub fn static_name_count() -> usize {
    STATIC_NAMES.len()
}

impl LocalName {
    /// The name's index: below [`static_name_count`] for static names, so
    /// callers can keep small per-name tables for them.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

macro_rules! static_names {
    ($($konst:ident => $s:literal,)*) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[repr(u32)]
        enum StaticId { $($konst,)* }

        /// Constants for every static local name.
        #[allow(missing_docs)]
        pub mod local {
            use super::{LocalName, StaticId};
            $(pub const $konst: LocalName = LocalName(StaticId::$konst as u32);)*
        }

        /// The text of every static name, indexed by its id.
        pub(crate) static STATIC_NAMES: &[&str] = &[$($s,)*];
    };
}

static_names! {
    EMPTY => "",
    // HTML elements the parser and engine refer to.
    A => "a", ADDRESS => "address", APPLET => "applet", AREA => "area", ARTICLE => "article",
    ASIDE => "aside", B => "b", BASE => "base", BASEFONT => "basefont", BGSOUND => "bgsound",
    BIG => "big", BLOCKQUOTE => "blockquote", BODY => "body", BR => "br", BUTTON => "button",
    CAPTION => "caption", CENTER => "center", CODE => "code", COL => "col", COLGROUP => "colgroup",
    DD => "dd", DETAILS => "details", DIALOG => "dialog", DIR => "dir", DIV => "div", DL => "dl",
    DT => "dt", EM => "em", EMBED => "embed", FIELDSET => "fieldset", FIGCAPTION => "figcaption",
    FIGURE => "figure", FONT => "font", FOOTER => "footer", FORM => "form", FRAME => "frame",
    FRAMESET => "frameset", H1 => "h1", H2 => "h2", H3 => "h3", H4 => "h4", H5 => "h5", H6 => "h6",
    HEAD => "head", HEADER => "header", HGROUP => "hgroup", HR => "hr", HTML => "html", I => "i",
    IFRAME => "iframe", IMAGE => "image", IMG => "img", INPUT => "input", KEYGEN => "keygen",
    LI => "li", LINK => "link", LISTING => "listing", MAIN => "main", MARQUEE => "marquee",
    MENU => "menu", META => "meta", NAV => "nav", NOBR => "nobr", NOEMBED => "noembed",
    NOFRAMES => "noframes", NOSCRIPT => "noscript", OBJECT => "object", OL => "ol",
    OPTGROUP => "optgroup", OPTION => "option", P => "p", PARAM => "param", PLAINTEXT => "plaintext",
    PRE => "pre", RB => "rb", RP => "rp", RT => "rt", RTC => "rtc", RUBY => "ruby", S => "s",
    SARCASM => "sarcasm", SCRIPT => "script", SEARCH => "search", SECTION => "section",
    SELECT => "select", SMALL => "small", SOURCE => "source", SPAN => "span", STRIKE => "strike",
    STRONG => "strong", STYLE => "style", SUB => "sub", SUMMARY => "summary", SUP => "sup",
    TABLE => "table", TBODY => "tbody", TD => "td", TEMPLATE => "template", TEXTAREA => "textarea",
    TFOOT => "tfoot", TH => "th", THEAD => "thead", TITLE => "title", TR => "tr", TRACK => "track",
    TT => "tt", U => "u", UL => "ul", VAR => "var", WBR => "wbr", XMP => "xmp",
    // MathML.
    MATH => "math", MI => "mi", MO => "mo", MN => "mn", MS => "ms", MTEXT => "mtext",
    ANNOTATION_XML => "annotation-xml", MGLYPH => "mglyph", MALIGNMARK => "malignmark",
    // SVG.
    SVG => "svg", FOREIGN_OBJECT => "foreignObject", DESC => "desc",
    // Attributes the parser looks at.
    TYPE => "type", ENCODING => "encoding", COLOR => "color", FACE => "face", SIZE => "size",
    DEFINITION_URL => "definitionURL", XMLNS => "xmlns", XLINK => "xlink",
    ACTUATE => "actuate", ARCROLE => "arcrole", HREF => "href", ROLE => "role", SHOW => "show",
    LANG => "lang", SPACE => "space", CHARSET => "charset", HTTP_EQUIV => "http-equiv",
    CONTENT => "content", SHADOWROOTMODE => "shadowrootmode", ID => "id", CLASS => "class",
}

fn static_index() -> &'static HashMap<&'static [u8], u32> {
    static INDEX: OnceLock<HashMap<&'static [u8], u32>> = OnceLock::new();
    INDEX.get_or_init(|| {
        STATIC_NAMES
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_bytes(), i as u32))
            .collect()
    })
}

/// Interns names for one document.
#[derive(Debug, Default, Clone)]
pub struct Interner {
    dynamic: Vec<Wtf8Buf>,
    index: HashMap<Wtf8Buf, u32>,
}

impl Interner {
    /// An interner that knows only the static names.
    pub fn new() -> Self {
        Self::default()
    }

    /// The name for `text`, interning it if it is new.
    pub fn intern(&mut self, text: Wtf8<'_>) -> LocalName {
        if let Some(&i) = static_index().get(text.as_bytes()) {
            return LocalName(i);
        }
        if let Some(&i) = self.index.get(text.as_bytes()) {
            return LocalName(i);
        }
        let i = (STATIC_NAMES.len() + self.dynamic.len()) as u32;
        let owned = Wtf8Buf::from(text);
        self.dynamic.push(owned.clone());
        self.index.insert(owned, i);
        LocalName(i)
    }

    /// The name for a `str`.
    pub fn intern_str(&mut self, text: &str) -> LocalName {
        self.intern(Wtf8::from_str(text))
    }

    /// The name for `text` if it is already known, without interning.
    pub fn get(&self, text: Wtf8<'_>) -> Option<LocalName> {
        static_index()
            .get(text.as_bytes())
            .or_else(|| self.index.get(text.as_bytes()))
            .map(|&i| LocalName(i))
    }

    /// The text of a name.
    ///
    /// # Panics
    /// If `name` came from a different interner and is not static.
    pub fn text(&self, name: LocalName) -> Wtf8<'_> {
        let i = name.0 as usize;
        match STATIC_NAMES.get(i) {
            Some(s) => Wtf8::from_str(s),
            None => self.dynamic[i - STATIC_NAMES.len()].as_wtf8(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_names_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for s in STATIC_NAMES {
            assert!(seen.insert(*s), "duplicate static name {s}");
        }
        let mut i = Interner::new();
        assert_eq!(i.intern_str("div"), local::DIV);
        assert_eq!(i.intern_str("foreignObject"), local::FOREIGN_OBJECT);
        assert_eq!(i.text(local::ANNOTATION_XML), "annotation-xml");
    }

    #[test]
    fn static_names_have_small_indices() {
        assert_eq!(static_name_count(), STATIC_NAMES.len());
        assert!(
            static_name_count() > 100,
            "the HTML tag names alone are more"
        );
        assert_eq!(
            local::DIV.index(),
            STATIC_NAMES.iter().position(|&s| s == "div").unwrap()
        );
        assert!(local::ANNOTATION_XML.index() < static_name_count());
    }

    #[test]
    fn namespace_urls() {
        assert_eq!(Namespace::None.url(), "");
        assert_eq!(Namespace::Html.url(), "http://www.w3.org/1999/xhtml");
        assert_eq!(
            Namespace::MathMl.url(),
            "http://www.w3.org/1998/Math/MathML"
        );
        assert_eq!(Namespace::Svg.url(), "http://www.w3.org/2000/svg");
        assert_eq!(Namespace::XLink.url(), "http://www.w3.org/1999/xlink");
        assert_eq!(Namespace::Xml.url(), "http://www.w3.org/XML/1998/namespace");
        assert_eq!(Namespace::Xmlns.url(), "http://www.w3.org/2000/xmlns/");
    }

    #[test]
    fn dynamic_names_intern_once() {
        let mut i = Interner::new();
        let a = i.intern_str("my-element");
        let b = i.intern_str("my-element");
        let c = i.intern_str("other");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(i.text(a), "my-element");
        assert_eq!(i.get(Wtf8::from_str("other")), Some(c));
        assert_eq!(i.get(Wtf8::from_str("missing")), None);
        // Each new name gets the next index after the static ones.
        assert_eq!(a.index(), static_name_count());
        assert_eq!(c.index(), static_name_count() + 1);
        assert_eq!(i.text(c), "other");
        // Case matters: names are already lowercased where HTML requires it.
        assert_ne!(i.intern_str("DIV"), local::DIV);
    }
}
