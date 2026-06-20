//! A small, dependency-free JUnit-XML parser (DESIGN.md §8.1).
//!
//! The Validation/Test runner's load-bearing job is to normalize a test tool's
//! JUnit-XML into the per-unit results of the one
//! [`ResultEnvelope`](spork_runner::ResultEnvelope) (DESIGN.md §8.1, the
//! `TestRunner` row: "jest / pytest / go-test (parses junit-xml)"). This module
//! maps the universal `<testsuite>/<testcase>` shape into
//! [`UnitResult`](spork_runner::UnitResult)s:
//!
//! - a `<testcase>` with a `<failure>` or `<error>` child →
//!   [`UnitStatus::Failed`](spork_runner::UnitStatus::Failed) (carrying the
//!   `message` attribute as the unit's detail);
//! - a `<testcase>` with a `<skipped>` child →
//!   [`UnitStatus::Skipped`](spork_runner::UnitStatus::Skipped);
//! - a bare `<testcase>` →
//!   [`UnitStatus::Passed`](spork_runner::UnitStatus::Passed).
//!
//! The unit's stable name is `classname.name` when a `classname` attribute is
//! present (the fully-qualified test name jest/pytest emit), else the bare
//! `name`. Nested `<testsuites>`/`<testsuite>` wrappers are handled by scanning
//! every `<testcase>` in document order, which is deterministic — the property a
//! cached, diffable result relies on (DESIGN.md §8.1 cache soundness).
//!
//! It is a *deliberately small* tokenizer, not a general XML library: it
//! understands the JUnit subset (elements, attributes, self-closing tags,
//! comments, the XML declaration, and CDATA) without pulling a dependency, and it
//! rejects malformed input loudly rather than silently dropping cases.

use spork_runner::{UnitResult, UnitStatus};

/// Parse a JUnit-XML document into ordered per-unit results.
///
/// Empty input (no `<testcase>`) yields an empty vector — a legitimate "suite ran
/// but reported nothing" — not an error. A structurally broken document (an
/// unterminated tag) is an `Err(String)` so a misparsed report fails loudly
/// instead of producing a falsely-green empty result.
///
/// # Errors
/// Returns the parse-failure description if the document is not well-formed
/// enough to scan its `<testcase>` elements.
pub fn parse_junit(xml: &str) -> Result<Vec<UnitResult>, String> {
    let tokens = tokenize(xml)?;
    let mut units = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Open(tag) if tag.name == "testcase" => {
                let unit = parse_testcase(tag, &tokens, &mut i)?;
                units.push(unit);
            }
            Token::SelfClose(tag) if tag.name == "testcase" => {
                // A bare self-closing testcase: a pass with no children.
                units.push(UnitResult::new(unit_name(tag), UnitStatus::Passed));
                i += 1;
            }
            _ => i += 1,
        }
    }
    Ok(units)
}

/// Parse a `<testcase>...</testcase>` element starting at `*i` (an `Open` token),
/// advancing `*i` past its matching close tag.
fn parse_testcase(open: &Tag, tokens: &[Token], i: &mut usize) -> Result<UnitResult, String> {
    let name = unit_name(open);
    let mut status = UnitStatus::Passed;
    let mut detail: Option<String> = None;
    *i += 1; // consume the opening <testcase>
    loop {
        let Some(token) = tokens.get(*i) else {
            return Err(format!("unterminated <testcase name={name:?}>"));
        };
        match token {
            Token::Close(close) if close == "testcase" => {
                *i += 1;
                break;
            }
            Token::Open(child) | Token::SelfClose(child) => {
                match child.name.as_str() {
                    "failure" | "error" => {
                        status = UnitStatus::Failed;
                        if detail.is_none() {
                            detail = child.attr("message").map(str::to_string);
                        }
                    }
                    // A skip never overrides a recorded failure (the guard).
                    "skipped" if status != UnitStatus::Failed => {
                        status = UnitStatus::Skipped;
                        if detail.is_none() {
                            detail = child.attr("message").map(str::to_string);
                        }
                    }
                    _ => {}
                }
                // Skip a nested element's body if it is not self-closing.
                if matches!(token, Token::Open(_)) {
                    skip_element(&child.name, tokens, i)?;
                } else {
                    *i += 1;
                }
            }
            _ => *i += 1,
        }
    }
    let mut unit = UnitResult::new(name, status);
    if let Some(d) = detail.filter(|d| !d.is_empty()) {
        unit = unit.with_detail(d);
    }
    Ok(unit)
}

