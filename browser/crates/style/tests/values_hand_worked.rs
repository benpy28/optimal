//! Hand-worked value grammar cases (css-values-4 at csswg-drafts 7027ab6,
//! and the specs the property table cites): each checks whether a
//! declaration's value is valid, with the working in a comment.

use optimal_css::parse_component_values;
use optimal_style::property::classify;
use optimal_wtf8::Wtf8;

fn valid(prop: &str, value: &str) -> bool {
    let (parsed, range) = parse_component_values(Wtf8::from_str(value));
    classify(prop, parsed.values(range)).is_some()
}

#[track_caller]
fn yes(prop: &str, value: &str) {
    assert!(valid(prop, value), "{prop}: {value} should be valid");
}

#[track_caller]
fn no(prop: &str, value: &str) {
    assert!(!valid(prop, value), "{prop}: {value} should be invalid");
}

#[test]
fn smoke() {
    yes("width", "10px");
    yes("width", "auto");
    no("width", "-10px");
    yes("width", "calc(100% - 10px)");
    no("width", "calc(100%-10px)");
    yes("color", "red");
    yes("color", "rgb(1, 2, 3)");
    yes("color", "rgb(1 2 3 / 50%)");
    no("color", "rgb(1, 2 3)");
    yes("background", "red");
    yes("background", "url(a.png) no-repeat, red");
    no("background", "red, url(a.png)");
    yes("font", "12px serif");
    yes("font", "italic bold 12px/30px Georgia, serif");
    no("font", "12px");
    yes("margin", "1px 2px 3px 4px");
    no("margin", "1px 2px 3px 4px 5px");
}

#[test]
fn list_style_gradients() {
    // list-style: <'list-style-position'> || <'list-style-image'> ||
    // <'list-style-type'>, in any order; a gradient is an <image>.
    yes(
        "list-style-image",
        "radial-gradient(circle, #006, #00a 90%, #0000af 100%,white 100%)",
    );
    yes("list-style", "disc inside");
    yes("list-style", "inside disc");
    yes("list-style", "disc url(a.png) inside");
    yes("list-style", "disc radial-gradient(red, blue) inside");
    // `outside` is also a <counter-style-name>, a <custom-ident>, so this
    // is a position and a type.
    yes("list-style", "inside outside");
    // Each component at most once: three idents can't fill two slots
    // without one twice.
    no("list-style", "inside outside disc");
}

#[test]
fn calc_white_space() {
    // values-4 10.1: + and - must have white space on both sides; * and /
    // need none.
    yes("width", "calc(1px + 2px)");
    no("width", "calc(1px +2px)");
    no("width", "calc(1px+ 2px)");
    yes("width", "calc(1px*2)");
    yes("width", "calc(2px/2)");
    yes("width", "calc( 1px )");
    // `-2px` is one dimension token, so `1px -2px` is two values with no
    // operator between them.
    no("width", "calc(1px -2px)");
    yes("width", "calc(1px - -2px)");
}

#[test]
fn calc_types() {
    // values-4 10.10: a sum's terms have the same type; a product
    // multiplies types; a quotient's right side is a <number>.
    no("width", "calc(1px + 2)");
    yes("width", "calc(1px * 2)");
    no("width", "calc(1px * 2px)");
    yes("width", "calc(1px * 2px / 1px)");
    no("width", "calc(2 / 1px)");
    yes("width", "calc(2px / 1px * 1px)");
    // <number> slots: a unitless calc.
    yes("opacity", "calc(1 / 2)");
    no("opacity", "calc(1px / 2)");
    yes("opacity", "calc(1px / 1px)");
    // An empty calc() and an unknown function are invalid.
    no("width", "calc()");
    no("width", "nope(1px)");
    // Nested parentheses and calc() inside calc().
    yes("width", "calc((1px + 2px) * 3)");
    yes("width", "calc(calc(1px) + (2px))");
    // Math function names are ASCII case-insensitive.
    yes("width", "CALC(1px)");
}

