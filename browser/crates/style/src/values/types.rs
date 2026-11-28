//! The basic data types (css-values-4 4 to 8, with the color keywords of
//! css-color-4) and how a single component value matches one.
//!
//! A numeric type matches a literal token of that type (with its unit, or
//! a unitless zero for `<length>`, 6.1) inside the type's bracketed range
//! (4.1), or a math function whose type matches ([`super::math`]). Math
//! functions are not range-checked at parse time: the spec clamps their
//! result later (10.12).

use optimal_css::{ComponentValue, Kind, Token};

use super::math::{self, Context};

/// A bracketed range on a numeric type (css-values-4 4.1), in the type's
/// canonical unit (`deg`, `s`, `Hz`, `dppx`; `px` for lengths, whose
/// ranges this engine only allows to bound the sign).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    /// The least value allowed.
    pub min: f64,
    /// The greatest value allowed.
    pub max: f64,
}

impl Bounds {
    /// No restriction.
    pub const ANY: Bounds = Bounds {
        min: f64::NEG_INFINITY,
        max: f64::INFINITY,
    };

    /// Reads `[min,max]`, each a number with an optional unit, `∞` or
    /// `-∞`.
    pub fn parse(text: &str) -> Option<Bounds> {
        let inner = text.strip_prefix('[')?.strip_suffix(']')?;
        let (a, b) = inner.split_once(',')?;
        let end = |s: &str| -> Option<f64> {
            let s = s.trim();
            match s {
                "∞" | "+∞" => return Some(f64::INFINITY),
                "-∞" | "−∞" => return Some(f64::NEG_INFINITY),
                _ => {}
            }
            let split = s
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
                .unwrap_or(s.len());
            let (num, unit) = s.split_at(split);
            let v: f64 = num.parse().ok()?;
            if unit.is_empty() {
                Some(v)
            } else {
                let u = unit_of(unit)?;
                u.factor.map(|f| v * f)
            }
        };
        let (min, max) = (end(a)?, end(b)?);
        (min <= max).then_some(Bounds { min, max })
    }

    fn contains(self, v: f64) -> bool {
        // -0 counts as 0: `<length [0,∞]>` accepts `-0px`.
        v >= self.min && v <= self.max
    }
}

/// The base types of numeric values (css-typed-om-1 "base types"), in
/// the spec's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Base {
    /// `<length>`
    Length,
    /// `<angle>`
    Angle,
    /// `<time>`
    Time,
    /// `<frequency>`
    Frequency,
    /// `<resolution>`
    Resolution,
    /// `<flex>` (css-grid-2; no property here takes it yet)
    Flex,
    /// `<percentage>`
    Percent,
}

/// A unit: its base type and, for units whose size is fixed, how many
/// canonical units it is (`None` for relative lengths).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unit {
    /// Its name, as the spec writes it.
    pub name: &'static str,
    /// Its base type.
    pub base: Base,
    /// Its size in the canonical unit, when fixed.
    pub factor: Option<f64>,
}

const fn rel(name: &'static str) -> Unit {
    Unit {
        name,
        base: Base::Length,
        factor: None,
    }
}

const fn abs(name: &'static str, base: Base, factor: f64) -> Unit {
    Unit {
        name,
        base,
        factor: Some(factor),
    }
}

/// Every unit of css-values-4 (6.1 to 7.4), in the spec's order. Unit
/// names are ASCII case-insensitive (css-values-4 6).
#[rustfmt::skip]
pub const UNITS: &[Unit] = &[
    // 6.1.1 font-relative lengths
    rel("em"), rel("rem"), rel("ex"), rel("rex"), rel("cap"), rel("rcap"), rel("ch"),
    rel("rch"), rel("ic"), rel("ric"), rel("lh"), rel("rlh"),
    // 6.1.2 viewport-percentage lengths
    rel("vw"), rel("svw"), rel("lvw"), rel("dvw"), rel("vh"), rel("svh"), rel("lvh"),
    rel("dvh"), rel("vi"), rel("svi"), rel("lvi"), rel("dvi"), rel("vb"), rel("svb"),
    rel("lvb"), rel("dvb"), rel("vmin"), rel("svmin"), rel("lvmin"), rel("dvmin"),
    rel("vmax"), rel("svmax"), rel("lvmax"), rel("dvmax"),
    // 6.2 absolute lengths, in px
    abs("cm", Base::Length, 96.0 / 2.54), abs("mm", Base::Length, 96.0 / 25.4),
    abs("Q", Base::Length, 96.0 / 101.6), abs("in", Base::Length, 96.0),
    abs("pc", Base::Length, 16.0), abs("pt", Base::Length, 4.0 / 3.0),
    abs("px", Base::Length, 1.0),
    // 7.1 angles, in deg
    abs("deg", Base::Angle, 1.0), abs("grad", Base::Angle, 0.9),
    abs("rad", Base::Angle, 180.0 / std::f64::consts::PI), abs("turn", Base::Angle, 360.0),
    // 7.2 time, in s
    abs("s", Base::Time, 1.0), abs("ms", Base::Time, 0.001),
    // 7.3 frequency, in Hz
    abs("Hz", Base::Frequency, 1.0), abs("kHz", Base::Frequency, 1000.0),
    // 7.4 resolution, in dppx
    abs("dpi", Base::Resolution, 1.0 / 96.0), abs("dpcm", Base::Resolution, 2.54 / 96.0),
    abs("dppx", Base::Resolution, 1.0), abs("x", Base::Resolution, 1.0),
];

