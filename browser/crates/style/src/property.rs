//! The property table: every longhand the engine knows, with whether it is
//! inherited and its initial value (css-cascade-5 7.1 and 7.2), the
//! shorthands that set them (css-cascade-5 3), the legacy name aliases
//! (css-cascade-5 3.1), and the CSS-wide keywords (css-cascade-5 7.3).
//!
//! Each entry cites the spec whose property definition table it copies,
//! at csswg-drafts commit 7027ab6; `tests/property_table.rs` reads those
//! tables (fetched by `scripts/fetch-test-suites.sh`) and checks every
//! entry against them. Where two levels of a spec define a property, the
//! newer level is cited (css-display-4, css-box-4, css-text-4; css-align-3
//! over css-flexbox-1).
//!
//! The table starts with what layout needs: the box model, positioning,
//! display, flex and alignment, fonts and text, backgrounds and borders,
//! lists, tables, writing modes and a few others. A property outside it is
//! unknown, and its declarations are dropped as invalid, as a browser drops
//! any property it doesn't support. Not yet in the table, each for a later
//! step: the logical properties (`inline-size`, `margin-block` and the rest,
//! which need logical property groups), grid, animations and transitions,
//! SVG properties, `clip`, masking, and the legacy shorthands
//! (`page-break-*`).
//!
//! `font-family`'s initial value "depends on user agent": this engine's is
//! `serif`.

use optimal_css::{Declaration, Kind, Parsed, Token, Validity, Values};

/// A longhand property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Longhand {
    /// Its name, lowercase.
    pub name: &'static str,
    /// Whether it is an inherited property.
    pub inherited: bool,
    /// Its initial value, as the definition table writes it.
    pub initial: &'static str,
    /// Its value grammar, as the definition table writes it (markup
    /// removed), except for the entries of [`GRAMMAR_DEVIATIONS`].
    pub value: &'static str,
    /// The csswg-drafts directory of the spec that defines it.
    pub spec: &'static str,
}

/// Why a property's grammar here differs from its definition table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The spec states a rule in running text, quoted here (found in the
    /// spec `quote_in`), which the grammar here applies.
    Rule {
        /// The csswg-drafts directory of the spec the quote is from.
        quote_in: &'static str,
        /// The rule, quoted.
        quote: &'static str,
    },
    /// Left out for now, and why.
    NotYet(&'static str),
}

/// A property whose grammar here differs from its definition table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deviation {
    /// The property.
    pub property: &'static str,
    /// The definition table's value text.
    pub spec: &'static str,
    /// Why the grammar here differs.
    pub why: Why,
}

/// Every property whose grammar here differs from its definition table.
pub const GRAMMAR_DEVIATIONS: &[Deviation] = &[
    Deviation {
        property: "flex",
        spec: "none | [ <'flex-grow'> <'flex-shrink'>? || <'flex-basis'> ]",
        why: Why::Rule {
            quote_in: "css-flexbox-1",
            quote: "A unitless zero that is not already preceded by two flex factors must be interpreted as a flex factor.",
        },
    },
    Deviation {
        property: "overflow",
        spec: "<'overflow-block'>{1,2}",
        why: Why::NotYet(
            "overflow-block is a logical property, not in the table yet; its grammar is overflow-x's",
        ),
    },
    Deviation {
        property: "quotes",
        spec: "auto | none | bikeshed-this | [ <string> <string> ]+",
        why: Why::NotYet(
            "bikeshed-this is a placeholder name for a value whose name is still open \
             (csswg-drafts issue 5478); it is left out until it is named",
        ),
    },
];

/// An index into [`LONGHANDS`].
pub type LonghandId = u16;

