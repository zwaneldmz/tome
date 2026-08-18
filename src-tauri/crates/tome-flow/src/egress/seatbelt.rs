//! macOS seatbelt (`sandbox-exec`) profile builder for gapped agent panes —
//! port of `src/main/egress.js`'s `seatbeltProfile(userData)`. Pure string
//! construction only: this module never shells out to `sandbox-exec` itself
//! (that belongs to a future `buildAgentEnv`-equivalent integrator, mirroring
//! `index.js`'s own `sandbox = process.platform === 'darwin' ? { cmd:
//! '/usr/bin/sandbox-exec', args: ['-p', egress.seatbeltProfile(userData)] }
//! : null`) — so the builder itself compiles, and its `#[cfg(test)]` suite
//! runs, on every OS this crate targets, even though the profile it produces
//! is only ever handed to `sandbox-exec` on macOS.
//!
//! Rule order matters in SBPL — "later rules win" (the JS original's own
//! comment) — so this is default-allow, then a blanket network-outbound
//! deny, then a narrow loopback-only re-allow: a gapped pane's ONLY route
//! out is its own per-pane proxy on `127.0.0.1` (the egress's whole value
//! proposition; see TOME-001..021 in git log). Two confinement rules follow:
//! an agent may read/write project files freely, but never tome's own
//! config directory (which would let it tamper with the allowlist or repo
//! consents), and never the auth file specifically (`egress-auth.json`,
//! which holds the TOTP secret) even though that file already lives inside
//! the just-denied config directory — the second, more specific rule guards
//! against the first ever being narrowed or reordered without this one
//! being re-checked on its own.

// Real and tested (see `#[cfg(test)]` below), but no in-slice caller yet —
// the future integrator that shells out to `sandbox-exec` with this
// profile is a different slice's file. Same rationale as
// `pty_authority.rs`'s module-level allow.
#![allow(dead_code)]

use std::path::Path;

