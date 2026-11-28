//! Specified `<color>` values (css-color-4) and their serialization
//! (css-color-4 "Serializing `<color>` Values", which replaces that
//! part of cssom-1).
//!
//! A color reaches here already matched against its grammar, so its
//! arguments are read without checks.
//!
//! - Keywords (named colors, system colors, `transparent`,
//!   `currentcolor`) keep the keyword, ASCII lowercase ("Resolving sRGB
//!   values", "Serializing other colors").
//! - sRGB colors from hex colors, `rgb()`, `hsl()` and `hwb()` with no
//!   missing component are resolved to sRGB and serialize as `rgb()` or
//!   `rgba()` with commas ("CSS serialization of sRGB values"). Their
//!   math functions are resolved and clamped ("Resolving sRGB values":
//!   "for historical reasons").
//! - With a missing component (`none`), `rgb()` serializes as
//!   `color(srgb ...)` and `hsl()`/`hwb()` in their modern syntax
//!   ("CSS serialization of sRGB values").
//! - `lab()`, `lch()`, `oklab()`, `oklch()` and `color()` keep their form
//!   ("Serializing Lab and LCH values", "Serializing Oklab and OkLCh
//!   values", "Serializing values of the color() function"),
//!   percentages converted to numbers by the reference ranges, `L` and
//!   `C` clamped as "Resolving Lab and LCH values" and "Resolving Oklab
//!   and OkLCh values" say, math functions
//!   kept as math functions.
//!
//! Readings (chosen, why, rejected):
//!
//! - **sRGB components are kept as 8-bit integers**, rounded half up
//!   after clamping: "the precision with which sRGB component values are
//!   retained ... is not defined ... but must at least be sufficient to
//!   round-trip eight bit values" ("CSS serialization of sRGB
//!   values"), and every browser keeps eight
//!   bits, which the tests expect (`hsl(120 30% 50%)` is
//!   `rgb(89, 166, 89)`). Alpha keeps its full value, except a hex
//!   color's, which is an 8-bit integer and serializes by "Serializing
//!   legacy alpha values".
//!   Rejected: fractional sRGB components (also allowed), which differ
//!   from every browser for no gain in a specified value.
//! - **An sRGB color whose math function can't be resolved yet** (one
//!   with `em`, say) has no serialization in the spec. It serializes in
//!   the modern syntax of its function (`rgb`, `hsl` or `hwb`), literal
//!   components as numbers (`0%` as `0`), the math function as one, and
//!   the alpha as a number, as browsers do and the tests expect.

use optimal_css::{ComponentValue, Kind, Token, Values};

use super::calc::{self, Calc, CalcUnit, MathValue};
use super::math::{Context, is_math_function};
use super::serialize::number;
use super::types::{DEPRECATED_SYSTEM_COLORS, NAMED_COLORS, SYSTEM_COLORS, unit_of};

/// A specified color.
#[derive(Clone, Debug, PartialEq)]
pub enum Color {
    /// A named color, a system color, `transparent` or `currentcolor`,
    /// in ASCII lowercase.
    Keyword(String),
    /// A resolved sRGB color: 8-bit components and the alpha.
    Rgb([u8; 3], Alpha),
    /// A color that serializes in a functional form.
    Function(Box<ColorFunction>),
}

/// An alpha value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Alpha {
    /// An 8-bit integer (from a hex color).
    Byte(u8),
    /// A value in [0, 1].
    Value(f64),
}

/// A color in a functional form: the function, the color space (for
/// `color()`), three components and the alpha (`None` when it is 1).
#[derive(Clone, Debug, PartialEq)]
pub struct ColorFunction {
    /// The function name, lowercase.
    pub name: &'static str,
    /// The color space of `color()`, lowercase.
    pub space: Option<String>,
    /// The components.
    pub channels: [Channel; 3],
    /// The alpha, unless it is 1.
    pub alpha: Option<Channel>,
}

/// A component of a [`ColorFunction`].
#[derive(Clone, Debug, PartialEq)]
pub enum Channel {
    /// `none`: a missing component.
    None,
    /// A number.
    Number(f64),
    /// A percentage.
    Percent(f64),
    /// A math function.
    Math(MathValue),
}

/// One argument of a color function as written.
#[derive(Clone, Debug)]
enum Arg {
    None,
    Number(f64),
    Percent(f64),
    /// An angle, in degrees.
    Angle(f64),
    /// A math function, simplified.
    Math(MathValue),
}

