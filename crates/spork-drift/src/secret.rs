//! Secret scanning at capture (DESIGN.md §15.5).
//!
//! Because editor buffers and gitignored files (e.g. `.env`) can be snapshotted
//! into the shadow store, Spork runs **secret-scanning at capture** and applies
//! an explicit, overridable exclusion policy (DESIGN.md §15.5). This module owns
//! that scan: a [`SecretScanner`] holds a `regex::RegexSet` of common secret
//! shapes (AWS access keys, generic `api_key=`/`secret=`/`token=` assignments,
//! PEM private-key blocks, GitHub/Slack tokens, JWT-shaped strings, AWS secret
//! keys) and answers [`scan`](SecretScanner::scan): *do these bytes contain a
//! secret?*
//!
//! The capture flow ([`crate::DriftCapture`]) consults the scanner per file and
//! **excludes** any matched file so its bytes never enter the CAS — the §15.5
//! "no secret enters the store" invariant.

use regex::bytes::RegexSet;

use crate::error::{DriftError, Result};

/// The current schema version of the scanner's pattern set (CLAUDE.md C5).
///
/// Bumping this signals the default pattern vocabulary changed, so a stored
/// "scanned clean under policy vN" assertion can be re-evaluated.
pub const SECRET_POLICY_VERSION: u16 = 1;

/// The frozen v1 default secret patterns (DESIGN.md §15.5).
///
/// These match on the raw bytes of a file so a binary blob with an embedded key
/// is still caught. The list is intentionally conservative-but-broad: a false
/// positive merely excludes a file from a snapshot (the §10.2 honesty
/// requirement reports excluded paths), whereas a false negative leaks a secret
/// into the store, so the bias is toward catching.
const DEFAULT_PATTERNS: &[&str] = &[
    // AWS access key id: AKIA / ASIA / AGPA / AIDA / AROA + 16 base32 chars.
    r"(?i)\b(?:AKIA|ASIA|AGPA|AIDA|AROA)[0-9A-Z]{16}\b",
    // AWS secret access key assignment (40 base64-ish chars after the key name).
    r#"(?i)aws_secret_access_key\s*[:=]\s*['"]?[A-Za-z0-9/+=]{40}"#,
    // Generic credential assignments: api_key / apikey / secret / token /
    // password / access_token = "value" with a non-trivial value.
    r#"(?i)(?:api[_-]?key|secret|token|password|access[_-]?token|client[_-]?secret)\s*[:=]\s*['"]?[A-Za-z0-9_\-./+=]{12,}"#,
    // PEM private key block header (RSA / EC / OPENSSH / generic).
    r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----",
    // GitHub personal-access / app tokens.
    r"\bgh[pousr]_[A-Za-z0-9]{36,}\b",
    // Slack tokens.
    r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
    // Google API key.
    r"\bAIza[0-9A-Za-z_\-]{35}\b",
    // A JWT (three base64url segments separated by dots), header begins eyJ.
    r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\b",
    // Stripe live/test secret keys.
    r"\bsk_(?:live|test)_[A-Za-z0-9]{16,}\b",
    // Private-key-like assignment (BEGIN ... PRIVATE).
    r"\bprivate_key\s*[:=]",
];

/// The secret scanner: a compiled set of secret-shape regexes (DESIGN.md §15.5).
///
/// Construct with [`with_default_patterns`](SecretScanner::with_default_patterns)
/// for the frozen v1 vocabulary, or [`with_patterns`](SecretScanner::with_patterns)
/// to supply a custom (additive) set. [`scan`](SecretScanner::scan) is a cheap
/// `bool` predicate the capture loop calls per file.
#[derive(Debug, Clone)]
pub struct SecretScanner {
    set: RegexSet,
}

impl Default for SecretScanner {
    fn default() -> Self {
        Self::with_default_patterns()
    }
}

