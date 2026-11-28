//! Math functions (css-values-4 10): their syntax (10.8) and type checking
//! (10.9), with the type algebra of css-typed-om-1 ("Numeric Value
//! Typing": add, multiply, invert, match, apply the percent hint).
//!
//! Only validity is decided here: a math function is valid when its
//! calculations parse and its type matches the slot it is in. Values are
//! computed in a later step.
//!
//! Readings of the spec (chosen, why, rejected):
//!
//! - **The calculation context is the grammar slot's.** "Math functions
//!   always inherit the calculation context from wherever they're used"
//!   (10.9.1), and the context decides how a percentage is typed. A slot
//!   is a basic data type in a property's grammar together with its `|`
//!   alternatives ([`Context`]): in `line-height`'s `<number> |
//!   <length-percentage>`, percentages resolve against lengths for the
//!   `<number>` too. Rejected: deriving the context from the type alone,
//!   which rejects `line-height: calc(10% / 10%)`, a valid `<number>`.
//! - **`mod()` and `rem()` need the same type** before percent hints are
//!   applied, as their definition says ("must have the *same* type"),
//!   besides adding the types as the type-checking section says for them.
//! - **Nesting is limited** to [`super::MAX_NESTING`] functions and blocks,
//!   which the spec allows ("at least 32 levels of nesting"; deeper is
//!   "treated as if it were invalid"). Terms and arguments are not
//!   limited: they are read iteratively.

use optimal_css::{BlockKind, ComponentValue, Kind, Token, Values};

use super::MAX_NESTING;
use super::types::{Base, Builtin, unit_of};

const BASES: [Base; 7] = [
    Base::Length,
    Base::Angle,
    Base::Time,
    Base::Frequency,
    Base::Resolution,
    Base::Flex,
    Base::Percent,
];

fn idx(b: Base) -> usize {
    BASES
        .iter()
        .position(|&x| x == b)
        .expect("every base is listed")
}

/// How percentages resolve where a math function is used (10.9.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Context {
    /// Percentages aren't allowed in the slot.
    NoPercent,
    /// Percentages are allowed but not resolved against another type
    /// (or resolved against a `<number>`).
    Percent,
    /// Percentages resolve against this base type (never `Percent`).
    Against(Base),
}

/// A numeric type: an exponent per base type and a percent hint
/// (css-typed-om-1 "type"). Exponents saturate instead of wrapping, so a
/// huge product can't come back around to a matching type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Type {
    exp: [i32; 7],
    hint: Option<Base>,
}

impl Type {
    const NUMBER: Type = Type {
        exp: [0; 7],
        hint: None,
    };

    fn of(b: Base) -> Type {
        let mut t = Type::NUMBER;
        t.exp[idx(b)] = 1;
        t
    }

    /// "apply the percent hint" (written for a type without a hint; used
    /// provisionally on hinted types by "add two types", as written).
    fn apply_hint(mut self, hint: Base) -> Type {
        self.hint = Some(hint);
        if hint != Base::Percent {
            let p = self.exp[idx(Base::Percent)];
            let h = &mut self.exp[idx(hint)];
            *h = h.saturating_add(p);
            self.exp[idx(Base::Percent)] = 0;
        }
        self
    }

    fn same_entries(&self, other: &Type) -> bool {
        self.exp == other.exp
    }

    /// "add two types".
    pub fn sum(self, other: Type) -> Option<Type> {
        let (mut t1, mut t2) = (self, other);
        match (t1.hint, t2.hint) {
            (Some(a), Some(b)) if a != b => return None,
            (Some(h), None) => t2 = t2.apply_hint(h),
            (None, Some(h)) => t1 = t1.apply_hint(h),
            _ => {}
        }
        if t1.same_entries(&t2) {
            return Some(t1);
        }
        // Step 4 tries each hint only when a type has a non-zero
        // "percent" and one of them a non-zero other entry. That test
        // needs no code: without a "percent", applying a hint changes no
        // entry, so the entries still differ as step 3 found; with only
        // "percent" entries, each hint moves them to the same key, where
        // they still differ. So the loop alone gives the same answers.
        for &hint in &BASES[..6] {
            let (a, b) = (t1.apply_hint(hint), t2.apply_hint(hint));
            if a.same_entries(&b) {
                return Some(Type {
                    exp: a.exp,
                    hint: Some(hint),
                });
            }
        }
        None
    }

