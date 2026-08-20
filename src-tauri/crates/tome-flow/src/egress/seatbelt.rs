//! macOS seatbelt (`sandbox-exec`) profile builder for gapped agent panes —
//! port of `src/main/egress.js`'s `seatbeltProfile(userData)`, hardened by
//! two pentest findings (F-01, F-03 — see below). Pure string construction
//! only: this module never shells out to `sandbox-exec` itself (that
//! belongs to the integrator, `ipc::pty::pty_create`, mirroring
//! `index.js`'s own `sandbox = process.platform === 'darwin' ? { cmd:
//! '/usr/bin/sandbox-exec', args: ['-p', egress.seatbeltProfile(userData)] }
//! : null`) — so the builder itself compiles, and its `#[cfg(test)]` suite
//! runs, on every OS this crate targets, even though the profile it produces
//! is only ever handed to `sandbox-exec` on macOS.
//!
//! Rule order matters in SBPL — "later rules win" (the JS original's own
//! comment) — so this is default-allow, then a blanket network-outbound
//! deny, then a narrow loopback-only re-allow: a gapped pane's ONLY route
//! out is its own per-pane proxy on `127.0.0.1:<proxy_port>` (the egress's
//! whole value proposition; see TOME-001..021 in git log). Two confinement
//! rules follow: an agent may read/write project files freely, but never
//! tome's own config directory — reads AND writes — which covers the auth
//! file (`egress-auth.json`), the allowlist, repo consents, and the event
//! log in one rule.
//!
//! ## F-01: the loopback carve-out is pinned to the pane's proxy port
//!
//! The Electron original re-allowed `(remote ip "localhost:*")` — every
//! loopback port on the host — which let a gapped pane reach ANY local
//! service directly, bypassing the proxy and its allowlist entirely (the
//! pentest's F-01). The profile now names the pane's own kernel-assigned
//! proxy port instead. The `host:port` form inside the `remote ip` filter
//! (rather than a separate port filter) is load-bearing: seatbelt has no
//! `(remote tcp-port ...)` filter, and a bare IP literal is rejected
//! ("host must be * or localhost in network address") — verified against
//! real `sandbox-exec` on macOS while landing this fix: the
//! `localhost:<port>` profile connects to `127.0.0.1:<port>` and refuses
//! `127.0.0.1:<other-port>` ("Operation not permitted") and every
//! non-loopback address. `localhost` here covers IPv4 loopback, which is
//! all the proxy env vars ever point at (`http://127.0.0.1:<port>`).
//!
//! ## F-03: config-dir reads are denied too, not just writes
//!
//! The original profile write-denied the whole config dir but read-denied
//! only the literal `egress-auth.json`, leaving `egress.json`,
//! `egress-repo-consents.json`, and `events.jsonl` readable from inside
//! the gap (the pentest's F-03). The read rule is now a `subpath` deny of
//! the whole directory, matching the write rule — anything main ever
//! stores there is unreadable by construction.
//!
//! ## Canonical-path caveat (verified live)
//!
//! SBPL `subpath` rules match against the path `sandbox-exec` resolves the
//! operation's target to — which on macOS canonicalizes through symlinks.
//! A directory whose REAL path differs from its spelled path (for example
//! anything under `/tmp`, which symlinks to `/private/tmp`) is NOT covered
//! by a `subpath` rule naming the spelled path — reproduced against real
//! `sandbox-exec` while landing this fix. Tauri's `app_data_dir`
//! (`~/Library/Application Support/<bundle-id>`) is a real path with no
//! symlinked ancestors, so production is unaffected, but a caller that
//! ever handed this builder a symlinked directory would get a profile that
//! silently fails to confine it. Do not canonicalize `app_data_dir` here
//! on macOS; that is the caller's invariant to preserve.