impl Arg {
    /// The value, if it is known: a literal, or a math function that
    /// simplified to one value (censored: NaN is 0, css-values-4 #calc-ieee). `None` for
    /// `none` and for math functions that aren't resolved yet.
    fn resolved(&self) -> Option<(f64, CalcUnit)> {
        match self {
            Arg::None => None,
            Arg::Number(v) => Some((*v, CalcUnit::Number)),
            Arg::Percent(v) => Some((*v, CalcUnit::Percent)),
            Arg::Angle(v) => Some((*v, CalcUnit::Number)),
            Arg::Math(m) => match m.root {
                Calc::Value(v, u) => Some((if v.is_nan() { 0.0 } else { v }, u)),
                _ => None,
            },
        }
    }
}

/// Reads one argument component value; `ctx` is its slot's calculation
/// context.
fn arg(v: ComponentValue<'_, '_>, ctx: Context) -> Arg {
    match v.kind() {
        Kind::Token(Token::Ident(_)) => Arg::None,
        Kind::Token(Token::Number(n)) => Arg::Number(n.value),
        Kind::Token(Token::Percentage { value, .. }) => Arg::Percent(*value),
        Kind::Token(Token::Dimension(n, unit)) => {
            let u = unit_of(unit).expect("a valid angle unit");
            Arg::Angle(n.value * u.factor.expect("angles have fixed sizes"))
        }
        Kind::Function(name, args) if is_math_function(name) => {
            Arg::Math(calc::parse(name, args, ctx))
        }
        _ => unreachable!("a valid color argument"),
    }
}

/// The arguments of a color function: the component values that aren't
/// white space, commas or the `/` (the alpha is always the fourth
/// component argument, the first after a `color()` space).
fn items<'t, 'a>(args: Values<'t, 'a>) -> Vec<ComponentValue<'t, 'a>> {
    args.filter(|v| {
        !matches!(
            v.kind(),
            Kind::Token(Token::Whitespace | Token::Comma | Token::Delim('/'))
        )
    })
    .collect()
}

/// The specified color a single component value matching `<color>` is.
pub fn parse(v: ComponentValue<'_, '_>) -> Color {
    match v.kind() {
        Kind::Token(Token::Ident(id)) => Color::Keyword(id.to_ascii_lowercase()),
        Kind::Token(Token::Hash(h, _)) => hex(h),
        Kind::Function(name, args) => function(&name.to_ascii_lowercase(), args),
        _ => unreachable!("a valid color"),
    }
}

/// A hex color ("The RGB Hexadecimal Notations: #RRGGBB").
fn hex(h: &str) -> Color {
    let digits: Vec<u8> = h
        .bytes()
        .map(|c| (c as char).to_digit(16).expect("hex digits") as u8)
        .collect();
    let bytes: Vec<u8> = match digits.len() {
        3 | 4 => digits.iter().map(|d| d * 17).collect(),
        _ => digits.chunks(2).map(|p| p[0] * 16 + p[1]).collect(),
    };
    let alpha = bytes.get(3).copied().unwrap_or(255);
    Color::Rgb([bytes[0], bytes[1], bytes[2]], Alpha::Byte(alpha))
}

/// The 8-bit value of a component in [0, 255], clamped, rounded half up
/// ("rounded towards +∞", "CSS serialization of sRGB values"). Floating-point error below 1e-9 of a
/// unit doesn't decide the rounding: a conversion that is exactly a half
/// in decimal (`hsl()`, `hwb()`) rounds up even if it comes out a hair
/// below.
fn byte(v: f64) -> u8 {
    let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 255.0) };
    ((v * 1e9).round() / 1e9 + 0.5).floor() as u8
}

