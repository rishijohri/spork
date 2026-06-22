//! The [`SymbolExtractor`] seam and its dependency-free v1
//! [`KeywordSymbolExtractor`].
//!
//! DESIGN.md §13.6 specifies a **tree-sitter** repo map. The load-bearing
//! invariant, though, is *a symbol-reference graph ranked by importance and
//! bounded to a token budget* — not the specific parser. So this crate freezes a
//! [`SymbolExtractor`] seam and ships a complete, dependency-free v1 keyword
//! extractor behind it; full tree-sitter grammars are an **additive** impl behind
//! the same seam (CLAUDE.md C3), exactly the narrow-but-complete pattern used for
//! the P6 plaintext-vs-TLS transport. The v1 covers the brace-and-keyword
//! languages (Rust/TS/JS/Go/C-like) by recognizing definition keywords and
//! collecting identifier references, stripping comments and string literals so it
//! does not pick names out of prose.

use serde::{Deserialize, Serialize};

/// The kind of a defined symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// A function or method.
    Function,
    /// A struct / record.
    Struct,
    /// An enum.
    Enum,
    /// A trait / interface.
    Trait,
    /// A class.
    Class,
    /// A module / namespace.
    Module,
    /// A type alias.
    TypeAlias,
    /// A constant or static.
    Const,
    /// Anything else a future extractor recognizes.
    Other,
}

impl SymbolKind {
    /// A short, stable label for rendering.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            SymbolKind::Function => "fn",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Class => "class",
            SymbolKind::Module => "mod",
            SymbolKind::TypeAlias => "type",
            SymbolKind::Const => "const",
            SymbolKind::Other => "sym",
        }
    }
}

/// A defined symbol: its name, the file that defines it, and its kind.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Symbol {
    /// The symbol's name.
    pub name: String,
    /// The repo-relative path of the file defining it.
    pub file: String,
    /// What kind of symbol it is.
    pub kind: SymbolKind,
}

impl Symbol {
    /// Construct a symbol.
    #[must_use]
    pub fn new(name: impl Into<String>, file: impl Into<String>, kind: SymbolKind) -> Self {
        Symbol {
            name: name.into(),
            file: file.into(),
            kind,
        }
    }
}

/// The extraction seam: pull a file's symbol definitions and the identifiers it
/// references (DESIGN.md §13.6).
///
/// F4-style discipline: this trait is the seam and the crate ships exactly one
/// real implementation ([`KeywordSymbolExtractor`]); a tree-sitter extractor is
/// an additive impl behind it (CLAUDE.md C3).
pub trait SymbolExtractor {
    /// Extract the symbols **defined** in `source` (from file `path`).
    fn definitions(&self, path: &str, source: &str) -> Vec<Symbol>;

    /// Extract the identifier names **referenced** in `source`, in order, with
    /// duplicates (mention counts matter for ranking).
    fn references(&self, path: &str, source: &str) -> Vec<String>;
}

/// Keywords that introduce a named definition (the next identifier is the name).
const DEF_KEYWORDS: &[(&str, SymbolKind)] = &[
    ("fn", SymbolKind::Function),
    ("function", SymbolKind::Function),
    ("struct", SymbolKind::Struct),
    ("enum", SymbolKind::Enum),
    ("trait", SymbolKind::Trait),
    ("interface", SymbolKind::Trait),
    ("class", SymbolKind::Class),
    ("mod", SymbolKind::Module),
    ("module", SymbolKind::Module),
    ("namespace", SymbolKind::Module),
    ("type", SymbolKind::TypeAlias),
    ("const", SymbolKind::Const),
    ("static", SymbolKind::Const),
];

/// Common keywords that are never symbol references worth ranking.
const STOP_WORDS: &[&str] = &[
    "let", "mut", "pub", "use", "if", "else", "for", "while", "loop", "match", "return", "self",
    "Self", "super", "crate", "as", "in", "where", "async", "await", "move", "ref", "dyn", "impl",
    "the", "and", "or", "not", "true", "false", "null", "None", "Some", "Ok", "Err", "this", "new",
    "var", "val", "void", "int", "string", "bool", "true", "false",
];

/// The dependency-free v1 keyword extractor (DESIGN.md §13.6).
#[derive(Debug, Clone, Copy, Default)]
pub struct KeywordSymbolExtractor;

