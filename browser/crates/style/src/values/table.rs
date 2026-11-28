//! The productions the property grammars use, each copied from the spec
//! that defines it (csswg-drafts commit 7027ab6), with the markup removed.
//!
//! `tests/value_grammars.rs` checks the table against the specs: a
//! [`Def::Prod`] must equal a definition in one of the spec's grammar
//! production blocks; a [`Def::Prose`] or [`Def::Builtin`], defined in
//! running text, must come with a quote found in the spec. Basic data
//! types (`<length>`, `<string>` and the rest) are not here: see
//! [`super::types`].

use super::types::Builtin;

/// How a production is defined.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Def {
    /// A grammar, as a production block of the spec writes it.
    Prod(&'static str),
    /// A grammar the spec gives in running text, and the quote that says
    /// so.
    Prose(&'static str, &'static str),
    /// A basic data type with a restriction the spec gives in running
    /// text, and the quote that says so.
    Builtin(Builtin, &'static str),
    /// A production as a production block of the spec writes it
    /// (`spec`, checked), restricted by a rule the spec states in running
    /// text (`quote`, found in the spec `quote_in`), written here as the
    /// equivalent grammar (`grammar`, which is what is matched).
    Rule {
        /// The grammar matched: the spec's, with the rule applied.
        grammar: &'static str,
        /// The spec's production text.
        spec: &'static str,
        /// The csswg-drafts directory of the spec the quote is from.
        quote_in: &'static str,
        /// The rule, quoted.
        quote: &'static str,
    },
    /// Not supported yet: matches nothing. Why.
    Unsupported(&'static str),
}

/// A production.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Production {
    /// Its name, without `<>`: `color`, `rgb()`.
    pub name: &'static str,
    /// The csswg-drafts directory of the spec that defines it.
    pub spec: &'static str,
    /// Its definition.
    pub def: Def,
}

const fn p(name: &'static str, spec: &'static str, def: Def) -> Production {
    Production { name, spec, def }
}

/// The generic family keywords (css-fonts-4 2.1.1), which an unquoted
/// family name can't contain.
pub const GENERIC_FAMILIES: &[&str] = &[
    "serif",
    "sans-serif",
    "system-ui",
    "cursive",
    "fantasy",
    "math",
    "monospace",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
];

/// Every production, by name. Names starting with `_` are this engine's
/// helpers for rules the specs state in running text; each cites its
/// quote.
#[rustfmt::skip]
pub const PRODUCTIONS: &[Production] = &[
    p("_family-ident", "css-fonts-4", Def::Builtin(Builtin::CustomIdent(GENERIC_FAMILIES), "Any identifier which could be misinterpreted as a pre-defined keyword in the font-family value definition, or the CSS-wide keywords, is not allowed.")),
    p("_flex-basis-with-unit", "css-flexbox-1", Def::Prose("content | auto | <_length-percentage-with-unit [0,∞]> | stretch | min-content | max-content | fit-content", "A unitless zero that is not already preceded by two flex factors must be interpreted as a flex factor.")),
    p("_four-character-tag", "css-fonts-4", Def::Builtin(Builtin::FourCharacterTag, "Tag strings longer or shorter than four characters, or containing characters outside the U+20–7E codepoint range are invalid.")),
    p("absolute-size", "css-fonts-4", Def::Prose("[ xx-small | x-small | small | medium | large | x-large | xx-large | xxx-large ]", "Possible values are: [ xx-small | x-small | small | medium | large | x-large | xx-large | xxx-large ]")),
    p("alpha-value", "css-color-4", Def::Prod("<number> | <percentage>")),
    p("alt-text", "css-content-3", Def::Prod("[ <string> | <counter> | <attr()> ]+")),
    p("angular-color-hint", "css-images-4", Def::Prod("<angle-percentage> | <zero>")),
    p("angular-color-stop", "css-images-4", Def::Prod("<color> <color-stop-angle>?")),
    p("angular-color-stop-list", "css-images-4", Def::Prod("<angular-color-stop> , [ <angular-color-hint>? , <angular-color-stop> ]#?")),
    p("attachment", "css-backgrounds-3", Def::Prod("scroll | fixed | local")),
    p("attr()", "css-content-3", Def::Unsupported("an arbitrary substitution function (css-values-5): a value holding one is not checked against its grammar")),
    p("baseline-metric", "css-inline-3", Def::Prod("text-bottom | alphabetic | ideographic | middle | central | mathematical | hanging | text-top")),
    p("baseline-position", "css-align-3", Def::Prod("[ first | last ]? && baseline")),
    p("bg-image", "css-backgrounds-3", Def::Prod("<image> | none")),
    p("bg-layer", "css-backgrounds-3", Def::Prod("<bg-image> || <bg-position> [ / <bg-size> ]? || <repeat-style> || <attachment> || <visual-box> || <visual-box>")),
    p("bg-position", "css-backgrounds-3", Def::Prod("[ [ left | center | right | top | bottom | <length-percentage> ] | [ left | center | right | <length-percentage> ] [ top | center | bottom | <length-percentage> ] | [ center | [ left | right ] <length-percentage>? ] && [ center | [ top | bottom ] <length-percentage>? ] ]")),
    p("bg-size", "css-backgrounds-3", Def::Prod("[ <length-percentage [0,∞]> | auto ]{1,2} | cover | contain")),
    p("box-size", "css-sizing-3", Def::Prod("<length-percentage [0,∞]> | stretch | min-content | max-content | fit-content")),
    p("color", "css-color-4", Def::Prod("<color-base> | currentColor | <system-color>")),
    p("color()", "css-color-4", Def::Prod("color( <colorspace-params> [ / [ <alpha-value> | none ] ]? )")),
    p("color-base", "css-color-4", Def::Prod("<hex-color> | <color-function> | <named-color> | transparent")),
    p("color-function", "css-color-4", Def::Prod("<rgb()> | <rgba()> | <hsl()> | <hsla()> | <hwb()> | <lab()> | <lch()> | <oklab()> | <oklch()> | <color()>")),
    p("color-interpolation-method", "css-color-4", Def::Prod("in [ <rectangular-color-space> | <polar-color-space> <hue-interpolation-method>? ]")),
    p("color-stop-angle", "css-images-4", Def::Prod("[ <angle-percentage> | <zero> ]{1,2}")),
    p("color-stop-length", "css-images-4", Def::Prod("<length-percentage>{1,2}")),
    p("color-stop-list", "css-images-4", Def::Prod("<linear-color-stop> , [ <linear-color-hint>? , <linear-color-stop> ]#?")),
    p("colorspace-params", "css-color-4", Def::Prod("[ <predefined-rgb-params> | <xyz-params>]")),
    p("common-lig-values", "css-fonts-4", Def::Prod("[ common-ligatures | no-common-ligatures ]")),
    p("compat-auto", "css-ui-4", Def::Prod("searchfield | textarea | checkbox | radio | menulist | listbox | meter | progress-bar | button")),
    p("compat-special", "css-ui-4", Def::Prod("textfield | menulist-button")),
    p("conic-gradient()", "css-images-4", Def::Prod("conic-gradient( [ <conic-gradient-syntax> ] )")),
    p("conic-gradient-syntax", "css-images-4", Def::Prod("[ [ [ from [ <angle> | <zero> ] ]? [ at <position> ]? ] || <color-interpolation-method> ]? , <angular-color-stop-list>")),
    p("content()", "css-content-3", Def::Unsupported("named strings (css-content-3 content()) are paged media; not supported yet")),
    p("content-distribution", "css-align-3", Def::Prod("space-between | space-around | space-evenly | stretch")),
    p("content-list", "css-content-3", Def::Prod("[ <string> | <image> | <attr()> | contents | <quote> | <leader()> | <target> | <string()> | <content()> | <counter> | <symbolic-glyph> ]+")),
    p("content-position", "css-align-3", Def::Prod("center | start | end | flow-start | flow-end | flex-start | flex-end")),
    p("content-replacement", "css-content-3", Def::Prod("<image>")),
    p("contextual-alt-values", "css-fonts-4", Def::Prod("[ contextual | no-contextual ]")),
    p("counter", "css-lists-3", Def::Prod("<counter()> | <counters()>")),
    p("counter()", "css-lists-3", Def::Prod("counter( <counter-name>, <counter-style>? )")),
    p("counter-name", "css-lists-3", Def::Builtin(Builtin::CustomIdent(&["none"]), "A <counter-name> name cannot match the keyword none; such an identifier is invalid as a <counter-name>.")),
    p("counter-style", "css-counter-styles-3", Def::Prod("<counter-style-name> | <symbols()>")),
    p("counter-style-name", "css-counter-styles-3", Def::Builtin(Builtin::CustomIdent(&["none"]), "The keyword none (including any ASCII case-insensitive permutation of it) is not a valid <counter-style-name>.")),
    p("counters()", "css-lists-3", Def::Prod("counters( <counter-name>, <string>, <counter-style>? )")),
    p("cross-fade()", "css-images-4", Def::Unsupported("not supported yet (css-images-4 cross-fade())")),
    p("cursor-image", "css-ui-4", Def::Prod("[ <url> | <url-set> ] <number>{2}?")),
    p("cursor-predefined", "css-ui-4", Def::Prod("auto | default | none | context-menu | help | pointer | progress | wait | cell | crosshair | text | vertical-text | alias | copy | move | no-drop | not-allowed | grab | grabbing | e-resize | n-resize | ne-resize | nw-resize | s-resize | se-resize | sw-resize | w-resize | ew-resize | ns-resize | nesw-resize | nwse-resize | col-resize | row-resize | all-scroll | zoom-in | zoom-out")),
    p("discretionary-lig-values", "css-fonts-4", Def::Prod("[ discretionary-ligatures | no-discretionary-ligatures ]")),
    p("display-box", "css-display-4", Def::Prod("contents | none")),
    p("display-inside", "css-display-4", Def::Prod("flow | flow-root | table | flex | grid | ruby")),
    p("display-internal", "css-display-4", Def::Prod("table-row-group | table-header-group | table-footer-group | table-row | table-cell | table-column-group | table-column | table-caption | ruby-base | ruby-text | ruby-base-container | ruby-text-container")),
    p("display-legacy", "css-display-4", Def::Prod("inline-block | inline-table | inline-flex | inline-grid")),
    p("display-listitem", "css-display-4", Def::Prod("<display-outside>? && [ flow | flow-root ]? && list-item")),
    p("display-outside", "css-display-4", Def::Prod("block | inline | run-in")),
    p("east-asian-variant-values", "css-fonts-4", Def::Prod("[ jis78 | jis83 | jis90 | jis04 | simplified | traditional ]")),
    p("east-asian-width-values", "css-fonts-4", Def::Prod("[ full-width | proportional-width ]")),
    p("element()", "css-images-4", Def::Unsupported("not supported yet (css-images-4 element() needs element rendering)")),
    p("feature-tag-value", "css-fonts-4", Def::Prod("<opentype-tag> [ <integer [0,∞]> | on | off ]?")),
    p("final-bg-layer", "css-backgrounds-3", Def::Prod("<bg-image> || <bg-position> [ / <bg-size> ]? || <repeat-style> || <attachment> || <visual-box> || <visual-box> || <'background-color'>")),
    p("font-family-name", "css-fonts-4", Def::Rule {
        grammar: "<string> | <_family-ident>+",
        spec: "<string> | <custom-ident>+",
        quote_in: "css-fonts-4",
        quote: "Any identifier which could be misinterpreted as a pre-defined keyword in the font-family value definition, or the CSS-wide keywords, is not allowed.",
    }),
    p("font-feature-value-name", "css-fonts-4", Def::Prod("<ident>")),
    p("font-variant-css2", "css-fonts-4", Def::Prod("normal | small-caps")),
    p("font-weight-absolute", "css-fonts-4", Def::Prod("[ normal | bold | <number [1,1000]> ]")),
    p("font-width-css3", "css-fonts-4", Def::Prod("normal | ultra-condensed | extra-condensed | condensed | semi-condensed | semi-expanded | expanded | extra-expanded | ultra-expanded")),
    p("generic-font-complete", "css-fonts-4", Def::Prod("serif | sans-serif | system-ui | cursive | fantasy | math | monospace")),
    p("generic-font-family", "css-fonts-4", Def::Prod("<generic-font-script-specific>| <generic-font-complete> | <generic-font-incomplete>")),
    p("generic-font-incomplete", "css-fonts-4", Def::Prod("ui-serif | ui-sans-serif | ui-monospace | ui-rounded")),
    p("generic-font-script-specific", "css-fonts-4", Def::Prod("generic(fangsong) | generic(kai) | generic(khmer-mul) | generic(nastaliq)")),
    p("gradient", "css-images-4", Def::Prod("[ <linear-gradient()> | <repeating-linear-gradient()> | <radial-gradient()> | <repeating-radial-gradient()> | <conic-gradient()> | <repeating-conic-gradient()> ]")),
    p("hex-color", "css-color-4", Def::Builtin(Builtin::HexColor, "The syntax of a <hex-color> is a <hash-token> token whose value consists of 3, 4, 6, or 8 hexadecimal digits.")),
    p("historical-lig-values", "css-fonts-4", Def::Prod("[ historical-ligatures | no-historical-ligatures ]")),
    p("hsl()", "css-color-4", Def::Prod("[ <legacy-hsl-syntax> | <modern-hsl-syntax> ]")),
    p("hsla()", "css-color-4", Def::Prod("[ <legacy-hsla-syntax> | <modern-hsla-syntax> ]")),
    p("hue", "css-color-4", Def::Prod("<number> | <angle>")),
    p("hue-interpolation-method", "css-color-4", Def::Prod("[ shorter | longer | increasing | decreasing ] hue")),
    p("hwb()", "css-color-4", Def::Prod("hwb( [<hue> | none] [<percentage> | <number> | none] [<percentage> | <number> | none] [ / [<alpha-value> | none] ]? )")),
    p("image", "css-images-4", Def::Prod("<url> | <image()> | <image-set()> | <cross-fade()> | <element()> | <gradient>")),
    p("image()", "css-images-4", Def::Unsupported("not supported yet (css-images-4 image(); no browser supports it widely)")),
    p("image-set()", "css-images-4", Def::Prod("image-set( <image-set-option># )")),
    p("image-set-option", "css-images-4", Def::Prod("[ <image> | <string> ] [ <resolution> || type(<string>) ]?")),
    p("lab()", "css-color-4", Def::Prod("lab( [<percentage> | <number> | none] [ <percentage> | <number> | none] [ <percentage> | <number> | none] [ / [<alpha-value> | none] ]? )")),
    p("lch()", "css-color-4", Def::Prod("lch( [<percentage> | <number> | none] [ <percentage> | <number> | none] [ <hue> | none] [ / [<alpha-value> | none] ]? )")),
    p("leader()", "css-content-3", Def::Unsupported("paged media (css-content-3 leader()); no paged media yet")),
    p("legacy-hsl-syntax", "css-color-4", Def::Prod("hsl( <hue>, <percentage>, <percentage>, <alpha-value>? )")),
    p("legacy-hsla-syntax", "css-color-4", Def::Prod("hsla( <hue>, <percentage>, <percentage>, <alpha-value>? )")),
    p("legacy-rgb-syntax", "css-color-4", Def::Prod("rgb( <percentage>#{3} , <alpha-value>? ) | rgb( <number>#{3} , <alpha-value>? )")),
    p("legacy-rgba-syntax", "css-color-4", Def::Prod("rgba( <percentage>#{3} , <alpha-value>? ) | rgba( <number>#{3} , <alpha-value>? )")),
    p("line-style", "css-backgrounds-3", Def::Prod("none | hidden | dotted | dashed | solid | double | groove | ridge | inset | outset")),
    p("line-width", "css-backgrounds-3", Def::Prod("<length [0,∞]> | thin | medium | thick")),
    p("linear-color-hint", "css-images-4", Def::Prod("<length-percentage>")),
    p("linear-color-stop", "css-images-4", Def::Prod("<color> <color-stop-length>?")),
    p("linear-gradient()", "css-images-3", Def::Prod("linear-gradient( [ <linear-gradient-syntax> ] )")),
    p("linear-gradient-syntax", "css-images-4", Def::Prod("[ [ <angle> | <zero> | to <side-or-corner> ] || <color-interpolation-method> ]? , <color-stop-list>")),
    p("matrix()", "css-transforms-1", Def::Prod("matrix( <number>#{6} )")),
    p("modern-hsl-syntax", "css-color-4", Def::Prod("hsl( [<hue> | none] [<percentage> | <number> | none] [<percentage> | <number> | none] [ / [<alpha-value> | none] ]? )")),
    p("modern-hsla-syntax", "css-color-4", Def::Prod("hsla( [<hue> | none] [<percentage> | <number> | none] [<percentage> | <number> | none] [ / [<alpha-value> | none] ]? )")),
    p("modern-rgb-syntax", "css-color-4", Def::Prod("rgb( [ <number> | <percentage> | none]{3} [ / [<alpha-value> | none] ]? )")),
    p("modern-rgba-syntax", "css-color-4", Def::Prod("rgba( [ <number> | <percentage> | none]{3} [ / [<alpha-value> | none] ]? )")),
    p("named-color", "css-color-4", Def::Builtin(Builtin::NamedColor, "A <named-color> is written as an <ident>")),
    p("numeric-figure-values", "css-fonts-4", Def::Prod("[ lining-nums | oldstyle-nums ]")),
    p("numeric-fraction-values", "css-fonts-4", Def::Prod("[ diagonal-fractions | stacked-fractions ]")),
    p("numeric-spacing-values", "css-fonts-4", Def::Prod("[ proportional-nums | tabular-nums ]")),
    p("oklab()", "css-color-4", Def::Prod("oklab( [ <percentage> | <number> | none] [ <percentage> | <number> | none] [ <percentage> | <number> | none] [ / [<alpha-value> | none] ]? )")),
    p("oklch()", "css-color-4", Def::Prod("oklch( [ <percentage> | <number> | none] [ <percentage> | <number> | none] [ <hue> | none] [ / [<alpha-value> | none] ]? )")),
    p("opacity-value", "css-color-4", Def::Prod("<number> | <percentage>")),
    p("opentype-tag", "css-fonts-4", Def::Rule {
        grammar: "<_four-character-tag>",
        spec: "<string>",
        quote_in: "css-fonts-4",
        quote: "Tag strings longer or shorter than four characters, or containing characters outside the U+20–7E codepoint range are invalid.",
    }),
    p("outline-line-style", "css-ui-4", Def::Prose("none | dotted | dashed | solid | double | groove | ridge | inset | outset", "<outline-line-style> accepts the same values as <line-style> ([[css-backgrounds-3#border-style]]) with the same meaning, except that hidden is not a legal outline style.")),
    p("overflow-position", "css-align-3", Def::Prod("unsafe | safe")),
    p("palette-identifier", "css-fonts-4", Def::Builtin(Builtin::DashedIdent, "<palette-identifier> is parsed as a <dashed-ident>.")),
    p("palette-mix()", "css-fonts-4", Def::Prod("palette-mix( <color-interpolation-method>? , [ <'font-palette'> && <percentage [0,100]>? ]# )")),
    p("polar-color-space", "css-color-4", Def::Prod("hsl | hwb | lch | oklch")),
    p("position", "css-values-4", Def::Prod("[ [ left | center | right | top | bottom | <length-percentage> ] | [ left | center | right ] && [ top | center | bottom ] | [ left | center | right | <length-percentage> ] [ top | center | bottom | <length-percentage> ] | [ [ left | right ] <length-percentage> ] && [ [ top | bottom ] <length-percentage> ] ]")),
    p("predefined-rgb", "css-color-4", Def::Prod("srgb | srgb-linear | display-p3 | display-p3-linear | a98-rgb | prophoto-rgb | rec2020")),
    p("predefined-rgb-params", "css-color-4", Def::Prod("<predefined-rgb> [ <number> | <percentage> | none ]{3}")),
    p("quote", "css-content-3", Def::Prod("open-quote | close-quote | no-open-quote | no-close-quote")),
    p("radial-extent", "css-images-3", Def::Prod("closest-corner | closest-side | farthest-corner | farthest-side")),
    p("radial-gradient()", "css-images-3", Def::Prod("radial-gradient( [ <radial-gradient-syntax> ] )")),
    p("radial-gradient-syntax", "css-images-4", Def::Rule {
        grammar: "[ [ [ circle || <length [0,∞]> ] [ at <position> ]? | [ ellipse || <length-percentage [0,∞]>{2} ] [ at <position> ]? | [ [ circle | ellipse ] || <radial-extent> ] [ at <position> ]? | at <position> ] || <color-interpolation-method> ]? , <color-stop-list>",
        spec: "[ [ [ <radial-shape> || <radial-size> ]? [ at <position> ]? ] || <color-interpolation-method>]? , <color-stop-list>",
        quote_in: "css-images-3",
        quote: "If <radial-shape> is specified as circle or is omitted, the <radial-size> may be given explicitly as:",
    }),
    p("rectangular-color-space", "css-color-4", Def::Prod("srgb | srgb-linear | display-p3 | display-p3-linear | a98-rgb | prophoto-rgb | rec2020 | lab | oklab | <xyz-space>")),
    p("relative-size", "css-fonts-4", Def::Prose("[ larger | smaller ]", "Possible values are: [ larger | smaller ]")),
    p("repeat-style", "css-backgrounds-3", Def::Prod("repeat-x | repeat-y | [repeat | space | round | no-repeat]{1,2}")),
    p("repeating-conic-gradient()", "css-images-4", Def::Prod("repeating-conic-gradient( [ <conic-gradient-syntax> ] )")),
    p("repeating-linear-gradient()", "css-images-4", Def::Prod("repeating-linear-gradient( [ <linear-gradient-syntax> ] )")),
    p("repeating-radial-gradient()", "css-images-4", Def::Prod("repeating-radial-gradient( [ <radial-gradient-syntax> ] )")),
    p("reversed-counter-name", "css-lists-3", Def::Prod("reversed( <counter-name> )")),
    p("rgb()", "css-color-4", Def::Prod("[ <legacy-rgb-syntax> | <modern-rgb-syntax> ]")),
    p("rgba()", "css-color-4", Def::Prod("[ <legacy-rgba-syntax> | <modern-rgba-syntax> ]")),
    p("rotate()", "css-transforms-1", Def::Prod("rotate( [ <angle> | <zero> ] )")),
    p("scale()", "css-transforms-1", Def::Prod("scale( <number> , <number>? )")),
    p("scaleX()", "css-transforms-1", Def::Prod("scaleX( <number> )")),
    p("scaleY()", "css-transforms-1", Def::Prod("scaleY( <number> )")),
    p("self-position", "css-align-3", Def::Prod("center | start | end | self-start | self-end | flow-start | flow-end | flex-start | flex-end")),
    p("shadow", "css-backgrounds-3", Def::Prod("<color>? && [ <length>{2} [ <length [0,∞]> <length>? ]? ] && inset?")),
    p("side-or-corner", "css-images-4", Def::Prod("[left | right] || [top | bottom]")),
    p("skew()", "css-transforms-1", Def::Prod("skew( [ <angle> | <zero> ] , [ <angle> | <zero> ]? )")),
    p("skewX()", "css-transforms-1", Def::Prod("skewX( [ <angle> | <zero> ] )")),
    p("skewY()", "css-transforms-1", Def::Prod("skewY( [ <angle> | <zero> ] )")),
    p("src()", "css-values-4", Def::Prod("src( <string> <url-modifier>* )")),
    p("string()", "css-content-3", Def::Unsupported("named strings (css-content-3 string()) are paged media; not supported yet")),
    p("symbolic-glyph", "css-counter-styles-3", Def::Prod("disc | circle | square | disclosure-open | disclosure-closed")),
    p("symbols()", "css-counter-styles-3", Def::Rule {
        grammar: "symbols( [ alphabetic | numeric ] [ <string> | <image> ]{2,} ) | symbols( [ cyclic | symbolic | fixed ]? [ <string> | <image> ]+ )",
        spec: "symbols( <symbols-type>? [ <string> | <image> ]+ )",
        quote_in: "css-counter-styles-3",
        quote: "If the system is alphabetic or numeric, there must be at least two <string>s or <image>s, or else the function is invalid.",
    }),
    p("system-color", "css-color-4", Def::Builtin(Builtin::SystemColor, "The <system-color> keywords")),
    p("system-font-family-name", "css-fonts-4", Def::Prod("caption | icon | menu | message-box | small-caption | status-bar")),
    p("target", "css-content-3", Def::Unsupported("cross-references (css-content-3 target-counter() and the rest) need link targets; not supported yet")),
    p("transform-function", "css-transforms-1", Def::Prose("<matrix()> | <translate()> | <translateX()> | <translateY()> | <scale()> | <scaleX()> | <scaleY()> | <rotate()> | <skew()> | <skewX()> | <skewY()>", "The value of the 'transform' property is a list of <transform-function>. The set of allowed transform functions is given below.")),
    p("transform-list", "css-transforms-1", Def::Prod("<transform-function>+")),
    p("translate()", "css-transforms-1", Def::Prod("translate( <length-percentage> , <length-percentage>? )")),
    p("translateX()", "css-transforms-1", Def::Prod("translateX( <length-percentage> )")),
    p("translateY()", "css-transforms-1", Def::Prod("translateY( <length-percentage> )")),
    p("url", "css-values-4", Def::Prod("<url()> | <src()>")),
    p("url()", "css-values-4", Def::Prod("url( <string> <url-modifier>* ) | <url-token>")),
    p("url-modifier", "css-values-4", Def::Unsupported("css-values-4 defines no <url-modifier>s")),
    p("url-set", "css-ui-4", Def::Prose("image-set( [ [ <url> | <string> ] [ <resolution> || type(<string>) ]? ]# )", "<url-set> is a limited version of image-set(), where the <image> sub-production is restricted to <url> only.")),
    p("visual-box", "css-box-4", Def::Prod("content-box | padding-box | border-box")),
    p("xyz-params", "css-color-4", Def::Prod("<xyz-space> [ <number> | <percentage> | none ]{3}")),
    p("xyz-space", "css-color-4", Def::Prod("xyz | xyz-d50 | xyz-d65")),
];
