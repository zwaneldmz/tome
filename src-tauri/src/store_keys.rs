//! The `store_get`/`store_set` authorization decision (TOME-004). Ports
//! `src/main/lib/store-keys.js` (and its vitest suite,
//! `test/store-keys.test.js`, as the `#[cfg(test)]` module below): which key
//! names are shape-valid, which are reserved (owned by main's own files —
//! the egress allowlist, the auth file, the repo-consent file, the event
//! log — and so may never be named by a store key), and which of the
//! remaining keys the lock screen may touch while the app is locked.
//!
//! `store:get`/`store:set` stay reachable pre-login for the lock screen
//! (mirroring Electron's `OPEN_CHANNELS` — see `lock_gate::CHANNEL_OF_COMMAND`
//! and `ipc::store`), which used to mean ANY well-shaped key was
//! readable/writable before login: chat transcripts (`chat-log-*`), policy
//! toggles (`chat-provider`, `custom-agents`, ...), even
//! `egress-repo-consents` — the SAME `app_data_dir` filename `store.rs`'s
//! sibling `egress` module (Phase 3) uses for its own egress-consent file,
//! so an unauthenticated `store_set` on that key could forge consent for
//! main to load on next boot. Two things are enforced here: which
//! `app_data_dir` filenames are main's own and may never be named by a store
//! key at all, and — while locked — which of the remaining keys the lock
//! screen is actually allowed to touch.
//!
//! `store.rs`'s `get`/`set` are the only callers.

/// Every `app_data_dir` filename main itself writes outside the JSON store:
/// the egress allowlist (`egress.json`), the auth file (`egress-auth.json`),
/// the repo-consent file (`egress-repo-consents.json`), the export
/// destinations file (`export-destinations.json`, `export.rs` — hash-pinned
/// consent records for where a finished run's promoted products may be
/// copied), the persistent event log (`events.jsonl`, `events.rs`), and the
/// in-app scheduler's own hash-pinned schedule store
/// (`flow-schedules.json`, `schedule.rs` — a `store_set` on this key could
/// otherwise forge a schedule whose `flowSha1` was never actually verified
/// against the flow file it claims to match, or silently flip
/// `enabled`/clear `suspended` without going through `schedules_set`'s
/// re-hash), and the remote-run-visibility source list
/// (`remote-sources.json`, `remote.rs` — same shape of risk: a `store_set`
/// on this key could forge a consented ssh destination whose `hash` was
/// never actually verified against the record it claims to match, letting
/// `remote_runs`/`remote_run_detail` be pointed at an arbitrary host/path
/// without ever going through `remote_consent`). None may ever be named by
/// a store key, at any lock state — a `store_set` on one of these would let
/// the renderer overwrite a file main treats as its own.
pub const RESERVED_KEYS: &[&str] = &[
    "egress",
    "egress-auth",
    "egress-repo-consents",
    "events",
    "export-destinations",
    "flow-schedules",
    "remote-sources",
    // Main-owned chat files (plan §4.3/§4.5): the user overlay
    // (chat-providers.json) may only be written through chat_provider_set/
    // chat_provider_delete (a `store_set` on it could redirect a built-in
    // row), and the vault fallback file (chat-secrets.json) holds keys in
    // plaintext — it must never be readable back through store:get, even
    // when the keyring is unavailable.
    "chat-providers",
    "chat-secrets",
];

/// The only store key any pre-auth UI actually reads: the renderer boots
/// theme state before lock-screen auth so the lock overlay paints in the
/// right palette. Keep this to exactly what's empirically read before
/// login — widening it re-opens whatever key gets added next.
pub const LOCKSCREEN_STORE_KEYS: &[&str] = &["theme"];

