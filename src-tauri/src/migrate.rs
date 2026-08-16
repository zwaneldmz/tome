//! First-boot data migration: copies a legacy Electron `userData` profile
//! into this build's own Tauri `app_data_dir`, so upgrading from the
//! Electron build keeps a user's passphrase/TOTP setup, repo egress
//! consents, saved workspaces/theme/preferences, event history, and any
//! downloaded whisper model. Ports the rewrite plan's "Data migration" line
//! under §7 Packaging + Migration: "first boot, copy `~/Library/
//! Application Support/Tome` → Tauri app-data dir (store/, airgap*.json,
//! events.jsonl, models/), preserving 0600."
//!
//! No Electron-side JS original exists for this file to port — Electron
//! was always the SOURCE profile format here, never the destination, so
//! there is nothing upstream to pin against the way every other module in
//! this crate pins against a `src/main/**` original. This file instead
//! follows this crate's own established shape for boot-time, `AppHandle`-
//! touching logic (see `store.rs`'s and `events.rs`'s own module doc
//! comments for the same split): a pure, `&Path`-only core ([`migrate_dir`]
//! and its two private helpers) that `#[cfg(test)]` below exercises
//! directly with `tempfile`, plus a thin `AppHandle`-resolving entry point
//! ([`run`]) that production code calls and this file's own tests do not —
//! building a real or mocked `AppHandle` needs Tauri's `test` cargo
//! feature, which this crate does not enable (see `events.rs`'s "Testing
//! boundary note" for the identical reasoning).
//!
//! ## What gets copied, and what deliberately doesn't
//! Every top-level `*.json` file directly under the source directory —
//! this is every flat store-key file `store.rs` owns (`workspaces.json`,
//! `theme.json`, `custom-agents.json`, `chat-log-*.json`, ... — one
//! `<app_data_dir>/<key>.json` file per key, no `store/` subdirectory; see
//! `store.rs`'s own module doc comment) PLUS every filename
//! `store_keys::RESERVED_KEYS` carves out of that same key space precisely
//! because a different file owner already claims it (`airgap.json`,
//! `airgap-auth.json`, `airgap-repo-consents.json`) — plus the flat
//! `events.jsonl` event log, plus the whole `models/` tree (whatever
//! `stt::model_path`'s downloaded whisper binaries left there). Everything
//! else a real Electron `userData` directory holds — `Cache/`, `Cookies`,
//! `Local Storage/`, `Session Storage/`, `GPUCache/`, `DawnGraphiteCache/`,
//! `Local State`, ... (Chromium's own on-disk state; confirmed by
//! inspecting a real installed copy of this repo's Electron build while
//! writing this file) — is deliberately left behind: none of it means
//! anything to this build's WebKit-based webview, so [`migrate_dir`] is a
//! targeted, named-allowlist copy, never a directory mirror.
//!
//! ## The `enc:v1:` TOTP secret: copied as opaque bytes, never touched
//! A migrated `airgap-auth.json` may hold a TOTP secret Electron's
//! `safeStorage` encrypted (`enc:v1:<base64>` — see `authlock.rs`'s own
//! module doc comment for the full story). This module copies that string
//! byte-for-byte like every other field in the file; it never parses,
//! inspects, or attempts to decrypt it. `authlock::AuthLock::load` already
//! deserializes whatever lands on disk permissively, and its existing
//! `enc:v1:`-prefix detection already fails that one field closed
//! (verification just returns `false`) — this
//! file adds no logic of its own for that path, on purpose.
//!
//! ## Idempotency and safety
//! See [`migrate_dir`]'s own doc comment for the exact "already used" test
//! and its best-effort, one-bad-entry-does-not-abort-the-rest error
//! handling.

use std::fs;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

use crate::events::log_event;

/// `events.jsonl` — matches `events.rs`'s own (private) `events_file_path`.
const EVENTS_FILE_NAME: &str = "events.jsonl";

/// `models/` — matches `stt::model_path`, which joins this same directory
/// name onto `app_data_dir`.
const MODELS_DIR_NAME: &str = "models";

