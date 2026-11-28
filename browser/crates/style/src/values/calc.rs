//! Calculation trees (css-values-4 #calc-internal to #calc-serialize): the
//! internal
//! representation of a math function, its simplification, and its
//! serialization.
//!
//! [`super::math`] decides whether a math function is valid; this module
//! is only given valid ones. [`parse`] builds the tree ("parse a
//! calculation", #calc-internal) and simplifies it as a specified value
//! ("simplify a calculation tree", #calc-simplification); [`serialize`] writes it
//! ("serialize a math function", #calc-serialize).
//!
//! Readings of the spec (chosen, why, rejected):
//!
//! - **"Enough information" at specified-value time.** Absolute units
//!   convert to their canonical unit (`px`, `deg`, `s`, `hz`, `dppx`).
//!   A percentage that resolves against another type (in `width`, say)
//!   is not known, so it only combines with other percentages in a sum.
//!   A percentage that resolves against nothing (in `opacity`) is a known
//!   value. A relative length (`em`, `vw`) is a non-negative multiple of
//!   an unknown basis, so values in the same relative unit compare
//!   (`min()`, `max()`, `clamp()`), and scale (`abs()`, `hypot()`), for
//!   any basis; operations that divide by the basis or ask for its sign
//!   (`round()`, `mod()`, `rem()`, `sign()`, `atan2()`) give a different
//!   answer when it is zero, so they wait for computed values. Rejected:
//!   treating relative units like absolute ones, which is wrong for a
//!   zero basis.
//! - **A numeric root serializes as the value inside `calc()`**, and a
//!   calc-operator root as its expression inside `calc()` with the outer
//!   parentheses removed. Step 5 of "serialize a math function" says to
//!   join the root's children, which for a Sum or a number would lose
//!   the operators or the value; the spec's own examples (`calc(50px)`,
//!   `calc(0% + 20px)`) serialize the whole root.
//! - **A Product with a single child is that child**, as a Sum with one
//!   is (#calc-simplification step 8.3; step 9 has no such step). Without it,
//!   `calc(1px + min(1em, 2px))` would serialize with parentheses around
//!   the `min()` and `calc(clamp(1px, 1em, 1vh))` keep its `calc()`,
//!   which neither implementations nor the tests do.
//! - **`round(line-width, ...)` never simplifies here**: it needs the
//!   device pixel size, a used-value fact.
//! - **Numbers in `calc()` are numbers.** A `<number-token>` is a number
//!   whatever its type flag; `<integer>` slots round at computed-value
//!   time (#calc-type-checking).

use optimal_css::{BlockKind, Kind, Token, Values};

use super::math::{Context, Item, arguments};
use super::serialize::number;
use super::types::{Base, UNITS, Unit, unit_of};

/// The unit of a numeric value in a calculation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CalcUnit {
    /// A plain number.
    Number,
    /// A percentage.
    Percent,
    /// A dimension's unit.
    Dim(Unit),
}

impl CalcUnit {
    /// Whether two units are the same unit ("identical units", #calc-simplification).
    fn same(self, other: CalcUnit) -> bool {
        match (self, other) {
            (CalcUnit::Number, CalcUnit::Number) | (CalcUnit::Percent, CalcUnit::Percent) => true,
            (CalcUnit::Dim(a), CalcUnit::Dim(b)) => a.name == b.name,
            _ => false,
        }
    }
}

/// A rounding strategy of `round()` (#round-func).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// `nearest` (the default).
    Nearest,
    /// `up`
    Up,
    /// `down`
    Down,
    /// `to-zero`
    ToZero,
    /// `line-width`
    LineWidth,
}

impl Strategy {
    fn name(self) -> &'static str {
        match self {
            Strategy::Nearest => "nearest",
            Strategy::Up => "up",
            Strategy::Down => "down",
            Strategy::ToZero => "to-zero",
            Strategy::LineWidth => "line-width",
        }
    }
}