/// Port of the JS `KEY_SHAPE` regex `/^[a-z0-9][a-z0-9-]*$/`: plain slugs
/// only (a lowercase-letter-or-digit first character, then any run of
/// lowercase letters, digits, or dashes) — no traversal, no dots, no
/// uppercase, never empty.
fn is_key_shape_valid(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Note on the JS original's `typeof key === 'string'` guard: that defends
/// `isReservedKey`/`isValidStoreKey` against non-string input reaching them
/// dynamically over IPC. Here `key: &str` is enforced statically — a
/// non-string JSON argument is rejected by Tauri/serde's command-argument
/// deserialization before `store::get`/`store::set` (and so these
/// functions) are ever reached, with a generic deserialization error
/// instead of a graceful `false`/`null`. Strictly stronger, so no runtime
/// check is ported for that part of the JS test suite.
pub fn is_reserved_key(key: &str) -> bool {
    RESERVED_KEYS.contains(&key)
}

/// Shape + reservation, independent of lock state: plain slugs only (no
/// traversal), never one of main's own files.
pub fn is_valid_store_key(key: &str) -> bool {
    is_key_shape_valid(key) && !is_reserved_key(key)
}

/// The full decision `store_get`/`store_set` apply: a key must be
/// shape-valid and unreserved always, and — while locked — must also be one
/// of the lock-screen keys above. `locked` mirrors `AppState.locked` at the
/// call site (the JS original's `{ locked }` options object collapses to a
/// plain bool here — Rust has no defaultable-object-parameter idiom, and
/// every call site already has a definite lock state to pass).
pub fn is_store_key_allowed(key: &str, locked: bool) -> bool {
    if !is_valid_store_key(key) {
        return false;
    }
    !locked || LOCKSCREEN_STORE_KEYS.contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- isReservedKey() / is_reserved_key() ----

    #[test]
    fn rejects_every_main_owned_user_data_filename() {
        // egress.json (egress allowlist), egress-auth.json (credentials),
        // egress-repo-consents.json (repo egress consent), events.jsonl (the
        // persistent event log) — none of these are store values.
        for key in RESERVED_KEYS.iter().copied() {
            assert!(is_reserved_key(key), "{key} should be reserved");
        }
    }

    #[test]
    fn does_not_reserve_an_ordinary_key() {
        assert!(!is_reserved_key("theme"));
        assert!(!is_reserved_key("workspaces"));
        assert!(!is_reserved_key("chat-log-abc123"));
    }

    // ---- isValidStoreKey() / is_valid_store_key() ----

    #[test]
    fn accepts_plain_slugs() {
        assert!(is_valid_store_key("theme"));
        assert!(is_valid_store_key("chat-log-abc123"));
        assert!(is_valid_store_key("a"));
        assert!(is_valid_store_key("9lives"));
    }

    #[test]
    fn rejects_reserved_keys_even_though_shape_valid() {
        for key in RESERVED_KEYS.iter().copied() {
            assert!(!is_valid_store_key(key), "{key} should be rejected");
        }
    }

    #[test]
    fn rejects_traversal_and_non_slug_characters() {
        assert!(!is_valid_store_key("../egress-auth"));
        assert!(!is_valid_store_key("a/b"));
        assert!(!is_valid_store_key("a.json"));
        assert!(!is_valid_store_key("UPPER"));
        assert!(!is_valid_store_key("-leading-dash"));
        assert!(!is_valid_store_key(""));
    }

    // ---- isStoreKeyAllowed() / is_store_key_allowed() ----

    #[test]
    fn rejects_egress_repo_consents_at_any_lock_state() {
        // A pre-auth store_set on this exact key is what let an
        // unauthenticated renderer write the file the (Phase 3) egress
        // module's repo-consent loader reads on next boot — forged egress
        // consent without ever logging in.
        assert!(!is_store_key_allowed("egress-repo-consents", true));
        assert!(!is_store_key_allowed("egress-repo-consents", false));
    }

    #[test]
    fn rejects_egress_auth_at_any_lock_state() {
        assert!(!is_store_key_allowed("egress-auth", true));
        assert!(!is_store_key_allowed("egress-auth", false));
    }

    #[test]
    fn denies_a_chat_transcript_key_while_locked() {
        assert!(!is_store_key_allowed("chat-log-x", true));
    }

    #[test]
    fn allows_a_chat_transcript_key_once_unlocked() {
        assert!(is_store_key_allowed("chat-log-x", false));
    }

    #[test]
    fn denies_policy_keys_while_locked() {
        for key in [
            "egress-default",
            "conductor-run",
            "chat-provider",
            "chat-model",
            "custom-agents",
            "core-vault",
            "onboarded-v1",
        ] {
            assert!(
                !is_store_key_allowed(key, true),
                "{key} should be denied while locked"
            );
        }
    }

    #[test]
    fn allows_theme_while_locked() {
        assert!(is_store_key_allowed("theme", true));
    }

    #[test]
    fn allows_a_normal_key_once_unlocked() {
        assert!(is_store_key_allowed("workspaces", false));
    }

    #[test]
    fn lockscreen_store_keys_stays_minimal() {
        // Every member must independently pass while locked.
        for key in LOCKSCREEN_STORE_KEYS.iter().copied() {
            assert!(
                is_store_key_allowed(key, true),
                "{key} should pass while locked"
            );
        }
    }
}