/// The legacy Electron `userData` directory this build might migrate FROM,
/// for the current OS — ground truth pinned by the rewrite plan's "Data
/// migration" section: `~/Library/Application Support/Tome` on macOS
/// (`productName: "Tome"` in the old `package.json`; Electron's `app.name`
/// prefers a package's `productName` over its lowercase `name` when both
/// are present, and `app.getPath('userData')` is derived from `app.name`),
/// `~/.config/Tome` on Linux (the XDG-derived default `userData` path
/// Electron picks for an app that never calls `app.setPath('userData',
/// ...)`, which this one never did). `None` on any other OS — this build
/// targets only macOS + Linux, the plan's "Locked decisions" — or if
/// `$HOME` doesn't resolve at all.
fn electron_user_data_dir() -> Option<PathBuf> {
    let home = std::env::home_dir()?;
    if cfg!(target_os = "macos") {
        Some(
            home.join("Library")
                .join("Application Support")
                .join("Tome"),
        )
    } else if cfg!(target_os = "linux") {
        Some(home.join(".config").join("Tome"))
    } else {
        None
    }
}

/// Copies one file's bytes AND its exact permission mode from `src` to
/// `dst` (Unix only, matching every other permission-setting call site in
/// this crate — `store.rs::set`, `authlock.rs`'s `AuthLock::save`,
/// `airgap::AirgapState::save_repo_consents`). `fs::copy` alone does not
/// guarantee this: the new file's mode is whatever the platform's own copy
/// syscall defaults to, not necessarily `src`'s — the explicit
/// `set_permissions` below, reading `src`'s OWN metadata, is what actually
/// pins it. This is what makes "preserve 0600 on the files that had it"
/// fall out for free without a hardcoded filename allowlist: every source
/// file that needs 0600 (`airgap-auth.json`, `airgap-repo-consents.json`,
/// every store-key file — all written 0600 by the modules named above)
/// already carries that mode on disk, so a faithful mode-preserving copy
/// reproduces it for exactly those files and none that don't.
fn copy_file_preserving_mode(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::copy(src, dst)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(src)?.permissions().mode();
        fs::set_permissions(dst, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Recursively copies `src` into `dst` — creating `dst` and every nested
/// subdirectory, and preserving each directory's own mode the same way
/// [`copy_file_preserving_mode`] preserves a file's. Symlinks are silently
/// skipped rather than followed: `DirEntry::file_type()` is lstat-based (it
/// reports the entry itself, not whatever it points at), so a symlink
/// entry is neither `is_file()` nor `is_dir()` and matches neither branch
/// below — meaning nothing outside `src`'s own tree can be pulled in by a
/// symlink planted anywhere under it. Returns the number of FILES copied
/// (directories don't count toward the total); best-effort throughout, the
/// same as [`migrate_dir`] — one unreadable file or subdirectory anywhere
/// in the tree is skipped, never fatal to its siblings.
fn copy_dir_recursive(src: &Path, dst: &Path) -> u64 {
    if fs::create_dir_all(dst).is_err() {
        return 0;
    }
    #[cfg(unix)]
    if let Ok(meta) = fs::metadata(src) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dst, fs::Permissions::from_mode(meta.permissions().mode()));
    }
    let Ok(entries) = fs::read_dir(src) else {
        return 0;
    };
    let mut copied = 0u64;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copied += copy_dir_recursive(&entry.path(), &dst_path);
        } else if file_type.is_file() && copy_file_preserving_mode(&entry.path(), &dst_path).is_ok()
        {
            copied += 1;
        }
    }
    copied
}

