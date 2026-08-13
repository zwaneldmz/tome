//! Login-shell environment harvest — ports `ensureLoginEnv` from
//! `src/main/index.js` (defined ~161-217, consumed by `resolveAgentSecrets`
//! ~245-250; `AGENT_SECRET_KEYS` itself is index.js:226-244).
//!
//! Two problems, one shell-out: apps launched from Finder/Spotlight (or any
//! non-terminal launcher) inherit launchd's bare PATH, and a non-interactive
//! login shell never sources `.zshrc` — where PATH additions like
//! `~/.local/bin` (where `claude` and friends often live) and provider
//! credential exports usually live. `ensureLoginEnv` runs `$SHELL -ilc`
//! once, parses the user's real interactive PATH out of it, merges in a
//! fallback list of well-known agent-CLI install prefixes, and separately
//! harvests a fixed allowlist of provider secret env vars — so agent panes
//! spawned later can both find their binary and authenticate.
//!
//! Computed once per process and cached in a `tokio::sync::OnceCell` — NOT
//! `AppState` (this phase's brief is explicit: cache here, not there). Every
//! caller wants the exact same value and there's no lock-gate/relock
//! relationship to anything else `AppState` holds, so a module-level cell is
//! simpler than threading a state field through. Call [`login_env`] to get
//! it; the first caller pays the two shell-outs, every later caller
//! (`OnceCell::get_or_init` serializes concurrent first-callers onto the
//! same in-flight future) gets the cached result immediately.
//!
//! DELIBERATE DEVIATION from the JS original: `ensureLoginEnv` mutates
//! `process.env.PATH` in place as a side effect and returns only `{
//! secrets }`. [`login_env`] instead returns BOTH the resolved PATH and the
//! secrets map as plain data on [`LoginEnv`] — `std::env::set_var` mutating
//! process-global state from an async task is exactly the action-at-a-
//! distance the Rust port should avoid when the alternative (callers read
//! `login_env().path` and build their own `Command`'s env explicitly, the
//! way `pty.rs`'s terminal/agent spawn paths need to anyway) is no harder
//! and doesn't leak a global mutation into every future test that happens
//! to touch env vars.
//!
//! INTEGRATION NOTE: this file originally carried a provisional
//! `AGENT_SECRET_KEYS` stand-in (see git history) because `agent_env.rs`
//! — the binding decision's named home for that constant — was still
//! landing in parallel. It has since landed; `compute` below now pulls
//! [`crate::agent_env::AGENT_SECRET_KEYS`] directly, and the provisional
//! copy is gone so there is exactly one Rust-side list of which env vars a
//! login shell's full `env` dump may leak into an agent pane.

// Every function below is exercised by its own #[cfg(test)] fixture, but in
// a plain (non-test, non-`--ignored`) build nothing calls `login_env()` yet:
// the real consumer (`pty.rs`'s terminal/agent spawn paths, per this
// phase's brief) is a different slice landing in parallel and may not be in
// the tree yet when this file's own `cargo check`/`cargo test` gates run.
// One module-level allow here, same rationale (and same shape) as
// `confine.rs`'s — see that module's top doc comment — rather than
// scattering `#[allow(dead_code)]` over every item.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use tokio::sync::OnceCell;

use crate::agent_env::AGENT_SECRET_KEYS;

/// Same 8s budget as the JS original's `execFileAsync(..., { timeout: 8000
/// })`, for both the PATH and `env` shell-outs.
const HARVEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Resolved login-shell environment: the merged PATH and the harvested
/// provider secrets. See the module doc comment for why this differs from
/// the JS original's `process.env.PATH` mutation + `{ secrets }` return.
///
/// `Debug` is hand-implemented, not derived: `secrets` holds raw provider
/// API keys, and a derived impl would print every value verbatim the first
/// time some future caller logs a `LoginEnv` for debugging (`{:?}` on a
/// command error path, a `tracing::debug!`, …). See [`LoginEnv`]'s manual
/// `impl Debug` below — it shows which keys were found, never their values.
#[derive(Clone)]
pub struct LoginEnv {
    /// The shell binary used for the harvest (`$SHELL`, or the platform
    /// default — see [`resolve_shell`]).
    pub shell: String,
    /// PATH after merging the login shell's resolved PATH (if the harvest
    /// succeeded; otherwise the process's own inherited PATH) with the
    /// fallback additions, minus whichever are already present.
    pub path: String,
    /// Provider credentials pulled from the login shell's `env` output,
    /// filtered to [`AGENT_SECRET_KEYS`]. Empty (not missing) if the
    /// harvest failed or timed out — same as the JS original, which leaves
    /// `secrets` as `{}` when `envRes` doesn't fulfill.
    pub secrets: HashMap<String, String>,
}

