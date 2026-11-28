//! The parsed form of a selector (selectors-4 section 3.1, "Structure and
//! Terminology").
//!
//! A [`SelectorList`] holds complex selectors. A [`Selector`] (a complex
//! selector) is a run of [`Unit`]s with a [`Combinator`] between each pair.
//! A unit is a compound selector (its [`Simple`] selectors) followed by any
//! number of pseudo-compound selectors ([`PseudoCompound`]), which is the
//! `<complex-selector-unit>` production of the grammar (section 16).

use optimal_css::AnB;

/// A list of complex selectors (`<selector-list>`, `<complex-selector-list>`
/// and the other non-forgiving lists).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectorList(pub Vec<Selector>);

/// A complex selector: units joined by combinators, read left to right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selector {
    /// The first unit.
    pub first: Unit,
    /// Each later unit, with the combinator that joins it to the one before.
    pub rest: Vec<(Combinator, Unit)>,
}

/// A relative selector (3.3): a complex selector that starts with a
/// combinator. When none is written, it is the descendant combinator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelativeSelector {
    /// The leading combinator.
    pub combinator: Combinator,
    /// Whether the combinator was written (only the descendant combinator
    /// can be left out); serialization keeps what was written.
    pub explicit: bool,
    /// The rest of the selector.
    pub selector: Selector,
}

/// A `<complex-selector-unit>`: a compound selector, then pseudo-compound
/// selectors. At least one of the two is present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    /// The compound selector's simple selectors, in source order. A type or
    /// universal selector, if any, is first.
    pub compound: Vec<Simple>,
    /// The pseudo-compound selectors, in source order.
    pub pseudos: Vec<PseudoCompound>,
}

/// A combinator (section 14).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Combinator {
    /// Whitespace.
    Descendant,
    /// `>`
    Child,
    /// `+`
    NextSibling,
    /// `~`
    SubsequentSibling,
}

/// The namespace a type, universal or attribute selector requires.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// Any namespace, or none (`*|`; also a type or universal selector with
    /// no prefix when no default namespace is declared).
    Any,
    /// No namespace (`|`; also an attribute selector with no prefix).
    None,
    /// This namespace URL.
    Url(Box<str>),
}

/// A simple selector (`<simple-selector>`), or the nesting selector `&`
/// (css-nesting-1, "Nesting Selector"), which can stand anywhere in a compound
/// selector except before a type selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Simple {
    /// A type selector (5.1).
    Type {
        /// The namespace it requires.
        ns: Namespace,
        /// The prefix to serialize before `|`, if one is needed to keep the
        /// namespace (see [`crate::serialize`]).
        prefix: Option<Box<str>>,
        /// The element's local name, as written.
        local: Box<str>,
    },
    /// The universal selector (5.2).
    Universal {
        /// The namespace it requires.
        ns: Namespace,
        /// The prefix to serialize before `|`, if one is needed.
        prefix: Option<Box<str>>,
    },
    /// An ID selector (6.7).
    Id(Box<str>),
    /// A class selector (6.6).
    Class(Box<str>),
    /// An attribute selector (section 6).
    Attribute(Box<Attribute>),
    /// A pseudo-class.
    PseudoClass(PseudoClass),
    /// The nesting selector `&`.
    Nesting,
}

/// An attribute selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    /// The namespace the attribute must be in.
    pub ns: Namespace,
    /// The prefix to serialize before `|`, if any.
    pub prefix: Option<Box<str>>,
    /// The attribute's local name, as written.
    pub local: Box<str>,
    /// The test on the value, if any.
    pub test: Option<(AttrMatcher, Box<str>)>,
    /// The `i` or `s` modifier, if written.
    pub case: Option<AttrCase>,
}

/// How an attribute selector compares the value (`<attr-matcher>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttrMatcher {
    /// `=`
    Equals,
    /// `~=`: one of the whitespace-separated words.
    Includes,
    /// `|=`: equal, or starts with the value then `-`.
    DashMatch,
    /// `^=`
    Prefix,
    /// `$=`
    Suffix,
    /// `*=`
    Substring,
}

/// The attribute selector modifiers (6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttrCase {
    /// `i`: ASCII case-insensitive.
    Insensitive,
    /// `s`: identical, whatever the document language says.
    Sensitive,
}

/// A pseudo-element selector followed by its pseudo-classes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PseudoCompound {
    /// The pseudo-element.
    pub element: PseudoElement,
    /// The pseudo-classes after it, in source order.
    pub classes: Vec<PseudoClass>,
}