/// Every longhand, sorted by name.
#[rustfmt::skip]
pub const LONGHANDS: &[Longhand] = &[
    Longhand { name: "accent-color", inherited: true, initial: "auto", value: "auto | <color>", spec: "css-ui-4" },
    Longhand { name: "align-content", inherited: false, initial: "normal", value: "normal | <baseline-position> | <content-distribution> | <overflow-position>? <content-position>", spec: "css-align-3" },
    Longhand { name: "align-items", inherited: false, initial: "normal", value: "normal | stretch | <baseline-position> | <overflow-position>? <self-position>", spec: "css-align-3" },
    Longhand { name: "align-self", inherited: false, initial: "auto", value: "auto | <overflow-position>? [ normal | <self-position> ]| stretch | <baseline-position>", spec: "css-align-3" },
    Longhand { name: "alignment-baseline", inherited: false, initial: "baseline", value: "baseline | <baseline-metric>", spec: "css-inline-3" },
    Longhand { name: "appearance", inherited: false, initial: "none", value: "none | auto | base | base-select | <compat-auto> | <compat-special>", spec: "css-ui-4" },
    Longhand { name: "background-attachment", inherited: false, initial: "scroll", value: "<attachment>#", spec: "css-backgrounds-3" },
    Longhand { name: "background-clip", inherited: false, initial: "border-box", value: "<visual-box>#", spec: "css-backgrounds-3" },
    Longhand { name: "background-color", inherited: false, initial: "transparent", value: "<color>", spec: "css-backgrounds-3" },
    Longhand { name: "background-image", inherited: false, initial: "none", value: "<bg-image>#", spec: "css-backgrounds-3" },
    Longhand { name: "background-origin", inherited: false, initial: "padding-box", value: "<visual-box>#", spec: "css-backgrounds-3" },
    Longhand { name: "background-position", inherited: false, initial: "0% 0%", value: "<bg-position>#", spec: "css-backgrounds-3" },
    Longhand { name: "background-repeat", inherited: false, initial: "repeat", value: "<repeat-style>#", spec: "css-backgrounds-3" },
    Longhand { name: "background-size", inherited: false, initial: "auto", value: "<bg-size>#", spec: "css-backgrounds-3" },
    Longhand { name: "baseline-shift", inherited: false, initial: "0", value: "<length-percentage> | sub | super | top | center | bottom", spec: "css-inline-3" },
    Longhand { name: "baseline-source", inherited: false, initial: "auto", value: "auto | first | last", spec: "css-inline-3" },
    Longhand { name: "border-bottom-color", inherited: false, initial: "currentColor", value: "<color>", spec: "css-backgrounds-3" },
    Longhand { name: "border-bottom-left-radius", inherited: false, initial: "0", value: "<length-percentage [0,∞]>{1,2}", spec: "css-backgrounds-3" },
    Longhand { name: "border-bottom-right-radius", inherited: false, initial: "0", value: "<length-percentage [0,∞]>{1,2}", spec: "css-backgrounds-3" },
    Longhand { name: "border-bottom-style", inherited: false, initial: "none", value: "<line-style>", spec: "css-backgrounds-3" },
    Longhand { name: "border-bottom-width", inherited: false, initial: "medium", value: "<line-width>", spec: "css-backgrounds-3" },
    Longhand { name: "border-collapse", inherited: true, initial: "separate", value: "separate | collapse", spec: "css-tables-3" },
    Longhand { name: "border-image-outset", inherited: false, initial: "0", value: "[ <length [0,∞]> | <number [0,∞]> ]{1,4}", spec: "css-backgrounds-3" },
    Longhand { name: "border-image-repeat", inherited: false, initial: "stretch", value: "[ stretch | repeat | round | space ]{1,2}", spec: "css-backgrounds-3" },
    Longhand { name: "border-image-slice", inherited: false, initial: "100%", value: "[<number [0,∞]> | <percentage [0,∞]>]{1,4} && fill?", spec: "css-backgrounds-3" },
    Longhand { name: "border-image-source", inherited: false, initial: "none", value: "none | <image>", spec: "css-backgrounds-3" },
    Longhand { name: "border-image-width", inherited: false, initial: "1", value: "[ <length-percentage [0,∞]> | <number [0,∞]> | auto ]{1,4}", spec: "css-backgrounds-3" },
    Longhand { name: "border-left-color", inherited: false, initial: "currentColor", value: "<color>", spec: "css-backgrounds-3" },
    Longhand { name: "border-left-style", inherited: false, initial: "none", value: "<line-style>", spec: "css-backgrounds-3" },
    Longhand { name: "border-left-width", inherited: false, initial: "medium", value: "<line-width>", spec: "css-backgrounds-3" },
    Longhand { name: "border-right-color", inherited: false, initial: "currentColor", value: "<color>", spec: "css-backgrounds-3" },
    Longhand { name: "border-right-style", inherited: false, initial: "none", value: "<line-style>", spec: "css-backgrounds-3" },
    Longhand { name: "border-right-width", inherited: false, initial: "medium", value: "<line-width>", spec: "css-backgrounds-3" },
    Longhand { name: "border-spacing", inherited: true, initial: "0px 0px", value: "<length [0,∞]>{1,2}", spec: "css-tables-3" },
    Longhand { name: "border-top-color", inherited: false, initial: "currentColor", value: "<color>", spec: "css-backgrounds-3" },
    Longhand { name: "border-top-left-radius", inherited: false, initial: "0", value: "<length-percentage [0,∞]>{1,2}", spec: "css-backgrounds-3" },
    Longhand { name: "border-top-right-radius", inherited: false, initial: "0", value: "<length-percentage [0,∞]>{1,2}", spec: "css-backgrounds-3" },
    Longhand { name: "border-top-style", inherited: false, initial: "none", value: "<line-style>", spec: "css-backgrounds-3" },
    Longhand { name: "border-top-width", inherited: false, initial: "medium", value: "<line-width>", spec: "css-backgrounds-3" },
    Longhand { name: "bottom", inherited: false, initial: "auto", value: "auto | <length-percentage>", spec: "css-position-3" },
    Longhand { name: "box-decoration-break", inherited: false, initial: "slice", value: "slice | clone", spec: "css-break-3" },
    Longhand { name: "box-shadow", inherited: false, initial: "none", value: "none | <shadow>#", spec: "css-backgrounds-3" },
    Longhand { name: "box-sizing", inherited: false, initial: "content-box", value: "content-box | border-box", spec: "css-sizing-3" },
    Longhand { name: "break-after", inherited: false, initial: "auto", value: "auto | avoid | avoid-page | page | left | right | recto | verso | avoid-column | column | avoid-region | region", spec: "css-break-3" },
    Longhand { name: "break-before", inherited: false, initial: "auto", value: "auto | avoid | avoid-page | page | left | right | recto | verso | avoid-column | column | avoid-region | region", spec: "css-break-3" },
    Longhand { name: "break-inside", inherited: false, initial: "auto", value: "auto | avoid | avoid-page | avoid-column | avoid-region", spec: "css-break-3" },
    Longhand { name: "caption-side", inherited: true, initial: "top", value: "top | bottom", spec: "css-tables-3" },
    Longhand { name: "caret-color", inherited: true, initial: "auto", value: "auto | <color> [auto | <color>]?", spec: "css-ui-4" },
    Longhand { name: "clear", inherited: false, initial: "none", value: "none | left | right | both | inherit", spec: "css2" },
    Longhand { name: "color", inherited: true, initial: "CanvasText", value: "<color>", spec: "css-color-4" },
    Longhand { name: "content", inherited: false, initial: "normal", value: "normal | none | [ <content-replacement> | <content-list> ] [/ <alt-text> ]?", spec: "css-content-3" },
    Longhand { name: "counter-increment", inherited: false, initial: "none", value: "[ <counter-name> <integer>? ]+ | none", spec: "css-lists-3" },
    Longhand { name: "counter-reset", inherited: false, initial: "none", value: "[ <counter-name> <integer>? | <reversed-counter-name> <integer>? ]+ | none", spec: "css-lists-3" },
    Longhand { name: "counter-set", inherited: false, initial: "none", value: "[ <counter-name> <integer>? ]+ | none", spec: "css-lists-3" },
    Longhand { name: "cursor", inherited: true, initial: "auto", value: "[<cursor-image>,]* <cursor-predefined>", spec: "css-ui-4" },
    Longhand { name: "direction", inherited: true, initial: "ltr", value: "ltr | rtl", spec: "css-writing-modes-4" },
    Longhand { name: "display", inherited: false, initial: "inline", value: "[ <display-outside> || <display-inside> ] | <display-listitem> | <display-internal> | <display-box> | <display-legacy>", spec: "css-display-4" },
    Longhand { name: "dominant-baseline", inherited: true, initial: "auto", value: "auto | <baseline-metric>", spec: "css-inline-3" },
    Longhand { name: "empty-cells", inherited: true, initial: "show", value: "show | hide", spec: "css-tables-3" },
    Longhand { name: "flex-basis", inherited: false, initial: "auto", value: "content | <'width'>", spec: "css-flexbox-1" },
    Longhand { name: "flex-direction", inherited: false, initial: "row", value: "row | row-reverse | column | column-reverse", spec: "css-flexbox-1" },
    Longhand { name: "flex-grow", inherited: false, initial: "0", value: "<number [0,∞]>", spec: "css-flexbox-1" },
    Longhand { name: "flex-shrink", inherited: false, initial: "1", value: "<number [0,∞]>", spec: "css-flexbox-1" },
    Longhand { name: "flex-wrap", inherited: false, initial: "nowrap", value: "nowrap | wrap | wrap-reverse", spec: "css-flexbox-1" },
    Longhand { name: "float", inherited: false, initial: "none", value: "left | right | none | inherit", spec: "css2" },
    Longhand { name: "font-family", inherited: true, initial: "serif", value: "[ <font-family-name> | <generic-font-family> ]#", spec: "css-fonts-4" },
    Longhand { name: "font-feature-settings", inherited: true, initial: "normal", value: "normal | <feature-tag-value>#", spec: "css-fonts-4" },
    Longhand { name: "font-kerning", inherited: true, initial: "auto", value: "auto | normal | none", spec: "css-fonts-4" },
    Longhand { name: "font-language-override", inherited: true, initial: "normal", value: "normal | <string>", spec: "css-fonts-4" },
    Longhand { name: "font-optical-sizing", inherited: true, initial: "auto", value: "auto | none", spec: "css-fonts-4" },
    Longhand { name: "font-palette", inherited: true, initial: "normal", value: "normal | light | dark | <palette-identifier> | <palette-mix()>", spec: "css-fonts-4" },
    Longhand { name: "font-size", inherited: true, initial: "medium", value: "<absolute-size> | <relative-size> | <length-percentage [0,∞]> | math", spec: "css-fonts-4" },
    Longhand { name: "font-size-adjust", inherited: true, initial: "none", value: "none | <number [0,∞]>", spec: "css-fonts-4" },
    Longhand { name: "font-style", inherited: true, initial: "normal", value: "normal | italic | left | right | oblique <angle [-90deg,90deg]>?", spec: "css-fonts-4" },
    Longhand { name: "font-synthesis-position", inherited: true, initial: "auto", value: "auto | none", spec: "css-fonts-4" },
    Longhand { name: "font-synthesis-small-caps", inherited: true, initial: "auto", value: "auto | none", spec: "css-fonts-4" },
    Longhand { name: "font-synthesis-style", inherited: true, initial: "auto", value: "auto | none | oblique-only", spec: "css-fonts-4" },
    Longhand { name: "font-synthesis-weight", inherited: true, initial: "auto", value: "auto | none", spec: "css-fonts-4" },
    Longhand { name: "font-variant-alternates", inherited: true, initial: "normal", value: "normal | [ stylistic(<font-feature-value-name>) || historical-forms || styleset(<font-feature-value-name>#) || character-variant(<font-feature-value-name>#) || swash(<font-feature-value-name>) || ornaments(<font-feature-value-name>) || annotation(<font-feature-value-name>) ]", spec: "css-fonts-4" },
    Longhand { name: "font-variant-caps", inherited: true, initial: "normal", value: "normal | small-caps | all-small-caps | petite-caps | all-petite-caps | unicase | titling-caps", spec: "css-fonts-4" },
    Longhand { name: "font-variant-east-asian", inherited: true, initial: "normal", value: "normal | [ <east-asian-variant-values> || <east-asian-width-values> || ruby ]", spec: "css-fonts-4" },
    Longhand { name: "font-variant-emoji", inherited: true, initial: "normal", value: "normal | text | emoji | unicode", spec: "css-fonts-4" },
    Longhand { name: "font-variant-ligatures", inherited: true, initial: "normal", value: "normal | none | [ <common-lig-values> || <discretionary-lig-values> || <historical-lig-values> || <contextual-alt-values> ]", spec: "css-fonts-4" },
    Longhand { name: "font-variant-numeric", inherited: true, initial: "normal", value: "normal | [ <numeric-figure-values> || <numeric-spacing-values> || <numeric-fraction-values> || ordinal || slashed-zero ]", spec: "css-fonts-4" },
    Longhand { name: "font-variant-position", inherited: true, initial: "normal", value: "normal | sub | super", spec: "css-fonts-4" },
    Longhand { name: "font-variation-settings", inherited: true, initial: "normal", value: "normal | [ <opentype-tag> <number> ]#", spec: "css-fonts-4" },
    Longhand { name: "font-weight", inherited: true, initial: "normal", value: "<font-weight-absolute> | bolder | lighter", spec: "css-fonts-4" },
    Longhand { name: "font-width", inherited: true, initial: "normal", value: "normal | <percentage [0,∞]> | ultra-condensed | extra-condensed | condensed | semi-condensed | semi-expanded | expanded | extra-expanded | ultra-expanded", spec: "css-fonts-4" },
    Longhand { name: "hanging-punctuation", inherited: true, initial: "none", value: "none | [ first || [ force-end | allow-end ] || last ]", spec: "css-text-4" },
    Longhand { name: "height", inherited: false, initial: "auto", value: "auto | <box-size>", spec: "css-sizing-3" },
    Longhand { name: "hyphens", inherited: true, initial: "manual", value: "none | manual | auto", spec: "css-text-4" },
    Longhand { name: "justify-content", inherited: false, initial: "normal", value: "normal | <content-distribution> | <overflow-position>? [ <content-position> | left | right ]", spec: "css-align-3" },
    Longhand { name: "justify-items", inherited: false, initial: "legacy", value: "normal | stretch | <baseline-position> | <overflow-position>? [ <self-position> | left | right ] | legacy | legacy && [ left | right | center ]", spec: "css-align-3" },
    Longhand { name: "justify-self", inherited: false, initial: "auto", value: "auto | <overflow-position>? [ normal | <self-position> | left | right ] | stretch | <baseline-position>", spec: "css-align-3" },
    Longhand { name: "left", inherited: false, initial: "auto", value: "auto | <length-percentage>", spec: "css-position-3" },
    Longhand { name: "letter-spacing", inherited: true, initial: "normal", value: "normal | <length-percentage>", spec: "css-text-4" },
    Longhand { name: "line-break", inherited: true, initial: "auto", value: "auto | loose | normal | strict | anywhere", spec: "css-text-4" },
    Longhand { name: "line-height", inherited: true, initial: "normal", value: "normal | <number [0,∞]> | <length-percentage [0,∞]>", spec: "css-inline-3" },
    Longhand { name: "list-style-image", inherited: true, initial: "none", value: "<image> | none", spec: "css-lists-3" },
    Longhand { name: "list-style-position", inherited: true, initial: "outside", value: "inside | outside", spec: "css-lists-3" },
    Longhand { name: "list-style-type", inherited: true, initial: "disc", value: "<counter-style> | <string> | none", spec: "css-lists-3" },
    Longhand { name: "margin-bottom", inherited: false, initial: "0", value: "<length-percentage> | auto", spec: "css-box-4" },
    Longhand { name: "margin-left", inherited: false, initial: "0", value: "<length-percentage> | auto", spec: "css-box-4" },
    Longhand { name: "margin-right", inherited: false, initial: "0", value: "<length-percentage> | auto", spec: "css-box-4" },
    Longhand { name: "margin-top", inherited: false, initial: "0", value: "<length-percentage> | auto", spec: "css-box-4" },
    Longhand { name: "max-height", inherited: false, initial: "none", value: "none | <box-size>", spec: "css-sizing-3" },
    Longhand { name: "max-width", inherited: false, initial: "none", value: "none | <box-size>", spec: "css-sizing-3" },
    Longhand { name: "min-height", inherited: false, initial: "auto", value: "auto | <box-size>", spec: "css-sizing-3" },
    Longhand { name: "min-width", inherited: false, initial: "auto", value: "auto | <box-size>", spec: "css-sizing-3" },
    Longhand { name: "opacity", inherited: false, initial: "1", value: "<opacity-value>", spec: "css-color-4" },
    Longhand { name: "order", inherited: false, initial: "0", value: "<integer>", spec: "css-display-4" },
    Longhand { name: "orphans", inherited: true, initial: "2", value: "<integer [1,∞]>", spec: "css-break-3" },
    Longhand { name: "outline-color", inherited: false, initial: "auto", value: "auto | <'border-top-color'>", spec: "css-ui-4" },
    Longhand { name: "outline-offset", inherited: false, initial: "0", value: "<length> | inset", spec: "css-ui-4" },
    Longhand { name: "outline-style", inherited: false, initial: "none", value: "auto | <outline-line-style>", spec: "css-ui-4" },
    Longhand { name: "outline-width", inherited: false, initial: "medium", value: "<line-width>", spec: "css-ui-4" },
    Longhand { name: "overflow-wrap", inherited: true, initial: "normal", value: "normal | break-word | anywhere", spec: "css-text-4" },
    Longhand { name: "overflow-x", inherited: false, initial: "visible", value: "visible | hidden | clip | scroll | auto", spec: "css-overflow-3" },
    Longhand { name: "overflow-y", inherited: false, initial: "visible", value: "visible | hidden | clip | scroll | auto", spec: "css-overflow-3" },
    Longhand { name: "padding-bottom", inherited: false, initial: "0", value: "<length-percentage [0,∞]>", spec: "css-box-4" },
    Longhand { name: "padding-left", inherited: false, initial: "0", value: "<length-percentage [0,∞]>", spec: "css-box-4" },
    Longhand { name: "padding-right", inherited: false, initial: "0", value: "<length-percentage [0,∞]>", spec: "css-box-4" },
    Longhand { name: "padding-top", inherited: false, initial: "0", value: "<length-percentage [0,∞]>", spec: "css-box-4" },
    Longhand { name: "pointer-events", inherited: true, initial: "auto", value: "auto | none", spec: "css-ui-4" },
    Longhand { name: "position", inherited: false, initial: "static", value: "static | relative | absolute | sticky | fixed", spec: "css-position-3" },
    Longhand { name: "quotes", inherited: true, initial: "auto", value: "auto | none | [ <string> <string> ]+", spec: "css-content-3" },
    Longhand { name: "resize", inherited: false, initial: "none", value: "none | both | horizontal | vertical | block | inline", spec: "css-ui-4" },
    Longhand { name: "right", inherited: false, initial: "auto", value: "auto | <length-percentage>", spec: "css-position-3" },
    Longhand { name: "tab-size", inherited: true, initial: "8", value: "<number [0,∞]> | <length [0,∞]>", spec: "css-text-4" },
    Longhand { name: "table-layout", inherited: false, initial: "auto", value: "auto | fixed", spec: "css-tables-3" },
    Longhand { name: "text-align-all", inherited: true, initial: "start", value: "start | end | left | right | center | <string> | justify | match-parent", spec: "css-text-4" },
    Longhand { name: "text-align-last", inherited: true, initial: "auto", value: "auto | start | end | left | right | center | justify | match-parent", spec: "css-text-4" },
    Longhand { name: "text-indent", inherited: true, initial: "0", value: "[ <length-percentage> ] && hanging? && each-line?", spec: "css-text-4" },
    Longhand { name: "text-justify", inherited: true, initial: "auto", value: "[ auto | none | inter-word | inter-character | ruby ] || no-compress", spec: "css-text-4" },
    Longhand { name: "text-orientation", inherited: true, initial: "mixed", value: "mixed | upright | sideways", spec: "css-writing-modes-4" },
    Longhand { name: "text-overflow", inherited: false, initial: "clip", value: "clip | ellipsis", spec: "css-overflow-3" },
    Longhand { name: "text-transform", inherited: true, initial: "none", value: "none | [capitalize | uppercase | lowercase ] || full-width || full-size-kana | math-auto", spec: "css-text-4" },
    Longhand { name: "text-wrap-mode", inherited: true, initial: "wrap", value: "wrap | nowrap", spec: "css-text-4" },
    Longhand { name: "text-wrap-style", inherited: true, initial: "auto", value: "auto | balance | stable | pretty | avoid-short-last-line", spec: "css-text-4" },
    Longhand { name: "top", inherited: false, initial: "auto", value: "auto | <length-percentage>", spec: "css-position-3" },
    Longhand { name: "transform", inherited: false, initial: "none", value: "none | <transform-list>", spec: "css-transforms-1" },
    Longhand { name: "transform-box", inherited: false, initial: "view-box", value: "content-box | border-box | fill-box | stroke-box | view-box", spec: "css-transforms-1" },
    Longhand { name: "transform-origin", inherited: false, initial: "50% 50%", value: "[ left | center | right | top | bottom | <length-percentage> ] | [ left | center | right | <length-percentage> ] [ top | center | bottom | <length-percentage> ] <length>? | [ [ center | left | right ] && [ center | top | bottom ] ] <length>?", spec: "css-transforms-1" },
    Longhand { name: "unicode-bidi", inherited: false, initial: "normal", value: "normal | embed | isolate | bidi-override | isolate-override | plaintext", spec: "css-writing-modes-4" },
    Longhand { name: "user-select", inherited: false, initial: "auto", value: "auto | text | none | contain | all", spec: "css-ui-4" },
    Longhand { name: "visibility", inherited: true, initial: "visible", value: "visible | hidden | force-hidden | collapse", spec: "css-display-4" },
    Longhand { name: "white-space-collapse", inherited: true, initial: "collapse", value: "collapse | discard | preserve | preserve-breaks | preserve-spaces | break-spaces", spec: "css-text-4" },
    Longhand { name: "white-space-trim", inherited: false, initial: "none", value: "none | discard-before || discard-after || discard-inner", spec: "css-text-4" },
    Longhand { name: "widows", inherited: true, initial: "2", value: "<integer [1,∞]>", spec: "css-break-3" },
    Longhand { name: "width", inherited: false, initial: "auto", value: "auto | <box-size>", spec: "css-sizing-3" },
    Longhand { name: "word-break", inherited: true, initial: "normal", value: "normal | break-all | keep-all | manual | auto-phrase | break-word", spec: "css-text-4" },
    Longhand { name: "word-spacing", inherited: true, initial: "normal", value: "normal | <length-percentage>", spec: "css-text-4" },
    Longhand { name: "writing-mode", inherited: true, initial: "horizontal-tb", value: "horizontal-tb | vertical-rl | vertical-lr | sideways-rl | sideways-lr", spec: "css-writing-modes-4" },
    Longhand { name: "z-index", inherited: false, initial: "auto", value: "auto | <integer> | inherit", spec: "css2" },
];