impl std::fmt::Debug for LoginEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut keys: Vec<&str> = self.secrets.keys().map(String::as_str).collect();
        keys.sort_unstable();
        f.debug_struct("LoginEnv")
            .field("shell", &self.shell)
            .field("path", &self.path)
            .field("secrets_present_for", &keys)
            .finish()
    }
}

static LOGIN_ENV: OnceCell<LoginEnv> = OnceCell::const_new();

/// Returns the cached [`LoginEnv`], computing it on the first call. Mirrors
/// the JS original's `loginEnvPromise` in-flight/cached promise, minus the
/// need to guard re-entrancy by hand — `OnceCell::get_or_init` already
/// serializes concurrent first-callers onto the same in-flight future.
pub async fn login_env() -> &'static LoginEnv {
    LOGIN_ENV.get_or_init(compute).await
}

async fn compute() -> LoginEnv {
    let shell = resolve_shell();
    let (path_out, env_out) = tokio::join!(
        run_shell(&shell, "echo -n \"$PATH\"", HARVEST_TIMEOUT),
        run_shell(&shell, "env", HARVEST_TIMEOUT),
    );

    let inherited = std::env::var("PATH").unwrap_or_default();
    let base_path = resolve_base_path(path_out.as_deref(), &inherited);
    let path = merge_path(&base_path, &fallback_path_extras());
    let secrets = resolve_secrets(env_out.as_deref(), AGENT_SECRET_KEYS);

    LoginEnv { shell, path, secrets }
}

/// Runs `<shell> -ilc <script>`, returning stdout (utf8, lossy-decoded) iff
/// the process starts, exits zero, and finishes within `timeout` — spawn
/// failure, non-zero exit, and timeout all collapse to `None`, matching the
/// JS original treating all three as a rejected `Promise.allSettled` entry
/// (dropped, not retried, not surfaced as an error to the caller).
///
/// `kill_on_drop(true)` is what makes the timeout case actually kill the
/// child rather than orphan it: `timeout()` dropping the
/// `wait_with_output()` future drops the `Child` moved inside it, and
/// `kill_on_drop` turns that drop into a kill. Node's `execFile` does the
/// analogous SIGTERM-on-timeout by default; a bare `tokio::process::Child`
/// has no such default, so it's opted in here explicitly.
async fn run_shell(shell: &str, script: &str, timeout: Duration) -> Option<String> {
    let child = tokio::process::Command::new(shell)
        .arg("-ilc")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        _ => None,
    }
}

// ---- default shell resolution ----

/// `process.env.SHELL || '/bin/zsh'`'s Rust equivalent — but per-platform.
/// The JS original hardcodes `/bin/zsh` as its unconditional fallback (it
/// predates this rewrite's Linux target; see the module doc comment), which
/// would spawn a binary absent on most Linux distros. Here: an unset-or-
/// empty `$SHELL` (checking emptiness too, matching JS's `||`, which treats
/// `""` as falsy the same as unset) falls back to `/bin/zsh` on macOS, and
/// on any other platform to `/bin/bash` if present, else `/bin/sh` (POSIX-
/// guaranteed, unlike bash).
///
/// DIVERGENCE FROM JS, called out per this phase's brief: intentional — a
/// target this phase newly claims (Linux) had no representation in the
/// Electron-only original, which only ever ran on macOS.
pub fn resolve_shell() -> String {
    if let Ok(s) = std::env::var("SHELL") {
        if !s.is_empty() {
            return s;
        }
    }
    default_shell_for_platform(std::env::consts::OS, Path::new("/bin/bash").exists()).to_string()
}

/// Pure decision core of [`resolve_shell`], fixture-testable without
/// touching the filesystem or env vars.
fn default_shell_for_platform(os: &str, bash_exists: bool) -> &'static str {
    if os == "macos" {
        "/bin/zsh"
    } else if bash_exists {
        "/bin/bash"
    } else {
        "/bin/sh"
    }
}