#[test]
fn calc_percentages() {
    // values-4 10.10: a percentage resolves against the slot's type when
    // the slot takes <length-percentage>, and is a percentage otherwise.
    yes("width", "calc(50% + 1px)");
    // opacity: <opacity-value> = <number> | <percentage>, so a % only
    // mixes with nothing.
    yes("opacity", "calc(50%)");
    no("opacity", "calc(50% + 1)");
    // A pure <length> slot takes no percentage at all.
    no("border-top-width", "calc(10%)");
    no("border-top-width", "calc(1px + 10%)");
    yes("border-top-width", "calc(1px + 1em)");
}

#[test]
fn math_functions() {
    // values-4 10.3-10.8.
    yes("width", "min(1px, 2em)");
    yes("width", "max(1px)");
    no("width", "min()");
    no("width", "min(1px, 2)");
    yes("width", "clamp(1px, 50%, 3em)");
    yes("width", "clamp(none, 50%, 3em)");
    no("width", "clamp(1px, 2px)");
    // round(): an optional strategy, then A and an optional B.
    yes("width", "round(1px, 2px)");
    yes("width", "round(up, 1px, 2px)");
    yes("width", "round(to-zero, 1px, 2px)");
    no("width", "round(sideways, 1px, 2px)");
    // mod() and rem() take two arguments of the same type.
    yes("width", "mod(10px, 3px)");
    no("width", "mod(10px, 3)");
    yes("width", "rem(10px, 3px)");
    // Trigonometry gives <number>; atan2 takes two of one type, giving an
    // <angle>.
    yes("opacity", "sin(30deg)");
    yes("opacity", "cos(1)");
    no("width", "sin(1)");
    yes("transform", "rotate(atan2(1px, 2px))");
    no("transform", "rotate(atan2(1px, 2))");
    yes("transform", "rotate(acos(0.5))");
    no("transform", "rotate(acos(1px))");
    // pow() takes two <number>s; sqrt one; hypot sums of one type.
    yes("opacity", "pow(2, 3)");
    no("opacity", "pow(2px, 3)");
    yes("width", "hypot(3px, 4px)");
    no("width", "hypot(3px, 4)");
    yes("opacity", "sqrt(4)");
    // abs() and sign(): sign gives a <number> from any type.
    yes("width", "abs(-1px)");
    yes("opacity", "sign(-1px)");
    yes("opacity", "exp(1)");
    yes("opacity", "log(8, 2)");
    // Constants: e, pi, infinity, -infinity and NaN, as <number>s.
    yes("width", "calc(1px * pi)");
    yes("width", "calc(1px * infinity)");
    yes("width", "calc(1px * -infinity)");
    yes("width", "calc(1px * NaN)");
    // ASCII case-insensitive like every keyword (values-4 10.7.1).
    yes("width", "calc(1px * nan)");
    yes("width", "calc(1px * InFiNiTy)");
    no("width", "calc(1px * tau)");
    yes("opacity", "calc(e)");
}

#[test]
fn ranges() {
    // A negative length is out of width's [0,∞] range; a calc() outside
    // it is clamped at computed time, so valid at parse time (values-4
    // 10.12).
    no("width", "-1px");
    yes("width", "calc(-1px)");
    yes("margin-top", "-1px");
    no("opacity", "-");
    yes("opacity", "-1");
    // order: <integer>; a non-integer number is not an <integer>.
    yes("order", "-3");
    no("order", "1.5");
    yes("order", "calc(1.5)");
    no("z-index", "1e1x");
    // orphans: <integer [1,∞]>.
    no("orphans", "0");
    yes("orphans", "1");
}