/// True when `dest` shows ANY sign of already being a live, in-use
/// `app_data_dir` — see [`migrate_dir`]'s doc comment for why "already has
/// an `airgap-auth.json`" is not a safe enough test on its own. A `dest`
/// that doesn't exist yet, or exists but is completely empty, is the only
/// thing treated as pristine; a single unrelated entry is enough to trip
/// this, deliberately erring toward never overwriting real user state over
/// correctly detecting every possible "this was actually still pristine"
/// edge case.
fn dest_already_used(dest: &Path) -> bool {
    fs::read_dir(dest)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// The core migration: see this module's doc comment for exactly which
/// filenames under `source` get copied into `dest` and which are left
/// behind. Idempotent and safe by construction, not by a separate "have I
/// run before" flag:
///
/// - No-ops (returns `0`, touches nothing in `dest`) if [`dest_already_used`]
///   is true. This is deliberately NOT scoped to "has an `airgap-auth.json`"
///   — the app is fully usable, and `store:set` reachable for every
///   non-reserved key (theme, workspaces, `chat-log-*`, `custom-agents`,
///   ...; see `lock_gate::is_locked`/`store_keys::is_store_key_allowed`),
///   before a passphrase is EVER configured. An auth-file-only test would
///   treat a `dest` that already holds real interim user data — written by
///   actual app usage, not migration — as still "pristine" and copy over
///   it again on every subsequent boot (`run()` fires from `.setup()` every
///   single launch), silently reverting that data to the frozen Electron
///   snapshot each time. Testing "does `dest` have ANYTHING in it yet"
///   instead closes that hole regardless of which file got there first —
///   an earlier migration pass, or the user just using the app.
/// - No-ops (returns `0`) if `source` doesn't exist, isn't readable, or
///   isn't a directory — `fs::read_dir` folds all three into one `Err`.
/// - Every entry is handled best-effort: one unreadable/uncopyable file
///   never aborts the rest of the walk, mirroring `events.rs`'s "logging
///   must never break the thing being logged" discipline for the same
///   underlying reason — a partial migration must never block boot.
fn migrate_dir(source: &Path, dest: &Path) -> u64 {
    if dest_already_used(dest) {
        return 0;
    }
    let Ok(entries) = fs::read_dir(source) else {
        return 0;
    };
    if fs::create_dir_all(dest).is_err() {
        return 0;
    }
    let mut copied = 0u64;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        // Non-UTF8 filename: best-effort skip. Never true in practice for
        // this app's own files (plain ASCII slugs — see `store_keys.rs`'s
        // KEY_SHAPE), only a theoretical concern for stray Electron/OS
        // cruft this migration wouldn't want anyway.
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let dst_path = dest.join(&name);
        if file_type.is_file() && (name_str == EVENTS_FILE_NAME || name_str.ends_with(".json")) {
            if copy_file_preserving_mode(&entry.path(), &dst_path).is_ok() {
                copied += 1;
            }
        } else if file_type.is_dir() && name_str == MODELS_DIR_NAME {
            copied += copy_dir_recursive(&entry.path(), &dst_path);
        }
        // Anything else (Cache/, Cookies, Local Storage/, symlinks, ...) is
        // deliberately left behind — see this module's doc comment.
    }
    copied
}