// ---- PATH line extraction ----

/// Ports the `pathRes.status === 'fulfilled'` branch's line-picking logic
/// (`index.js` ~180-186): split stdout on `\n`, strip ANSI CSI sequences
/// and trim each line, keep only lines containing `/usr/bin`, take the
/// LAST match. A login shell can print MOTD/rc-file banner noise on stdout
/// before the `echo -n "$PATH"` output lands — filtering + taking the last
/// hit is the original's defense against that, ported as-is rather than
/// "improved": a banner line that itself happens to contain the literal
/// substring `/usr/bin` after the real PATH line would still misfire in
/// both versions identically.
pub fn extract_path_line(stdout: &str) -> Option<String> {
    stdout
        .split('\n')
        .map(|l| strip_ansi_csi(l).trim().to_string())
        .rfind(|l| l.contains("/usr/bin"))
}

/// The base-PATH selection this file adds beyond a direct JS port: prefer
/// the harvested-and-extracted PATH line; if the shell-out failed/timed
/// out (`harvested` is `None`) or its output had no line containing
/// `/usr/bin` ([`extract_path_line`] returns `None`), fall back to
/// `inherited` — the process's own PATH at launch. Mirrors the JS
/// original's net effect (`if (line) process.env.PATH = line`, otherwise
/// `process.env.PATH` is simply never reassigned and keeps its prior
/// value) without the in-place mutation.
fn resolve_base_path(harvested: Option<&str>, inherited: &str) -> String {
    harvested
        .and_then(extract_path_line)
        .unwrap_or_else(|| inherited.to_string())
}

/// Strips ANSI CSI sequences (`ESC [ <0-9;>* <final-byte>`), matching the
/// JS original's `l.replace(/\x1b\[[0-9;]*[A-Za-z]/g, '')`. Hand-rolled
/// rather than pulling in the `regex` crate: this phase's Cargo.toml is
/// off-limits to every slice but the PTY one (see the phase brief), and a
/// single fixed pattern over short lines doesn't need a regex engine. A
/// malformed/truncated escape (no final byte before the string ends) is
/// left in place character-by-character, the same net effect as the regex
/// simply not matching it.
fn strip_ansi_csi(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'[') {
            let mut j = i + 2;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == ';') {
                j += 1;
            }
            if j < chars.len() && chars[j].is_ascii_alphabetic() {
                i = j + 1; // consumed the whole ESC [ ... <letter> sequence
                continue;
            }
            // no final byte found — fall through and copy the ESC character
            // itself; the next iteration re-examines '[' as an ordinary char.
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// ---- PATH merge ----

/// The four fallback PATH entries the JS original always appends
/// (`index.js` ~189-196), gated per platform: `/opt/homebrew/bin` is Apple
/// Silicon Homebrew's prefix and is meaningless on Linux, so it's
/// `cfg(target_os = "macos")`-only here — the JS original had no such gate
/// because it only ever ran on macOS. The other three are cross-platform
/// (`~/.local/bin` and `~/.opencode/bin` are user-local install prefixes on
/// any OS; `/usr/local/bin` is a standard locally-installed-software path
/// on Linux too, not just macOS).
///
/// NOTE: Linuxbrew's prefix (`/home/linuxbrew/.linuxbrew/bin`) is
/// deliberately NOT added here — out of scope for this binding decision,
/// which names exactly these four JS entries. Flag for a future addition
/// if agent-CLI-via-Linuxbrew turns out to be a real install path users hit.
fn fallback_path_extras() -> Vec<String> {
    let home = std::env::home_dir().unwrap_or_default();
    let mut extras = vec![
        home.join(".local/bin").to_string_lossy().into_owned(),
        home.join(".opencode/bin").to_string_lossy().into_owned(),
    ];
    #[cfg(target_os = "macos")]
    extras.push("/opt/homebrew/bin".to_string());
    extras.push("/usr/local/bin".to_string());
    extras
}