    /// "multiply two types".
    pub fn multiply(self, other: Type) -> Option<Type> {
        let (mut t1, mut t2) = (self, other);
        match (t1.hint, t2.hint) {
            (Some(a), Some(b)) if a != b => return None,
            (Some(h), None) => t2 = t2.apply_hint(h),
            (None, Some(h)) => t1 = t1.apply_hint(h),
            _ => {}
        }
        let mut exp = t1.exp;
        for (e, o) in exp.iter_mut().zip(t2.exp) {
            *e = e.saturating_add(o);
        }
        Some(Type { exp, hint: t1.hint })
    }

    /// "invert a type".
    pub fn invert(self) -> Type {
        let mut exp = self.exp;
        for e in &mut exp {
            *e = e.saturating_neg();
        }
        Type {
            exp,
            hint: self.hint,
        }
    }

    /// "make a type consistent" with `input`.
    fn made_consistent(mut self, input: Type) -> Option<Type> {
        match (self.hint, input.hint) {
            (Some(a), Some(b)) if a != b => None,
            (None, h) => {
                self.hint = h;
                Some(self)
            }
            _ => Some(self),
        }
    }

    fn only(&self, b: Option<Base>) -> bool {
        (0..7).all(|i| self.exp[i] == if Some(i) == b.map(idx) { 1 } else { 0 })
    }

    fn hint_ok(&self, ctx: Context, for_number: bool) -> bool {
        match (ctx, self.hint) {
            (_, None) => true,
            (Context::Against(x), Some(h)) => h == x,
            (Context::Percent, Some(h)) => for_number && h == Base::Percent,
            (Context::NoPercent, Some(_)) => false,
        }
    }

    /// Whether the type matches `<number>` in `ctx` ("match").
    pub fn matches_number(&self, ctx: Context) -> bool {
        self.only(None) && self.hint_ok(ctx, true)
    }

    fn matches_base(&self, b: Base, ctx: Context) -> bool {
        self.only(Some(b)) && self.hint_ok(ctx, false)
    }

    fn matches_percentage(&self) -> bool {
        self.only(Some(Base::Percent)) && matches!(self.hint, None | Some(Base::Percent))
    }

    /// Whether the type matches a numeric builtin in `ctx`. A math
    /// function that resolves to `<number>` is allowed where only
    /// `<integer>` is (10.9).
    pub fn matches_builtin(&self, b: Builtin, ctx: Context) -> bool {
        match b {
            Builtin::Integer | Builtin::Number => self.matches_number(ctx),
            Builtin::Percentage => self.matches_percentage(),
            Builtin::Length => self.matches_base(Base::Length, ctx),
            Builtin::Angle => self.matches_base(Base::Angle, ctx),
            Builtin::Time => self.matches_base(Base::Time, ctx),
            Builtin::Frequency => self.matches_base(Base::Frequency, ctx),
            Builtin::Resolution => self.matches_base(Base::Resolution, ctx),
            Builtin::LengthPercentage | Builtin::LengthPercentageWithUnit => {
                self.matches_base(Base::Length, ctx) || self.matches_percentage()
            }
            Builtin::AnglePercentage => {
                self.matches_base(Base::Angle, ctx) || self.matches_percentage()
            }
            Builtin::TimePercentage => {
                self.matches_base(Base::Time, ctx) || self.matches_percentage()
            }
            Builtin::FrequencyPercentage => {
                self.matches_base(Base::Frequency, ctx) || self.matches_percentage()
            }
            _ => false,
        }
    }
}