/// A math function other than `calc()`: an operator node (#calc-internal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum MathFn {
    Min,
    Max,
    Clamp,
    Round(Strategy),
    Mod,
    Rem,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Pow,
    Sqrt,
    Hypot,
    Log,
    Exp,
    Abs,
    Sign,
}

impl MathFn {
    /// The function's name, lowercase.
    pub fn name(self) -> &'static str {
        match self {
            MathFn::Min => "min",
            MathFn::Max => "max",
            MathFn::Clamp => "clamp",
            MathFn::Round(_) => "round",
            MathFn::Mod => "mod",
            MathFn::Rem => "rem",
            MathFn::Sin => "sin",
            MathFn::Cos => "cos",
            MathFn::Tan => "tan",
            MathFn::Asin => "asin",
            MathFn::Acos => "acos",
            MathFn::Atan => "atan",
            MathFn::Atan2 => "atan2",
            MathFn::Pow => "pow",
            MathFn::Sqrt => "sqrt",
            MathFn::Hypot => "hypot",
            MathFn::Log => "log",
            MathFn::Exp => "exp",
            MathFn::Abs => "abs",
            MathFn::Sign => "sign",
        }
    }

    fn from_name(name: &str) -> Option<MathFn> {
        const ALL: [MathFn; 20] = [
            MathFn::Min,
            MathFn::Max,
            MathFn::Clamp,
            MathFn::Round(Strategy::Nearest),
            MathFn::Mod,
            MathFn::Rem,
            MathFn::Sin,
            MathFn::Cos,
            MathFn::Tan,
            MathFn::Asin,
            MathFn::Acos,
            MathFn::Atan,
            MathFn::Atan2,
            MathFn::Pow,
            MathFn::Sqrt,
            MathFn::Hypot,
            MathFn::Log,
            MathFn::Exp,
            MathFn::Abs,
            MathFn::Sign,
        ];
        ALL.into_iter()
            .find(|f| f.name().eq_ignore_ascii_case(name))
    }
}

/// A node of a calculation tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Calc {
    /// A numeric value. Infinities and NaN can only come from a
    /// calculation; zero keeps its sign (#calc-ieee).
    Value(f64, CalcUnit),
    /// The `none` of `clamp()`, which only appears there.
    None,
    /// Sum
    Sum(Vec<Calc>),
    /// Product
    Product(Vec<Calc>),
    /// Negate
    Negate(Box<Calc>),
    /// Invert
    Invert(Box<Calc>),
    /// Any other math function, with its calculation arguments in order.
    Fn(MathFn, Vec<Calc>),
}

/// A math function: its calculation tree, simplified as a specified
/// value.
#[derive(Clone, Debug, PartialEq)]
pub struct MathValue {
    /// The tree.
    pub root: Calc,
}

/// The calculation tree of a valid math function `name(args)`, simplified
/// in `ctx` (the calculation context of its grammar slot).
pub fn parse(name: &str, args: Values<'_, '_>, ctx: Context) -> MathValue {
    MathValue {
        root: simplify(internal(name, args), ctx),
    }
}

/// The internal representation of a math function (#calc-internal), unsimplified.
fn internal(name: &str, args: Values<'_, '_>) -> Calc {
    let args = arguments(args);
    if name.eq_ignore_ascii_case("calc") {
        return calculation(&args[0]);
    }
    let mut f = MathFn::from_name(name).expect("a valid math function");
    let mut rest: &[Vec<Item<'_, '_>>] = &args;
    if f == MathFn::Round(Strategy::Nearest)
        && let Some([one]) = args.first().map(|a| a.as_slice())
        && let Kind::Token(Token::Ident(id)) = one.v.kind()
    {
        let strategy = [
            Strategy::Nearest,
            Strategy::Up,
            Strategy::Down,
            Strategy::ToZero,
            Strategy::LineWidth,
        ]
        .into_iter()
        .find(|s| s.name().eq_ignore_ascii_case(id));
        if let Some(s) = strategy {
            f = MathFn::Round(s);
            rest = &args[1..];
        }
    }
    let children = rest
        .iter()
        .map(|a| {
            let none = matches!(a.as_slice(), [one] if matches!(one.v.kind(), Kind::Token(Token::Ident(id)) if id.eq_ignore_ascii_case("none")));
            if f == MathFn::Clamp && none {
                Calc::None
            } else {
                calculation(a)
            }
        })
        .collect();
    Calc::Fn(f, children)
}