impl SecretScanner {
    /// Build a scanner over the frozen v1 default patterns.
    ///
    /// # Panics
    /// Never in practice: the default patterns are fixed and known to compile.
    /// (Kept infallible so the common path needs no error handling.)
    #[must_use]
    pub fn with_default_patterns() -> Self {
        SecretScanner {
            set: RegexSet::new(DEFAULT_PATTERNS).expect("frozen v1 secret patterns must compile"),
        }
    }

    /// Build a scanner over a caller-supplied pattern set (additive policy).
    ///
    /// # Errors
    /// Returns [`DriftError::Pattern`] if any pattern fails to compile.
    pub fn with_patterns<I, S>(patterns: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let set = RegexSet::new(patterns.into_iter().map(|p| p.as_ref().to_string()))
            .map_err(|e| DriftError::Pattern(e.to_string()))?;
        Ok(SecretScanner { set })
    }

    /// Whether `bytes` contain anything matching the secret patterns.
    ///
    /// This is the `scan(bytes) -> bool` predicate the capture flow uses to
    /// decide whether a file is excluded/redacted so it never enters the CAS
    /// (DESIGN.md §15.5).
    #[must_use]
    pub fn scan(&self, bytes: &[u8]) -> bool {
        self.set.is_match(bytes)
    }

    /// The number of patterns in the active set (for diagnostics/tests).
    #[must_use]
    pub fn pattern_count(&self) -> usize {
        self.set.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_fake_aws_access_key() {
        let s = SecretScanner::with_default_patterns();
        // A planted, structurally-valid (but fake) AWS access key id.
        let body = b"const key = \"AKIAIOSFODNN7EXAMPLE\";\n";
        assert!(s.scan(body), "AWS access key id must be detected");
    }

    #[test]
    fn detects_aws_secret_access_key_assignment() {
        let s = SecretScanner::with_default_patterns();
        let body = b"aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        assert!(s.scan(body));
    }

    #[test]
    fn detects_generic_api_key_assignment() {
        let s = SecretScanner::with_default_patterns();
        assert!(s.scan(b"api_key = \"sk_a1b2c3d4e5f6g7h8\""));
        assert!(s.scan(b"API-KEY: 0123456789abcdef0123"));
        assert!(s.scan(b"client_secret=abcdefghijkl0123456789"));
    }

    #[test]
    fn detects_pem_private_key_block() {
        let s = SecretScanner::with_default_patterns();
        let pem =
            b"-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\n-----END RSA PRIVATE KEY-----\n";
        assert!(s.scan(pem));
        assert!(s.scan(b"-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(s.scan(b"-----BEGIN PRIVATE KEY-----"));
    }

    #[test]
    fn detects_provider_tokens() {
        let s = SecretScanner::with_default_patterns();
        assert!(s.scan(b"ghp_0123456789abcdef0123456789abcdef0123"));
        assert!(s.scan(b"xoxb-0123456789-ABCDEFghijkl"));
        assert!(s.scan(b"sk_live_0123456789abcdef0123"));
    }

    #[test]
    fn passes_clean_source_code() {
        let s = SecretScanner::with_default_patterns();
        let clean = br#"
            pub fn add(a: i32, b: i32) -> i32 { a + b }
            // a comment mentioning "key" and "token" harmlessly
            let port = 8080;
        "#;
        assert!(!s.scan(clean), "ordinary code must not be flagged");
        assert!(!s.scan(b""));
        assert!(!s.scan(b"hello world"));
    }

    #[test]
    fn custom_patterns_compile_and_match() {
        let s = SecretScanner::with_patterns(["FOOBAR-[0-9]{4}"]).unwrap();
        assert!(s.scan(b"token FOOBAR-1234 end"));
        assert!(!s.scan(b"FOOBAR-12"));
        assert_eq!(s.pattern_count(), 1);
    }

    #[test]
    fn invalid_custom_pattern_is_an_error() {
        let err = SecretScanner::with_patterns(["("]).unwrap_err();
        assert!(matches!(err, DriftError::Pattern(_)));
    }

    #[test]
    fn default_set_is_nonempty() {
        assert!(SecretScanner::default().pattern_count() > 0);
        assert_eq!(SECRET_POLICY_VERSION, 1);
    }
}
