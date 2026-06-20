//! The versioned scope grammar: [`Scope`] and [`RequestedScope`].
//!
//! A capability says *what kind* of side effect is permitted; a [`Scope`] says
//! *how far* that permission reaches. The grammar has four orthogonal
//! dimensions, mirroring the DESIGN.md §15.2 capability table:
//!
//! * **path globs** — which paths a `snapshot.read`/`snapshot.write` may touch;
//! * **host allowlist** — which hosts a `net.connect` may reach;
//! * **token budget** — the per-grant ceiling on `model.invoke` tokens;
//! * **USD budget (micros)** — the per-grant ceiling on `model.invoke` spend.
//!
//! A [`Scope`] is the *granted* envelope (attached to a [`Grant`](crate::Grant)).
//! A [`RequestedScope`] is the *concrete* thing a caller asks to do right now (a
//! specific path, a specific host, a specific token/USD cost). Authorization is
//! the question "does this grant's [`Scope`] cover this [`RequestedScope`]?",
//! answered by [`Scope::covers`].
//!
//! # Versioning (CLAUDE.md C5)
//!
//! [`Scope`] carries an explicit [`Scope::schema_version`] so the grammar can
//! grow new dimensions (a method allowlist, a rate limit, an env-var allowlist)
//! without invalidating older persisted grants. [`SCOPE_SCHEMA_VERSION`] is the
//! version this build writes; [`Scope::new`] stamps it automatically.
//!
//! # Glob matching
//!
//! Path globs use a small, self-contained matcher (no external glob crate) that
//! supports the `*` (within a path segment) and `**` (across segments)
//! wildcards plus `?` (single non-separator char) — enough to express the
//! "path globs" the design calls for while keeping the grammar's matching rules
//! frozen and inspectable. Matching is anchored: the whole requested path must
//! match the whole glob.
//!
//! Design references: DESIGN.md §15.2 (scope examples per capability — path
//! globs, host allowlist, per-node token/USD budget).

use serde::{Deserialize, Serialize};

/// The schema version of the [`Scope`] grammar this build emits.
///
/// Persisted [`Scope`]s carry their own [`Scope::schema_version`]; the broker
/// reads any version it understands and stamps freshly built scopes with this
/// constant (CLAUDE.md C5 — every persisted struct is self-describing).
pub const SCOPE_SCHEMA_VERSION: u16 = 1;

/// The granted envelope of a capability: how far the permission reaches.
///
/// All four fields are independent constraints. Empty/`None` fields mean "no
/// permission along that dimension" for the dimensions a capability actually
/// uses — there is no implicit wildcard. To grant unrestricted path access,
/// include an explicit `"**"` glob; to allow any host, include an explicit
/// `"*"` host entry. This keeps deny-by-default honest: a forgotten field grants
/// nothing rather than everything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// Self-describing grammar version (CLAUDE.md C5).
    pub schema_version: u16,
    /// Glob patterns matched against requested paths for path-scoped
    /// capabilities (`snapshot.read` / `snapshot.write`). A request is in scope
    /// when at least one glob matches the requested path.
    pub path_globs: Vec<String>,
    /// Exact host names (or the single wildcard `"*"`) a host-scoped capability
    /// (`net.connect`) may connect to.
    pub host_allowlist: Vec<String>,
    /// The maximum number of model tokens this grant permits for `model.invoke`,
    /// or `None` for no token ceiling along this dimension.
    pub model_token_budget: Option<u64>,
    /// The maximum spend in USD micros (1 USD = 1_000_000 micros) this grant
    /// permits for `model.invoke`, or `None` for no USD ceiling.
    pub model_usd_budget_micros: Option<u64>,
}

impl Scope {
    /// Build an empty [`Scope`] stamped with the current
    /// [`SCOPE_SCHEMA_VERSION`].
    ///
    /// An empty scope grants nothing along any dimension; use the builder-style
    /// helpers ([`Scope::with_path_globs`], [`Scope::with_hosts`],
    /// [`Scope::with_token_budget`], [`Scope::with_usd_budget_micros`]) to widen
    /// it explicitly.
    #[must_use]
    pub fn new() -> Self {
        Scope {
            schema_version: SCOPE_SCHEMA_VERSION,
            path_globs: Vec::new(),
            host_allowlist: Vec::new(),
            model_token_budget: None,
            model_usd_budget_micros: None,
        }
    }

    /// Return a copy of this scope with `globs` as its path-glob set.
    #[must_use]
    pub fn with_path_globs<I, S>(mut self, globs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.path_globs = globs.into_iter().map(Into::into).collect();
        self
    }