// Real and tested (see `#[cfg(test)]` below), with one real call site
// (`ipc::pty::pty_create` builds the profile AFTER creating the pane's
// proxy, because the profile must name its port — F-01). Same rationale
// as `pty_authority.rs`'s module-level allow.
#![allow(dead_code)]

use std::path::Path;

/// `sandbox-exec`'s fixed absolute path on every macOS install (Apple ships
/// it outside the user's shell `PATH` by design, so this is not a "resolve
/// from PATH" constant). macOS-only: a Linux/Windows caller has no seatbelt
/// to invoke at all — the plan's Linux enforcement path is bubblewrap, a
/// wholly different mechanism (see `src-tauri/src/egress/mod.rs`'s module
/// doc comment for this crate's Linux story). Exists here purely as a
/// convenience for the integrator wiring `-p`/`seatbelt_profile`'s output
/// into an actual `sandbox-exec` invocation, so that code does not need to
/// hand-copy this literal from `index.js` a second time.
#[cfg(target_os = "macos")]
pub const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// Builds the SBPL profile text for a gapped macOS pane. Two deliberate
/// DIVERGENCES from `src/main/egress.js`'s `seatbeltProfile(userData)`,
/// both pentest-driven hardening (F-01/F-03 — see the module doc comment):
///
/// - `proxy_port` (the pane's kernel-assigned loopback proxy port, which
///   the caller creates FIRST so the profile can name it) replaces the
///   original's `localhost:*` blanket: the pane can reach the proxy and
///   nothing else on loopback.
/// - `(deny file-read* (subpath app_data_dir))` replaces the original's
///   read-deny of just `egress-auth.json`: every main-owned file under the
///   config dir (auth, allowlist, consents, event log, store keys) is now
///   unreadable AND unwritable from inside the gap.
///
/// A third hardening, Docker-socket denial, is NEW in this port (not a
/// divergence from the JS original, which had no such rule): the profile
/// read- AND write-denies the known Docker socket paths by literal —
/// `~/.docker/run/docker.sock` and `~/.docker/desktop/docker.sock` for
/// Docker Desktop, plus the legacy `/var/run/docker.sock` and its
/// macOS-canonical `/private/var/run/docker.sock` form. These close the
/// container-runtime escape: the blanket `(deny network-outbound)` does
/// NOT cover them (seatbelt's network filters match TCP/UDP sockets, not
/// AF_UNIX paths), so without them a gapped pane could reach the Docker
/// daemon and break out of the sandbox by spawning a privileged container
/// on the host.
///
/// `app_data_dir` is Tauri's per-OS app-data directory — the direct
/// counterpart of Electron's `app.getPath('userData')`, which is what the
/// JS original's `userData` parameter always receives at its one real call
/// site in `index.js`. Not validated, canonicalized, or required to exist
/// here — same as the JS original, which does plain string interpolation
/// with no existence check; a caller handing in a relative or nonexistent
/// path gets a profile that says exactly that back, verbatim.
///
/// `home_dir` is the pane user's home directory, used only to name the two
/// Docker Desktop socket paths; the `/var/run` paths are absolute and
/// independent of it. Carries the same "not validated or required to
/// exist" contract as `app_data_dir`.
pub fn seatbelt_profile(app_data_dir: &Path, proxy_port: u16, home_dir: &Path) -> String {
    let docker_sockets = [
        home_dir.join(".docker/run/docker.sock"),
        home_dir.join(".docker/desktop/docker.sock"),
        std::path::PathBuf::from("/var/run/docker.sock"),
        std::path::PathBuf::from("/private/var/run/docker.sock"),
    ];
    let mut lines = vec![
        "(version 1)".to_string(),
        "(allow default)".to_string(),
        "(deny network-outbound)".to_string(),
        format!("(allow network-outbound (remote ip \"localhost:{proxy_port}\"))"),
        format!(
            "(deny file-write* (subpath \"{}\"))",
            app_data_dir.display()
        ),
        format!("(deny file-read* (subpath \"{}\"))", app_data_dir.display()),
    ];
    for sock in &docker_sockets {
        lines.push(format!(
            "(deny file-read* (literal \"{}\"))",
            sock.display()
        ));
        lines.push(format!(
            "(deny file-write* (literal \"{}\"))",
            sock.display()
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Pins the NEW hardened profile text (F-01/F-03 — deliberate
    /// divergence from the Node original, which the module doc comment
    /// explains; the `localhost:<port>` loopback form was verified against
    /// a real `sandbox-exec` binary before landing, see the module doc).
    #[test]
    fn profile_names_the_pane_proxy_port_and_denies_config_dir_reads_and_writes() {
        let dir = PathBuf::from("/Users/test/Library/Application Support/Tome");
        let home = PathBuf::from("/Users/test");
        let expected = "(version 1)\n\
            (allow default)\n\
            (deny network-outbound)\n\
            (allow network-outbound (remote ip \"localhost:54321\"))\n\
            (deny file-write* (subpath \"/Users/test/Library/Application Support/Tome\"))\n\
            (deny file-read* (subpath \"/Users/test/Library/Application Support/Tome\"))\n\
            (deny file-read* (literal \"/Users/test/.docker/run/docker.sock\"))\n\
            (deny file-write* (literal \"/Users/test/.docker/run/docker.sock\"))\n\
            (deny file-read* (literal \"/Users/test/.docker/desktop/docker.sock\"))\n\
            (deny file-write* (literal \"/Users/test/.docker/desktop/docker.sock\"))\n\
            (deny file-read* (literal \"/var/run/docker.sock\"))\n\
            (deny file-write* (literal \"/var/run/docker.sock\"))\n\
            (deny file-read* (literal \"/private/var/run/docker.sock\"))\n\
            (deny file-write* (literal \"/private/var/run/docker.sock\"))";
        assert_eq!(seatbelt_profile(&dir, 54321, &home), expected);
    }

    #[test]
    fn rule_order_is_default_allow_then_deny_then_narrow_back_to_the_proxy_port() {
        // SBPL is "later rule wins" — pin the ORDER itself, not just the
        // final joined text, so a future edit that reorders these rules
        // (silently changing what "later wins" means) fails loudly here
        // even if the golden-text fixture above were ever updated to match
        // a bad reorder.
        let profile = seatbelt_profile(
            &PathBuf::from("/tmp/tome-test"),
            8443,
            &PathBuf::from("/Users/test"),
        );
        let lines: Vec<&str> = profile.lines().collect();
        assert_eq!(lines.len(), 14);
        assert_eq!(lines[0], "(version 1)");
        assert_eq!(lines[1], "(allow default)");
        assert_eq!(lines[2], "(deny network-outbound)");
        assert_eq!(
            lines[3],
            "(allow network-outbound (remote ip \"localhost:8443\"))"
        );
        assert!(lines[4].starts_with("(deny file-write* (subpath "));
        assert!(lines[5].starts_with("(deny file-read* (subpath "));
        // The eight Docker-socket denies follow, read-then-write for each
        // of the four known socket paths, in the fixed order the builder
        // emits them (see the `docker_sockets` array).
        assert_eq!(
            lines[6],
            "(deny file-read* (literal \"/Users/test/.docker/run/docker.sock\"))"
        );
        assert_eq!(
            lines[7],
            "(deny file-write* (literal \"/Users/test/.docker/run/docker.sock\"))"
        );
        assert_eq!(
            lines[8],
            "(deny file-read* (literal \"/Users/test/.docker/desktop/docker.sock\"))"
        );
        assert_eq!(
            lines[9],
            "(deny file-write* (literal \"/Users/test/.docker/desktop/docker.sock\"))"
        );
        assert_eq!(
            lines[10],
            "(deny file-read* (literal \"/var/run/docker.sock\"))"
        );
        assert_eq!(
            lines[11],
            "(deny file-write* (literal \"/var/run/docker.sock\"))"
        );
        assert_eq!(
            lines[12],
            "(deny file-read* (literal \"/private/var/run/docker.sock\"))"
        );
        assert_eq!(
            lines[13],
            "(deny file-write* (literal \"/private/var/run/docker.sock\"))"
        );
    }

    #[test]
    fn denies_write_to_the_whole_app_data_subtree() {
        let profile = seatbelt_profile(
            &PathBuf::from("/tmp/tome-test"),
            1,
            &PathBuf::from("/Users/test"),
        );
        assert!(profile.contains("(deny file-write* (subpath \"/tmp/tome-test\"))"));
    }

    #[test]
    fn denies_read_of_the_whole_app_data_subtree_not_just_the_auth_file() {
        // F-03: the previous profile read-denied only egress-auth.json
        // (by literal), leaving egress.json / egress-repo-consents.json /
        // events.jsonl readable from inside the gap. The subpath read-deny
        // now covers every main-owned file under the config dir in one
        // rule — a store key, a chat transcript, or a future file added
        // there is denied by construction rather than by being remembered.
        let profile = seatbelt_profile(
            &PathBuf::from("/tmp/tome-test"),
            1,
            &PathBuf::from("/Users/test"),
        );
        assert!(profile.contains("(deny file-read* (subpath \"/tmp/tome-test\"))"));
    }

    #[test]
    fn the_loopback_allow_names_exactly_the_given_proxy_port() {
        // F-01: `localhost:*` let a gapped pane reach every local service
        // on the host directly. The profile must now name ONLY the pane's
        // own proxy port, so the proxy (and its allowlist) is the sole
        // route out even on loopback.
        let profile = seatbelt_profile(&PathBuf::from("/x"), 4242, &PathBuf::from("/Users/test"));
        assert!(profile.contains("(remote ip \"localhost:4242\")"));
        assert!(!profile.contains("localhost:*"));
    }

    #[test]
    fn denies_the_docker_socket_by_literal_read_and_write() {
        // The container-runtime escape: a unix socket path the
        // `(deny network-outbound)` rule does not cover (seatbelt network
        // filters match TCP/UDP, not AF_UNIX). Pin that the Docker Desktop
        // socket under the pane's home dir is denied for BOTH reads and
        // writes, so a gapped pane cannot reach the Docker daemon to spawn
        // a privileged host container.
        let home = PathBuf::from("/Users/test");
        let profile = seatbelt_profile(&PathBuf::from("/tmp/tome-test"), 1, &home);
        assert!(
            profile.contains("(deny file-read* (literal \"/Users/test/.docker/run/docker.sock\"))")
        );
        assert!(profile
            .contains("(deny file-write* (literal \"/Users/test/.docker/run/docker.sock\"))"));
    }

    #[test]
    fn compiles_and_produces_a_profile_on_every_target_os() {
        // The whole point of keeping this builder OS-unconditional (see the
        // module doc comment): this test itself carries no #[cfg] and must
        // pass on a Linux CI runner too, not just macOS — only
        // `SANDBOX_EXEC_PATH` above is behind `cfg(target_os = "macos")`.
        let profile = seatbelt_profile(
            &PathBuf::from("/any/path"),
            1,
            &PathBuf::from("/Users/test"),
        );
        assert!(profile.starts_with("(version 1)\n"));
        assert!(profile.ends_with("(deny file-write* (literal \"/private/var/run/docker.sock\"))"));
    }

    #[test]
    fn does_not_touch_the_filesystem_or_require_the_path_to_exist() {
        // Matches the JS original's plain template-literal interpolation —
        // no existence check, no canonicalization.
        let profile = seatbelt_profile(
            &PathBuf::from("/this/path/does/not/exist/anywhere"),
            1,
            &PathBuf::from("/Users/test"),
        );
        assert!(profile.contains("/this/path/does/not/exist/anywhere"));
    }
}