/// "Parse a calculation" (#calc-internal) steps 1 to 5, on items with white space
/// already dropped.
fn calculation(items: &[Item<'_, '_>]) -> Calc {
    // Values alternate with operators: [v, op, v, op, v, ...].
    let op = |i: usize| match items[i].v.kind() {
        Kind::Token(Token::Delim(c)) => *c,
        _ => unreachable!("a valid calculation alternates values and operators"),
    };
    let mut terms: Vec<Calc> = Vec::new();
    let mut product: Vec<Calc> = vec![leaf(&items[0])];
    let mut negate_product = false;
    let mut i = 1;
    let finish = |product: Vec<Calc>, negate: bool, terms: &mut Vec<Calc>| {
        let p = Calc::Product(product);
        terms.push(if negate { Calc::Negate(Box::new(p)) } else { p });
    };
    while i < items.len() {
        let c = op(i);
        let rhs = leaf(&items[i + 1]);
        match c {
            '*' => product.push(rhs),
            '/' => product.push(Calc::Invert(Box::new(rhs))),
            _ => {
                finish(std::mem::take(&mut product), negate_product, &mut terms);
                negate_product = c == '-';
                product.push(rhs);
            }
        }
        i += 2;
    }
    finish(product, negate_product, &mut terms);
    // Step 4.2: a single Product stays itself (a parenthesized block was
    // already parsed into its tree by `leaf`).
    if terms.len() == 1 && matches!(terms[0], Calc::Product(_)) {
        return terms.pop().expect("one term");
    }
    Calc::Sum(terms)
}

/// A value item of a calculation, as a tree (step 5).
fn leaf(item: &Item<'_, '_>) -> Calc {
    // Literal zeros are unsigned zeros, which count as positive (#calc-ieee).
    let unsigned = |v: f64| if v == 0.0 { 0.0 } else { v };
    match item.v.kind() {
        Kind::Token(Token::Number(n)) => Calc::Value(unsigned(n.value), CalcUnit::Number),
        Kind::Token(Token::Percentage { value, .. }) => {
            Calc::Value(unsigned(*value), CalcUnit::Percent)
        }
        Kind::Token(Token::Dimension(n, unit)) => Calc::Value(
            unsigned(n.value),
            CalcUnit::Dim(unit_of(unit).expect("a valid calculation's units are known")),
        ),
        Kind::Token(Token::Ident(id)) => {
            let v = match id.to_ascii_lowercase().as_str() {
                "e" => std::f64::consts::E,
                "pi" => std::f64::consts::PI,
                "infinity" => f64::INFINITY,
                "-infinity" => f64::NEG_INFINITY,
                _ => f64::NAN,
            };
            Calc::Value(v, CalcUnit::Number)
        }
        Kind::Function(name, args) => internal(name, args),
        Kind::Block(BlockKind::Paren, inner) => calculation(&arguments(inner)[0]),
        _ => unreachable!("a valid calculation has no other values"),
    }
}

/// The canonical unit of a base type.
fn canonical(base: Base) -> Option<Unit> {
    let name = match base {
        Base::Length => "px",
        Base::Angle => "deg",
        Base::Time => "s",
        Base::Frequency => "Hz",
        Base::Resolution => "dppx",
        Base::Flex | Base::Percent => return None,
    };
    UNITS.iter().copied().find(|u| u.name == name)
}