/// A color function.
fn function(name: &str, args: Values<'_, '_>) -> Color {
    let vals = items(args);
    let percent = Context::Percent;
    let hue = Context::NoPercent;
    match name {
        "rgb" | "rgba" | "hsl" | "hsla" | "hwb" => {
            let polar = !name.starts_with("rgb");
            let ctx = |i: usize| if polar && i == 0 { hue } else { percent };
            let a: Vec<Arg> = vals
                .iter()
                .enumerate()
                .map(|(i, v)| arg(*v, ctx(i)))
                .collect();
            let base: &'static str = match &name[..3] {
                "rgb" => "rgb",
                "hsl" => "hsl",
                _ => "hwb",
            };
            srgb(base, &a)
        }
        "lab" | "lch" | "oklab" | "oklch" => {
            let polar = name.ends_with("ch");
            let ctx = |i: usize| if polar && i == 2 { hue } else { percent };
            let a: Vec<Arg> = vals
                .iter()
                .enumerate()
                .map(|(i, v)| arg(*v, ctx(i)))
                .collect();
            let name: &'static str = match name {
                "lab" => "lab",
                "lch" => "lch",
                "oklab" => "oklab",
                _ => "oklch",
            };
            lab_like(name, &a)
        }
        _ => {
            // color( <colorspace-params> [ / <alpha> ]? )
            let space = match vals[0].kind() {
                Kind::Token(Token::Ident(id)) => id.to_ascii_lowercase(),
                _ => unreachable!("a color space"),
            };
            let a: Vec<Arg> = vals[1..].iter().map(|v| arg(*v, percent)).collect();
            let channel = |a: &Arg| match a {
                Arg::Percent(p) => Channel::Number(p / 100.0),
                a => literal(a),
            };
            Color::Function(Box::new(ColorFunction {
                name: "color",
                space: Some(space),
                channels: [channel(&a[0]), channel(&a[1]), channel(&a[2])],
                alpha: alpha(a.get(3)),
            }))
        }
    }
}

/// A literal or math argument as a number channel, or `none`.
fn literal(a: &Arg) -> Channel {
    match a {
        Arg::None => Channel::None,
        Arg::Number(v) | Arg::Angle(v) => Channel::Number(*v),
        Arg::Percent(v) => Channel::Percent(*v),
        Arg::Math(m) => Channel::Math(m.clone()),
    }
}

/// The alpha channel: literals clamped to [0, 1] and omitted when 1;
/// math functions kept ("Serializing modern alpha values": "not clamped when
/// the specified form is
/// serialized").
fn alpha(a: Option<&Arg>) -> Option<Channel> {
    match a? {
        Arg::None => Some(Channel::None),
        Arg::Math(m) => Some(Channel::Math(m.clone())),
        a => {
            let (v, u) = a.resolved().expect("a literal");
            let v = if u == CalcUnit::Percent { v / 100.0 } else { v };
            let v = v.clamp(0.0, 1.0);
            (v != 1.0).then_some(Channel::Number(v))
        }
    }
}

/// The value of a resolved alpha for an sRGB color: clamped, 1 if
/// omitted.
fn srgb_alpha(a: Option<&Arg>) -> Option<f64> {
    let Some(a) = a else { return Some(1.0) };
    let (v, u) = a.resolved()?;
    let v = if u == CalcUnit::Percent { v / 100.0 } else { v };
    Some(if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) })
}

/// A hue in degrees, normalized to [0, 360) (an infinite hue is NaN,
/// censored to 0).
fn normalize_hue(h: f64) -> f64 {
    let h = h.rem_euclid(360.0);
    if h.is_nan() { 0.0 } else { h }
}