/// The math functions (10.8), ASCII case-insensitively.
const MATH_FUNCTIONS: &[&str] = &[
    "calc", "min", "max", "clamp", "round", "mod", "rem", "sin", "cos", "tan", "asin", "acos",
    "atan", "atan2", "pow", "sqrt", "hypot", "log", "exp", "abs", "sign",
];

/// Whether `name` names a math function.
pub fn is_math_function(name: &str) -> bool {
    MATH_FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(name))
}

const ROUNDING_STRATEGIES: &[&str] = &["nearest", "up", "down", "to-zero", "line-width"];

/// A component value of a calculation, with whether white space comes
/// right before and right after it.
#[derive(Clone, Copy)]
struct Item<'t, 'a> {
    v: ComponentValue<'t, 'a>,
    ws_before: bool,
    ws_after: bool,
}

fn is_ws(v: &ComponentValue<'_, '_>) -> bool {
    matches!(v.kind(), Kind::Token(Token::Whitespace))
}

/// The arguments of a function: comma-separated runs of items, white
/// space dropped but remembered on its neighbours.
fn arguments<'t, 'a>(args: Values<'t, 'a>) -> Vec<Vec<Item<'t, 'a>>> {
    let mut out: Vec<Vec<Item<'t, 'a>>> = vec![Vec::new()];
    let mut ws = false;
    for v in args {
        if is_ws(&v) {
            ws = true;
            if let Some(last) = out.last_mut().and_then(|a| a.last_mut()) {
                last.ws_after = true;
            }
            continue;
        }
        if matches!(v.kind(), Kind::Token(Token::Comma)) {
            out.push(Vec::new());
            ws = false;
            continue;
        }
        out.last_mut().expect("never empty").push(Item {
            v,
            ws_before: ws,
            ws_after: false,
        });
        ws = false;
    }
    out
}

fn delim(item: &Item<'_, '_>) -> Option<char> {
    match item.v.kind() {
        Kind::Token(Token::Delim(c)) => Some(*c),
        _ => None,
    }
}

/// The type of one `<calc-value>`, or `None` if it isn't one.
fn value_type(item: &Item<'_, '_>, ctx: Context, depth: u32) -> Option<Type> {
    match item.v.kind() {
        Kind::Token(Token::Number(_)) => Some(Type::NUMBER),
        Kind::Token(Token::Percentage { .. }) => Some(match ctx {
            Context::Against(b) => Type::of(b).apply_hint(b),
            _ => Type::of(Base::Percent).apply_hint(Base::Percent),
        }),
        Kind::Token(Token::Dimension(_, unit)) => Some(Type::of(unit_of(unit)?.base)),
        Kind::Token(Token::Ident(id)) => ["e", "pi", "infinity", "-infinity", "nan"]
            .iter()
            .any(|k| k.eq_ignore_ascii_case(id))
            .then_some(Type::NUMBER),
        Kind::Function(name, args) => {
            if is_math_function(name) {
                function_type(name, args, ctx, depth + 1)
            } else {
                None
            }
        }
        Kind::Block(BlockKind::Paren, inner) => {
            if depth + 1 > MAX_NESTING {
                return None;
            }
            let mut args = arguments(inner);
            if args.len() != 1 {
                return None;
            }
            sum_type(&args.pop().expect("one argument"), ctx, depth + 1)
        }
        _ => None,
    }
}