/// Whether the value of a unit is known at specified-value time: a
/// number, an absolute unit, or a percentage that resolves against
/// nothing.
fn known(unit: CalcUnit, ctx: Context) -> bool {
    match unit {
        CalcUnit::Number => true,
        CalcUnit::Percent => !matches!(ctx, Context::Against(_)),
        CalcUnit::Dim(u) => u.factor.is_some(),
    }
}

/// "Simplify a calculation tree" (#calc-simplification), as a specified value.
pub fn simplify(root: Calc, ctx: Context) -> Calc {
    match root {
        Calc::Value(v, CalcUnit::Dim(u)) => match (u.factor, canonical(u.base)) {
            (Some(f), Some(c)) if c.name != u.name => Calc::Value(v * f, CalcUnit::Dim(c)),
            _ => Calc::Value(v, CalcUnit::Dim(u)),
        },
        Calc::Value(..) | Calc::None => root,
        Calc::Fn(f, children) => {
            let children: Vec<Calc> = children.into_iter().map(|c| simplify(c, ctx)).collect();
            simplify_fn(f, children, ctx)
        }
        Calc::Negate(child) => match simplify(*child, ctx) {
            Calc::Value(v, u) => Calc::Value(-v, u),
            Calc::Negate(inner) => *inner,
            Calc::Sum(terms) => Calc::Sum(
                terms
                    .into_iter()
                    .map(|t| match t {
                        Calc::Value(v, u) => Calc::Value(-v, u),
                        Calc::Negate(inner) => *inner,
                        t => Calc::Negate(Box::new(t)),
                    })
                    .collect(),
            ),
            c => Calc::Negate(Box::new(c)),
        },
        Calc::Invert(child) => match simplify(*child, ctx) {
            Calc::Value(v, CalcUnit::Number) => Calc::Value(1.0 / v, CalcUnit::Number),
            Calc::Invert(inner) => *inner,
            c => Calc::Invert(Box::new(c)),
        },
        Calc::Sum(terms) => {
            let mut flat = Vec::new();
            for t in terms {
                match simplify(t, ctx) {
                    Calc::Sum(inner) => flat.extend(inner),
                    t => flat.push(t),
                }
            }
            let mut out: Vec<Calc> = Vec::new();
            for t in flat {
                if let Calc::Value(v, u) = t
                    && let Some(Calc::Value(acc, _)) = out
                        .iter_mut()
                        .find(|o| matches!(o, Calc::Value(_, ou) if ou.same(u)))
                {
                    *acc += v;
                    continue;
                }
                out.push(t);
            }
            if out.len() == 1 {
                out.pop().expect("one term")
            } else {
                Calc::Sum(out)
            }
        }
        Calc::Product(factors) => {
            let mut flat = Vec::new();
            for f in factors {
                match simplify(f, ctx) {
                    Calc::Product(inner) => flat.extend(inner),
                    f => flat.push(f),
                }
            }
            simplify_product(flat)
        }
    }
}

