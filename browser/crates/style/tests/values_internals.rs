//! The value grammar machinery on small grammars of its own: the
//! definition syntax reader (css-values-4 2), the compiler's checks and
//! calculation contexts, grammar features the property tables don't use
//! yet, the unit table, and the basic type matcher. Each case has its
//! working in a comment.

use optimal_css::{ComponentValue, Kind, Token, parse_component_values};
use optimal_style::values::math::Context;
use optimal_style::values::syntax::{Grammar, Node, SyntaxError};
use optimal_style::values::table::{Def, Production};
use optimal_style::values::types::{self, Base, Bounds, Builtin, unit_of};
use optimal_style::values::{CompileError, Compiled, MAX_NESTING, compile_with, matcher};
use optimal_wtf8::Wtf8;

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Compiles productions (name, grammar) and longhands (name, grammar).
fn build(
    prods: &[(&'static str, &'static str)],
    longhands: &[(&'static str, &'static str)],
    shorthands: &[(&'static str, &'static str)],
) -> Result<Compiled, CompileError> {
    let table: Vec<Production> = prods
        .iter()
        .map(|&(name, text)| Production {
            name,
            spec: "test",
            def: Def::Prod(text),
        })
        .collect();
    compile_with(&table, longhands, shorthands)
}

/// Whether `value` matches longhand `i` of `g`.
fn matches(g: &Compiled, i: usize, value: &str) -> bool {
    let (parsed, range) = parse_component_values(Wtf8::from_str(value));
    matcher::matches_list(g, g.longhands[i], parsed.values(range), 0)
}

/// The id of the only `Node::Type(b, _)` in `g`.
fn type_node(g: &Compiled, b: Builtin) -> usize {
    let ids: Vec<usize> = (0..g.nodes.len())
        .filter(|&i| matches!(g.nodes[i], Node::Type(t, _) if t == b))
        .collect();
    assert_eq!(ids.len(), 1, "{b:?} nodes");
    ids[0]
}

// ---------------------------------------------------------------------
// The definition syntax reader.

fn read(text: &'static str) -> Result<(Vec<Node>, u32), SyntaxError> {
    let mut g = Grammar::default();
    let top = g.read(text)?;
    Ok((g.nodes, top))
}

fn err(text: &'static str, at: usize, what: &'static str) -> Result<(Vec<Node>, u32), SyntaxError> {
    Err(SyntaxError {
        grammar: text,
        at,
        what,
    })
}

#[test]
fn reads_blocks() {
    // A `(` that doesn't follow a name is a ()-block (2.6): a, the block
    // of it, b, and the juxtaposition of the two.
    assert_eq!(
        read("( a ) b"),
        Ok((
            vec![
                Node::Keyword("a"),
                Node::Block(0),
                Node::Keyword("b"),
                Node::Seq(Box::new([1, 2])),
            ],
            3
        ))
    );
    assert_eq!(read("( a b"), err("( a b", 5, "expected )"));
}

#[test]
fn reads_quoted_delimiters() {
    // 'x' is a delimiter, one character between quotes; text goes on
    // right after the closing quote.
    assert_eq!(
        read("a'+'"),
        Ok((
            vec![
                Node::Keyword("a"),
                Node::Delim('+'),
                Node::Seq(Box::new([0, 1]))
            ],
            2
        ))
    );
    assert_eq!(
        read("'×'b"),
        Ok((
            vec![
                Node::Delim('×'),
                Node::Keyword("b"),
                Node::Seq(Box::new([0, 1]))
            ],
            2
        ))
    );
    assert_eq!(
        read("a'++'"),
        err("a'++'", 1, "a quoted delimiter is one character")
    );
    assert_eq!(read("a'"), err("a'", 1, "unterminated quote"));
}

#[test]
fn reads_required_groups() {
    // `]!`: the group must produce something (2.3).
    assert_eq!(
        read("[ a ]!"),
        Ok((vec![Node::Keyword("a"), Node::Required(0)], 1))
    );
    assert_eq!(
        read("[ a ]! b"),
        Ok((
            vec![
                Node::Keyword("a"),
                Node::Required(0),
                Node::Keyword("b"),
                Node::Seq(Box::new([1, 2])),
            ],
            3
        ))
    );
    // A `!` must follow the `]` directly.
    assert_eq!(read("[ a ] !"), err("[ a ] !", 6, "unexpected character"));
}

