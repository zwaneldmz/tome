//! Provider allowlist for the egress proxies: hostname patterns where `*`
//! matches exactly one DNS label (`[a-z0-9-]+`), case-insensitive, matched
//! label-by-label with equal label counts required — so `*.amazonaws.com`
//! can never match `amazonaws.com.evil.com` (label counts differ) nor
//! `evilamazonaws.com` (a wildcard consumes a whole label, never a
//! substring). Deliberately NOT regex-based: label comparison is
//! bypass-proof by construction and auditable by inspection, without
//! having to reason about regex-engine edge cases (anchoring mistakes,
//! `.` matching more than intended, catastrophic backtracking). Ports
//! `src/main/lib/allowlist.js` 1:1, pinned by `test/egress.test.js`'s
//! "wildcard hostname compiler" / "exact hosts" / "DEFAULT_ALLOW" suites
//! and `test/repo-egress.test.js`'s `validateRepoAllowlist` suite (both
//! ported below as `#[cfg(test)] mod tests`).
//!
//! Nothing in the crate calls into this module yet — the egress
//! orchestration layer (`mod.rs`, a later slice — see that file's own doc
//! comment for the ownership split) and `proxy.rs` (this same slice) are
//! its only intended callers.
#![allow(dead_code)]

use serde_json::Value;

/// The 16 shipped default provider hostname patterns — verbatim from
/// `lib/allowlist.js`'s `DEFAULT_ALLOW`, same order (matching itself
/// doesn't depend on order, but parity makes diffing the two files easy).
pub const DEFAULT_ALLOW: &[&str] = &[
    "api.anthropic.com",
    "claude.ai",
    "console.anthropic.com",
    "statsig.anthropic.com",
    "api.openai.com",
    "auth.openai.com",
    "generativelanguage.googleapis.com",
    "oauth2.googleapis.com",
    "openrouter.ai",
    "router.requesty.ai",
    "api.deepseek.com",
    "api.moonshot.ai",
    "api.groq.com",
    "api.mistral.ai",
    "api.x.ai",
    "bedrock-runtime.*.amazonaws.com",
];

/// One compiled hostname pattern. Port of `compileAllowlist`'s per-pattern
/// closure: the pattern is lowercased and split on `.` once, up front;
/// `matches` re-splits the candidate host the same way and requires an
/// EQUAL label count before comparing label-by-label, so a wildcard can
/// never absorb extra labels — the suffix-bypass this whole design exists
/// to prevent (`*.amazonaws.com` must never match
/// `x.amazonaws.com.evil.com`).
#[derive(Debug, Clone)]
pub struct HostMatcher {
    labels: Vec<String>,
}

impl HostMatcher {
    pub fn new(pattern: &str) -> Self {
        Self {
            labels: pattern
                .to_lowercase()
                .split('.')
                .map(str::to_string)
                .collect(),
        }
    }

    pub fn matches(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        let got: Vec<&str> = host.split('.').collect();
        if got.len() != self.labels.len() {
            return false;
        }
        self.labels.iter().zip(got.iter()).all(|(want, got)| {
            if want == "*" {
                is_dns_label(got)
            } else {
                want == got
            }
        })
    }
}