/// The type of a `<calc-sum>` (10.8, typed per 10.9), or `None` if the
/// items aren't one or its type is failure.
fn sum_type(items: &[Item<'_, '_>], ctx: Context, depth: u32) -> Option<Type> {
    if items.is_empty() {
        return None;
    }
    let mut sum: Option<Type> = None;
    let mut product: Option<Type> = None;
    // What the next item must be: a value, or an operator.
    let mut want_value = true;
    let mut pending: Option<char> = None;
    for item in items {
        if want_value {
            let t = value_type(item, ctx, depth)?;
            product = Some(match (product, pending) {
                (None, _) => t,
                (Some(p), Some('*')) => p.multiply(t)?,
                (Some(p), Some('/')) => p.multiply(t.invert())?,
                _ => unreachable!("a value after a product follows * or /"),
            });
            want_value = false;
            continue;
        }
        match delim(item) {
            Some(c @ ('*' | '/')) => pending = Some(c),
            // + and - need white space on both sides (10.8).
            Some('+' | '-') if item.ws_before && item.ws_after => {
                let p = product.take().expect("a product before + or -");
                sum = Some(match sum {
                    None => p,
                    Some(s) => s.sum(p)?,
                });
                pending = None;
            }
            _ => return None,
        }
        want_value = true;
    }
    if want_value {
        return None;
    }
    let p = product.expect("a product at the end");
    match sum {
        None => Some(p),
        Some(s) => s.sum(p),
    }
}

fn add_all(types: impl IntoIterator<Item = Type>) -> Option<Type> {
    let mut it = types.into_iter();
    let first = it.next()?;
    it.try_fold(first, Type::sum)
}

/// The type of a math function, or `None` if it is invalid. `depth` is
/// the number of functions and blocks around it, it included.
pub fn function_type(name: &str, args: Values<'_, '_>, ctx: Context, depth: u32) -> Option<Type> {
    if depth > MAX_NESTING {
        return None;
    }
    let args = arguments(args);
    let lower = name.to_ascii_lowercase();
    let calc = |a: &[Item<'_, '_>]| sum_type(a, ctx, depth);
    let is_none = |a: &[Item<'_, '_>]| matches!(a, [one] if matches!(one.v.kind(), Kind::Token(Token::Ident(id)) if id.eq_ignore_ascii_case("none")));
    let number = |t: Type| t.matches_number(ctx).then_some(t);
    match (lower.as_str(), args.as_slice()) {
        ("calc" | "abs", [a]) => calc(a),
        ("min" | "max" | "hypot", list) => {
            let types: Option<Vec<Type>> = list.iter().map(|a| calc(a)).collect();
            add_all(types?)
        }
        ("clamp", [lo, val, hi]) => {
            let mut types = vec![calc(val)?];
            for bound in [lo, hi] {
                if !is_none(bound) {
                    types.push(calc(bound)?);
                }
            }
            add_all(types)
        }
        ("round", list) => {
            let (strategy, rest) = match list.first().map(|a| a.as_slice()) {
                Some([one]) => match one.v.kind() {
                    Kind::Token(Token::Ident(id))
                        if ROUNDING_STRATEGIES
                            .iter()
                            .any(|s| s.eq_ignore_ascii_case(id)) =>
                    {
                        (Some(id.to_ascii_lowercase()), &list[1..])
                    }
                    _ => (None, list),
                },
                _ => (None, list),
            };
            let line_width = strategy.as_deref() == Some("line-width");
            match rest {
                [a] => {
                    let t = calc(a)?;
                    if line_width {
                        t.matches_builtin(Builtin::Length, ctx).then_some(t)
                    } else {
                        number(t)
                    }
                }
                [a, b] => {
                    let t = calc(a)?;
                    if line_width && !t.matches_builtin(Builtin::Length, ctx) {
                        return None;
                    }
                    t.sum(calc(b)?)
                }
                _ => None,
            }
        }
        ("mod" | "rem", [a, b]) => {
            let (ta, tb) = (calc(a)?, calc(b)?);
            if !ta.same_entries(&tb) {
                return None;
            }
            ta.sum(tb)
        }
        ("sin" | "cos" | "tan", [a]) => {
            let t = calc(a)?;
            if !(t.matches_number(ctx) || t.matches_builtin(Builtin::Angle, ctx)) {
                return None;
            }
            Type::NUMBER.made_consistent(t)
        }
        ("asin" | "acos" | "atan", [a]) => {
            let t = number(calc(a)?)?;
            Type::of(Base::Angle).made_consistent(t)
        }
        ("atan2", [a, b]) => {
            let t = calc(a)?.sum(calc(b)?)?;
            Type::of(Base::Angle).made_consistent(t)
        }
        ("pow", [a, b]) => {
            let (ta, tb) = (number(calc(a)?)?, number(calc(b)?)?);
            ta.sum(tb)
        }
        ("sqrt" | "exp", [a]) => {
            let t = number(calc(a)?)?;
            Type::NUMBER.made_consistent(t)
        }
        ("log", [a]) => Type::NUMBER.made_consistent(number(calc(a)?)?),
        ("log", [a, b]) => {
            let t = number(calc(a)?)?.sum(number(calc(b)?)?)?;
            Type::NUMBER.made_consistent(t)
        }
        ("sign", [a]) => Type::NUMBER.made_consistent(calc(a)?),
        _ => None,
    }
}