    /// Return a copy of this scope with `hosts` as its host allowlist.
    #[must_use]
    pub fn with_hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.host_allowlist = hosts.into_iter().map(Into::into).collect();
        self
    }

    /// Return a copy of this scope with a model token budget.
    #[must_use]
    pub fn with_token_budget(mut self, tokens: u64) -> Self {
        self.model_token_budget = Some(tokens);
        self
    }

    /// Return a copy of this scope with a model USD budget (in micros).
    #[must_use]
    pub fn with_usd_budget_micros(mut self, micros: u64) -> Self {
        self.model_usd_budget_micros = Some(micros);
        self
    }

    /// Does this granted scope cover the concrete `requested` action for `cap`?
    ///
    /// The check considers only the dimensions the capability actually uses
    /// (per [`Capability::is_path_scoped`](crate::Capability::is_path_scoped)
    /// and friends):
    ///
    /// * **path-scoped** capabilities require the requested path to match one of
    ///   `path_globs`;
    /// * **host-scoped** capabilities require the requested host to be in
    ///   `host_allowlist` (an exact match, or the wildcard `"*"`);
    /// * **budget-scoped** capabilities require the requested token count and
    ///   USD cost to fall within the (optional) budgets;
    /// * capabilities with none of these dimensions (e.g. `process.spawn`,
    ///   `nodes.readOutputs`, `secrets.get`) are covered by the mere existence
    ///   of this grant — the grant *is* the permission.
    ///
    /// On a covered request it returns `Ok(())`; on a mismatch it returns
    /// `Err(reason)` with a human-readable explanation suitable for the audit
    /// trail.
    ///
    /// # Errors
    /// Returns a human-readable `String` describing the first dimension that
    /// failed when the granted scope does not cover the request.
    pub(crate) fn covers(
        &self,
        cap: crate::Capability,
        requested: &RequestedScope,
    ) -> Result<(), String> {
        if cap.is_path_scoped() {
            let Some(path) = requested.path.as_deref() else {
                return Err(format!(
                    "{} is path-scoped but the request named no path",
                    cap.as_str()
                ));
            };
            if !self.path_globs.iter().any(|g| glob_match(g, path)) {
                return Err(format!(
                    "path {path:?} is not covered by any granted glob {:?}",
                    self.path_globs
                ));
            }
        }

        if cap.is_host_scoped() {
            let Some(host) = requested.host.as_deref() else {
                return Err(format!(
                    "{} is host-scoped but the request named no host",
                    cap.as_str()
                ));
            };
            let allowed = self
                .host_allowlist
                .iter()
                .any(|h| h == "*" || h.eq_ignore_ascii_case(host));
            if !allowed {
                return Err(format!(
                    "host {host:?} is not in the allowlist {:?}",
                    self.host_allowlist
                ));
            }
        }

        if cap.is_budget_scoped() {
            // Each budget constrains only when it is set. An unset budget does
            // not constrain that axis: the model grant itself is the gate, and a
            // grant may bound tokens, USD, both, or neither. A request that
            // exceeds a *set* budget is refused.
            if let (Some(budget), Some(req)) = (self.model_token_budget, requested.tokens) {
                if req > budget {
                    return Err(format!(
                        "requested {req} tokens exceeds the granted budget of {budget}"
                    ));
                }
            }

            if let (Some(budget), Some(req)) = (self.model_usd_budget_micros, requested.usd_micros)
            {
                if req > budget {
                    return Err(format!(
                        "requested {req} USD micros exceeds the granted budget of {budget}"
                    ));
                }
            }
        }

        Ok(())
    }
}

impl Default for Scope {
    fn default() -> Self {
        Scope::new()
    }
}

/// The concrete action a caller asks the broker to authorize right now.
///
/// Where a [`Scope`] is the *granted* envelope, a `RequestedScope` is the
/// *specific* request: the one path being read, the one host being dialed, the
/// token/USD cost of this one model call. The broker checks a request against
/// the matching grant's [`Scope`] via [`Scope::covers`].
///
/// Construct via [`RequestedScope::path`], [`RequestedScope::host`],
/// [`RequestedScope::model`], or [`RequestedScope::none`] for capabilities that
/// carry no scope dimension.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestedScope {
    /// The concrete path being acted on (for path-scoped capabilities).
    pub path: Option<String>,
    /// The concrete host being connected to (for `net.connect`).
    pub host: Option<String>,
    /// The token cost of this model call (for `model.invoke`).
    pub tokens: Option<u64>,
    /// The USD-micros cost of this model call (for `model.invoke`).
    pub usd_micros: Option<u64>,
}

impl RequestedScope {
    /// A request that names no scope dimension.
    ///
    /// Appropriate for capabilities that are not path/host/budget scoped
    /// (`process.spawn`, `nodes.readOutputs`, `secrets.get`): the request is
    /// covered by the existence of a matching grant.
    #[must_use]
    pub fn none() -> Self {
        RequestedScope::default()
    }

    /// A request to act on a concrete `path` (path-scoped capabilities).
    #[must_use]
    pub fn path(path: impl Into<String>) -> Self {
        RequestedScope {
            path: Some(path.into()),
            ..Default::default()
        }
    }