/// `/^[a-z0-9-]+$/` — a whole DNS label: one or more letters/digits/
/// hyphens. Applied only to an already-lowercased candidate (see
/// `HostMatcher::matches`), same as the JS original applies its regex to
/// an already-lowercased `got[i]`; the `i` flag JS's OWN label-shape check
/// in `validateRepoAllowlist` uses (`/^[a-z0-9-]+$/i`) is handled here by
/// `is_ascii_alphanumeric` accepting both cases directly, so this one
/// function serves both call sites correctly regardless of whether the
/// input was pre-lowercased.
fn is_dns_label(label: &str) -> bool {
    !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Compiles a list of hostname patterns (shipped defaults, a user
/// override, or consented repo hosts) into matchers. Accepts anything
/// iterable of string-likes so callers can pass `DEFAULT_ALLOW` (`&[&str]`)
/// or a `Vec<String>` (user/repo sources) without an intermediate
/// allocation to unify the element type first.
pub fn compile_allowlist<I, S>(patterns: I) -> Vec<HostMatcher>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    patterns
        .into_iter()
        .map(|p| HostMatcher::new(p.as_ref()))
        .collect()
}

/// `allowMatchers.some((re) => re.test(host))` — the actual per-request
/// allow check `proxy.rs` calls against a pane's current compiled set.
pub fn is_allowed(matchers: &[HostMatcher], host: &str) -> bool {
    matchers.iter().any(|m| m.matches(host))
}

/// Parses a repo's raw `.tome/egress.json` text into its `allow` array.
/// Mirrors `parseRepoAllowlist`: errs on bad JSON or when `allow` isn't a
/// JSON array. Element values are NOT required to be strings here (mirrors
/// `{ hosts: cfg.allow }` passing the parsed array through unchecked) —
/// [`validate_repo_allowlist`] is what rejects a non-string entry, one at
/// a time, with a reason. Callers (the future `read_repo_allowlist` port)
/// treat any `Err` as "file absent", so a malformed file can never widen
/// the gap.
pub fn parse_repo_allowlist(text: &str) -> Result<Vec<Value>, String> {
    let cfg: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    match cfg.get("allow") {
        Some(Value::Array(items)) => Ok(items.clone()),
        _ => Err("allow must be an array".to_string()),
    }
}

/// One rejected pattern and why. `pattern` keeps the original JSON value
/// (which may not even be a string — see [`parse_repo_allowlist`]'s doc
/// comment) so a caller can report exactly what the file contained.
#[derive(Debug, Clone, PartialEq)]
pub struct RejectedPattern {
    pub pattern: Value,
    pub reason: String,
}

/// Result of validating a repo-supplied allowlist: `ok` patterns are safe
/// to compile and apply; `rejected` entries never widen the gap and always
/// carry a human-readable reason.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValidationResult {
    pub ok: Vec<String>,
    pub rejected: Vec<RejectedPattern>,
}

/// Validates one candidate pattern, in the JS original's exact check
/// order. Returns the (case-unmodified) pattern on success, or a reason on
/// rejection. See `validateRepoAllowlist`'s doc comment in the JS source
/// for the rationale behind each rule — the breadth boundary is
/// positional and deliberately has no public-suffix awareness (`*.co.uk`
/// is accepted, same breadth class as `*.example.com`; the consent prompt
/// is the backstop).
fn validate_one(value: &Value) -> Result<String, String> {
    let Some(pattern) = value.as_str() else {
        return Err("not a string".to_string());
    };
    if pattern.is_empty() {
        return Err("empty pattern".to_string());
    }
    // 253 = max DNS name length; a longer pattern can never match a real
    // host and only bloats the matcher list.
    if pattern.chars().count() > 253 {
        return Err("over 253 characters".to_string());
    }
    if pattern.chars().any(|c| c.is_whitespace()) {
        return Err("contains whitespace".to_string());
    }
    // `://` catches schemes, `/` paths, `@` userinfo — a pattern must be a
    // bare hostname, or a proxy CONNECT target could be talked around the
    // matcher with URL syntax.
    if pattern.contains("://") {
        return Err("contains a URL scheme — hostnames only".to_string());
    }
    if pattern.contains('/') {
        return Err("contains a path — hostnames only".to_string());
    }
    if pattern.contains('@') {
        return Err("contains userinfo — hostnames only".to_string());
    }
    let labels: Vec<&str> = pattern.split('.').collect();
    if labels.len() < 2 {
        return Err("single-label host — needs a dot (e.g. api.example.com)".to_string());
    }
    // Every label must be a literal DNS fragment or exactly `*` (one whole
    // label). Partial wildcards like `*api` would compile to a prefix
    // match (`[a-z0-9-]+api`, matching `evilapi`) — too easy to smuggle
    // breadth past a reader.
    if let Some(bad) = labels.iter().find(|l| **l != "*" && !is_dns_label(l)) {
        return Err(format!("bad label \"{bad}\" — use * only as a whole label"));
    }
    if pattern == "*" {
        return Err("bare * matches every host".to_string());
    }
    // The last label is the effective TLD: wildcarding it (`*.com`, `*.*`)
    // matches whole TLDs, i.e. a large slice of the internet.
    if labels.last() == Some(&"*") {
        return Err("wildcard TLD matches whole slices of the internet".to_string());
    }
    if labels.first() == Some(&"*") && labels.len() < 3 {
        return Err("wildcard base domain is too broad (e.g. *.com)".to_string());
    }
    Ok(pattern.to_string())
}