/// The unit a dimension's unit text names, ASCII case-insensitively.
pub fn unit_of(text: &str) -> Option<Unit> {
    UNITS
        .iter()
        .copied()
        .find(|u| u.name.eq_ignore_ascii_case(text))
}

/// A basic data type: what a `<name>` in a grammar stands for when no
/// production defines it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    /// `<integer>` (5.2)
    Integer,
    /// `<number>` (5.3)
    Number,
    /// `<percentage>` (5.5)
    Percentage,
    /// `<length>` (6)
    Length,
    /// `<angle>` (7.1)
    Angle,
    /// `<time>` (7.2)
    Time,
    /// `<frequency>` (7.3)
    Frequency,
    /// `<resolution>` (7.4)
    Resolution,
    /// `<length-percentage>` (5.6)
    LengthPercentage,
    /// `<length-percentage>` without the unitless zero: the `flex`
    /// shorthand's basis where a bare `0` must be a flex factor
    /// (css-flexbox-1 7.2).
    LengthPercentageWithUnit,
    /// `<angle-percentage>` (5.6)
    AnglePercentage,
    /// `<time-percentage>` (5.6)
    TimePercentage,
    /// `<frequency-percentage>` (5.6)
    FrequencyPercentage,
    /// `<zero>`: a literal number token with the value 0 (5.3).
    Zero,
    /// `<ident>`: any identifier (4.2).
    Ident,
    /// `<custom-ident>` (4.2): any identifier but the CSS-wide keywords,
    /// `default`, and the listed words, ASCII case-insensitively.
    CustomIdent(&'static [&'static str]),
    /// `<dashed-ident>` (4.3): an identifier starting with `--`.
    DashedIdent,
    /// `<string>` (4.4)
    String,
    /// `<url-token>` (css-syntax-3)
    UrlToken,
    /// A string of exactly four characters from U+0020 to U+007E: an
    /// OpenType tag (css-fonts-4 6.12, 7.2).
    FourCharacterTag,
    /// `<hex-color>` (css-color-4 5.2): a hash token of 3, 4, 6 or 8 hex
    /// digits.
    HexColor,
    /// `<named-color>` (css-color-4 6.1), without `transparent`, which the
    /// grammar lists itself.
    NamedColor,
    /// `<system-color>` (css-color-4 6.2), and the deprecated system
    /// colors, which "user agents must support" (appendix A).
    SystemColor,
}

/// The CSS-wide keywords and `default`, which no `<custom-ident>` can be
/// (css-values-4 4.2).
pub const NOT_CUSTOM_IDENT: &[&str] = &[
    "initial",
    "inherit",
    "unset",
    "revert",
    "revert-layer",
    "revert-rule",
    "default",
];

/// The system colors (css-color-4 6.2), in the spec's order.
pub const SYSTEM_COLORS: &[&str] = &[
    "AccentColor",
    "AccentColorText",
    "ActiveText",
    "ButtonBorder",
    "ButtonFace",
    "ButtonText",
    "Canvas",
    "CanvasText",
    "Field",
    "FieldText",
    "GrayText",
    "Highlight",
    "HighlightText",
    "LinkText",
    "Mark",
    "MarkText",
    "SelectedItem",
    "SelectedItemText",
    "VisitedText",
];

/// The deprecated system colors (css-color-4 appendix A), each with the
/// system color it computes to.
pub const DEPRECATED_SYSTEM_COLORS: &[(&str, &str)] = &[
    ("ActiveBorder", "ButtonBorder"),
    ("ActiveCaption", "Canvas"),
    ("AppWorkspace", "Canvas"),
    ("Background", "Canvas"),
    ("ButtonHighlight", "ButtonFace"),
    ("ButtonShadow", "ButtonFace"),
    ("CaptionText", "CanvasText"),
    ("InactiveBorder", "ButtonBorder"),
    ("InactiveCaption", "Canvas"),
    ("InactiveCaptionText", "GrayText"),
    ("InfoBackground", "Canvas"),
    ("InfoText", "CanvasText"),
    ("Menu", "Canvas"),
    ("MenuText", "CanvasText"),
    ("Scrollbar", "Canvas"),
    ("ThreeDDarkShadow", "ButtonBorder"),
    ("ThreeDFace", "ButtonFace"),
    ("ThreeDHighlight", "ButtonBorder"),
    ("ThreeDLightShadow", "ButtonBorder"),
    ("ThreeDShadow", "ButtonBorder"),
    ("Window", "Canvas"),
    ("WindowFrame", "ButtonBorder"),
    ("WindowText", "CanvasText"),
];