/// Entry point — called once from `lib.rs::run()`'s `.setup()`, BEFORE
/// `boot_auth_and_airgap`, so a freshly-migrated `airgap-auth.json`/
/// `airgap-repo-consents.json` (if this boot's [`migrate_dir`] call copies
/// either) is what that function's own `AuthLock::load`/
/// `AirgapState::load_repo_consents` calls see on THIS boot, not the next
/// one. Not itself fallible in any way that should abort `.setup()`: an
/// `app_data_dir()` resolution failure here collapses to a silent no-op —
/// the exact same fallback `boot_auth_and_airgap` itself already tolerates
/// for the identical call (see that function's own doc comment).
pub fn run(app: &AppHandle) {
    let Ok(dest) = app.path().app_data_dir() else {
        return;
    };
    let Some(source) = electron_user_data_dir() else {
        return;
    };
    let copied = migrate_dir(&source, &dest);
    if copied > 0 {
        log_event(
            app,
            "migrate:electron",
            vec![
                ("source", serde_json::json!(source.to_string_lossy())),
                ("filesCopied", serde_json::json!(copied)),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    // ---- electron_user_data_dir ----

    #[test]
    fn electron_user_data_dir_matches_this_os_convention() {
        let home = std::env::home_dir().expect("HOME must resolve in test env");
        let expected = if cfg!(target_os = "macos") {
            Some(
                home.join("Library")
                    .join("Application Support")
                    .join("Tome"),
            )
        } else if cfg!(target_os = "linux") {
            Some(home.join(".config").join("Tome"))
        } else {
            None
        };
        assert_eq!(electron_user_data_dir(), expected);
    }

    // ---- migrate_dir: happy path ----

    #[test]
    fn migrate_dir_copies_top_level_json_files() {
        let src = tempdir();
        let dst = tempdir();
        write(src.path(), "workspaces.json", r#"{"a":1}"#);
        write(src.path(), "theme.json", r#""dark""#);
        let copied = migrate_dir(src.path(), dst.path());
        assert_eq!(copied, 2);
        assert_eq!(
            fs::read_to_string(dst.path().join("workspaces.json")).unwrap(),
            r#"{"a":1}"#
        );
        assert_eq!(
            fs::read_to_string(dst.path().join("theme.json")).unwrap(),
            r#""dark""#
        );
    }

    #[test]
    fn migrate_dir_copies_the_reserved_airgap_files() {
        let src = tempdir();
        let dst = tempdir();
        write(src.path(), "airgap.json", r#"{"allow":[]}"#);
        write(src.path(), "airgap-auth.json", r#"{"salt":"s","hash":"h"}"#);
        write(src.path(), "airgap-repo-consents.json", r#"{}"#);
        let copied = migrate_dir(src.path(), dst.path());
        assert_eq!(copied, 3);
        assert!(dst.path().join("airgap.json").exists());
        assert!(dst.path().join("airgap-auth.json").exists());
        assert!(dst.path().join("airgap-repo-consents.json").exists());
    }

    #[test]
    fn migrate_dir_copies_events_jsonl() {
        let src = tempdir();
        let dst = tempdir();
        write(src.path(), "events.jsonl", "{\"ts\":\"t\"}\n");
        let copied = migrate_dir(src.path(), dst.path());
        assert_eq!(copied, 1);
        assert_eq!(
            fs::read_to_string(dst.path().join("events.jsonl")).unwrap(),
            "{\"ts\":\"t\"}\n"
        );
    }

    #[test]
    fn migrate_dir_copies_the_models_dir_recursively() {
        let src = tempdir();
        let dst = tempdir();
        write(src.path(), "models/ggml-base.en.bin", "fake-model-bytes");
        write(src.path(), "models/nested/extra.bin", "nested-bytes");
        let copied = migrate_dir(src.path(), dst.path());
        assert_eq!(copied, 2);
        assert_eq!(
            fs::read_to_string(dst.path().join("models/ggml-base.en.bin")).unwrap(),
            "fake-model-bytes"
        );
        assert_eq!(
            fs::read_to_string(dst.path().join("models/nested/extra.bin")).unwrap(),
            "nested-bytes"
        );
    }

    #[test]
    fn migrate_dir_ignores_electron_chromium_internals() {
        let src = tempdir();
        let dst = tempdir();
        write(src.path(), "Cookies", "sqlite-bytes");
        write(src.path(), "Local State", "{}");
        write(src.path(), "Cache/abc", "cache-bytes");
        write(src.path(), "workspaces.json", "{}"); // one real file alongside the noise
        let copied = migrate_dir(src.path(), dst.path());
        assert_eq!(copied, 1);
        assert!(dst.path().join("workspaces.json").exists());
        assert!(!dst.path().join("Cookies").exists());
        assert!(!dst.path().join("Local State").exists());
        assert!(!dst.path().join("Cache").exists());
    }

    // ---- migrate_dir: mode preservation ----

    #[cfg(unix)]
    #[test]
    fn migrate_dir_preserves_0600_on_the_auth_file() {
        let src = tempdir();
        let dst = tempdir();
        let auth = write(src.path(), "airgap-auth.json", "{}");
        chmod(&auth, 0o600);
        migrate_dir(src.path(), dst.path());
        assert_eq!(mode_of(&dst.path().join("airgap-auth.json")), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn migrate_dir_preserves_whatever_mode_an_ordinary_json_file_had() {
        // Not hardcoded to 0600 for everything — a genuine mirror of
        // source, whatever that source mode happens to be.
        let src = tempdir();
        let dst = tempdir();
        let theme = write(src.path(), "theme.json", "\"dark\"");
        chmod(&theme, 0o640);
        migrate_dir(src.path(), dst.path());
        assert_eq!(mode_of(&dst.path().join("theme.json")), 0o640);
    }

    #[cfg(unix)]
    #[test]
    fn migrate_dir_preserves_modes_recursively_under_models() {
        let src = tempdir();
        let dst = tempdir();
        let model = write(src.path(), "models/ggml-base.en.bin", "bytes");
        chmod(&model, 0o644);
        migrate_dir(src.path(), dst.path());
        assert_eq!(mode_of(&dst.path().join("models/ggml-base.en.bin")), 0o644);
    }

    // ---- migrate_dir: idempotency / safety ----

    #[test]
    fn migrate_dir_skips_when_dest_already_has_an_auth_file() {
        let src = tempdir();
        let dst = tempdir();
        write(src.path(), "workspaces.json", "{\"new\":true}");
        let original_auth = write(dst.path(), "airgap-auth.json", "{\"already\":\"used\"}");
        let copied = migrate_dir(src.path(), dst.path());
        assert_eq!(copied, 0);
        assert_eq!(
            fs::read_to_string(&original_auth).unwrap(),
            "{\"already\":\"used\"}"
        );
        assert!(!dst.path().join("workspaces.json").exists());
    }

    #[test]
    fn migrate_dir_skips_when_dest_has_real_user_data_but_no_auth_file_yet() {
        // Regression test for the data-loss bug this module used to have:
        // the app is fully usable — and `store:set` writes real files like
        // theme.json — before a passphrase is ever configured, so a `dest`
        // with genuine interim user data and no `airgap-auth.json` must
        // still be treated as "already used", not re-migrated over. Without
        // this, `migrate_dir` re-ran on every boot and clobbered `dest`'s
        // theme.json back to the stale Electron snapshot on every launch
        // until the user finally set a passphrase.
        let src = tempdir();
        let dst = tempdir();
        write(src.path(), "theme.json", r#""dark""#); // stale Electron snapshot
        write(dst.path(), "theme.json", r#""light""#); // real interim user change
        let copied = migrate_dir(src.path(), dst.path());
        assert_eq!(copied, 0);
        assert_eq!(
            fs::read_to_string(dst.path().join("theme.json")).unwrap(),
            r#""light""#
        );
    }

    #[test]
    fn migrate_dir_skips_when_dest_has_any_unrelated_pre_existing_file() {
        // Broader than the auth-file/theme-file cases above: ANY file
        // already in `dest` — even one migration itself would never write
        // — is enough to mark it "already used" and refuse to touch it.
        let src = tempdir();
        let dst = tempdir();
        write(src.path(), "workspaces.json", "{\"new\":true}");
        write(dst.path(), "some-unrelated-file.txt", "not json, not ours");
        let copied = migrate_dir(src.path(), dst.path());
        assert_eq!(copied, 0);
        assert!(!dst.path().join("workspaces.json").exists());
    }

    #[test]
    fn migrate_dir_is_noop_when_source_is_absent() {
        let src_parent = tempdir();
        let missing_source = src_parent.path().join("does-not-exist");
        let dst = tempdir();
        let copied = migrate_dir(&missing_source, dst.path());
        assert_eq!(copied, 0);
        assert_eq!(fs::read_dir(dst.path()).unwrap().count(), 0);
    }

    #[test]
    fn migrate_dir_is_noop_when_source_is_a_plain_file_not_a_directory() {
        let tmp = tempdir();
        let src = write(tmp.path(), "not-a-dir", "oops");
        let dst = tempdir();
        let copied = migrate_dir(&src, dst.path());
        assert_eq!(copied, 0);
    }

    #[test]
    fn migrate_dir_creates_a_missing_dest_directory() {
        let src = tempdir();
        let dst_parent = tempdir();
        let dst = dst_parent.path().join("nested/deeper");
        write(src.path(), "workspaces.json", "{}");
        let copied = migrate_dir(src.path(), &dst);
        assert_eq!(copied, 1);
        assert!(dst.join("workspaces.json").exists());
    }

    #[test]
    fn migrate_dir_returns_zero_for_a_completely_empty_source() {
        let src = tempdir();
        let dst = tempdir();
        assert_eq!(migrate_dir(src.path(), dst.path()), 0);
    }

    #[cfg(unix)]
    #[test]
    fn migrate_dir_skips_a_symlink_even_when_its_name_matches() {
        let src = tempdir();
        let dst = tempdir();
        let real_target = write(src.path(), "elsewhere.txt", "not json really");
        std::os::unix::fs::symlink(&real_target, src.path().join("workspaces.json")).unwrap();
        let copied = migrate_dir(src.path(), dst.path());
        assert_eq!(copied, 0);
        assert!(!dst.path().join("workspaces.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn migrate_dir_skips_a_symlinked_directory_named_models() {
        let src = tempdir();
        let real_dir = tempdir(); // outside src entirely
        write(real_dir.path(), "secret.bin", "should never be reached");
        let dst = tempdir();
        std::os::unix::fs::symlink(real_dir.path(), src.path().join("models")).unwrap();
        let copied = migrate_dir(src.path(), dst.path());
        assert_eq!(copied, 0);
        assert!(!dst.path().join("models").exists());
    }
}