/// Steps 9.2 to 9.5 of the simplification, on flattened factors.
fn simplify_product(flat: Vec<Calc>) -> Calc {
    let numbers = flat
        .iter()
        .filter(|f| matches!(f, Calc::Value(_, CalcUnit::Number)))
        .count();
    let mut factors = if numbers > 1 {
        let mut product = 1.0;
        let mut rest = Vec::new();
        let mut at = None;
        for f in flat {
            match f {
                Calc::Value(v, CalcUnit::Number) => {
                    product *= v;
                    at.get_or_insert(rest.len());
                }
                f => rest.push(f),
            }
        }
        rest.insert(
            at.expect("numbers were seen"),
            Calc::Value(product, CalcUnit::Number),
        );
        rest
    } else {
        flat
    };
    // 9.3: a number times a sum of numeric values.
    if let [a, b] = factors.as_slice() {
        let (n, sum) = match (a, b) {
            (Calc::Value(n, CalcUnit::Number), Calc::Sum(s))
            | (Calc::Sum(s), Calc::Value(n, CalcUnit::Number)) => (*n, s),
            _ => (0.0, &Vec::new()),
        };
        if !sum.is_empty() && sum.iter().all(|t| matches!(t, Calc::Value(..))) {
            return Calc::Sum(
                sum.iter()
                    .map(|t| match t {
                        Calc::Value(v, u) => Calc::Value(v * n, *u),
                        _ => unreachable!("checked numeric"),
                    })
                    .collect(),
            );
        }
    }
    // 9.4: only numeric values and inverted numeric values, whose units
    // multiply out to at most one unit to the first power.
    let mut value = 1.0;
    let mut units: Vec<(CalcUnit, i32)> = Vec::new();
    let mut all_numeric = true;
    for f in &factors {
        let (v, u, e) = match f {
            Calc::Value(v, u) => (*v, *u, 1),
            Calc::Invert(inner) => match **inner {
                Calc::Value(v, u) => (1.0 / v, u, -1),
                _ => {
                    all_numeric = false;
                    break;
                }
            },
            _ => {
                all_numeric = false;
                break;
            }
        };
        value *= v;
        if u != CalcUnit::Number {
            match units.iter_mut().find(|(x, _)| x.same(u)) {
                Some((_, n)) => *n += e,
                None => units.push((u, e)),
            }
        }
    }
    if all_numeric {
        units.retain(|&(_, n)| n != 0);
        match units.as_slice() {
            [] => return Calc::Value(value, CalcUnit::Number),
            [(u, 1)] => return Calc::Value(value, *u),
            _ => {}
        }
    }
    if factors.len() == 1 {
        return factors.pop().expect("one factor");
    }
    Calc::Product(factors)
}

/// Steps 4 and 5 for an operator node that isn't a calc-operator node.
fn simplify_fn(f: MathFn, mut children: Vec<Calc>, ctx: Context) -> Calc {
    if let Some(v) = evaluate(f, &children, ctx) {
        return v;
    }
    if matches!(f, MathFn::Min | MathFn::Max) {
        let mut out: Vec<Calc> = Vec::new();
        for c in children {
            if let Calc::Value(v, u) = c
                && known_or_relative(u, ctx)
                && let Some(Calc::Value(acc, _)) = out
                    .iter_mut()
                    .find(|o| matches!(o, Calc::Value(_, ou) if ou.same(u)))
            {
                *acc = if f == MathFn::Min {
                    min(*acc, v)
                } else {
                    max(*acc, v)
                };
                continue;
            }
            out.push(c);
        }
        if out.len() == 1 {
            return out.pop().expect("one child");
        }
        children = out;
    }
    Calc::Fn(f, children)
}

/// Whether values in this unit can be compared with values in the same
/// unit (see the module notes on relative lengths).
fn known_or_relative(unit: CalcUnit, ctx: Context) -> bool {
    known(unit, ctx) || matches!(unit, CalcUnit::Dim(u) if u.base == Base::Length)
}

/// IEEE minimum, with 0⁻ below 0⁺ and NaN infectious (#calc-ieee).
fn min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_negative() { a } else { b }
    } else {
        a.min(b)
    }
}

/// IEEE maximum, with 0⁺ above 0⁻ and NaN infectious (#calc-ieee).
fn max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_positive() { a } else { b }
    } else {
        a.max(b)
    }
}