/// `rgb()`, `hsl()` or `hwb()` (the name without its `a`).
fn srgb(name: &'static str, a: &[Arg]) -> Color {
    let missing = a.iter().any(|a| matches!(a, Arg::None));
    let resolved: Option<Vec<(f64, CalcUnit)>> = a.iter().map(Arg::resolved).collect();
    if let Some(r) = &resolved {
        let alpha = srgb_alpha(a.get(3)).expect("resolved");
        let comp = |i: usize| r[i].0;
        let rgb = match name {
            "rgb" => {
                let c = |i: usize| match r[i].1 {
                    CalcUnit::Percent => r[i].0 * 2.55,
                    _ => r[i].0,
                };
                [c(0), c(1), c(2)]
            }
            "hsl" => hsl_to_rgb(normalize_hue(comp(0)), comp(1).max(0.0), comp(2)),
            _ => hwb_to_rgb(normalize_hue(comp(0)), comp(1), comp(2)),
        };
        return Color::Rgb(
            [byte(rgb[0]), byte(rgb[1]), byte(rgb[2])],
            Alpha::Value(alpha),
        );
    }
    let unresolved = a
        .iter()
        .any(|a| matches!(a, Arg::Math(m) if !matches!(m.root, Calc::Value(..))));
    if missing && !unresolved {
        // The none-preserving forms of "CSS serialization of sRGB values".
        let known = |a: &Arg| a.resolved();
        if name == "rgb" {
            let channel = |a: &Arg| match known(a) {
                None => Channel::None,
                Some((v, CalcUnit::Percent)) => Channel::Number(v.clamp(0.0, 100.0) / 100.0),
                Some((v, _)) => Channel::Number(v.clamp(0.0, 255.0) / 255.0),
            };
            return Color::Function(Box::new(ColorFunction {
                name: "color",
                space: Some("srgb".to_string()),
                channels: [channel(&a[0]), channel(&a[1]), channel(&a[2])],
                alpha: srgb_none_alpha(a.get(3)),
            }));
        }
        let hue = match known(&a[0]) {
            None => Channel::None,
            Some((v, _)) => Channel::Number(normalize_hue(v)),
        };
        let pct = |a: &Arg, floor: bool| match known(a) {
            None => Channel::None,
            Some((v, _)) => Channel::Percent(if floor { v.max(0.0) } else { v }),
        };
        return Color::Function(Box::new(ColorFunction {
            name,
            space: None,
            channels: [hue, pct(&a[1], name == "hsl"), pct(&a[2], false)],
            alpha: srgb_none_alpha(a.get(3)),
        }));
    }
    // Not resolvable yet: the modern syntax, literals as numbers.
    let channel = |i: usize| -> Channel {
        match &a[i] {
            Arg::None => Channel::None,
            Arg::Math(m) if !matches!(m.root, Calc::Value(..)) => Channel::Math(m.clone()),
            a => {
                let (v, u) = a.resolved().expect("a known value");
                // Literals are clamped as when resolved: sRGB components
                // to [0, 255], hsl() saturation to at least 0.
                let v = match (name, i, u) {
                    ("rgb", _, CalcUnit::Percent) => (v * 2.55).clamp(0.0, 255.0),
                    ("rgb", _, _) => v.clamp(0.0, 255.0),
                    ("hsl" | "hwb", 0, _) => normalize_hue(v),
                    ("hsl", 1, _) => v.max(0.0),
                    _ => v,
                };
                Channel::Number(v)
            }
        }
    };
    let alpha = match a.get(3) {
        None => None,
        Some(Arg::None) => Some(Channel::None),
        Some(Arg::Math(m)) if !matches!(m.root, Calc::Value(..)) => Some(Channel::Math(m.clone())),
        Some(x) => {
            let v = srgb_alpha(Some(x)).expect("a known value");
            (v != 1.0).then_some(Channel::Number(v))
        }
    };
    Color::Function(Box::new(ColorFunction {
        name,
        space: None,
        channels: [channel(0), channel(1), channel(2)],
        alpha,
    }))
}

/// The alpha of a none-preserving sRGB form: `none`, or the resolved
/// value unless it is 1.
fn srgb_none_alpha(a: Option<&Arg>) -> Option<Channel> {
    match a {
        Some(Arg::None) => Some(Channel::None),
        a => {
            let v = srgb_alpha(a).expect("resolved");
            (v != 1.0).then_some(Channel::Number(v))
        }
    }
}

/// The `hslToRgb` of css-color-4 "Converting HSL Colors to sRGB", in [0, 255].
fn hsl_to_rgb(hue: f64, sat: f64, light: f64) -> [f64; 3] {
    let (sat, light) = (sat / 100.0, light / 100.0);
    let f = |n: f64| {
        let k = (n + hue / 30.0) % 12.0;
        let a = sat * light.min(1.0 - light);
        light - a * (-1.0f64).max((k - 3.0).min(9.0 - k).min(1.0))
    };
    [f(0.0) * 255.0, f(8.0) * 255.0, f(4.0) * 255.0]
}

/// The `hwbToRgb` of css-color-4 "Converting HWB Colors to sRGB", in [0, 255]. The whiteness and
/// blackness stay percentages until the end (`(100 - w - b) / 100`
/// rather than `1 - w/100 - b/100`, scaled to 255 last), so that values
/// exact in percent, like `hwb(120 30% 50%)`, give exact halves that
/// round as halves.
fn hwb_to_rgb(hue: f64, white: f64, black: f64) -> [f64; 3] {
    if white + black >= 100.0 {
        let gray = white / (white + black) * 255.0;
        return [gray; 3];
    }
    let rgb = hsl_to_rgb(hue, 100.0, 50.0);
    rgb.map(|c| (c / 255.0 * (100.0 - white - black) + white) * 255.0 / 100.0)
}