#[test]
fn unitless_zero() {
    // values-4 6.2: a unitless 0 is a <length>, but not an <angle>,
    // <time> or a unitless 0 inside calc() in a length slot.
    yes("width", "0");
    yes("margin", "0 0 0 0");
    no("width", "1");
    // font-style: oblique <angle [-90deg,90deg]>?, no <zero>.
    no("font-style", "oblique 0");
    yes("font-style", "oblique 0deg");
    no("font-style", "oblique 91deg");
    // transforms-1 rotate() names <zero> explicitly.
    yes("transform", "rotate(0)");
    no("width", "calc(0)");
    yes("width", "calc(0px)");
}

#[test]
fn comma_rule() {
    // values-4 2.1: a grammar comma is omitted when nothing before it is
    // matched. box-shadow's <shadow># lists need commas between items.
    yes("box-shadow", "1px 1px red, 2px 2px blue");
    no("box-shadow", "1px 1px red 2px 2px blue");
    no("box-shadow", "1px 1px red,");
    no("box-shadow", ", 1px 1px red");
    no("box-shadow", "1px 1px red,, 2px 2px blue");
    // Gradient: [ <linear-gradient-syntax> ] where the direction and its
    // comma are optional together.
    yes("background-image", "linear-gradient(red, blue)");
    yes("background-image", "linear-gradient(to right, red, blue)");
    no("background-image", "linear-gradient(to right red, blue)");
    no("background-image", "linear-gradient(, red, blue)");
    no("background-image", "linear-gradient(to right, , red, blue)");
    // ...but a single stop is a whole list: images-4 `<linear-color-stop> ,
    // [ <linear-color-hint>? , <linear-color-stop> ]#?`, the comma omitted
    // as nothing after it is matched.
    yes("background-image", "linear-gradient(red)");
    no("background-image", "linear-gradient()");
    // Hints sit between stops.
    yes("background-image", "linear-gradient(red, 50%, blue)");
    no("background-image", "linear-gradient(red, 50%, 50%, blue)");
    no("background-image", "linear-gradient(50%, red, blue)");
}

#[test]
fn flex_zero_rule() {
    // flexbox-1 7.2: a unitless zero that isn't preceded by two flex
    // factors is a flex factor, not a basis, so `1 1 0` and `0 0` work and
    // the third value may be a unitless 0.
    yes("flex", "none");
    yes("flex", "1");
    yes("flex", "1 1 0");
    yes("flex", "1 1 0px");
    yes("flex", "0 0");
    yes("flex", "10px");
    yes("flex", "1 10px");
    yes("flex", "10px 1");
    yes("flex", "1 2 10px");
    no("flex", "1 2 3 4");
    no("flex", "none 1");
    no("flex", "10px 20px");
}

#[test]
fn font_family_names() {
    // fonts-4 4.2: a generic name as an identifier is the generic family;
    // a family name of several identifiers is valid; CSS-wide keywords and
    // `default` can't start an unquoted name.
    yes("font-family", "serif");
    yes("font-family", "Times New Roman, serif");
    yes("font-family", "\"serif\"");
    yes("font-family", "Gill Sans Extrabold, sans-serif");
    no("font-family", "serif sans");
    no("font-family", "a,");
    no("font-family", "a, , b");
    no("font-family", "default");
    no("font-family", "a, default");
    // Every identifier of the name is a <custom-ident>, which `default`
    // never is (values-4 4.2).
    no("font-family", "a default");
    no("font-family", "1px");
}