#[test]
fn reads_only_the_listed_characters() {
    // A single `&` is no combinator.
    assert_eq!(read("a & b"), err("a & b", 2, "unexpected character"));
    assert_eq!(
        read("a && b"),
        Ok((
            vec![
                Node::Keyword("a"),
                Node::Keyword("b"),
                Node::All(Box::new([0, 1]))
            ],
            2
        ))
    );
    // A keyword starts with a letter or `-`; digits can follow.
    assert_eq!(read("1"), err("1", 0, "unexpected character"));
    assert_eq!(read("-x2"), Ok((vec![Node::Keyword("-x2")], 0)));
    // A multiplier with nothing before it is not a component; the error
    // points at it.
    assert_eq!(read("* a"), err("* a", 0, "expected a component"));
    assert_eq!(read("a | +"), err("a | +", 4, "expected a component"));
}

// ---------------------------------------------------------------------
// The compiler's checks.

#[test]
fn ranges_only_on_numeric_types() {
    // A range on a numeric scalar or a mix is fine; on a <string> it
    // means nothing.
    assert!(build(&[], &[("p", "<number [0,1]>")], &[]).is_ok());
    assert!(build(&[], &[("p", "<angle-percentage [0,∞]>")], &[]).is_ok());
    assert_eq!(
        build(&[], &[("p", "<string [0,1]>")], &[]).err(),
        Some(CompileError::BadRange("string"))
    );
    // Lengths take only [0,∞] (relative lengths have no fixed size, so
    // only their sign can be checked when parsing).
    assert!(build(&[], &[("p", "<length [0,∞]>")], &[]).is_ok());
    assert!(build(&[], &[("p", "<length-percentage [-∞,∞]>")], &[]).is_ok());
    assert_eq!(
        build(&[], &[("p", "<length [1px,∞]>")], &[]).err(),
        Some(CompileError::BadRange("length"))
    );
    assert_eq!(
        build(&[], &[("p", "<length [0,10px]>")], &[]).err(),
        Some(CompileError::BadRange("length"))
    );
}

#[test]
fn reorderable_group_limit() {
    // 12 children of && compile, 13 don't.
    for (k, ok) in [(12, true), (13, false)] {
        let text = leak(
            (0..k)
                .map(|i| format!("k{i}"))
                .collect::<Vec<_>>()
                .join(" && "),
        );
        let r = build(&[], &[("p", text)], &[]);
        assert_eq!(r.is_ok(), ok, "{k} children");
        if !ok {
            assert_eq!(r.err(), Some(CompileError::TooManyReorderable));
        }
    }
}

#[test]
fn reference_cycles() {
    let cycle = |prods: &[(&'static str, &'static str)]| {
        matches!(
            build(prods, &[("p", "<a>")], &[]).err(),
            Some(CompileError::Cycle(_))
        )
    };
    // Through a reference and an alternation: <a> is <b>, which may be
    // <a>.
    assert!(cycle(&[("a", "<b>"), ("b", "<a> | x")]));
    // Through a multiplier: <a>* repeats itself.
    assert!(cycle(&[("a", "<a>*")]));
    // Through juxtaposition.
    assert!(cycle(&[("a", "x <a>")]));
    // A function starts a new list, so recursion through one is fine.
    assert!(build(&[("a", "f( <a>? )")], &[("p", "<a>")], &[]).is_ok());
    // A production used twice is no cycle.
    assert!(build(&[("a", "x"), ("b", "<a> <a>")], &[("p", "<b> | <a>")], &[]).is_ok());
}