/// `lab()`, `lch()`, `oklab()` or `oklch()`: percentages by the reference
/// ranges, L clamped to its range, C to at least 0, and the hue
/// normalized to [0, 360) ("Resolving Lab and LCH values", "Resolving Oklab and OkLCh
/// values": "after clamping of L, C and H").
fn lab_like(name: &'static str, a: &[Arg]) -> Color {
    let ok = name.starts_with("ok");
    let polar = name.ends_with("ch");
    let (l_max, ab_ref, c_ref) = if ok {
        (1.0, 0.4, 0.4)
    } else {
        (100.0, 125.0, 150.0)
    };
    let channel = |i: usize| -> Channel {
        let (reference, clamp): (f64, Option<(f64, f64)>) = match (i, polar) {
            (0, _) => (l_max, Some((0.0, l_max))),
            (1, true) => (c_ref, Some((0.0, f64::INFINITY))),
            (1, false) | (2, false) => (ab_ref, None),
            _ => (0.0, None), // hue: numbers and angles only
        };
        match &a[i] {
            Arg::None => Channel::None,
            Arg::Math(m) => Channel::Math(m.clone()),
            arg => {
                let v = match arg {
                    Arg::Percent(p) => p / 100.0 * reference,
                    Arg::Number(v) | Arg::Angle(v) => *v,
                    _ => unreachable!("a literal"),
                };
                Channel::Number(match clamp {
                    Some((lo, hi)) => v.clamp(lo, hi),
                    None if polar && i == 2 => normalize_hue(v),
                    None => v,
                })
            }
        }
    };
    Color::Function(Box::new(ColorFunction {
        name,
        space: None,
        channels: [channel(0), channel(1), channel(2)],
        alpha: alpha(a.get(3)),
    }))
}

/// "Serializing legacy alpha values" for an 8-bit alpha.
fn legacy_byte_alpha(alpha: u8) -> String {
    let a = alpha as f64;
    for i in 0..=100u32 {
        if (i as f64 * 2.55 + 0.5).floor() == a {
            return number(i as f64 / 100.0);
        }
    }
    number((a / 0.255 + 0.5).floor() / 1000.0)
}

/// The serialization of a specified color.
pub fn serialize(c: &Color, out: &mut String) {
    match c {
        Color::Keyword(k) => out.push_str(k),
        Color::Rgb([r, g, b], alpha) => {
            let a = match alpha {
                Alpha::Byte(255) => None,
                Alpha::Byte(x) => Some(legacy_byte_alpha(*x)),
                Alpha::Value(v) if *v == 1.0 => None,
                Alpha::Value(v) => Some(number(*v)),
            };
            match a {
                None => out.push_str(&format!("rgb({r}, {g}, {b})")),
                Some(a) => out.push_str(&format!("rgba({r}, {g}, {b}, {a})")),
            }
        }
        Color::Function(f) => {
            out.push_str(f.name);
            out.push('(');
            if let Some(space) = &f.space {
                out.push_str(space);
                out.push(' ');
            }
            for (i, ch) in f.channels.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                channel(ch, out);
            }
            if let Some(a) = &f.alpha {
                out.push_str(" / ");
                channel(a, out);
            }
            out.push(')');
        }
    }
}

fn channel(c: &Channel, out: &mut String) {
    match c {
        Channel::None => out.push_str("none"),
        Channel::Number(v) => out.push_str(&number(*v)),
        Channel::Percent(v) => {
            out.push_str(&number(*v));
            out.push('%');
        }
        Channel::Math(m) => out.push_str(&calc::serialize(m)),
    }
}

/// Whether `name` (any case) is a system color or a deprecated one.
pub fn is_system_color(name: &str) -> bool {
    SYSTEM_COLORS.iter().any(|n| n.eq_ignore_ascii_case(name))
        || DEPRECATED_SYSTEM_COLORS
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(name))
}

/// The sRGB value of a named color.
pub fn named(name: &str) -> Option<[u8; 3]> {
    NAMED_COLORS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|&(_, rgb)| rgb)
}