#[test]
fn colors() {
    // color-4: legacy comma syntax is all-commas, modern is
    // space-separated with an optional `/ alpha`.
    yes("color", "rgb(1,2,3)");
    yes("color", "rgba(1, 2, 3, 0.5)");
    yes("color", "rgb(1 2 3)");
    no("color", "rgb(1 2 3, 0.5)");
    no("color", "rgb(1, 2, 3 / 0.5)");
    yes("color", "rgb(none 2 3)");
    no("color", "rgb(none, 2, 3)");
    // Legacy rgb() doesn't mix numbers and percentages.
    no("color", "rgb(1, 2%, 3)");
    yes("color", "rgb(1 2% 3)");
    yes("color", "hsl(120deg 50% 50%)");
    yes("color", "hsl(120, 50%, 50%)");
    yes("color", "#abc");
    yes("color", "#abcd");
    yes("color", "#aabbcc");
    yes("color", "#aabbccdd");
    no("color", "#abcde");
    no("color", "#ggg");
    yes("color", "transparent");
    yes("color", "currentColor");
    yes("color", "RebeccaPurple");
    no("color", "notacolor");
    yes("color", "Canvas");
    yes("color", "lab(50% 20 30)");
    yes("color", "oklch(0.5 0.1 120deg / 50%)");
    yes("color", "color(display-p3 1 0 0)");
    no("color", "color(nope 1 0 0)");
}

#[test]
fn reorderable() {
    // && takes every child once, in any order; || at least one, each at
    // most once.
    yes("border", "solid");
    yes("border", "red solid 1px");
    yes("border", "1px solid red");
    no("border", "1px 2px");
    no("border", "solid solid");
    yes("outline", "red dotted 2px");
    no("outline", "dotted dotted");
    // place-self: <'align-self'> <'justify-self'>?
    yes("place-self", "center");
    yes("place-self", "center start");
    no("place-self", "center start end");
}

#[test]
fn css_wide_and_substitution() {
    // A CSS-wide keyword is valid alone, not with other values; a value
    // with var() is valid at parse time whatever its grammar.
    yes("width", "inherit");
    no("width", "inherit 1px");
    yes("width", "var(--x) nonsense");
    yes("width", "calc(var(--x) + 1px)");
    no("width", "");
    // Keywords are ASCII case-insensitive.
    yes("width", "AUTO");
    yes("display", "Block");
}

#[test]
fn math_nesting_limit() {
    // values-4 10.8: at least 32 levels of nesting must be supported;
    // deeper ones may be treated as invalid. This engine allows
    // MAX_NESTING functions and ()-blocks, counting the outermost math
    // function, and refuses one more.
    let max = optimal_style::values::MAX_NESTING as usize;
    let calcs = |n: usize| format!("{}1px{}", "calc(".repeat(n), ")".repeat(n));
    yes("width", &calcs(max));
    no("width", &calcs(max + 1));
    // One calc() and max - 1 parentheses: max levels; then one more.
    let parens = |n: usize| format!("calc({}1px{})", "(".repeat(n), ")".repeat(n));
    yes("width", &parens(max - 1));
    no("width", &parens(max));
    // Mixed: calc( ( min( ( 1px ) ) ) ) is four levels.
    let mixed = |n: usize| format!("{}1px{}", "calc((".repeat(n), "))".repeat(n));
    yes("width", &mixed(max / 2));
    no("width", &format!("calc({})", mixed(max / 2)));
}

#[test]
fn math_function_arities() {
    // round(A) and round(A, B), with or without a strategy (10.6.1);
    // log(A) and log(A, B) (10.7).
    yes("opacity", "round(1.5)");
    yes("opacity", "round(1.5, 1)");
    yes("opacity", "round(down, 1.5, 1)");
    no("opacity", "round(1, 2, 3)");
    yes("opacity", "log(2)");
    yes("opacity", "log(8, 2)");
    no("opacity", "log(2px)");
    // round(line-width, A, B): A must be a <length>.
    yes("width", "round(line-width, 3px, 1px)");
    yes("width", "round(line-width, 3px)");
    no("opacity", "round(line-width, 3, 1)");
}

#[test]
fn number_slot_takes_percent_context() {
    // line-height: normal | <number [0,∞]> | <length-percentage [0,∞]>.
    // The <number> shares the slot's context, so calc(10% / 10%) (a
    // number, hint length) is a valid <number> here (see values::math).
    yes("line-height", "calc(10% / 10%)");
    yes("line-height", "calc(10% / 1px)");
    no("opacity", "calc(10% / 1px)");
}
