//! A small, dependency-free regular-expression matcher for `search_history`'s
//! `regex` mode (DESIGN.md §13.7).
//!
//! The History MCP must not pull a regex crate (the substrate stays dependency-
//! light, mirroring the P6 hand-rolled HTTP decision), so this is a compact,
//! well-tested backtracking matcher over a documented subset:
//!
//! - literals and `.` (any character),
//! - quantifiers `*` `+` `?` on the preceding element,
//! - anchors `^` (start) and `$` (end),
//! - character classes `[abc]`, ranges `[a-z]`, negation `[^abc]`,
//! - `\` escapes the next metacharacter.
//!
//! A pattern with no `^` is unanchored (matches anywhere). The full PCRE feature
//! set (groups, alternation, backreferences) is an additive richer matcher behind
//! the same `regex_is_match` entry point (CLAUDE.md C3).

/// What a single token matches.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Atom {
    /// Any single character (`.`).
    Any,
    /// A specific literal character.
    Literal(char),
    /// A character class: `negated` flips the membership test.
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

/// A quantifier on a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quant {
    /// Exactly one.
    One,
    /// Zero or more (`*`).
    Star,
    /// One or more (`+`).
    Plus,
    /// Zero or one (`?`).
    Opt,
}

#[derive(Debug, Clone)]
struct Token {
    atom: Atom,
    quant: Quant,
}

/// Whether `text` matches `pattern` under the supported regex subset.
///
/// Unanchored unless the pattern begins with `^`. Returns `false` on a malformed
/// pattern (e.g. an unterminated class) rather than panicking — a search should
/// never crash the server.
#[must_use]
pub fn regex_is_match(pattern: &str, text: &str) -> bool {
    let Some((tokens, anchored_start, anchored_end)) = compile(pattern) else {
        return false;
    };
    let chars: Vec<char> = text.chars().collect();
    // Bound total backtracking work so an adversarial pattern (e.g. many adjacent
    // quantifiers — `a*a*a*…b`) cannot hang the daemon's request thread (ReDoS).
    // The cap is generous for real searches but finite; exhausting it returns
    // "no match" rather than looping — the search never crashes the server.
    let mut budget: u64 = regex_step_budget(tokens.len(), chars.len());
    if anchored_start {
        return match_here(&tokens, &chars, 0, anchored_end, &mut budget).is_some();
    }
    // Unanchored: try every start position (including the empty-tail position so
    // patterns like `a*` match the empty string). The budget is shared across all
    // start positions so the total work stays bounded.
    for start in 0..=chars.len() {
        if match_here(&tokens, &chars, start, anchored_end, &mut budget).is_some() {
            return true;
        }
        if budget == 0 {
            break;
        }
    }
    false
}

/// The total backtracking-step budget for one match, proportional to the problem
/// size with a floor so small patterns always have ample room. Caps adversarial
/// exponential blowup at a polynomial ceiling.
fn regex_step_budget(tokens: usize, text_len: usize) -> u64 {
    const FLOOR: u64 = 1_000_000;
    let product = (tokens as u64)
        .saturating_mul(text_len as u64)
        .saturating_mul(64);
    product.max(FLOOR)
}

/// Compile a pattern into tokens plus the two anchor flags. Returns `None` on a
/// malformed pattern.
fn compile(pattern: &str) -> Option<(Vec<Token>, bool, bool)> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    let anchored_start = chars.first() == Some(&'^');
    if anchored_start {
        i = 1;
    }
    let mut anchored_end = false;
    let mut tokens: Vec<Token> = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '$' && i == chars.len() - 1 {
            anchored_end = true;
            break;
        }
        let atom = match c {
            '\\' => {
                i += 1;
                let lit = *chars.get(i)?;
                Atom::Literal(lit)
            }
            '.' => Atom::Any,
            '[' => {
                let (class, consumed) = compile_class(&chars[i..])?;
                i += consumed - 1; // -1 because the loop adds 1 below
                class
            }
            // A bare quantifier with nothing to bind to is malformed.
            '*' | '+' | '?' => return None,
            other => Atom::Literal(other),
        };
        i += 1;
        // A trailing quantifier on this atom.
        let quant = match chars.get(i) {
            Some('*') => {
                i += 1;
                Quant::Star
            }
            Some('+') => {
                i += 1;
                Quant::Plus
            }
            Some('?') => {
                i += 1;
                Quant::Opt
            }
            _ => Quant::One,
        };
        tokens.push(Token { atom, quant });
    }
    Some((tokens, anchored_start, anchored_end))
}

/// Compile a `[...]` class, returning the atom and how many chars were consumed
/// (including the closing `]`).
fn compile_class(chars: &[char]) -> Option<(Atom, usize)> {
    debug_assert_eq!(chars.first(), Some(&'['));
    let mut i = 1;
    let negated = chars.get(i) == Some(&'^');
    if negated {
        i += 1;
    }
    let mut ranges: Vec<(char, char)> = Vec::new();
    while i < chars.len() && chars[i] != ']' {
        let lo = chars[i];
        // A range `a-z` (but not a trailing `-`).
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|c| *c != ']') {
            let hi = chars[i + 2];
            // A reversed range (`[z-a]`) is malformed — reject the whole pattern
            // rather than silently matching the normalized `a..z`.
            if lo > hi {
                return None;
            }
            ranges.push((lo, hi));
            i += 3;
        } else {
            ranges.push((lo, lo));
            i += 1;
        }
    }
    if chars.get(i) != Some(&']') {
        return None; // unterminated class
    }
    Some((Atom::Class { negated, ranges }, i + 1))
}