/// Ports `index.js`'s PATH-merge tail (~189-196): split `current` on `:`,
/// append every `extras` entry not already present, rejoin on `:`. Matches
/// JS's `Array.prototype.includes` membership check (exact string equality
/// per segment, no path normalization) and preserves `extras`' order for
/// whichever entries get appended.
pub fn merge_path(current: &str, extras: &[String]) -> String {
    let cur: Vec<&str> = if current.is_empty() {
        Vec::new()
    } else {
        current.split(':').collect()
    };
    let mut out: Vec<String> = cur.iter().map(|s| s.to_string()).collect();
    for e in extras {
        if !cur.contains(&e.as_str()) {
            out.push(e.clone());
        }
    }
    out.join(":")
}

// ---- secret harvest ----

/// Ports `index.js`'s `env`-output line-parsing loop (~199-209): split on
/// `\n`, split each line on the FIRST `=` (so a value that itself contains
/// `=` — base64, a JSON blob, a JWT — survives intact), keep only lines
/// whose key is non-empty (JS's `i < 1` guard rejects both "no `=` at all"
/// — `indexOf` returns `-1` — and "`=` is the very first character") and
/// non-empty-valued, and whose key is in `secret_keys`.
///
/// NOT fixed here, ported as-is: a login-shell environment variable whose
/// VALUE contains an embedded literal newline (rare, but legal — some
/// multi-line credentials/config blobs do this) gets split across two
/// "lines" by this function exactly as it is by the JS original's
/// `stdout.split('\n')`, silently truncating the value at the first
/// newline. Both versions share the limitation; porting it faithfully
/// rather than silently fixing it matches this phase's brief (vitest
/// suites are the spec for ported behavior — there is no vitest suite for
/// this function specifically, since `ensureLoginEnv` was never extracted
/// to a testable pure module in JS, but "match the original's behavior"
/// still applies).
pub fn harvest_secrets(stdout: &str, secret_keys: &[&str]) -> HashMap<String, String> {
    let mut secrets = HashMap::new();
    for line in stdout.split('\n') {
        let Some(i) = line.find('=') else { continue };
        if i < 1 {
            continue;
        }
        let key = &line[..i];
        let val = &line[i + 1..];
        if !val.is_empty() && secret_keys.contains(&key) {
            secrets.insert(key.to_string(), val.to_string());
        }
    }
    secrets
}