#[test]
fn nested_required_groups() {
    // A `!` inside another `!` in the same list is refused (the matcher
    // tracks one `!` per list), however deep inside it is.
    let nested = |prods: &[(&'static str, &'static str)], text: &'static str| {
        build(prods, &[("p", text)], &[]).err() == Some(CompileError::NestedRequired)
    };
    assert!(nested(&[], "[ [ a ]! ]!"));
    assert!(nested(&[], "[ [ a ]! b ]!"));
    assert!(nested(&[], "[ x [ y | [ a ]! ]? ]!"));
    assert!(nested(&[("r", "[ a ]!")], "[ <r> b ]!"));
    // Side by side, or inside a function's own list, is fine.
    assert!(build(&[], &[("p", "[ a ]!")], &[]).is_ok());
    assert!(build(&[], &[("p", "[ a? ]! [ b? ]!")], &[]).is_ok());
    assert!(build(&[], &[("p", "[ f( [ a? ]! ) b? ]!")], &[]).is_ok());
    assert!(build(&[("r", "[ a? ]!")], &[("p", "<r> <r>")], &[]).is_ok());
}

#[test]
fn property_references() {
    // <'name'> is that property's grammar, longhand or shorthand, without
    // a top-level # (2.1).
    let g = build(
        &[],
        &[("l1", "x#"), ("l2", "<'l1'> / <'l1'>"), ("l3", "<'s2'>")],
        &[("s1", "y"), ("s2", "z")],
    )
    .expect("compiles");
    assert!(matches(&g, 1, "x / x"));
    assert!(!matches(&g, 1, "x, x / x"));
    assert!(matches(&g, 0, "x, x"));
    assert!(matches(&g, 2, "z"));
    assert!(!matches(&g, 2, "y"));
    assert_eq!(
        build(&[], &[("p", "<'nope'>")], &[]).err(),
        Some(CompileError::Undefined("'nope'"))
    );
}

// ---------------------------------------------------------------------
// Calculation contexts (see `math`): a numeric type's percentages
// resolve against the percentage-taking type among its | alternatives.

#[test]
fn contexts_from_alternatives() {
    // <number> | <length-percentage>: both resolve % against lengths.
    let g = build(&[], &[("p", "<number> | <length-percentage>")], &[]).unwrap();
    let against = Context::Against(Base::Length);
    assert_eq!(g.ctx[type_node(&g, Builtin::Number)], against);
    assert_eq!(g.ctx[type_node(&g, Builtin::LengthPercentage)], against);
    // The same through a reference to the <length-percentage>.
    let g = build(
        &[("lp", "<length-percentage>")],
        &[("p", "<number> | <lp>")],
        &[],
    )
    .unwrap();
    assert_eq!(g.ctx[type_node(&g, Builtin::Number)], against);
    // <percentage> next to a mix takes the mix's context.
    let g = build(&[], &[("p", "<percentage> | <length-percentage>")], &[]).unwrap();
    assert_eq!(g.ctx[type_node(&g, Builtin::Percentage)], against);
    // Alone, <number> has none and <percentage> is a plain percentage.
    let g = build(&[], &[("p", "<number> <percentage>")], &[]).unwrap();
    assert_eq!(g.ctx[type_node(&g, Builtin::Number)], Context::NoPercent);
    assert_eq!(g.ctx[type_node(&g, Builtin::Percentage)], Context::Percent);
    // <number> | <percentage>: percentages are plain.
    let g = build(&[], &[("p", "<number> | <percentage>")], &[]).unwrap();
    assert_eq!(g.ctx[type_node(&g, Builtin::Number)], Context::Percent);
}

#[test]
fn conflicting_contexts() {
    // Two mixes against different types in one slot: no single context.
    let r = build(
        &[],
        &[("p", "<length-percentage> | <angle-percentage>")],
        &[],
    );
    assert_eq!(r.err(), Some(CompileError::ConflictingContext(2)));
    // Two against the same type are fine.
    assert!(
        build(
            &[],
            &[("p", "<length-percentage> | <length-percentage [0,∞]>")],
            &[]
        )
        .is_ok()
    );
    // One <number> node (production n) in two slots that disagree.
    let r = build(
        &[("n", "<number>")],
        &[
            ("p", "<n> | <length-percentage>"),
            ("q", "<n> | <angle-percentage>"),
        ],
        &[],
    );
    assert_eq!(r.err(), Some(CompileError::ConflictingContext(0)));
}

// ---------------------------------------------------------------------
// Matcher features the property tables don't use yet.