    /// A request to connect to a concrete `host` (`net.connect`).
    #[must_use]
    pub fn host(host: impl Into<String>) -> Self {
        RequestedScope {
            host: Some(host.into()),
            ..Default::default()
        }
    }

    /// A request to spend `tokens` and `usd_micros` on a model call
    /// (`model.invoke`).
    #[must_use]
    pub fn model(tokens: u64, usd_micros: u64) -> Self {
        RequestedScope {
            tokens: Some(tokens),
            usd_micros: Some(usd_micros),
            ..Default::default()
        }
    }

    /// A short, stable human-readable rendering of this request used in the
    /// `scope_used` column of an [`AuditEntry`](crate::AuditEntry).
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = &self.path {
            parts.push(format!("path={p}"));
        }
        if let Some(h) = &self.host {
            parts.push(format!("host={h}"));
        }
        if let Some(t) = self.tokens {
            parts.push(format!("tokens={t}"));
        }
        if let Some(u) = self.usd_micros {
            parts.push(format!("usd_micros={u}"));
        }
        if parts.is_empty() {
            "(none)".to_string()
        } else {
            parts.join(" ")
        }
    }
}

/// Anchored glob match supporting `**` (across separators), `*` (within a
/// segment, not crossing `/`), and `?` (one non-`/` character).
///
/// The whole `path` must match the whole `pattern`. Path separators are `/`.
/// This is a small, dependency-free matcher whose rules are part of the frozen
/// scope grammar.
fn glob_match(pattern: &str, path: &str) -> bool {
    glob_match_bytes(pattern.as_bytes(), path.as_bytes())
}

/// Recursive byte-wise backtracking glob matcher (see [`glob_match`]).
fn glob_match_bytes(pat: &[u8], txt: &[u8]) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    // Saved backtrack point for the most recent `*` (single-segment wildcard).
    let mut star: Option<(usize, usize)> = None;

    while ti < txt.len() {
        if pi < pat.len() {
            match pat[pi] {
                b'*' => {
                    // Distinguish `**` (cross-separator) from `*` (within
                    // segment). `**` recurses to try every suffix split.
                    if pi + 1 < pat.len() && pat[pi + 1] == b'*' {
                        let rest = &pat[pi + 2..];
                        // `**` matches zero or more of anything, including `/`.
                        if rest.is_empty() {
                            return true;
                        }
                        // `**/x` also matches a *zero-segment* prefix, collapsing
                        // the trailing separator: `src/**/test.rs` matches
                        // `src/test.rs`. Try the after-slash pattern against the
                        // current position first.
                        if rest[0] == b'/' {
                            let after_slash = &rest[1..];
                            if glob_match_bytes(after_slash, &txt[ti..]) {
                                return true;
                            }
                        }
                        for split in ti..=txt.len() {
                            if glob_match_bytes(rest, &txt[split..]) {
                                return true;
                            }
                        }
                        return false;
                    }
                    // Single `*`: remember this position to backtrack into, then
                    // tentatively consume nothing.
                    star = Some((pi, ti));
                    pi += 1;
                    continue;
                }
                b'?' => {
                    if txt[ti] != b'/' {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                c => {
                    if c == txt[ti] {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
            }
        }

        // Mismatch (or pattern exhausted with text remaining): backtrack into
        // the last single `*`, extending what it consumes — but a single `*`
        // never consumes a `/`.
        if let Some((sp, st)) = star {
            if txt[st] == b'/' {
                return false;
            }
            star = Some((sp, st + 1));
            pi = sp + 1;
            ti = st + 1;
        } else {
            return false;
        }
    }

    // Text consumed: any trailing pattern must be all `*`/`**`.
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

#[cfg(test)]
mod glob_tests {
    use super::glob_match;

    #[test]
    fn exact_match() {
        assert!(glob_match("src/main.rs", "src/main.rs"));
        assert!(!glob_match("src/main.rs", "src/lib.rs"));
    }

    #[test]
    fn single_star_stays_within_segment() {
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/sub/main.rs"));
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "a/main.rs"));
    }

    #[test]
    fn double_star_crosses_segments() {
        assert!(glob_match("src/**", "src/a/b/c.rs"));
        assert!(glob_match("src/**", "src/main.rs"));
        assert!(glob_match("**", "anything/at/all"));
        assert!(glob_match("**/*.rs", "a/b/c.rs"));
        assert!(glob_match("src/**/test.rs", "src/a/b/test.rs"));
        assert!(glob_match("src/**/test.rs", "src/test.rs"));
        assert!(!glob_match("src/**/test.rs", "src/a/b/other.rs"));
    }

    #[test]
    fn question_mark_one_non_separator() {
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "a/c"));
        assert!(!glob_match("a?c", "ac"));
    }

    #[test]
    fn anchored_whole_string() {
        assert!(!glob_match("src", "src/main.rs"));
        assert!(!glob_match("main.rs", "src/main.rs"));
    }
}