/// Validates a repo's committed `.tome/egress.json` `allow` array —
/// untrusted input, since anyone who can commit to the repo can edit it.
/// The checks exist to stop a repo from silently punching the egress wide
/// open. Never panics: a hostile file degrades to per-entry rejections
/// instead of breaking the read path. Mirrors `validateRepoAllowlist`.
pub fn validate_repo_allowlist(patterns: &[Value]) -> ValidationResult {
    let mut result = ValidationResult::default();
    for value in patterns {
        match validate_one(value) {
            Ok(pattern) => result.ok.push(pattern),
            Err(reason) => result.rejected.push(RejectedPattern {
                pattern: value.clone(),
                reason,
            }),
        }
    }
    result
}

/// Mirrors `validateRepoAllowlist`'s tolerance for a non-array top-level
/// value (JS: `const list = Array.isArray(patterns) ? patterns : []`).
/// [`validate_repo_allowlist`] above already requires a proper Rust slice
/// — which, unlike a JS value, cannot itself fail to be an array — so it
/// has no way to exercise that fallback. This is the one entry point that
/// keeps the JS test case ("treats a non-array input as empty, never
/// throws") meaningfully portable: a caller holding a raw, not-yet-typed
/// `serde_json::Value` gets the same never-throws guarantee here. Nothing
/// in the real `parse_repo_allowlist` -> `validate_repo_allowlist` pipeline
/// needs this (parsing already guarantees an array by construction); it
/// exists for parity with the JS function's own defensive robustness.
pub fn validate_repo_allowlist_value(patterns: &Value) -> ValidationResult {
    match patterns.as_array() {
        Some(items) => validate_repo_allowlist(items),
        None => ValidationResult::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn matches_any(patterns: &[&str], host: &str) -> bool {
        is_allowed(&compile_allowlist(patterns.iter().copied()), host)
    }

    // ---- wildcard hostname compiler ----

    const BEDROCK: &str = "bedrock-runtime.*.amazonaws.com";

    #[test]
    fn matches_a_real_regional_endpoint() {
        assert!(matches_any(
            &[BEDROCK],
            "bedrock-runtime.us-east-1.amazonaws.com"
        ));
        assert!(matches_any(
            &[BEDROCK],
            "bedrock-runtime.eu-central-1.amazonaws.com"
        ));
    }

    #[test]
    fn rejects_suffix_bypass_hostnames() {
        assert!(!matches_any(
            &[BEDROCK],
            "bedrock-runtime.us-east-1.amazonaws.com.evil.com"
        ));
    }

    #[test]
    fn rejects_the_bare_suffix_wildcard_must_consume_a_label() {
        assert!(!matches_any(&[BEDROCK], "amazonaws.com"));
        assert!(!matches_any(&[BEDROCK], "bedrock-runtime.amazonaws.com"));
    }

    #[test]
    fn wildcard_does_not_span_multiple_labels() {
        assert!(!matches_any(
            &[BEDROCK],
            "bedrock-runtime.a.b.amazonaws.com"
        ));
    }

    #[test]
    fn compiler_is_case_insensitive() {
        assert!(matches_any(
            &[BEDROCK],
            "Bedrock-Runtime.US-East-1.AmazonAWS.com"
        ));
    }

    // ---- exact hosts ----

    #[test]
    fn exact_hosts_match_exactly_no_more() {
        let p = ["api.anthropic.com"];
        assert!(matches_any(&p, "api.anthropic.com"));
        assert!(!matches_any(&p, "api.anthropic.com.evil.com"));
        assert!(!matches_any(&p, "evil-api.anthropic.com"));
        assert!(!matches_any(&p, "anthropic.com"));
        assert!(!matches_any(&p, "apixanthropicxcom"));
    }

    // ---- DEFAULT_ALLOW ----

    #[test]
    fn default_allow_contains_the_chat_providers_the_app_depends_on() {
        assert!(DEFAULT_ALLOW.contains(&"router.requesty.ai"));
        assert!(DEFAULT_ALLOW.contains(&"api.anthropic.com"));
    }

    #[test]
    fn default_allow_has_exactly_sixteen_patterns() {
        assert_eq!(DEFAULT_ALLOW.len(), 16);
    }

    #[test]
    fn every_default_pattern_compiles_and_matches_its_own_literal_form() {
        for p in DEFAULT_ALLOW {
            let literal = p.replace('*', "x");
            assert!(
                matches_any(&[p], &literal),
                "pattern {p} should match its own literal form {literal}"
            );
        }
    }

    // ---- validateRepoAllowlist accepts ----

    #[test]
    fn accepts_valid_hostname_patterns() {
        for p in [
            "api.example.com",
            "*.example.com",
            "bedrock-runtime.*.amazonaws.com",
            "deep.sub.domain.example.co.uk",
            "API.EXAMPLE.COM",
        ] {
            let r = validate_repo_allowlist(&[json!(p)]);
            assert_eq!(r.ok, vec![p.to_string()]);
            assert!(r.rejected.is_empty());
        }
    }

    #[test]
    fn keeps_valid_entries_when_mixed_with_invalid_ones() {
        let r = validate_repo_allowlist(&[
            json!("api.example.com"),
            json!("*"),
            json!("*.example.com"),
        ]);
        assert_eq!(
            r.ok,
            vec!["api.example.com".to_string(), "*.example.com".to_string()]
        );
        assert_eq!(r.rejected.len(), 1);
        assert_eq!(r.rejected[0].pattern, json!("*"));
    }

    // ---- validateRepoAllowlist rejects ----

    #[test]
    fn rejects_bad_patterns_with_exactly_one_reason_each() {
        for p in [
            "*",
            "*.com",
            "*.*",
            "localhost",
            "https://x.com",
            "x.com/path",
            "user@x.com",
            "has space.com",
            "tab\there.com",
            "",
            "api.example.com ",
        ] {
            let r = validate_repo_allowlist(&[json!(p)]);
            assert!(r.ok.is_empty(), "expected {p:?} to be rejected");
            assert_eq!(
                r.rejected.len(),
                1,
                "expected exactly one rejection for {p:?}"
            );
        }
    }

    #[test]
    fn rejects_non_strings() {
        let r = validate_repo_allowlist(&[
            json!(42),
            json!(null),
            json!(null),
            json!({}),
            json!(["x.com"]),
        ]);
        assert!(r.ok.is_empty());
        assert_eq!(r.rejected.len(), 5);
    }

    #[test]
    fn rejects_over_long_patterns_over_253_chars() {
        let long = format!("{}.com", "a".repeat(250));
        assert!(long.chars().count() > 253);
        let r = validate_repo_allowlist(&[json!(long)]);
        assert!(r.ok.is_empty());
        assert_eq!(r.rejected.len(), 1);
    }

    #[test]
    fn rejects_partial_wildcards_that_would_compile_to_a_prefix_match() {
        assert!(validate_repo_allowlist(&[json!("*api.example.com")])
            .ok
            .is_empty());
        assert!(validate_repo_allowlist(&[json!("api*.example.com")])
            .ok
            .is_empty());
    }

    #[test]
    fn every_rejection_carries_a_human_reason() {
        let r = validate_repo_allowlist(&[
            json!("*"),
            json!("localhost"),
            json!(42),
            json!("https://x.com"),
        ]);
        for rej in r.rejected {
            assert!(!rej.reason.is_empty());
        }
    }

    #[test]
    fn treats_a_non_array_input_as_empty_never_throws() {
        assert_eq!(
            validate_repo_allowlist_value(&json!(null)),
            ValidationResult::default()
        );
        assert_eq!(
            validate_repo_allowlist_value(&json!("api.example.com")),
            ValidationResult::default()
        );
    }

    // ---- breadth boundary (pinned as-designed) ----

    #[test]
    fn accepts_interior_double_wildcard() {
        // Matches multi-label subdomains; the same breadth class as the
        // shipped bedrock-runtime.*.amazonaws.com default.
        let r = validate_repo_allowlist(&[json!("*.*.example.com")]);
        assert_eq!(r.ok, vec!["*.*.example.com".to_string()]);
    }

    #[test]
    fn accepts_leading_wildcard_with_three_labels_even_over_a_known_public_suffix() {
        // KNOWN boundary: no public-suffix list, so this is the same class
        // as *.example.com even though co.uk is a suffix.
        let r = validate_repo_allowlist(&[json!("*.co.uk")]);
        assert_eq!(r.ok, vec!["*.co.uk".to_string()]);
    }

    #[test]
    fn accepts_interior_wildcard_over_a_short_base() {
        let r = validate_repo_allowlist(&[json!("a.*.com")]);
        assert_eq!(r.ok, vec!["a.*.com".to_string()]);
    }

    #[test]
    fn accepts_and_case_insensitively_matches_uppercase_wildcard_pattern() {
        let r = validate_repo_allowlist(&[json!("*.EXAMPLE.COM")]);
        assert_eq!(r.ok, vec!["*.EXAMPLE.COM".to_string()]);
        let m = HostMatcher::new("*.EXAMPLE.COM");
        assert!(m.matches("api.example.com"));
        assert!(m.matches("API.EXAMPLE.COM"));
    }
}