#[test]
fn blocks_colons_and_semicolons() {
    let g = build(
        &[],
        &[("p", "( a ) b"), ("q", "a : b"), ("r", "f( a ; b )")],
        &[],
    )
    .unwrap();
    assert!(matches(&g, 0, "(a) b"));
    assert!(matches(&g, 0, "( a )b"));
    assert!(!matches(&g, 0, "(b) b"));
    assert!(!matches(&g, 0, "a b"));
    assert!(!matches(&g, 0, "[a] b"));
    assert!(matches(&g, 1, "a:b"));
    assert!(!matches(&g, 1, "a;b"));
    assert!(matches(&g, 2, "f(a;b)"));
    assert!(!matches(&g, 2, "f(a:b)"));
}

#[test]
fn function_nesting_limit() {
    // <nest> = f( <nest>? ): the top-level value is depth 0 and each f()
    // opens one more list; MAX_NESTING functions are allowed, one more
    // isn't.
    let g = build(&[("nest", "f( <nest>? )")], &[("p", "<nest>")], &[]).unwrap();
    let nested = |n: u32| {
        let n = n as usize;
        format!("{}{}", "f(".repeat(n), ")".repeat(n))
    };
    assert!(matches(&g, 0, &nested(1)));
    assert!(matches(&g, 0, &nested(MAX_NESTING)));
    assert!(!matches(&g, 0, &nested(MAX_NESTING + 1)));
}

#[test]
fn ambiguous_optionals_stay_small() {
    // 48 optional a's: after k of them, the states are the positions 0
    // to min(k, n), so matching is quick. Keeping every way of getting
    // there instead (without merging equal states) would need 2^48.
    let g = build(&[], &[("p", leak("a? ".repeat(48)))], &[]).unwrap();
    assert!(matches(&g, 0, &"a ".repeat(24)));
    assert!(matches(&g, 0, &"a ".repeat(48)));
    assert!(!matches(&g, 0, &"a ".repeat(49)));
}

// ---------------------------------------------------------------------
// Units (css-values-4 6.2, 7) and the basic types.

#[test]
fn unit_sizes() {
    // Canonical units: px, deg, s, Hz, dppx. Worked from the spec's
    // definitions: 1in = 2.54cm = 96px; 1cm = 10mm = 40Q; 1in = 72pt =
    // 6pc; 1turn = 360deg = 400grad = 2pi rad; 1s = 1000ms; 1kHz =
    // 1000Hz; 1dppx = 96dpi; 1in = 2.54cm so 1dpcm = 2.54dpi.
    let size = |u: &str| unit_of(u).and_then(|u| u.factor).expect(u);
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-12 * b.abs();
    let cases: [(&str, f64, f64); 15] = [
        ("cm", 2.54, 96.0),
        ("mm", 25.4, 96.0),
        ("Q", 101.6, 96.0),
        ("in", 1.0, 96.0),
        ("pt", 72.0, 96.0),
        ("pc", 6.0, 96.0),
        ("px", 1.0, 1.0),
        ("grad", 400.0, 360.0),
        ("rad", 2.0 * std::f64::consts::PI, 360.0),
        ("turn", 1.0, 360.0),
        ("ms", 1000.0, 1.0),
        ("kHz", 1.0, 1000.0),
        ("dpi", 96.0, 1.0),
        ("dpcm", 96.0 / 2.54, 1.0),
        ("x", 1.0, 1.0),
    ];
    for (u, count, total) in cases {
        assert!(close(size(u) * count, total), "{count}{u} = {total}");
    }
    assert_eq!(unit_of("em").map(|u| u.factor), Some(None));
    assert_eq!(unit_of("Q").map(|u| u.base), Some(Base::Length));
    assert_eq!(unit_of("KHZ").map(|u| u.base), Some(Base::Frequency));
}

#[test]
fn range_bounds() {
    assert_eq!(Bounds::parse("[-∞,∞]"), Some(Bounds::ANY));
    assert_eq!(Bounds::parse("[−∞,+∞]"), Some(Bounds::ANY));
    let b = Bounds::parse("[0,1in]").expect("parses");
    assert_eq!((b.min, b.max), (0.0, 96.0));
    let b = Bounds::parse("[-1turn,0.5turn]").expect("parses");
    assert_eq!((b.min, b.max), (-360.0, 180.0));
    assert_eq!(Bounds::parse("[1,0]"), None);
    assert_eq!(Bounds::parse("[0,1em]"), None, "no fixed size");
}