/// A shorthand property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shorthand {
    /// Its name, lowercase.
    pub name: &'static str,
    /// Its value grammar, as the definition table writes it (markup
    /// removed), except for the entries of [`GRAMMAR_DEVIATIONS`].
    pub value: &'static str,
    /// The longhands it sets, reset-only sub-properties included.
    pub longhands: &'static [&'static str],
    /// The csswg-drafts directory of the spec that defines it.
    pub spec: &'static str,
}

const SIDES_WIDTH: &[&str] = &[
    "border-top-width",
    "border-right-width",
    "border-bottom-width",
    "border-left-width",
];
const SIDES_STYLE: &[&str] = &[
    "border-top-style",
    "border-right-style",
    "border-bottom-style",
    "border-left-style",
];
const SIDES_COLOR: &[&str] = &[
    "border-top-color",
    "border-right-color",
    "border-bottom-color",
    "border-left-color",
];
const BORDER_IMAGE: &[&str] = &[
    "border-image-source",
    "border-image-slice",
    "border-image-width",
    "border-image-outset",
    "border-image-repeat",
];
const FONT_VARIANT: &[&str] = &[
    "font-variant-ligatures",
    "font-variant-position",
    "font-variant-caps",
    "font-variant-numeric",
    "font-variant-alternates",
    "font-variant-east-asian",
    "font-variant-emoji",
];