/// `sandbox-exec`'s fixed absolute path on every macOS install (Apple ships
/// it outside the user's shell `PATH` by design, so this is not a "resolve
/// from PATH" constant). macOS-only: a Linux/Windows caller has no seatbelt
/// to invoke at all — the plan's Linux enforcement path is bubblewrap, a
/// wholly different mechanism (see `src-tauri/src/egress/mod.rs`'s module
/// doc comment for this crate's Linux story). Exists here purely as a
/// convenience for the future integrator wiring `-p`/`seatbelt_profile`'s
/// output into an actual `sandbox-exec` invocation, so that code does not
/// need to hand-copy this literal from `index.js` a second time.
#[cfg(target_os = "macos")]
pub const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// Builds the exact SBPL profile text `src/main/egress.js`'s
/// `seatbeltProfile(userData)` produces for the same `app_data_dir` —
/// verified byte-for-byte against real Node output (see this module's
/// `#[cfg(test)]`, which pins a fixture captured by actually running the JS
/// function rather than hand-transcribing it).
///
/// `app_data_dir` is Tauri's per-OS app-data directory — the direct
/// counterpart of Electron's `app.getPath('userData')`, which is what the
/// JS original's `userData` parameter always receives at its one real call
/// site in `index.js`. Not validated, canonicalized, or required to exist
/// here — same as the JS original, which does plain string interpolation
/// with no existence check; a caller handing in a relative or nonexistent
/// path gets a profile that says exactly that back, verbatim.
pub fn seatbelt_profile(app_data_dir: &Path) -> String {
    let auth_file = app_data_dir.join("egress-auth.json");
    [
        "(version 1)".to_string(),
        "(allow default)".to_string(),
        "(deny network-outbound)".to_string(),
        "(allow network-outbound (remote ip \"localhost:*\"))".to_string(),
        format!(
            "(deny file-write* (subpath \"{}\"))",
            app_data_dir.display()
        ),
        format!("(deny file-read* (literal \"{}\"))", auth_file.display()),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Captured by actually running `seatbeltProfile('/Users/test/Library/
    /// Application Support/Tome')` from `src/main/egress.js` under real
    /// Node (`node -e "import('./src/main/egress.js').then(m =>
    /// console.log(JSON.stringify(m.seatbeltProfile(...))))"`), not
    /// hand-transcribed — the byte-for-byte cross-language pin this slice's
    /// task brief asks for, applied to the seatbelt profile the same way
    /// `auth_fixtures.json` pins the scrypt/TOTP crypto elsewhere in this
    /// phase.
    #[test]
    fn matches_real_node_output_byte_for_byte() {
        let dir = PathBuf::from("/Users/test/Library/Application Support/Tome");
        let expected = "(version 1)\n\
            (allow default)\n\
            (deny network-outbound)\n\
            (allow network-outbound (remote ip \"localhost:*\"))\n\
            (deny file-write* (subpath \"/Users/test/Library/Application Support/Tome\"))\n\
            (deny file-read* (literal \"/Users/test/Library/Application Support/Tome/egress-auth.json\"))";
        assert_eq!(seatbelt_profile(&dir), expected);
    }

    #[test]
    fn rule_order_is_default_allow_then_deny_then_narrow_back_to_loopback() {
        // SBPL is "later rule wins" — pin the ORDER itself, not just the
        // final joined text, so a future edit that reorders these rules
        // (silently changing what "later wins" means) fails loudly here
        // even if the golden-text fixture above were ever updated to match
        // a bad reorder.
        let profile = seatbelt_profile(&PathBuf::from("/tmp/tome-test"));
        let lines: Vec<&str> = profile.lines().collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "(version 1)");
        assert_eq!(lines[1], "(allow default)");
        assert_eq!(lines[2], "(deny network-outbound)");
        assert_eq!(
            lines[3],
            "(allow network-outbound (remote ip \"localhost:*\"))"
        );
        assert!(lines[4].starts_with("(deny file-write* (subpath "));
        assert!(lines[5].starts_with("(deny file-read* (literal "));
    }

    #[test]
    fn denies_write_to_the_whole_app_data_subtree() {
        let profile = seatbelt_profile(&PathBuf::from("/tmp/tome-test"));
        assert!(profile.contains("(deny file-write* (subpath \"/tmp/tome-test\"))"));
    }

    #[test]
    fn denies_read_of_the_auth_file_specifically_by_literal_not_subpath() {
        // A `literal` match (not `subpath`) is deliberate: it names exactly
        // one file, so this rule can never accidentally widen into denying
        // read of the whole app-data directory — only writes are
        // blanket-denied above; a gapped agent may still read elsewhere in
        // that directory if anything is ever added there.
        let profile = seatbelt_profile(&PathBuf::from("/tmp/tome-test"));
        assert!(profile.contains("(deny file-read* (literal \"/tmp/tome-test/egress-auth.json\"))"));
    }

    #[test]
    fn compiles_and_produces_a_profile_on_every_target_os() {
        // The whole point of keeping this builder OS-unconditional (see the
        // module doc comment): this test itself carries no #[cfg] and must
        // pass on a Linux CI runner too, not just macOS — only
        // `SANDBOX_EXEC_PATH` above is behind `cfg(target_os = "macos")`.
        let profile = seatbelt_profile(&PathBuf::from("/any/path"));
        assert!(profile.starts_with("(version 1)\n"));
        assert!(profile.ends_with("egress-auth.json\"))"));
    }

    #[test]
    fn does_not_touch_the_filesystem_or_require_the_path_to_exist() {
        // Matches the JS original's plain template-literal interpolation —
        // no existence check, no canonicalization.
        let profile = seatbelt_profile(&PathBuf::from("/this/path/does/not/exist/anywhere"));
        assert!(profile.contains("/this/path/does/not/exist/anywhere"));
    }
}