/// `envRes.status === 'fulfilled'` gate around the harvest loop above: `None`
/// (shell-out failed or timed out) collapses to an empty map, same as the
/// JS original leaving `secrets` at its `{}` initial value.
fn resolve_secrets(harvested: Option<&str>, secret_keys: &[&str]) -> HashMap<String, String> {
    harvested
        .map(|out| harvest_secrets(out, secret_keys))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- strip_ansi_csi ----

    #[test]
    fn strip_ansi_csi_leaves_plain_text_untouched() {
        assert_eq!(strip_ansi_csi("plain text"), "plain text");
    }

    #[test]
    fn strip_ansi_csi_removes_sgr_color_codes() {
        assert_eq!(strip_ansi_csi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(
            strip_ansi_csi("\x1b[1;32mgreen bold\x1b[0m tail"),
            "green bold tail"
        );
    }

    #[test]
    fn strip_ansi_csi_handles_a_sequence_with_no_digits() {
        assert_eq!(strip_ansi_csi("\x1b[Kclear"), "clear");
    }

    #[test]
    fn strip_ansi_csi_leaves_a_truncated_sequence_alone() {
        // No final byte before the string ends — the regex wouldn't match
        // either, so nothing should be dropped.
        assert_eq!(strip_ansi_csi("\x1b["), "\x1b[");
        assert_eq!(strip_ansi_csi("abc\x1b[12"), "abc\x1b[12");
    }

    // ---- extract_path_line ----

    #[test]
    fn extract_path_line_returns_a_single_line_verbatim() {
        assert_eq!(
            extract_path_line("/usr/bin:/bin:/usr/local/bin"),
            Some("/usr/bin:/bin:/usr/local/bin".to_string())
        );
    }

    #[test]
    fn extract_path_line_skips_leading_motd_noise() {
        let out = "Last login: Tue Jan  1\nWelcome to the machine!\n/usr/bin:/bin";
        assert_eq!(extract_path_line(out), Some("/usr/bin:/bin".to_string()));
    }

    #[test]
    fn extract_path_line_strips_ansi_before_matching() {
        let out = "\x1b[32m/usr/bin:/bin\x1b[0m";
        assert_eq!(extract_path_line(out), Some("/usr/bin:/bin".to_string()));
    }

    #[test]
    fn extract_path_line_returns_none_when_nothing_matches() {
        assert_eq!(extract_path_line("no path here\nnothing useful"), None);
        assert_eq!(extract_path_line(""), None);
    }

    #[test]
    fn extract_path_line_takes_the_last_match_not_the_first() {
        let out = "/usr/bin:/bin\nsome noise\n/usr/bin:/bin:/opt/extra";
        assert_eq!(
            extract_path_line(out),
            Some("/usr/bin:/bin:/opt/extra".to_string())
        );
    }

    // ---- resolve_base_path ----

    #[test]
    fn resolve_base_path_prefers_the_harvested_line() {
        assert_eq!(
            resolve_base_path(Some("/usr/bin:/bin"), "/inherited/path"),
            "/usr/bin:/bin"
        );
    }

    #[test]
    fn resolve_base_path_falls_back_when_harvest_failed() {
        assert_eq!(resolve_base_path(None, "/inherited/path"), "/inherited/path");
    }

    #[test]
    fn resolve_base_path_falls_back_when_harvest_had_no_matching_line() {
        assert_eq!(
            resolve_base_path(Some("nothing useful here"), "/inherited/path"),
            "/inherited/path"
        );
    }

    // ---- merge_path ----

    #[test]
    fn merge_path_appends_all_extras_to_an_empty_current() {
        let extras = vec!["/a/bin".to_string(), "/b/bin".to_string()];
        assert_eq!(merge_path("", &extras), "/a/bin:/b/bin");
    }

    #[test]
    fn merge_path_does_not_duplicate_an_extra_already_present() {
        let extras = vec!["/usr/local/bin".to_string(), "/new/bin".to_string()];
        assert_eq!(
            merge_path("/usr/bin:/usr/local/bin", &extras),
            "/usr/bin:/usr/local/bin:/new/bin"
        );
    }

    #[test]
    fn merge_path_preserves_current_order_then_extras_order() {
        let extras = vec!["/e1".to_string(), "/e2".to_string(), "/e3".to_string()];
        assert_eq!(merge_path("/c1:/c2", &extras), "/c1:/c2:/e1:/e2:/e3");
    }

    #[test]
    fn merge_path_with_no_extras_returns_current_unchanged() {
        assert_eq!(merge_path("/c1:/c2", &[]), "/c1:/c2");
    }

    // ---- fallback_path_extras ----

    #[test]
    fn fallback_path_extras_includes_cross_platform_entries_and_gates_homebrew() {
        let extras = fallback_path_extras();
        let home = std::env::home_dir().unwrap_or_default();
        assert!(extras.contains(&home.join(".local/bin").to_string_lossy().into_owned()));
        assert!(extras.contains(&home.join(".opencode/bin").to_string_lossy().into_owned()));
        assert!(extras.contains(&"/usr/local/bin".to_string()));
        let has_homebrew = extras.contains(&"/opt/homebrew/bin".to_string());
        assert_eq!(has_homebrew, cfg!(target_os = "macos"));
    }

    // ---- harvest_secrets ----

    const FIXTURE_KEYS: &[&str] = &["FOO_API_KEY", "BAR_TOKEN"];

    #[test]
    fn harvest_secrets_keeps_only_allowlisted_keys() {
        let env = "FOO_API_KEY=abc123\nGITHUB_TOKEN=leaked\nBAR_TOKEN=xyz789\nPATH=/bin";
        let got = harvest_secrets(env, FIXTURE_KEYS);
        assert_eq!(got.len(), 2);
        assert_eq!(got.get("FOO_API_KEY"), Some(&"abc123".to_string()));
        assert_eq!(got.get("BAR_TOKEN"), Some(&"xyz789".to_string()));
        assert!(!got.contains_key("GITHUB_TOKEN"));
        assert!(!got.contains_key("PATH"));
    }

    #[test]
    fn harvest_secrets_drops_empty_values() {
        let env = "FOO_API_KEY=\nBAR_TOKEN=present";
        let got = harvest_secrets(env, FIXTURE_KEYS);
        assert_eq!(got.len(), 1);
        assert!(!got.contains_key("FOO_API_KEY"));
        assert_eq!(got.get("BAR_TOKEN"), Some(&"present".to_string()));
    }

    #[test]
    fn harvest_secrets_splits_on_the_first_equals_only() {
        // A value that itself contains '=' (base64/JWT-shaped) must survive
        // intact — only the FIRST '=' delimits key from value.
        let env = "FOO_API_KEY=sk-abc=def==";
        let got = harvest_secrets(env, FIXTURE_KEYS);
        assert_eq!(got.get("FOO_API_KEY"), Some(&"sk-abc=def==".to_string()));
    }

    #[test]
    fn harvest_secrets_skips_lines_with_no_equals_or_a_leading_equals() {
        let env = "JUST_A_WORD\n=oops\nFOO_API_KEY=ok";
        let got = harvest_secrets(env, FIXTURE_KEYS);
        assert_eq!(got.len(), 1);
        assert_eq!(got.get("FOO_API_KEY"), Some(&"ok".to_string()));
    }

    #[test]
    fn harvest_secrets_of_empty_input_is_empty() {
        assert!(harvest_secrets("", FIXTURE_KEYS).is_empty());
    }

    // ---- resolve_secrets ----

    #[test]
    fn resolve_secrets_is_empty_when_harvest_failed() {
        assert!(resolve_secrets(None, FIXTURE_KEYS).is_empty());
    }

    #[test]
    fn resolve_secrets_delegates_to_harvest_secrets_when_present() {
        let got = resolve_secrets(Some("FOO_API_KEY=v"), FIXTURE_KEYS);
        assert_eq!(got.get("FOO_API_KEY"), Some(&"v".to_string()));
    }

    // ---- AGENT_SECRET_KEYS exact-membership coverage lives in
    // agent_env.rs now (its canonical home — see the module doc comment's
    // "INTEGRATION NOTE"); `harvest_secrets_keeps_only_allowlisted_keys`
    // above already exercises this file's own use of whatever list it's
    // given, against a local fixture, so nothing is lost by not
    // duplicating that assertion here too.

    // ---- default_shell_for_platform ----

    #[test]
    fn default_shell_for_platform_is_always_zsh_on_macos() {
        assert_eq!(default_shell_for_platform("macos", true), "/bin/zsh");
        assert_eq!(default_shell_for_platform("macos", false), "/bin/zsh");
    }

    #[test]
    fn default_shell_for_platform_prefers_bash_on_linux_when_present() {
        assert_eq!(default_shell_for_platform("linux", true), "/bin/bash");
    }

    #[test]
    fn default_shell_for_platform_falls_back_to_sh_on_linux_without_bash() {
        assert_eq!(default_shell_for_platform("linux", false), "/bin/sh");
    }

    #[test]
    fn default_shell_for_platform_treats_other_unix_like_non_macos() {
        assert_eq!(default_shell_for_platform("freebsd", true), "/bin/bash");
        assert_eq!(default_shell_for_platform("freebsd", false), "/bin/sh");
    }

    // ---- LoginEnv's redacting Debug impl ----

    #[test]
    fn login_env_debug_never_prints_secret_values() {
        let mut secrets = HashMap::new();
        secrets.insert(
            "ANTHROPIC_API_KEY".to_string(),
            "sk-ant-super-secret-value".to_string(),
        );
        secrets.insert("OPENAI_API_KEY".to_string(), "sk-openai-also-secret".to_string());
        let env = LoginEnv {
            shell: "/bin/zsh".to_string(),
            path: "/usr/bin:/bin".to_string(),
            secrets,
        };
        let debug = format!("{env:?}");
        assert!(!debug.contains("sk-ant-super-secret-value"));
        assert!(!debug.contains("sk-openai-also-secret"));
        // the keys ARE useful for debugging ("which providers were found")
        // and are not secret, so they're expected to show up.
        assert!(debug.contains("ANTHROPIC_API_KEY"));
        assert!(debug.contains("OPENAI_API_KEY"));
    }

    // ---- smoke test: real shell-out, opt-in only ----

    #[tokio::test]
    #[ignore = "shells out to $SHELL -ilc against the real environment; run explicitly with `cargo test -- --ignored` to sanity check"]
    async fn smoke_login_env_against_real_shell() {
        let env = login_env().await;
        assert!(!env.shell.is_empty());
        assert!(
            !env.path.is_empty(),
            "even a harvest failure should fall back to the inherited PATH"
        );
    }
}