/// Every shorthand except `all` (see [`Target::All`]), sorted by name.
pub const SHORTHANDS: &[Shorthand] = &[
    Shorthand {
        name: "background",
        value: "<bg-layer>#? , <final-bg-layer>",
        longhands: &[
            "background-color",
            "background-image",
            "background-repeat",
            "background-attachment",
            "background-position",
            "background-clip",
            "background-origin",
            "background-size",
        ],
        spec: "css-backgrounds-3",
    },
    Shorthand {
        // css-backgrounds-3 4.4: sets all four borders' width, style and
        // color, and resets 'border-image' to its initial value.
        name: "border",
        value: "<line-width> || <line-style> || <color>",
        longhands: &[
            "border-top-width",
            "border-right-width",
            "border-bottom-width",
            "border-left-width",
            "border-top-style",
            "border-right-style",
            "border-bottom-style",
            "border-left-style",
            "border-top-color",
            "border-right-color",
            "border-bottom-color",
            "border-left-color",
            "border-image-source",
            "border-image-slice",
            "border-image-width",
            "border-image-outset",
            "border-image-repeat",
        ],
        spec: "css-backgrounds-3",
    },
    Shorthand {
        name: "border-bottom",
        value: "<line-width> || <line-style> || <color>",
        longhands: &[
            "border-bottom-width",
            "border-bottom-style",
            "border-bottom-color",
        ],
        spec: "css-backgrounds-3",
    },
    Shorthand {
        name: "border-color",
        value: "<color>{1,4}",
        longhands: SIDES_COLOR,
        spec: "css-backgrounds-3",
    },
    Shorthand {
        name: "border-image",
        value: "<'border-image-source'> || <'border-image-slice'> [ / <'border-image-width'> | / <'border-image-width'>? / <'border-image-outset'> ]? || <'border-image-repeat'>",
        longhands: BORDER_IMAGE,
        spec: "css-backgrounds-3",
    },
    Shorthand {
        name: "border-left",
        value: "<line-width> || <line-style> || <color>",
        longhands: &[
            "border-left-width",
            "border-left-style",
            "border-left-color",
        ],
        spec: "css-backgrounds-3",
    },
    Shorthand {
        name: "border-radius",
        value: "<length-percentage [0,∞]>{1,4} [ / <length-percentage [0,∞]>{1,4} ]?",
        longhands: &[
            "border-top-left-radius",
            "border-top-right-radius",
            "border-bottom-right-radius",
            "border-bottom-left-radius",
        ],
        spec: "css-backgrounds-3",
    },
    Shorthand {
        name: "border-right",
        value: "<line-width> || <line-style> || <color>",
        longhands: &[
            "border-right-width",
            "border-right-style",
            "border-right-color",
        ],
        spec: "css-backgrounds-3",
    },
    Shorthand {
        name: "border-style",
        value: "<line-style>{1,4}",
        longhands: SIDES_STYLE,
        spec: "css-backgrounds-3",
    },
    Shorthand {
        name: "border-top",
        value: "<line-width> || <line-style> || <color>",
        longhands: &["border-top-width", "border-top-style", "border-top-color"],
        spec: "css-backgrounds-3",
    },
    Shorthand {
        name: "border-width",
        value: "<line-width>{1,4}",
        longhands: SIDES_WIDTH,
        spec: "css-backgrounds-3",
    },
    Shorthand {
        name: "flex",
        value: "none | [ <'flex-grow'> <'flex-shrink'>? || <_flex-basis-with-unit> ] | <'flex-grow'> <'flex-shrink'> <'flex-basis'>",
        longhands: &["flex-grow", "flex-shrink", "flex-basis"],
        spec: "css-flexbox-1",
    },
    Shorthand {
        name: "flex-flow",
        value: "<'flex-direction'> || <'flex-wrap'>",
        longhands: &["flex-direction", "flex-wrap"],
        spec: "css-flexbox-1",
    },
    Shorthand {
        // css-fonts-4 2.8: "Set Explicitly" and "Reset Implicitly".
        name: "font",
        value: "[ [ <'font-style'> || <font-variant-css2> || <'font-weight'> || <font-width-css3> ]? <'font-size'> [ / <'line-height'> ]? <'font-family'># ] | <system-font-family-name>",
        longhands: &[
            "font-family",
            "font-size",
            "font-width",
            "font-style",
            "font-variant-caps",
            "font-weight",
            "line-height",
            "font-feature-settings",
            "font-kerning",
            "font-language-override",
            "font-optical-sizing",
            "font-size-adjust",
            "font-variant-alternates",
            "font-variant-east-asian",
            "font-variant-emoji",
            "font-variant-ligatures",
            "font-variant-numeric",
            "font-variant-position",
            "font-variation-settings",
        ],
        spec: "css-fonts-4",
    },
    Shorthand {
        name: "font-synthesis",
        value: "none | [ weight || style || small-caps || position]",
        longhands: &[
            "font-synthesis-weight",
            "font-synthesis-style",
            "font-synthesis-small-caps",
            "font-synthesis-position",
        ],
        spec: "css-fonts-4",
    },
    Shorthand {
        name: "font-variant",
        value: "normal | none | [ [ <common-lig-values> || <discretionary-lig-values> || <historical-lig-values> || <contextual-alt-values> ] || [ small-caps | all-small-caps | petite-caps | all-petite-caps | unicase | titling-caps ] || [ stylistic(<font-feature-value-name>) || historical-forms || styleset(<font-feature-value-name>#) || character-variant(<font-feature-value-name>#) || swash(<font-feature-value-name>) || ornaments(<font-feature-value-name>) || annotation(<font-feature-value-name>) ] || [ <numeric-figure-values> || <numeric-spacing-values> || <numeric-fraction-values> || ordinal || slashed-zero ] || [ <east-asian-variant-values> || <east-asian-width-values> || ruby ] || [ sub | super ] || [ text | emoji | unicode ] ]",
        longhands: FONT_VARIANT,
        spec: "css-fonts-4",
    },
    Shorthand {
        name: "inset",
        value: "<'top'>{1,4}",
        longhands: &["top", "right", "bottom", "left"],
        spec: "css-position-3",
    },
    Shorthand {
        name: "list-style",
        value: "<'list-style-position'> || <'list-style-image'> || <'list-style-type'>",
        longhands: &["list-style-position", "list-style-image", "list-style-type"],
        spec: "css-lists-3",
    },
    Shorthand {
        name: "margin",
        value: "<'margin-top'>{1,4}",
        longhands: &["margin-top", "margin-right", "margin-bottom", "margin-left"],
        spec: "css-box-4",
    },
    Shorthand {
        name: "outline",
        value: "<'outline-width'> || <'outline-style'> || <'outline-color'>",
        longhands: &["outline-color", "outline-style", "outline-width"],
        spec: "css-ui-4",
    },
    Shorthand {
        name: "overflow",
        value: "<'overflow-x'>{1,2}",
        longhands: &["overflow-x", "overflow-y"],
        spec: "css-overflow-3",
    },
    Shorthand {
        name: "padding",
        value: "<'padding-top'>{1,4}",
        longhands: &[
            "padding-top",
            "padding-right",
            "padding-bottom",
            "padding-left",
        ],
        spec: "css-box-4",
    },
    Shorthand {
        name: "place-content",
        value: "<'align-content'> <'justify-content'>?",
        longhands: &["align-content", "justify-content"],
        spec: "css-align-3",
    },
    Shorthand {
        name: "place-items",
        value: "<'align-items'> <'justify-items'>?",
        longhands: &["align-items", "justify-items"],
        spec: "css-align-3",
    },
    Shorthand {
        name: "place-self",
        value: "<'align-self'> <'justify-self'>?",
        longhands: &["align-self", "justify-self"],
        spec: "css-align-3",
    },
    Shorthand {
        name: "text-align",
        value: "start | end | left | right | center | <string> | justify | match-parent | justify-all",
        longhands: &["text-align-all", "text-align-last"],
        spec: "css-text-4",
    },
    Shorthand {
        name: "text-wrap",
        value: "<'text-wrap-mode'> || <'text-wrap-style'>",
        longhands: &["text-wrap-mode", "text-wrap-style"],
        spec: "css-text-4",
    },
    Shorthand {
        name: "vertical-align",
        value: "[ first | last] || <'alignment-baseline'> || <'baseline-shift'>",
        longhands: &["alignment-baseline", "baseline-shift", "baseline-source"],
        spec: "css-inline-3",
    },
    Shorthand {
        name: "white-space",
        value: "normal | pre | pre-wrap | pre-line | <'white-space-collapse'> || <'text-wrap-mode'> || <'white-space-trim'>",
        longhands: &["white-space-collapse", "text-wrap-mode", "white-space-trim"],
        spec: "css-text-4",
    },
];