/// The value of an operator node whose children are numeric values with
/// enough information, in its result's canonical unit, or `None`.
fn evaluate(f: MathFn, children: &[Calc], ctx: Context) -> Option<Calc> {
    // The children's values (`None` for clamp's `none`) and their common
    // unit (numbers mix with nothing else here: arguments have one type).
    let mut vals: Vec<Option<f64>> = Vec::new();
    let mut unit: Option<CalcUnit> = None;
    for c in children {
        match c {
            Calc::Value(v, u) => {
                match unit {
                    Some(x) if !x.same(*u) => return None,
                    _ => unit = Some(*u),
                }
                vals.push(Some(*v));
            }
            Calc::None => vals.push(None),
            _ => return None,
        }
    }
    let unit = unit?;
    let scales = matches!(
        f,
        MathFn::Min | MathFn::Max | MathFn::Clamp | MathFn::Abs | MathFn::Hypot
    );
    if !known(unit, ctx) && !(scales && known_or_relative(unit, ctx)) {
        return None;
    }
    let v = |i: usize| vals[i].expect("not none");
    let deg = canonical(Base::Angle).map(CalcUnit::Dim)?;
    let radians = |x: f64| match unit {
        CalcUnit::Dim(u) => x * u.factor.expect("a known angle") * std::f64::consts::PI / 180.0,
        _ => x,
    };
    let to_deg = |r: f64| r * 180.0 / std::f64::consts::PI;
    let number = CalcUnit::Number;
    let (value, unit) = match f {
        MathFn::Min => (
            vals.iter()
                .map(|x| x.expect("not none"))
                .fold(f64::INFINITY, min),
            unit,
        ),
        MathFn::Max => (
            vals.iter()
                .map(|x| x.expect("not none"))
                .fold(f64::NEG_INFINITY, max),
            unit,
        ),
        MathFn::Clamp => {
            let mut x = v(1);
            if let Some(hi) = vals[2] {
                x = min(x, hi);
            }
            if let Some(lo) = vals[0] {
                x = max(lo, x);
            }
            (x, unit)
        }
        MathFn::Round(Strategy::LineWidth) => return None,
        MathFn::Round(s) => {
            let b = if vals.len() == 2 { v(1) } else { 1.0 };
            (round(s, v(0), b), unit)
        }
        MathFn::Mod => (modulo(v(0), v(1), true), unit),
        MathFn::Rem => (modulo(v(0), v(1), false), unit),
        MathFn::Sin => (sin(radians(v(0))), number),
        MathFn::Cos => (radians(v(0)).cos(), number),
        MathFn::Tan => (tan(v(0), unit), number),
        MathFn::Asin => (to_deg(v(0).asin()), deg),
        MathFn::Acos => (to_deg(v(0).acos()), deg),
        MathFn::Atan => (to_deg(v(0).atan()), deg),
        MathFn::Atan2 => (to_deg(v(0).atan2(v(1))), deg),
        MathFn::Pow => (pow(v(0), v(1)), number),
        MathFn::Sqrt => (v(0).sqrt(), number),
        MathFn::Hypot => {
            let xs: Vec<f64> = vals.iter().map(|x| x.expect("not none")).collect();
            let r = if xs.iter().any(|x| x.is_nan()) {
                f64::NAN
            } else if xs.iter().any(|x| x.is_infinite()) {
                f64::INFINITY
            } else {
                xs.iter().map(|x| x * x).sum::<f64>().sqrt()
            };
            (r, unit)
        }
        MathFn::Log => {
            let a = v(0);
            let r = if vals.len() == 2 {
                let b = v(1);
                if b == 1.0 || b < 0.0 {
                    f64::NAN
                } else {
                    a.ln() / b.ln()
                }
            } else {
                a.ln()
            };
            (r, number)
        }
        MathFn::Exp => (v(0).exp(), number),
        MathFn::Abs => (v(0).abs(), unit),
        MathFn::Sign => {
            let a = v(0);
            let s = if a.is_nan() || a == 0.0 {
                a
            } else {
                a.signum()
            };
            (s, number)
        }
    };
    Some(Calc::Value(value, unit))
}

/// `sin()`, keeping 0⁻ (#trig-infinities).
fn sin(r: f64) -> f64 {
    if r == 0.0 { r } else { r.sin() }
}

