//! A reader for the JavaScript in the WPT test files: enough of the lexical
//! grammar for string literals, names, numbers and punctuation.

/// A token.
#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// A string literal, escapes resolved.
    Str(String),
    /// A name or number.
    Word(String),
    /// Any other character.
    Punct(char),
}

/// Splits JavaScript source into tokens, dropping comments.
pub fn lex(src: &str) -> Vec<Tok> {
    let cs: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && cs.get(i + 1) == Some(&'/') {
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && cs.get(i + 1) == Some(&'*') {
            i += 2;
            while !(cs[i] == '*' && cs[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '\'' || c == '"' || c == '`' {
            let mut s = String::new();
            i += 1;
            while cs[i] != c {
                if cs[i] == '\\' {
                    i += 1;
                    match cs[i] {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        'f' => s.push('\u{c}'),
                        '0' => s.push('\0'),
                        'u' => {
                            let hex: String = cs[i + 1..i + 5].iter().collect();
                            s.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
                            i += 4;
                        }
                        '\n' => {}
                        other => s.push(other),
                    }
                } else {
                    assert!(
                        !(c == '`' && cs[i] == '$'),
                        "template substitutions are not read"
                    );
                    s.push(cs[i]);
                }
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(s));
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            let start = i;
            while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_' || cs[i] == '$') {
                i += 1;
            }
            out.push(Tok::Word(cs[start..i].iter().collect()));
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    out
}