/// Legacy name aliases (css-cascade-5 3.1): (old name, new name, the spec
/// that defines the alias), sorted by old name.
/// The old name is converted to the new one at parse time.
pub const ALIASES: &[(&str, &str, &str)] = &[
    ("font-stretch", "font-width", "css-fonts-4"),
    ("word-wrap", "overflow-wrap", "css-text-4"),
];

/// The two properties `all` doesn't reset (css-cascade-5 3.2).
pub const NOT_RESET_BY_ALL: &[&str] = &["direction", "unicode-bidi"];

/// What a declaration's property name names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// A longhand.
    Longhand(LonghandId),
    /// A shorthand: an index into [`SHORTHANDS`].
    Shorthand(u16),
    /// The `all` shorthand.
    All,
    /// A custom property (`--*`): always inherited, initially the
    /// guaranteed-invalid value (css-variables-2 2).
    Custom,
}

/// The id of a longhand, by its lowercase name.
pub fn longhand(name: &str) -> Option<LonghandId> {
    LONGHANDS
        .binary_search_by(|l| l.name.cmp(name))
        .ok()
        .map(|i| i as LonghandId)
}

/// What a property name names, `None` if it is unknown. `name` is as
/// [`crate::Decl::name`] holds it: lowercase unless custom.
pub fn target(name: &str) -> Option<Target> {
    if name.starts_with("--") {
        return Some(Target::Custom);
    }
    if name == "all" {
        return Some(Target::All);
    }
    let name = match ALIASES.binary_search_by(|a| a.0.cmp(name)) {
        Ok(i) => ALIASES[i].1,
        Err(_) => name,
    };
    if let Some(l) = longhand(name) {
        return Some(Target::Longhand(l));
    }
    SHORTHANDS
        .binary_search_by(|s| s.name.cmp(name))
        .ok()
        .map(|i| Target::Shorthand(i as u16))
}