/// A pseudo-class selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PseudoClass {
    /// One that takes no argument.
    Keyword(PseudoClassKeyword),
    /// `:is()` (4.2), with its forgiving argument.
    Is(Forgiving),
    /// `:where()` (4.4), with its forgiving argument.
    Where(Forgiving),
    /// `:not()` (4.3).
    Not(SelectorList),
    /// `:has()` (4.5).
    Has(Vec<RelativeSelector>),
    /// `:nth-child(An+B [of S]?)` (13.3.1).
    NthChild(AnB, Option<SelectorList>),
    /// `:nth-last-child(An+B [of S]?)` (13.3.2).
    NthLastChild(AnB, Option<SelectorList>),
    /// `:nth-of-type(An+B)` (13.4.1).
    NthOfType(AnB),
    /// `:nth-last-of-type(An+B)` (13.4.2).
    NthLastOfType(AnB),
    /// `:lang()` (7.2): the language ranges, as written.
    Lang(Vec<Box<str>>),
    /// `:dir()` (7.1): the identifier, as written.
    Dir(Box<str>),
    /// `:state()` (HTML, custom element states): the state's name.
    State(Box<str>),
    /// `:host()` (css-shadow-1): its compound selector argument.
    Host(Vec<Simple>),
    /// `:host-context()` (css-shadow-1): its compound selector argument.
    HostContext(Vec<Simple>),
}

/// The argument of `:is()` or `:where()` after "parse as a forgiving
/// selector list" (section 16.1).
///
/// The spec removes the items that fail to parse or are invalid. They are
/// kept here, as source text, only so that serialization gives them back as
/// written (CSSWG resolution in csswg-drafts issue 8356, which the WPT
/// parsing tests check); they never match and have zero specificity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forgiving(pub Vec<ForgivingItem>);

/// One comma-separated item of a forgiving selector list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgivingItem {
    /// A valid selector.
    Valid(Selector),
    /// An item that was removed, as its source text without the whitespace
    /// around it.
    Invalid(Box<str>),
}

/// A pseudo-element selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PseudoElement {
    /// One that takes no argument.
    Keyword(PseudoElementKeyword),
    /// `::highlight()` (css-pseudo-4 3.1): a custom highlight name, or
    /// `None` for `*`.
    Highlight(Option<Box<str>>),
    /// `::part()` (css-shadow-1): the part names.
    Part(Vec<Box<str>>),
    /// `::slotted()` (css-shadow-1): its compound selector argument.
    Slotted(Vec<Simple>),
    /// An unknown `-webkit-` pseudo-element (selectors-4 Appendix B), by its
    /// ASCII-lowercased name. Valid, matches nothing.
    UnknownWebkit(Box<str>),
}

macro_rules! keywords {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal,)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)*
        }

        impl $name {
            /// Every keyword, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant,)*];

            /// The keyword's name, in its canonical (lowercase) form.
            pub fn name(self) -> &'static str {
                match self {
                    $($name::$variant => $text,)*
                }
            }

            /// The keyword with this name, matched ASCII case-insensitively.
            pub fn from_name(name: &str) -> Option<$name> {
                Self::ALL.iter().copied().find(|k| k.name().eq_ignore_ascii_case(name))
            }
        }
    };
}

keywords! {
    /// The pseudo-classes that take no argument: those of selectors-4 and
    /// css-shadow-1 at the pinned csswg-drafts commit.
    PseudoClassKeyword {
        /// `:active` (9.2)
        Active = "active",
        /// `:any-link` (8.1)
        AnyLink = "any-link",
        /// `:autofill` (12.1.4); `:-webkit-autofill` is its legacy alias.
        Autofill = "autofill",
        /// `:buffering` (10.2)
        Buffering = "buffering",
        /// `:checked` (12.2.1)
        Checked = "checked",
        /// `:default` (12.1.5)
        Default = "default",
        /// `:defined` (5.4)
        Defined = "defined",
        /// `:disabled` (12.1.1)
        Disabled = "disabled",
        /// `:empty` (13.2)
        Empty = "empty",
        /// `:enabled` (12.1.1)
        Enabled = "enabled",
        /// `:first-child` (13.3.3)
        FirstChild = "first-child",
        /// `:first-of-type` (13.4.3)
        FirstOfType = "first-of-type",
        /// `:focus` (9.3)
        Focus = "focus",
        /// `:focus-visible` (9.4)
        FocusVisible = "focus-visible",
        /// `:focus-within` (9.5)
        FocusWithin = "focus-within",
        /// `:fullscreen` (11.4)
        Fullscreen = "fullscreen",
        /// `:has-slotted` (css-shadow-1)
        HasSlotted = "has-slotted",
        /// `:host` (css-shadow-1), the form without an argument.
        Host = "host",
        /// `:hover` (9.1)
        Hover = "hover",
        /// `:in-range` (12.3.2)
        InRange = "in-range",
        /// `:indeterminate` (12.2.1)
        Indeterminate = "indeterminate",
        /// `:invalid` (12.3.1)
        Invalid = "invalid",
        /// `:last-child` (13.3.4)
        LastChild = "last-child",
        /// `:last-of-type` (13.4.4)
        LastOfType = "last-of-type",
        /// `:link` (8.2)
        Link = "link",
        /// `:modal` (11.3)
        Modal = "modal",
        /// `:muted` (10.3)
        Muted = "muted",
        /// `:only-child` (13.3.5)
        OnlyChild = "only-child",
        /// `:only-of-type` (13.4.5)
        OnlyOfType = "only-of-type",
        /// `:open` (11.1)
        Open = "open",
        /// `:optional` (12.3.3)
        Optional = "optional",
        /// `:out-of-range` (12.3.2)
        OutOfRange = "out-of-range",
        /// `:paused` (10.1)
        Paused = "paused",
        /// `:picture-in-picture` (11.5)
        PictureInPicture = "picture-in-picture",
        /// `:placeholder-shown` (12.1.3)
        PlaceholderShown = "placeholder-shown",
        /// `:playing` (10.1)
        Playing = "playing",
        /// `:popover-open` (11.2)
        PopoverOpen = "popover-open",
        /// `:read-only` (12.1.2)
        ReadOnly = "read-only",
        /// `:read-write` (12.1.2)
        ReadWrite = "read-write",
        /// `:required` (12.3.3)
        Required = "required",
        /// `:root` (13.1)
        Root = "root",
        /// `:scope` (8.4)
        Scope = "scope",
        /// `:seeking` (10.2)
        Seeking = "seeking",
        /// `:stalled` (10.2)
        Stalled = "stalled",
        /// `:target` (8.3)
        Target = "target",
        /// `:unchecked` (12.2.1)
        Unchecked = "unchecked",
        /// `:user-invalid` (12.3.4)
        UserInvalid = "user-invalid",
        /// `:user-valid` (12.3.4)
        UserValid = "user-valid",
        /// `:valid` (12.3.1)
        Valid = "valid",
        /// `:visited` (8.2)
        Visited = "visited",
        /// `:volume-locked` (10.3)
        VolumeLocked = "volume-locked",
    }
}