/// The type algebra on its own, step by step as css-typed-om-1 writes it.
/// Some of these types (such as an unhinted "percent") never come out of
/// a calculation here, but the algorithms are defined for every type.
#[cfg(test)]
mod tests {
    use super::*;

    /// A type with the given (base, exponent) entries and hint.
    fn ty(entries: &[(Base, i32)], hint: Option<Base>) -> Type {
        let mut t = Type::NUMBER;
        for &(b, e) in entries {
            t.exp[idx(b)] = e;
        }
        t.hint = hint;
        t
    }

    use Base::{Angle, Length, Percent, Time};

    #[test]
    fn sum_applies_one_sided_hints_first() {
        // Step 2: type1 has hint length, type2 none, so type2 gets it:
        // {percent 1} becomes {length 1}, which differs from {angle 1}, and
        // neither has a "percent" left, so step 4 doesn't apply: failure.
        // (Trying hints without step 2 would match at hint angle.)
        let angle_hinted = ty(&[(Angle, 1)], Some(Length));
        let percent = ty(&[(Percent, 1)], None);
        assert_eq!(angle_hinted.sum(percent), None);
        assert_eq!(percent.sum(angle_hinted), None);
        // And when the hint makes them equal, the result keeps it.
        let length_hinted = ty(&[(Length, 1)], Some(Length));
        assert_eq!(length_hinted.sum(percent), Some(length_hinted));
        assert_eq!(percent.sum(length_hinted), Some(length_hinted));
    }

    #[test]
    fn sum_fails_on_two_different_hints() {
        // Step 2: both hinted, differently: failure, even though hint
        // length would make {percent 1} and {length 1} equal.
        let a = ty(&[(Percent, 1)], Some(Percent));
        let b = ty(&[(Length, 1)], Some(Length));
        assert_eq!(a.sum(b), None);
        // The same hint on both is fine.
        assert_eq!(b.sum(b), Some(b));
    }

    #[test]
    fn sum_tries_each_hint() {
        // Step 4: a "percent" on either side and another entry on either
        // side: the first hint that makes them equal wins.
        let length = ty(&[(Length, 1)], None);
        let percent = ty(&[(Percent, 1)], None);
        let want = Some(ty(&[(Length, 1)], Some(Length)));
        assert_eq!(length.sum(percent), want);
        assert_eq!(percent.sum(length), want);
        let time = ty(&[(Time, 1)], None);
        assert_eq!(percent.sum(time), Some(ty(&[(Time, 1)], Some(Time))));
        // No hint helps {length 1} + {length 2}, or {percent 1} +
        // {percent 2}, or a number and a percent.
        assert_eq!(length.sum(ty(&[(Length, 2)], None)), None);
        assert_eq!(percent.sum(ty(&[(Percent, 2)], None)), None);
        assert_eq!(percent.sum(Type::NUMBER), None);
        // {length 1, percent -1} + number: hint length cancels.
        let cancel = ty(&[(Length, 1), (Percent, -1)], None);
        assert_eq!(cancel.sum(Type::NUMBER), Some(ty(&[], Some(Length))));
    }