/// The longhands a declaration of `target` sets, in table order; none for
/// a custom property.
pub fn longhands(target: Target) -> impl Iterator<Item = LonghandId> {
    let (one, many, all): (Option<LonghandId>, &'static [&'static str], bool) = match target {
        Target::Longhand(l) => (Some(l), &[], false),
        Target::Shorthand(s) => (None, SHORTHANDS[s as usize].longhands, false),
        Target::All => (None, &[], true),
        Target::Custom => (None, &[], false),
    };
    let all_ids = (0..if all { LONGHANDS.len() } else { 0 })
        .filter(|&i| !NOT_RESET_BY_ALL.contains(&LONGHANDS[i].name))
        .map(|i| i as LonghandId);
    let many_ids = many
        .iter()
        .map(|n| longhand(n).expect("a shorthand's longhands are in the table"));
    one.into_iter().chain(many_ids).chain(all_ids)
}

/// The CSS-wide keywords (css-cascade-5 7.3; css-values-4 #common-keywords).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Keyword {
    /// `initial`
    Initial,
    /// `inherit`
    Inherit,
    /// `unset`
    Unset,
    /// `revert`
    Revert,
    /// `revert-layer`
    RevertLayer,
    /// `revert-rule`
    RevertRule,
}

impl Keyword {
    /// The keyword an identifier spells, ASCII case-insensitively.
    pub fn parse(ident: &str) -> Option<Keyword> {
        const ALL: &[(&str, Keyword)] = &[
            ("initial", Keyword::Initial),
            ("inherit", Keyword::Inherit),
            ("unset", Keyword::Unset),
            ("revert", Keyword::Revert),
            ("revert-layer", Keyword::RevertLayer),
            ("revert-rule", Keyword::RevertRule),
        ];
        ALL.iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(ident))
            .map(|&(_, k)| k)
    }
}