keywords! {
    /// The pseudo-elements that take no argument: those of css-pseudo-4 at
    /// the pinned csswg-drafts commit.
    PseudoElementKeyword {
        /// `::after` (css-pseudo-4 4.1); also `:after`.
        After = "after",
        /// `::before` (css-pseudo-4 4.1); also `:before`.
        Before = "before",
        /// `::details-content` (css-pseudo-4 5.2)
        DetailsContent = "details-content",
        /// `::file-selector-button` (css-pseudo-4 5.1)
        FileSelectorButton = "file-selector-button",
        /// `::first-letter` (css-pseudo-4 2.2); also `:first-letter`.
        FirstLetter = "first-letter",
        /// `::first-line` (css-pseudo-4 2.1); also `:first-line`.
        FirstLine = "first-line",
        /// `::grammar-error` (css-pseudo-4 3.1)
        GrammarError = "grammar-error",
        /// `::interest-button` (css-pseudo-4 4.4)
        InterestButton = "interest-button",
        /// `::marker` (css-pseudo-4 4.2)
        Marker = "marker",
        /// `::placeholder` (css-pseudo-4 4.3)
        Placeholder = "placeholder",
        /// `::prefix`, only after `::first-letter` (css-pseudo-4 2.2).
        Prefix = "prefix",
        /// `::search-text` (css-pseudo-4 3.1)
        SearchText = "search-text",
        /// `::selection` (css-pseudo-4 3.1)
        Selection = "selection",
        /// `::spelling-error` (css-pseudo-4 3.1)
        SpellingError = "spelling-error",
        /// `::suffix`, only after `::first-letter` (css-pseudo-4 2.2).
        Suffix = "suffix",
        /// `::target-text` (css-pseudo-4 3.1)
        TargetText = "target-text",
    }
}

impl PseudoClassKeyword {
    /// The user action pseudo-classes (section 9), which may follow any
    /// pseudo-element (3.6.3).
    pub fn is_user_action(self) -> bool {
        use PseudoClassKeyword::*;
        matches!(self, Hover | Active | Focus | FocusVisible | FocusWithin)
    }
}

impl PseudoElement {
    /// Element-backed pseudo-elements (css-pseudo-4 section 5): any
    /// pseudo-class or pseudo-element may follow them.
    pub fn is_element_backed(&self) -> bool {
        use PseudoElementKeyword::*;
        matches!(
            self,
            PseudoElement::Part(_) | PseudoElement::Keyword(FileSelectorButton | DetailsContent)
        )
    }

    /// Tree-abiding pseudo-elements (css-pseudo-4 section 4, and the
    /// element-backed ones, which are always tree-abiding).
    pub fn is_tree_abiding(&self) -> bool {
        use PseudoElementKeyword::*;
        self.is_element_backed()
            || matches!(
                self,
                PseudoElement::Keyword(Before | After | Marker | Placeholder | InterestButton)
            )
    }
}