/// Advance `*i` past the matching close tag of an already-opened element named
/// `name` (handling nested same-named elements).
fn skip_element(name: &str, tokens: &[Token], i: &mut usize) -> Result<(), String> {
    *i += 1; // consume the opening tag
    let mut depth = 1usize;
    while depth > 0 {
        let Some(token) = tokens.get(*i) else {
            return Err(format!("unterminated <{name}>"));
        };
        match token {
            Token::Open(t) if t.name == name => depth += 1,
            Token::Close(t) if t == name => depth -= 1,
            _ => {}
        }
        *i += 1;
    }
    Ok(())
}

/// The fully-qualified unit name: `classname.name` when a `classname` attribute
/// is present, else the bare `name` (or `"<unnamed>"` if neither).
fn unit_name(tag: &Tag) -> String {
    let name = tag.attr("name").unwrap_or("<unnamed>");
    match tag.attr("classname") {
        Some(class) if !class.is_empty() => format!("{class}.{name}"),
        _ => name.to_string(),
    }
}

/// One parsed XML element tag (open or self-closing): its name and attributes.
#[derive(Debug, Clone)]
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// A token in the JUnit document: an element boundary (text and comments are
/// discarded — JUnit's structure lives entirely in elements and attributes).
#[derive(Debug, Clone)]
enum Token {
    /// `<name ...>`
    Open(Tag),
    /// `<name ... />`
    SelfClose(Tag),
    /// `</name>`
    Close(String),
}

/// Tokenize an XML document into element-boundary tokens, skipping the XML
/// declaration, comments, CDATA, and text content.
fn tokenize(xml: &str) -> Result<Vec<Token>, String> {
    let bytes: Vec<char> = xml.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != '<' {
            i += 1;
            continue;
        }
        // Skip comments <!-- ... -->
        if starts_with(&bytes, i, "<!--") {
            i = find_after(&bytes, i + 4, "-->").ok_or("unterminated comment")?;
            continue;
        }
        // Skip CDATA <![CDATA[ ... ]]>
        if starts_with(&bytes, i, "<![CDATA[") {
            i = find_after(&bytes, i + 9, "]]>").ok_or("unterminated CDATA")?;
            continue;
        }
        // Skip the XML declaration / processing instructions <? ... ?> and
        // doctype <! ... >.
        if starts_with(&bytes, i, "<?") {
            i = find_after(&bytes, i + 2, "?>").ok_or("unterminated processing instruction")?;
            continue;
        }
        if starts_with(&bytes, i, "<!") {
            i = find_after(&bytes, i + 2, ">").ok_or("unterminated declaration")?;
            continue;
        }
        // A real element tag: read up to the matching '>'.
        let end = find_char(&bytes, i + 1, '>').ok_or("unterminated tag")?;
        let inner: String = bytes[i + 1..end].iter().collect();
        let inner = inner.trim();
        if let Some(rest) = inner.strip_prefix('/') {
            // Close tag </name>
            tokens.push(Token::Close(rest.trim().to_string()));
        } else if let Some(rest) = inner.strip_suffix('/') {
            // Self-closing <name .../>
            tokens.push(Token::SelfClose(parse_tag(rest.trim())?));
        } else {
            tokens.push(Token::Open(parse_tag(inner)?));
        }
        i = end + 1;
    }
    Ok(tokens)
}

/// Parse a tag's inner text (`name attr="v" ...`) into a [`Tag`].
fn parse_tag(inner: &str) -> Result<Tag, String> {
    let mut chars = inner.char_indices().peekable();
    // The element name runs up to the first whitespace.
    let mut name = String::new();
    for (_, c) in chars.by_ref() {
        if c.is_whitespace() {
            break;
        }
        name.push(c);
    }
    if name.is_empty() {
        return Err("empty element name".to_string());
    }
    // Parse the remaining attributes.
    let rest: String = inner[name.len()..].to_string();
    let attrs = parse_attrs(&rest)?;
    Ok(Tag { name, attrs })
}

/// Parse a run of `key="value"` (or `key='value'`) attributes.
fn parse_attrs(s: &str) -> Result<Vec<(String, String)>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut attrs = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        // Skip whitespace.
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        // Read the key up to '='.
        let mut key = String::new();
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() {
            key.push(chars[i]);
            i += 1;
        }
        // Skip whitespace before '='.
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '=' {
            // A bare attribute with no value: skip it (JUnit does not use these).
            if !key.is_empty() {
                continue;
            }
            break;
        }
        i += 1; // consume '='
                // Skip whitespace after '='.
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            return Err(format!("attribute {key:?} has no value"));
        }
        let quote = chars[i];
        if quote != '"' && quote != '\'' {
            return Err(format!("attribute {key:?} value is not quoted"));
        }
        i += 1; // consume the opening quote
        let mut value = String::new();
        while i < chars.len() && chars[i] != quote {
            value.push(chars[i]);
            i += 1;
        }
        if i >= chars.len() {
            return Err(format!("unterminated attribute value for {key:?}"));
        }
        i += 1; // consume the closing quote
        attrs.push((key, unescape(&value)));
    }
    Ok(attrs)
}