/// Whether `atom` matches a single character.
fn atom_matches(atom: &Atom, c: char) -> bool {
    match atom {
        Atom::Any => true,
        Atom::Literal(l) => *l == c,
        Atom::Class { negated, ranges } => {
            let inside = ranges.iter().any(|(lo, hi)| c >= *lo && c <= *hi);
            inside != *negated
        }
    }
}

/// Try to match `tokens` against `chars[pos..]`. On success returns the position
/// after the match (so `$` anchoring can require it to be the end).
///
/// `budget` bounds total backtracking steps across the whole match; when it hits
/// zero the matcher gives up and returns `None` (no match) rather than spinning —
/// the ReDoS guard.
fn match_here(
    tokens: &[Token],
    chars: &[char],
    pos: usize,
    anchored_end: bool,
    budget: &mut u64,
) -> Option<usize> {
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    let Some((tok, rest)) = tokens.split_first() else {
        // All tokens consumed.
        if anchored_end && pos != chars.len() {
            return None;
        }
        return Some(pos);
    };
    match tok.quant {
        Quant::One => {
            if pos < chars.len() && atom_matches(&tok.atom, chars[pos]) {
                match_here(rest, chars, pos + 1, anchored_end, budget)
            } else {
                None
            }
        }
        Quant::Opt => {
            // Try consuming one, then try skipping.
            if pos < chars.len() && atom_matches(&tok.atom, chars[pos]) {
                if let Some(end) = match_here(rest, chars, pos + 1, anchored_end, budget) {
                    return Some(end);
                }
            }
            match_here(rest, chars, pos, anchored_end, budget)
        }
        Quant::Star | Quant::Plus => {
            // Count how many we can consume greedily, then backtrack.
            let mut max = pos;
            while max < chars.len() && atom_matches(&tok.atom, chars[max]) {
                max += 1;
            }
            let min = if tok.quant == Quant::Plus {
                pos + 1
            } else {
                pos
            };
            if tok.quant == Quant::Plus && max < min {
                return None;
            }
            // Backtrack from the greediest match down to the minimum.
            let mut take = max;
            loop {
                if take >= min {
                    if let Some(end) = match_here(rest, chars, take, anchored_end, budget) {
                        return Some(end);
                    }
                }
                if take == min || *budget == 0 {
                    break;
                }
                take -= 1;
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_substring_unanchored() {
        assert!(regex_is_match("retry", "added a retry loop"));
        assert!(!regex_is_match("retry", "no match here"));
    }

    #[test]
    fn dot_matches_any() {
        assert!(regex_is_match("r.try", "retry"));
        assert!(regex_is_match("r.try", "rXtry"));
        assert!(!regex_is_match("r.try", "rtry"));
    }

    #[test]
    fn star_is_greedy_and_backtracks() {
        assert!(regex_is_match("a*b", "aaab"));
        assert!(regex_is_match("a*b", "b"));
        assert!(regex_is_match("fn .*retry", "fn do_retry"));
        assert!(regex_is_match(".*", ""));
    }

    #[test]
    fn plus_requires_one() {
        assert!(regex_is_match("a+b", "aaab"));
        assert!(!regex_is_match("^a+b$", "b"));
    }

    #[test]
    fn opt_is_zero_or_one() {
        assert!(regex_is_match("^colou?r$", "color"));
        assert!(regex_is_match("^colou?r$", "colour"));
        assert!(!regex_is_match("^colou?r$", "colouur"));
    }

    #[test]
    fn anchors() {
        assert!(regex_is_match("^fn ", "fn main"));
        assert!(!regex_is_match("^fn ", "  fn main"));
        assert!(regex_is_match("loop$", "the main loop"));
        assert!(!regex_is_match("loop$", "loop body"));
    }

    #[test]
    fn char_classes() {
        assert!(regex_is_match("[0-9]+", "abc123"));
        assert!(regex_is_match("^[A-Za-z_][A-Za-z0-9_]*$", "valid_ident1"));
        assert!(!regex_is_match("^[A-Za-z_][A-Za-z0-9_]*$", "1bad"));
        assert!(regex_is_match("[^0-9]", "a"));
        assert!(!regex_is_match("^[^0-9]$", "5"));
    }

    #[test]
    fn escapes_metacharacters() {
        assert!(regex_is_match(r"a\.b", "a.b"));
        assert!(!regex_is_match(r"^a\.b$", "axb"));
        assert!(regex_is_match(r"\*", "a*b"));
    }

    #[test]
    fn malformed_pattern_does_not_panic() {
        assert!(!regex_is_match("[unterminated", "anything"));
        assert!(!regex_is_match("*nostart", "anything"));
    }

    #[test]
    fn reversed_class_range_is_rejected() {
        // `[z-a]` is malformed (lo > hi); it must not silently match `a..z`.
        assert!(!regex_is_match("^[z-a]$", "m"));
    }

    #[test]
    fn adversarial_pattern_is_bounded_not_exponential() {
        // A classic catastrophic-backtracking pattern over a non-matching string.
        // With the step budget this returns (false) promptly instead of hanging.
        let pattern = "a*".repeat(20) + "b";
        let text = "a".repeat(40);
        // No panic, no hang: the budget caps the work and the call returns.
        assert!(!regex_is_match(&pattern, &text));
    }
}