/// `tan()`: ±∞ at the asymptotes when the angle is held exactly in a
/// unit where they are exact (degrees, grads, turns), as #trig-infinities asks of
/// implementations that can; otherwise the nearest representable answer.
fn tan(x: f64, unit: CalcUnit) -> f64 {
    if let CalcUnit::Dim(u) = unit
        && u.name != "rad"
        && x.is_finite()
    {
        let deg = x * u.factor.expect("a known angle");
        let m = deg.rem_euclid(360.0);
        if m == 90.0 {
            return f64::INFINITY;
        }
        if m == 270.0 {
            return f64::NEG_INFINITY;
        }
    }
    let r = match unit {
        CalcUnit::Dim(u) => x * u.factor.expect("a known angle") * std::f64::consts::PI / 180.0,
        _ => x,
    };
    if r == 0.0 { r } else { r.tan() }
}

/// `round(strategy, a, b)` (#round-func with its argument ranges, #round-infinities).
fn round(s: Strategy, a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() || b == 0.0 || (a.is_infinite() && b.is_infinite()) {
        return f64::NAN;
    }
    if a.is_infinite() {
        return a;
    }
    let pos_zero = |a: f64| if a.is_sign_positive() { 0.0 } else { -0.0 };
    if b.is_infinite() {
        return match s {
            Strategy::Up if a > 0.0 => f64::INFINITY,
            Strategy::Down if a < 0.0 => f64::NEG_INFINITY,
            _ => pos_zero(a),
        };
    }
    let step = b.abs();
    let q = a / step;
    if q == q.floor() {
        return a;
    }
    let lower = q.floor() * step;
    let upper = q.ceil() * step;
    // A zero lower multiple is 0⁺, a zero upper one 0⁻.
    let lower = if lower == 0.0 { 0.0 } else { lower };
    let upper = if upper == 0.0 { -0.0 } else { upper };
    match s {
        Strategy::Up => upper,
        Strategy::Down => lower,
        Strategy::ToZero => {
            if a.is_sign_negative() {
                upper
            } else {
                lower
            }
        }
        _ => {
            if (a - lower).abs() < (upper - a).abs() {
                lower
            } else {
                upper
            }
        }
    }
}

/// `mod(a, b)` (`floored`) or `rem(a, b)` (#round-func, #round-infinities).
fn modulo(a: f64, b: f64, floored: bool) -> f64 {
    if a.is_nan() || b.is_nan() || b == 0.0 || a.is_infinite() {
        return f64::NAN;
    }
    if b.is_infinite() {
        if floored && a.is_sign_negative() != b.is_sign_negative() {
            return f64::NAN;
        }
        return a;
    }
    let r = a % b; // truncated: the sign of a, so rem() is done
    if !floored {
        return r;
    }
    if r == 0.0 {
        // mod(): the range starts at 0⁺ for a positive B, 0⁻ for a
        // negative one.
        return if b > 0.0 { 0.0 } else { -0.0 };
    }
    if (r < 0.0) != (b < 0.0) { r + b } else { r }
}

/// `pow(a, b)` (#exponent-infinities): IEEE `pow` except that NaN is infectious and
/// ±1 to an infinite power is NaN.
fn pow(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() || (b.is_infinite() && a.abs() == 1.0) {
        return f64::NAN;
    }
    a.powf(b)
}

/// "Serialize a math function" (#calc-serialize), for a specified value.
pub fn serialize(m: &MathValue) -> String {
    let root = &m.root;
    if let Calc::Value(v, u) = root
        && !v.is_finite()
    {
        let mut s = String::from("calc(");
        s.push_str(special(*v));
        if *u != CalcUnit::Number {
            s.push_str(" * 1");
            s.push_str(&unit_text(canonical_of(*u)));
        }
        s.push(')');
        return s;
    }
    match root {
        Calc::Fn(f, children) => function(*f, children),
        _ => format!("calc({})", strip(tree(root))),
    }
}

/// The unit a value of this unit's type serializes in when infinite.
fn canonical_of(u: CalcUnit) -> CalcUnit {
    match u {
        CalcUnit::Dim(unit) => canonical(unit.base).map_or(u, CalcUnit::Dim),
        u => u,
    }
}

fn special(v: f64) -> &'static str {
    if v.is_nan() {
        "NaN"
    } else if v > 0.0 {
        "infinity"
    } else {
        "-infinity"
    }
}