/// Unescape the five predefined XML entities in an attribute value.
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Whether `bytes[i..]` starts with `needle`.
fn starts_with(bytes: &[char], i: usize, needle: &str) -> bool {
    let n: Vec<char> = needle.chars().collect();
    if i + n.len() > bytes.len() {
        return false;
    }
    bytes[i..i + n.len()] == n[..]
}

/// The index of the character just after the next occurrence of `needle` at or
/// after `from`.
fn find_after(bytes: &[char], from: usize, needle: &str) -> Option<usize> {
    let n: Vec<char> = needle.chars().collect();
    let mut i = from;
    while i + n.len() <= bytes.len() {
        if bytes[i..i + n.len()] == n[..] {
            return Some(i + n.len());
        }
        i += 1;
    }
    None
}

/// The index of the next `c` at or after `from`.
fn find_char(bytes: &[char], from: usize, c: char) -> Option<usize> {
    (from..bytes.len()).find(|&i| bytes[i] == c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_document_yields_no_units() {
        assert!(parse_junit("").unwrap().is_empty());
        assert!(parse_junit("<testsuite/>").unwrap().is_empty());
    }

    #[test]
    fn passing_failing_skipping_cases_map_correctly() {
        let xml = r#"<testsuite>
            <testcase name="a"/>
            <testcase name="b"><failure message="bad">trace</failure></testcase>
            <testcase name="c"><skipped/></testcase>
            <testcase name="d"><error message="boom"/></testcase>
        </testsuite>"#;
        let units = parse_junit(xml).unwrap();
        assert_eq!(units.len(), 4);
        assert_eq!(units[0].status, UnitStatus::Passed);
        assert_eq!(units[1].status, UnitStatus::Failed);
        assert_eq!(units[1].detail.as_deref(), Some("bad"));
        assert_eq!(units[2].status, UnitStatus::Skipped);
        assert_eq!(units[3].status, UnitStatus::Failed);
        assert_eq!(units[3].detail.as_deref(), Some("boom"));
    }

    #[test]
    fn classname_qualifies_the_unit_name() {
        let xml = r#"<testsuite><testcase classname="mod::suite" name="case"/></testsuite>"#;
        let units = parse_junit(xml).unwrap();
        assert_eq!(units[0].name, "mod::suite.case");
    }

    #[test]
    fn xml_declaration_and_comments_are_ignored() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <!-- a comment -->
            <testsuites>
              <testsuite name="outer">
                <testcase name="only"/>
              </testsuite>
            </testsuites>"#;
        let units = parse_junit(xml).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].name, "only");
    }

    #[test]
    fn nested_testsuites_are_flattened_in_order() {
        let xml = r#"<testsuites>
            <testsuite name="s1"><testcase name="one"/></testsuite>
            <testsuite name="s2"><testcase name="two"/><testcase name="three"/></testsuite>
        </testsuites>"#;
        let units = parse_junit(xml).unwrap();
        let names: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, vec!["one", "two", "three"]);
    }

    #[test]
    fn failure_then_skipped_keeps_failure() {
        // A testcase with both a failure and a (rare) skipped child stays failed.
        let xml = r#"<testsuite><testcase name="x"><failure message="f"/><skipped/></testcase></testsuite>"#;
        let units = parse_junit(xml).unwrap();
        assert_eq!(units[0].status, UnitStatus::Failed);
    }

    #[test]
    fn attribute_entities_are_unescaped() {
        let xml = r#"<testsuite><testcase name="a"><failure message="x &amp; y &lt;z&gt;"/></testcase></testsuite>"#;
        let units = parse_junit(xml).unwrap();
        assert_eq!(units[0].detail.as_deref(), Some("x & y <z>"));
    }

    #[test]
    fn unterminated_tag_is_an_error() {
        assert!(parse_junit("<testsuite><testcase name=\"a\"").is_err());
    }

    #[test]
    fn single_quoted_attributes_parse() {
        let xml = "<testsuite><testcase name='solo'/></testsuite>";
        let units = parse_junit(xml).unwrap();
        assert_eq!(units[0].name, "solo");
    }

    #[test]
    fn cdata_in_failure_body_is_skipped() {
        let xml = r#"<testsuite><testcase name="a"><failure message="m"><![CDATA[<not a tag>]]></failure></testcase></testsuite>"#;
        let units = parse_junit(xml).unwrap();
        assert_eq!(units[0].status, UnitStatus::Failed);
        assert_eq!(units[0].detail.as_deref(), Some("m"));
    }
}