/// The arbitrary substitution functions (css-values-5 appendix A, with
/// css-variables-2 for `var()`, css-env-1 for `env()` and css-mixins-1
/// for dashed functions, which start with `--`). A value holding one is
/// assumed valid at parse time; substitution is a later step.
const SUBSTITUTION_FUNCTIONS: &[&str] = &[
    "attr",
    "env",
    "ident",
    "if",
    "inherit",
    "random-item",
    "var",
];

fn has_substitution(values: Values<'_, '_>) -> bool {
    // Iterative, so deeply nested values can't exhaust the stack.
    let mut stack = vec![values];
    while let Some(vs) = stack.pop() {
        for v in vs {
            match v.kind() {
                Kind::Function(name, inner) => {
                    if name.starts_with("--")
                        || SUBSTITUTION_FUNCTIONS
                            .iter()
                            .any(|f| f.eq_ignore_ascii_case(name))
                    {
                        return true;
                    }
                    stack.push(inner);
                }
                Kind::Block(_, inner) => stack.push(inner),
                Kind::Token(_) => {}
            }
        }
    }
    false
}

/// A declaration as the cascade reads it: what it sets, and the CSS-wide
/// keyword its value is, if it is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Classified {
    /// What its property name names.
    pub target: Target,
    /// Its value's CSS-wide keyword.
    pub keyword: Option<Keyword>,
}

