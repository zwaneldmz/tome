//! The server-owner-controlled egress allowlist `tome-runner`'s per-node
//! `PaneProxy` is built from: `~/.config/tome-runner/egress.json`, shape
//! `{"allow": [...]}` — the same shape as the desktop app's own
//! `<app_data_dir>/egress.json` override file and a repo's own
//! `.tome/egress.json` (see `tome_flow::egress::allowlist`'s doc comment
//! for the pattern-matching rules every entry is validated against).
//!
//! **Deliberately NEVER the repo checkout's own `.tome/egress.json`, and
//! this is load-bearing, not a convenience choice.** The interactive
//! desktop app treats a repo's `.tome/egress.json` as a CONSENT-gated
//! suggestion: a human reads the diff in a review UI, clicks Allow, and
//! only then does `EgressState::consent_repo_allowlist` fold its hosts
//! into the effective allowlist. `tome-runner` runs unattended — there is
//! no human at the keyboard to click Allow — and the flow file (plus
//! everything else checked into the same repo) is exactly the input an
//! agent's own prior output, or a malicious PR, can shape. If this binary
//! ever read a repo-supplied allowlist automatically, a compromised or
//! adversarial flow.json could ship its own `.tome/egress.json` alongside
//! itself and grant its own future runs wider egress with nobody to
//! approve anything — a straightforward prompt-injection / supply-chain
//! escalation, not a hypothetical one. The ONLY allowlist source this
//! binary ever reads is a file under the SERVER OWNER's `$HOME`, which
//! nothing inside the repo checkout — or anything an agent writes to it —
//! can reach or edit. [`load_allowed`]'s signature reflects this: it takes
//! `config_dir` (`~/.config/tome-runner`, from [`crate::home`]), never a
//! flow's own root or anything derived from the checkout.

use std::path::Path;

use tome_flow::egress::allowlist::{self, DEFAULT_ALLOW};

/// Reads and validates `<config_dir>/egress.json`'s `allow` array, and
/// returns the shipped provider defaults ([`DEFAULT_ALLOW`] — every
/// gapped pane, everywhere in this codebase, starts from this same
/// baseline) plus every additional pattern that passes
/// [`tome_flow::egress::allowlist::validate_repo_allowlist`].
///
/// Missing file, unreadable file, malformed JSON, or a missing/non-array
/// `allow` key all collapse to "no extra hosts" — the same fail-closed
/// posture `EgressState::read_repo_allowlist` uses for this exact file
/// shape: a server owner who hasn't configured this file yet gets the
/// shipped provider hosts only, never a wider gap by accident. Individual
/// rejected entries are silently dropped (not reported) here — this is a
/// headless, unattended path with no UI to show a rejection reason to; a
/// server owner debugging "why can't my agent reach X" reads this
/// module's doc comment for the pattern rules, or `docs/remote-runner.md`.
pub fn load_allowed(config_dir: &Path) -> Vec<String> {
    let mut hosts: Vec<String> = DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect();
    let Ok(text) = std::fs::read_to_string(config_dir.join("egress.json")) else {
        return hosts;
    };
    let Ok(patterns) = allowlist::parse_repo_allowlist(&text) else {
        return hosts;
    };
    hosts.extend(allowlist::validate_repo_allowlist(&patterns).ok);
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tome-runner-egress-config-test-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_config_file_yields_exactly_the_shipped_defaults() {
        let dir = scratch_dir("missing");
        let hosts = load_allowed(&dir);
        assert_eq!(hosts.len(), DEFAULT_ALLOW.len());
        for p in DEFAULT_ALLOW {
            assert!(hosts.contains(&p.to_string()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_json_falls_back_to_defaults_only() {
        let dir = scratch_dir("malformed");
        std::fs::write(dir.join("egress.json"), "not json").unwrap();
        assert_eq!(load_allowed(&dir).len(), DEFAULT_ALLOW.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_allow_key_falls_back_to_defaults_only() {
        let dir = scratch_dir("no-allow-key");
        std::fs::write(dir.join("egress.json"), r#"{"other":[]}"#).unwrap();
        assert_eq!(load_allowed(&dir).len(), DEFAULT_ALLOW.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_extra_hosts_are_added_on_top_of_the_defaults() {
        let dir = scratch_dir("valid-extra");
        std::fs::write(
            dir.join("egress.json"),
            r#"{"allow":["internal.example.com","*.corp.example.net"]}"#,
        )
        .unwrap();
        let hosts = load_allowed(&dir);
        assert_eq!(hosts.len(), DEFAULT_ALLOW.len() + 2);
        assert!(hosts.contains(&"internal.example.com".to_string()));
        assert!(hosts.contains(&"*.corp.example.net".to_string()));
        for p in DEFAULT_ALLOW {
            assert!(hosts.contains(&p.to_string()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_entries_are_dropped_but_valid_siblings_still_apply() {
        let dir = scratch_dir("mixed");
        std::fs::write(
            dir.join("egress.json"),
            r#"{"allow":["good.example.com","*","not a host"]}"#,
        )
        .unwrap();
        let hosts = load_allowed(&dir);
        assert!(hosts.contains(&"good.example.com".to_string()));
        assert!(!hosts.contains(&"*".to_string()));
        assert!(!hosts.contains(&"not a host".to_string()));
        assert_eq!(hosts.len(), DEFAULT_ALLOW.len() + 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_repo_checkout_style_dot_tome_allowlist_is_never_consulted() {
        // Regression guard for this module's own load-bearing property:
        // load_allowed's only input is config_dir — nothing here ever
        // joins ".tome" or reads anything relative to a flow's own root.
        let dir = scratch_dir("no-repo-path");
        let repo_style = dir.join(".tome");
        std::fs::create_dir_all(&repo_style).unwrap();
        std::fs::write(
            repo_style.join("egress.json"),
            r#"{"allow":["evil.example.com"]}"#,
        )
        .unwrap();
        let hosts = load_allowed(&dir);
        assert!(!hosts.contains(&"evil.example.com".to_string()));
        assert_eq!(hosts.len(), DEFAULT_ALLOW.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