    #[test]
    fn multiply_applies_hints_then_adds_exponents() {
        // Step 2 as for sums; then entries add, and the result has
        // type1's hint (after step 2, both have the same one).
        let length_hinted = ty(&[(Length, 1)], Some(Length));
        let percent = ty(&[(Percent, 1)], None);
        let want = Some(ty(&[(Length, 2)], Some(Length)));
        assert_eq!(length_hinted.multiply(percent), want);
        assert_eq!(percent.multiply(length_hinted), want);
        // Two different hints: failure; the same hint: fine.
        let other = ty(&[(Percent, 1)], Some(Percent));
        assert_eq!(length_hinted.multiply(other), None);
        assert_eq!(
            length_hinted.multiply(length_hinted),
            Some(ty(&[(Length, 2)], Some(Length)))
        );
        // No hints: entries add, the result has none.
        let angle = ty(&[(Angle, 1)], None);
        assert_eq!(
            angle.multiply(angle.invert()),
            Some(ty(&[], None)),
            "deg/deg is a number"
        );
    }

    #[test]
    fn made_consistent_takes_the_input_hint() {
        // "make a type consistent": different hints fail; a missing hint
        // takes the input's; the same hint stays.
        let length = ty(&[], Some(Length));
        let percent = ty(&[], Some(Percent));
        assert_eq!(length.made_consistent(percent), None);
        assert_eq!(length.made_consistent(length), Some(length));
        assert_eq!(Type::NUMBER.made_consistent(percent), Some(percent));
        assert_eq!(length.made_consistent(Type::NUMBER), Some(length));
    }

    #[test]
    fn hints_and_contexts() {
        // A hinted <number> matches where percentages resolve against the
        // hint, or, for hint percent, where they are plain percentages;
        // a <length> never matches with hint percent.
        let number_percent = ty(&[], Some(Percent));
        assert!(number_percent.matches_number(Context::Percent));
        assert!(!number_percent.matches_number(Context::NoPercent));
        assert!(!number_percent.matches_number(Context::Against(Length)));
        let length_percent = ty(&[(Length, 1)], Some(Percent));
        assert!(!length_percent.matches_builtin(Builtin::Length, Context::Percent));
        let length_hinted = ty(&[(Length, 1)], Some(Length));
        assert!(length_hinted.matches_builtin(Builtin::Length, Context::Against(Length)));
        assert!(!length_hinted.matches_builtin(Builtin::Length, Context::Percent));
    }

    #[test]
    fn every_numeric_builtin() {
        // Each base type matches its own builtin and the builtin's
        // percentage mix; a plain percentage matches only the mixes and
        // <percentage>.
        let cases = [
            (Base::Length, Builtin::Length, Builtin::LengthPercentage),
            (Base::Angle, Builtin::Angle, Builtin::AnglePercentage),
            (Base::Time, Builtin::Time, Builtin::TimePercentage),
            (
                Base::Frequency,
                Builtin::Frequency,
                Builtin::FrequencyPercentage,
            ),
        ];
        let percent = ty(&[(Percent, 1)], None);
        for (base, plain, mix) in cases {
            let t = Type::of(base);
            assert!(t.matches_builtin(plain, Context::NoPercent), "{plain:?}");
            assert!(t.matches_builtin(mix, Context::Against(base)), "{mix:?}");
            assert!(!t.matches_builtin(Builtin::Number, Context::NoPercent));
            assert!(!percent.matches_builtin(plain, Context::NoPercent));
            assert!(percent.matches_builtin(mix, Context::Against(base)));
        }
        let res = Type::of(Base::Resolution);
        assert!(res.matches_builtin(Builtin::Resolution, Context::NoPercent));
        assert!(!res.matches_builtin(Builtin::Time, Context::NoPercent));
        assert!(percent.matches_builtin(Builtin::Percentage, Context::Percent));
        assert!(!res.matches_builtin(Builtin::String, Context::NoPercent));
    }

    #[test]
    fn math_function_names() {
        assert!(is_math_function("calc"));
        assert!(is_math_function("ATAN2"));
        assert!(!is_math_function("var"));
        assert!(!is_math_function("calc-size"));
    }
}