impl KeywordSymbolExtractor {
    /// Construct the v1 extractor.
    #[must_use]
    pub fn new() -> Self {
        KeywordSymbolExtractor
    }
}

impl SymbolExtractor for KeywordSymbolExtractor {
    fn definitions(&self, path: &str, source: &str) -> Vec<Symbol> {
        let toks = tokenize(source);
        let mut defs = Vec::new();
        let mut i = 0;
        while i + 1 < toks.len() {
            if let Some((_, kind)) = DEF_KEYWORDS.iter().find(|(kw, _)| *kw == toks[i]) {
                let name = &toks[i + 1];
                if is_identifier(name) && !is_keyword(name) {
                    defs.push(Symbol::new(name.clone(), path, *kind));
                }
            }
            i += 1;
        }
        defs
    }

    fn references(&self, _path: &str, source: &str) -> Vec<String> {
        tokenize(source)
            .into_iter()
            .filter(|t| is_identifier(t) && !is_keyword(t) && !STOP_WORDS.contains(&t.as_str()))
            .collect()
    }
}

/// Whether a token is a definition keyword (so its following identifier is a
/// definition name, not a reference).
fn is_keyword(token: &str) -> bool {
    DEF_KEYWORDS.iter().any(|(kw, _)| *kw == token)
}

/// Whether a token is a plain identifier (starts with a letter/underscore).
fn is_identifier(token: &str) -> bool {
    let mut chars = token.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// Tokenize source into identifier tokens, after stripping line comments, block
/// comments, and double-quoted string literals (so prose/strings never leak
/// symbol names). Identifiers shorter than two characters are dropped.
fn tokenize(source: &str) -> Vec<String> {
    let cleaned = strip_noise(source);
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in cleaned.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            cur.push(c);
        } else {
            push_token(&mut out, &mut cur);
        }
    }
    push_token(&mut out, &mut cur);
    out
}

fn push_token(out: &mut Vec<String>, cur: &mut String) {
    if cur.len() >= 2 {
        out.push(std::mem::take(cur));
    } else {
        cur.clear();
    }
}

/// Replace comments and string literals with spaces, preserving byte structure
/// loosely (we only need token boundaries).
fn strip_noise(source: &str) -> String {
    let bytes: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let next = bytes.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            // Line comment to end of line.
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            // Block comment.
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == '*' && bytes[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '"' || c == '\'' {
            // String / char literal (skip escaped quotes).
            let quote = c;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            out.push(' ');
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_definitions() {
        let src = r#"
            pub struct Widget { x: u32 }
            pub fn build_widget() -> Widget { Widget { x: 0 } }
            enum Color { Red, Green }
            trait Render { fn render(&self); }
            const MAX: u32 = 10;
            type Id = u64;
        "#;
        let defs = KeywordSymbolExtractor::new().definitions("src/lib.rs", src);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"Widget"));
        assert!(names.contains(&"build_widget"));
        assert!(names.contains(&"Color"));
        assert!(names.contains(&"Render"));
        assert!(names.contains(&"render"));
        assert!(names.contains(&"MAX"));
        assert!(names.contains(&"Id"));
    }

    #[test]
    fn definitions_carry_kind_and_file() {
        let defs = KeywordSymbolExtractor::new().definitions("a.rs", "fn foo() {}");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].kind, SymbolKind::Function);
        assert_eq!(defs[0].file, "a.rs");
    }

    #[test]
    fn comments_and_strings_do_not_leak_symbols() {
        let src = r#"
            // fn commented_out
            let s = "fn not_a_def struct also_not";
            /* block fn blocked */
            fn real() {}
        "#;
        let defs = KeywordSymbolExtractor::new().definitions("a.rs", src);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn references_collect_identifiers_excluding_stopwords() {
        let refs = KeywordSymbolExtractor::new().references("a.rs", "let y = compute(widget);");
        assert!(refs.contains(&"compute".to_string()));
        assert!(refs.contains(&"widget".to_string()));
        assert!(!refs.contains(&"let".to_string()));
    }

    #[test]
    fn short_tokens_are_dropped() {
        let refs = KeywordSymbolExtractor::new().references("a.rs", "a bb ccc");
        assert!(!refs.contains(&"a".to_string()));
        assert!(refs.contains(&"bb".to_string()));
        assert!(refs.contains(&"ccc".to_string()));
    }
}