include!("named_colors.rs");

/// The type a numeric builtin stands for, as a base type with or without
/// a percentage mix, and the math context its percentages resolve in.
fn numeric(b: Builtin) -> Option<(Option<Base>, bool)> {
    Some(match b {
        Builtin::Integer | Builtin::Number => (None, false),
        Builtin::Percentage => (Some(Base::Percent), false),
        Builtin::Length => (Some(Base::Length), false),
        Builtin::Angle => (Some(Base::Angle), false),
        Builtin::Time => (Some(Base::Time), false),
        Builtin::Frequency => (Some(Base::Frequency), false),
        Builtin::Resolution => (Some(Base::Resolution), false),
        Builtin::LengthPercentage | Builtin::LengthPercentageWithUnit => (Some(Base::Length), true),
        Builtin::AnglePercentage => (Some(Base::Angle), true),
        Builtin::TimePercentage => (Some(Base::Time), true),
        Builtin::FrequencyPercentage => (Some(Base::Frequency), true),
        _ => return None,
    })
}

/// The base type a numeric builtin's percentages resolve against, if it
/// is a `<*-percentage>` mix; `Some(Percent)` for `<percentage>` itself.
pub fn percent_base(b: Builtin) -> Option<Base> {
    match numeric(b)? {
        (Some(base), true) => Some(base),
        (Some(Base::Percent), false) => Some(Base::Percent),
        _ => None,
    }
}

fn dimension_matches(value: f64, unit: &str, base: Base, bounds: Bounds) -> bool {
    let Some(u) = unit_of(unit) else { return false };
    if u.base != base {
        return false;
    }
    match u.factor {
        Some(f) => bounds.contains(value * f),
        // Relative lengths: the compiler only allows sign bounds on them.
        None => bounds.contains(value),
    }
}

/// Whether one component value matches a builtin type. `ctx` is the
/// calculation context of the grammar slot, for math functions;
/// `depth` counts the functions and blocks around the value.
pub fn matches(
    b: Builtin,
    bounds: Bounds,
    v: ComponentValue<'_, '_>,
    ctx: Context,
    depth: u32,
) -> bool {
    if let Some((base, mix)) = numeric(b) {
        if let Kind::Function(name, args) = v.kind() {
            return math::is_math_function(name)
                && math::function_type(name, args, ctx, depth + 1)
                    .is_some_and(|t| t.matches_builtin(b, ctx));
        }
        let Kind::Token(t) = v.kind() else {
            return false;
        };
        return match (t, base) {
            (Token::Number(n), None) => {
                (b != Builtin::Integer || n.is_integer) && bounds.contains(n.value)
            }
            (Token::Percentage { value, .. }, Some(Base::Percent)) => bounds.contains(*value),
            (Token::Percentage { value, .. }, Some(_)) if mix => {
                // A mix's bounds apply to its percentages as written.
                bounds.contains(*value)
            }
            // No unit has the base type Percent, so a dimension never
            // matches <percentage>.
            (Token::Dimension(n, unit), Some(base)) => {
                dimension_matches(n.value, unit, base, bounds)
            }
            // A unitless zero is a <length> (css-values-4 6.1).
            (Token::Number(n), Some(Base::Length)) => {
                n.value == 0.0 && b != Builtin::LengthPercentageWithUnit
            }
            _ => false,
        };
    }
    let Kind::Token(t) = v.kind() else {
        return false;
    };
    match (b, t) {
        (Builtin::Zero, Token::Number(n)) => n.value == 0.0,
        (Builtin::Ident, Token::Ident(_)) => true,
        (Builtin::CustomIdent(except), Token::Ident(id)) => !NOT_CUSTOM_IDENT
            .iter()
            .chain(except.iter())
            .any(|k| k.eq_ignore_ascii_case(id)),
        (Builtin::DashedIdent, Token::Ident(id)) => id.starts_with("--"),
        (Builtin::String, Token::String(_)) => true,
        (Builtin::UrlToken, Token::Url(_)) => true,
        (Builtin::FourCharacterTag, Token::String(t)) => {
            t.chars().count() == 4 && t.chars().all(|c| ('\u{20}'..='\u{7e}').contains(&c))
        }
        (Builtin::HexColor, Token::Hash(h, _)) => {
            matches!(h.len(), 3 | 4 | 6 | 8) && h.bytes().all(|c| c.is_ascii_hexdigit())
        }
        (Builtin::NamedColor, Token::Ident(id)) => {
            NAMED_COLORS.iter().any(|(n, _)| n.eq_ignore_ascii_case(id))
        }
        (Builtin::SystemColor, Token::Ident(id)) => {
            SYSTEM_COLORS.iter().any(|n| n.eq_ignore_ascii_case(id))
                || DEPRECATED_SYSTEM_COLORS
                    .iter()
                    .any(|(n, _)| n.eq_ignore_ascii_case(id))
        }
        _ => false,
    }
}