fn unit_text(u: CalcUnit) -> String {
    match u {
        CalcUnit::Number => String::new(),
        CalcUnit::Percent => "%".to_string(),
        CalcUnit::Dim(unit) => unit.name.to_ascii_lowercase(),
    }
}

/// Removes one pair of enclosing parentheses, if the text has them.
fn strip(s: String) -> String {
    if s.starts_with('(') && s.ends_with(')') {
        s[1..s.len() - 1].to_string()
    } else {
        s
    }
}

/// An operator node that isn't a calc-operator node, as a function.
fn function(f: MathFn, children: &[Calc]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let MathFn::Round(s) = f
        && s != Strategy::Nearest
    {
        parts.push(s.name().to_string());
    }
    parts.extend(children.iter().map(|c| strip(tree(c))));
    format!("{}({})", f.name(), parts.join(", "))
}

/// A numeric value, as its own serialization ("per the normal rules").
fn value(v: f64, u: CalcUnit) -> String {
    if v.is_finite() {
        format!("{}{}", number(v), unit_text(u))
    } else if u == CalcUnit::Number {
        special(v).to_string()
    } else {
        format!("({} * 1{})", special(v), unit_text(u))
    }
}

/// "Serialize a calculation tree".
fn tree(root: &Calc) -> String {
    match root {
        Calc::Value(v, u) => value(*v, *u),
        Calc::None => "none".to_string(),
        Calc::Fn(f, children) => function(*f, children),
        Calc::Negate(c) => format!("(-1 * {})", tree(c)),
        Calc::Invert(c) => format!("(1 / {})", tree(c)),
        Calc::Sum(children) => {
            let sorted = sort(children);
            let mut s = String::from("(");
            s.push_str(&tree(sorted[0]));
            for c in &sorted[1..] {
                match c {
                    Calc::Negate(inner) => {
                        s.push_str(" - ");
                        s.push_str(&tree(inner));
                    }
                    Calc::Value(v, u) if *v < 0.0 => {
                        s.push_str(" - ");
                        s.push_str(&value(-v, *u));
                    }
                    c => {
                        s.push_str(" + ");
                        s.push_str(&tree(c));
                    }
                }
            }
            s.push(')');
            s
        }
        Calc::Product(children) => {
            let sorted = sort(children);
            let mut s = String::from("(");
            s.push_str(&tree(sorted[0]));
            for c in &sorted[1..] {
                match c {
                    Calc::Invert(inner) => {
                        s.push_str(" / ");
                        s.push_str(&tree(inner));
                    }
                    c => {
                        s.push_str(" * ");
                        s.push_str(&tree(c));
                    }
                }
            }
            s.push(')');
            s
        }
    }
}

/// "Sort a calculation's children".
fn sort(nodes: &[Calc]) -> Vec<&Calc> {
    fn take<'c>(rest: &mut Vec<&'c Calc>, ret: &mut Vec<&'c Calc>, want: CalcUnit) {
        if let Some(i) = rest
            .iter()
            .position(|n| matches!(n, Calc::Value(_, u) if u.same(want)))
        {
            ret.push(rest.remove(i));
        }
    }
    let mut rest: Vec<&Calc> = nodes.iter().collect();
    let mut ret = Vec::new();
    take(&mut rest, &mut ret, CalcUnit::Number);
    take(&mut rest, &mut ret, CalcUnit::Percent);
    let mut dims: Vec<&Calc> = Vec::new();
    rest.retain(|n| {
        if matches!(n, Calc::Value(_, CalcUnit::Dim(_))) {
            dims.push(n);
            false
        } else {
            true
        }
    });
    dims.sort_by_key(|n| match n {
        Calc::Value(_, CalcUnit::Dim(u)) => u.name.to_ascii_lowercase(),
        _ => unreachable!("only dimensions"),
    });
    ret.extend(dims);
    ret.extend(rest);
    ret
}
