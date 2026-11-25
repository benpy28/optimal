//! Parse errors.
//!
//! The HTML standard names every tokenizer parse error with a stable code
//! (for example `eof-in-tag`). Parse errors never stop parsing; they are
//! reported for developer tools and conformance checking only.

use core::fmt;

macro_rules! parse_errors {
    ($($variant:ident => $code:literal,)*) => {
        /// A parse error defined by the HTML standard, by its code.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum ParseErrorCode {
            $(
                #[doc = concat!("`", $code, "`")]
                $variant,
            )*
        }

        impl ParseErrorCode {
            /// Every code, in the order the standard lists them.
            pub const ALL: &'static [ParseErrorCode] = &[$(ParseErrorCode::$variant,)*];

            /// The code as the standard spells it.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(ParseErrorCode::$variant => $code,)*
                }
            }
        }
    };
}

parse_errors! {
    AbruptClosingOfEmptyComment => "abrupt-closing-of-empty-comment",
    AbruptDoctypePublicIdentifier => "abrupt-doctype-public-identifier",
    AbruptDoctypeSystemIdentifier => "abrupt-doctype-system-identifier",
    AbsenceOfDigitsInNumericCharacterReference => "absence-of-digits-in-numeric-character-reference",
    CdataInHtmlContent => "cdata-in-html-content",
    CharacterReferenceOutsideUnicodeRange => "character-reference-outside-unicode-range",
    ControlCharacterInInputStream => "control-character-in-input-stream",
    ControlCharacterReference => "control-character-reference",
    DisallowedProcessingInstructionTarget => "disallowed-processing-instruction-target",
    DuplicateAttribute => "duplicate-attribute",
    EndTagWithAttributes => "end-tag-with-attributes",
    EndTagWithTrailingSolidus => "end-tag-with-trailing-solidus",
    EofBeforeTagName => "eof-before-tag-name",
    EofInCdata => "eof-in-cdata",
    EofInComment => "eof-in-comment",
    EofInDoctype => "eof-in-doctype",
    EofInProcessingInstruction => "eof-in-processing-instruction",
    EofInScriptHtmlCommentLikeText => "eof-in-script-html-comment-like-text",
    EofInTag => "eof-in-tag",
    IncorrectlyClosedComment => "incorrectly-closed-comment",
    IncorrectlyOpenedComment => "incorrectly-opened-comment",
    InvalidCharacterSequenceAfterDoctypeName => "invalid-character-sequence-after-doctype-name",
    InvalidFirstCharacterOfProcessingInstructionTarget => "invalid-first-character-of-processing-instruction-target",
    InvalidFirstCharacterOfTagName => "invalid-first-character-of-tag-name",
    InvalidProcessingInstructionTarget => "invalid-processing-instruction-target",
    MissingAttributeValue => "missing-attribute-value",
    MissingDoctypeName => "missing-doctype-name",
    MissingDoctypePublicIdentifier => "missing-doctype-public-identifier",
    MissingDoctypeSystemIdentifier => "missing-doctype-system-identifier",
    MissingEndTagName => "missing-end-tag-name",
    MissingQuoteBeforeDoctypePublicIdentifier => "missing-quote-before-doctype-public-identifier",
    MissingQuoteBeforeDoctypeSystemIdentifier => "missing-quote-before-doctype-system-identifier",
    MissingSemicolonAfterCharacterReference => "missing-semicolon-after-character-reference",
    MissingWhitespaceAfterDoctypePublicKeyword => "missing-whitespace-after-doctype-public-keyword",
    MissingWhitespaceAfterDoctypeSystemKeyword => "missing-whitespace-after-doctype-system-keyword",
    MissingWhitespaceBeforeDoctypeName => "missing-whitespace-before-doctype-name",
    MissingWhitespaceBetweenAttributes => "missing-whitespace-between-attributes",
    MissingWhitespaceBetweenDoctypePublicAndSystemIdentifiers => "missing-whitespace-between-doctype-public-and-system-identifiers",
    NestedComment => "nested-comment",
    NoncharacterCharacterReference => "noncharacter-character-reference",
    NoncharacterInInputStream => "noncharacter-in-input-stream",
    NonVoidHtmlElementStartTagWithTrailingSolidus => "non-void-html-element-start-tag-with-trailing-solidus",
    NullCharacterReference => "null-character-reference",
    SurrogateCharacterReference => "surrogate-character-reference",
    SurrogateInInputStream => "surrogate-in-input-stream",
    UnexpectedCharacterAfterDoctypeSystemIdentifier => "unexpected-character-after-doctype-system-identifier",
    UnexpectedCharacterInAttributeName => "unexpected-character-in-attribute-name",
    UnexpectedCharacterInUnquotedAttributeValue => "unexpected-character-in-unquoted-attribute-value",
    UnexpectedEqualsSignBeforeAttributeName => "unexpected-equals-sign-before-attribute-name",
    UnexpectedNullCharacter => "unexpected-null-character",
    UnexpectedSolidusInTag => "unexpected-solidus-in-tag",
    UnknownNamedCharacterReference => "unknown-named-character-reference",
}

impl fmt::Display for ParseErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where in the input a parse error was found.
///
/// Lines and columns count from 1. Columns count UTF-16 code units, the unit
/// JavaScript and developer tools use, so a position here matches what a web
/// developer sees. Line breaks are counted after newline normalization, so
/// CR LF and lone CR each count as one line break.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Position {
    /// 1-based line number.
    pub line: u64,
    /// 1-based column, in UTF-16 code units.
    pub column: u64,
}

/// A parse error and where it was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParseError {
    /// Which error.
    pub code: ParseErrorCode,
    /// Where it was found.
    pub position: Position,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.position.line, self.position.column, self.code
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_uses_spec_codes_and_positions() {
        let e = ParseError {
            code: ParseErrorCode::EofInTag,
            position: Position {
                line: 3,
                column: 14,
            },
        };
        assert_eq!(e.to_string(), "3:14: eof-in-tag");
        assert_eq!(ParseErrorCode::NestedComment.to_string(), "nested-comment");
    }

    #[test]
    fn codes_are_unique_and_lowercase() {
        let mut seen = std::collections::HashSet::new();
        for c in ParseErrorCode::ALL {
            assert!(seen.insert(c.as_str()), "{c}");
            assert!(
                c.as_str()
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'-')
            );
        }
        assert_eq!(seen.len(), 52);
    }
}