/// Whether a declaration of `name` (as [`crate::Decl::name`] holds it)
/// with value `values` is valid, and what it is. `None` means invalid:
///
/// - an unknown property name;
/// - a CSS-wide keyword combined with other values (css-cascade-5
///   #shorthand: "these keywords cannot be combined with other values in a
///   single declaration"), looked for at the top level of the value;
/// - `all` with anything but a CSS-wide keyword (its grammar);
/// - an empty value for anything but a custom property.
///
/// A value that is one CSS-wide keyword (ASCII case-insensitive) is that
/// keyword, for custom properties too (css-variables-2 2: the keywords
/// trigger explicit defaulting). A value holding an arbitrary substitution
/// function is assumed valid (css-values-5 #arbitrary-substitution). Each
/// property's own grammar isn't checked yet: that comes with value parsing.
pub fn classify(name: &str, values: Values<'_, '_>) -> Option<Classified> {
    let target = target(name)?;
    let mut vs = values;
    if let (Some(only), None) = (vs.next(), vs.next())
        && let Kind::Token(Token::Ident(id)) = only.kind()
        && let Some(k) = Keyword::parse(id)
    {
        return Some(Classified {
            target,
            keyword: Some(k),
        });
    }
    let value = Some(Classified {
        target,
        keyword: None,
    });
    if target == Target::Custom || has_substitution(values) {
        return value;
    }
    if target == Target::All || values.is_empty() {
        return None;
    }
    let mut all_values = values;
    let combined = all_values
        .any(|v| matches!(v.kind(), Kind::Token(Token::Ident(id)) if Keyword::parse(id).is_some()));
    if combined || !crate::values::value_matches(target, values) {
        None
    } else {
        value
    }
}

/// The parser's validity checks for style sheets and style attributes
/// (css-syntax-3 "valid in the current context"): a declaration is valid
/// when [`classify`] says so. Rules are checked later, in the walk (see
/// [`crate::collect`]). Declarations inside at-rules that aren't applied
/// yet (`@font-face`, `@keyframes`, `@page`) are checked as properties
/// too, which only changes the contents of rules that are dropped.
#[derive(Clone, Copy, Debug, Default)]
pub struct PropertyValidity;

impl Validity for PropertyValidity {
    fn declaration(&mut self, parsed: &Parsed<'_>, decl: &Declaration, _nested: bool) -> bool {
        let name = crate::collect::property_name(parsed.name(decl.name));
        classify(&name, parsed.values(decl.value.clone())).is_some()
    }
}