/// The first component value of `text`, not white space.
fn with_value<R>(text: &str, f: impl FnOnce(ComponentValue<'_, '_>) -> R) -> R {
    let (parsed, range) = parse_component_values(Wtf8::from_str(text));
    let v = parsed
        .values(range)
        .find(|v| !matches!(v.kind(), Kind::Token(Token::Whitespace)))
        .expect("a value");
    f(v)
}

fn is(b: Builtin, bounds: Bounds, ctx: Context, text: &str) -> bool {
    with_value(text, |v| types::matches(b, bounds, v, ctx, 0))
}

#[test]
fn time_frequency_resolution() {
    use Builtin::*;
    let any = Bounds::ANY;
    let none = Context::NoPercent;
    assert!(is(Time, any, none, "1s"));
    assert!(is(Time, any, none, "10MS"));
    assert!(!is(Time, any, none, "1Hz"));
    assert!(
        !is(Time, any, none, "0"),
        "a unitless zero is a length only"
    );
    assert!(is(Frequency, any, none, "2kHz"));
    assert!(!is(Frequency, any, none, "2s"));
    assert!(is(Resolution, any, none, "2dppx"));
    assert!(is(Resolution, any, none, "2x"));
    let time = Context::Against(Base::Time);
    let freq = Context::Against(Base::Frequency);
    assert!(is(TimePercentage, any, time, "1s"));
    assert!(is(TimePercentage, any, time, "10%"));
    assert!(!is(TimePercentage, any, time, "1px"));
    assert!(is(FrequencyPercentage, any, freq, "1Hz"));
    assert!(is(FrequencyPercentage, any, freq, "10%"));
    assert!(!is(FrequencyPercentage, any, freq, "1deg"));
    // And as math functions.
    assert!(is(Time, any, none, "calc(1s + 1ms)"));
    assert!(is(TimePercentage, any, time, "calc(1s + 10%)"));
    assert!(is(Frequency, any, none, "calc(1Hz)"));
    assert!(is(FrequencyPercentage, any, freq, "calc(1Hz - 10%)"));
    assert!(is(Resolution, any, none, "calc(2x)"));
    assert!(!is(Time, any, none, "calc(1Hz)"));
}

#[test]
fn ranges_in_canonical_units() {
    // <angle [0,360deg]>: 1turn is 360deg (inside), 2turn 720deg
    // (outside); 400grad is 360deg, 401grad 360.9deg.
    let b = Bounds {
        min: 0.0,
        max: 360.0,
    };
    let none = Context::NoPercent;
    assert!(is(Builtin::Angle, b, none, "1turn"));
    assert!(!is(Builtin::Angle, b, none, "2turn"));
    assert!(is(Builtin::Angle, b, none, "400grad"));
    assert!(!is(Builtin::Angle, b, none, "401grad"));
    // <resolution [0,2dppx]>: 192dpi is 2dppx.
    let b = Bounds { min: 0.0, max: 2.0 };
    assert!(is(Builtin::Resolution, b, none, "192dpi"));
    assert!(!is(Builtin::Resolution, b, none, "193dpi"));
    // A dimension is never a <percentage>.
    assert!(!is(
        Builtin::Percentage,
        Bounds::ANY,
        Context::Percent,
        "1px"
    ));
    assert!(is(Builtin::Percentage, Bounds::ANY, Context::Percent, "1%"));
}

#[test]
fn percentages_typed_by_the_slot() {
    // In a slot whose percentages resolve against lengths, 10% is
    // {length 1} with hint length, so 10% / 10% is a <number> with hint
    // length, which the slot accepts. Typed as a plain percentage it
    // would have hint percent, which a length context refuses.
    let lengths = Context::Against(Base::Length);
    assert!(is(Builtin::Number, Bounds::ANY, lengths, "calc(10% / 10%)"));
    assert!(is(
        Builtin::Number,
        Bounds::ANY,
        Context::Percent,
        "calc(10% / 10%)"
    ));
    assert!(!is(
        Builtin::Number,
        Bounds::ANY,
        Context::NoPercent,
        "calc(10% / 10%)"
    ));
}
