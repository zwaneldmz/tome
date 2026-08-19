//! `tome-shim`'s command-line contract — hand-parsed (no `clap`: this
//! binary's entire argv is built by ONE trusted caller, the host process,
//! never typed by a human, so a rich CLI-parsing crate buys nothing a
//! 40-line loop doesn't already give us) and, critically, OS-agnostic: no
//! `nix`/`libc` types appear anywhere in this file, only `std` primitives
//! and `PathBuf`/`String`. That is what lets [`parse_args`] carry its own
//! `#[cfg(test)]` suite that runs on every host this workspace builds on
//! (including the macOS box this slice was authored on) — see the crate's
//! top-level design note (in `main.rs`) on the policy/mechanism split this
//! is one half of.
//!
//! Wire contract (mirrors the `bwrap ... -- tome-shim --port P --sock
//! /run/tome/proxy.sock -- zsh -l -c '<agent>'` invocation the plan's
//! "Linux sandbox" section specifies):
//!
//! ```text
//! tome-shim --port <u16> --sock <path> [--self-unshare] [--new-session]
//!           [--deny-write <path>] [--deny-read <path>] -- <argv...>
//! ```
//!
//! `--port`/`--sock` are always required (the loopback-bridge contract has
//! no meaningful default for either). `--self-unshare` is the
//! fallback-ladder's step-2 opt-in (see `linux.rs`'s `self_unshare`): its
//! ABSENCE means "assume bwrap already unshared the user+net namespaces
//! before exec'ing me," not "don't sandbox at all" — there is no flag that
//! disables sandboxing; the caller either invokes this binary with a
//! namespace already prepared (bwrap) or asks this binary to prepare one
//! itself (`--self-unshare`). `--new-session`/`--deny-write`/`--deny-read`
//! are the OTHER rung-2 flags a real invocation carries (see
//! `egress::linux::build_self_unshare_argv`, the sibling-crate builder that
//! emits them) — all three are optional and independent of `--self-unshare`
//! itself (this parser does not require any particular combination), though
//! in practice the only real caller only ever emits them alongside
//! `--self-unshare`. Everything after the first bare `--` is the command to
//! exec, captured completely verbatim — including any token that happens to
//! look like one of this parser's own flags (see [`parse_args`]'s doc
//! comment).
//!
//! **Cross-crate contract**: this wire shape is produced by a DIFFERENT
//! crate (`egress::linux::build_bwrap_argv`/`build_self_unshare_argv`, in
//! the main `tome` package) than the one that parses it (this file). The
//! two were once allowed to drift — `build_self_unshare_argv` emitted
//! `--deny-write`/`--deny-read` for a long stretch before this parser had
//! any arm for them, which made every rung-2 spawn crash with
//! `UnknownFlag` before `linux::run` ever ran. See the
//! `tome_shim_args_parses_the_real_build_self_unshare_argv_output*` and
//! `tome_shim_args_parses_the_embedded_shim_invocation_inside_build_bwrap_argv`
//! tests in `egress::linux`'s own test suite (main crate, a
//! `[dev-dependencies]` path dependency on this package's `[lib]` target —
//! see this crate's `Cargo.toml`) — they feed THIS module's real
//! [`parse_args`] the REAL output of both argv builders, specifically so
//! this class of drift fails a build instead of shipping silently again.
//!
//! [`parse_args`]'s only real (non-test) caller is `main.rs`'s
//! `#[cfg(target_os = "linux")]` branch, so on a native macOS build
//! nothing outside this file's own `#[cfg(test)]` module ever calls it —
//! `#![allow(dead_code)]` below for that reason, same rationale (and same
//! pattern) as `src-tauri/src/egress/mod.rs`'s and
//! `src-tauri/src/pty_authority.rs`'s own module-level allows for code
//! whose only real caller is a different slice/target.
#![allow(dead_code)]

use std::path::PathBuf;

/// The fully-parsed, validated shape of `tome-shim`'s argv. Every field a
/// plain, allocation-owning value (no borrows) so a `#[cfg(test)]` can
/// build and compare these freely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimArgs {
    /// The port the loopback bridge binds to INSIDE the sandbox's network
    /// namespace — must equal the host `PaneProxy`'s real TCP port, so that
    /// `HTTP_PROXY=http://127.0.0.1:<port>` (set by the host, unchanged by
    /// entering the sandbox) resolves to something real from inside it.
    pub port: u16,
    /// Path to the bind-mounted copy of the host's per-pane proxy unix
    /// socket (`/run/tome/proxy.sock` in the canonical bwrap invocation) —
    /// the one thing that actually crosses the fresh network namespace's
    /// boundary, since unix sockets are bind-mountable where TCP ports are
    /// not.
    pub sock: PathBuf,
    /// Fallback-ladder step 2: when set, `tome-shim` unshares its own
    /// user+network namespaces before doing anything else, instead of
    /// assuming bwrap already did. See this module's top doc comment.
    pub self_unshare: bool,
    /// Run the exec'd child in its own new session (`setsid(2)`) before it
    /// execs — `linux::run`'s pre_exec closure calls this when set. Mirrors
    /// bwrap's own `--new-session` (see that flag's doc comment in
    /// `egress::linux::build_bwrap_argv`, the sibling crate that builds
    /// this argv): on rung 1, bwrap itself interprets `--new-session`; on
    /// rung 2 there is no bwrap, so `build_self_unshare_argv` passes the
    /// SAME flag name to `tome-shim` directly, and this binary has to be
    /// the one that calls `setsid()`.
    pub new_session: bool,
    /// Path to deny WRITE access to, once this rung's Landlock enforcement
    /// exists — `build_self_unshare_argv`'s own choice of flag name for
    /// the config-dir deny bwrap's `--tmpfs <appConfigDir>` achieves on
    /// rung 1 (see that function's doc comment in the sibling crate).
    /// Accepted and stored here so the wire contract that builder already
    /// emits parses successfully; **not yet enforced** — see `linux.rs`'s
    /// `self_unshare` and its `TODO(landlock)`. `run` prints a stderr NOTE
    /// when this is `Some` so the gap is visible at runtime, not only in
    /// source comments.
    pub deny_write: Option<PathBuf>,
    /// Path to deny READ access to (the auth file), once Landlock
    /// enforcement exists. Same status as [`ShimArgs::deny_write`].
    pub deny_read: Option<PathBuf>,
    /// Landlock read-allow roots (F-02 — the pentest's Linux file-
    /// confinement finding): repeatable `--allow-read <path>` flags,
    /// emitted only by rung 2 (`build_self_unshare_argv`). Enforced by
    /// `linux::run` via a Landlock `PathBeneath` whitelist when present;
    /// together with `allow_write` they REPLACE the deny paths as the real
    /// mechanism (Landlock is an allow-list LSM — see
    /// `docs/LINUX-LANDLOCK-DESIGN.md`), while `--deny-write`/`--deny-read`
    /// stay on the wire for compatibility and name the excluded roots.
    pub allow_read: Vec<PathBuf>,
    /// Landlock write-allow roots — see [`ShimArgs::allow_read`].
    pub allow_write: Vec<PathBuf>,
    /// The command to exec, in `argv[0], argv[1], ...` form — everything
    /// after the first bare `--`, untouched. Always non-empty: [`parse_args`]
    /// refuses a `--` with nothing after it (see [`ArgError::EmptyArgv`]),
    /// so every caller downstream (`linux::run`'s `Command::new(&args.argv[0])`)
    /// may index `argv[0]` without an explicit length check.
    pub argv: Vec<String>,
}

/// Every way [`parse_args`] can refuse an argv, each carrying enough
/// context for a human operator (this binary's stderr, on a real refusal)
/// to fix the invocation — not a user-facing error in the usual sense
/// (nobody types this command line by hand), but a *caller-debugging*
/// error: a bug in the Rust code that builds this argv should produce a
/// message that says exactly what was wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    /// A required flag never appeared before the argv separator (or at
    /// all, if there was no separator either).
    MissingFlag(&'static str),
    /// A flag that takes a value was the last token, or was immediately
    /// followed by another flag/the separator instead of a value.
    MissingValue(&'static str),
    /// `--port`'s value didn't parse as a `u16` (non-numeric, negative, or
    /// larger than 65535).
    InvalidPort(String),
    /// A token before the separator wasn't one of this parser's known
    /// flags.
    UnknownFlag(String),
    /// No bare `--` token ever appeared — there is no command to exec.
    MissingSeparator,
    /// A bare `--` appeared but nothing followed it.
    EmptyArgv,
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::MissingFlag(name) => write!(f, "missing required flag {name}"),
            ArgError::MissingValue(name) => write!(f, "{name} requires a value"),
            ArgError::InvalidPort(v) => write!(f, "--port value {v:?} is not a valid port (0-65535)"),
            ArgError::UnknownFlag(v) => write!(
                f,
                "unrecognized flag {v:?} (expected --port/--sock/--self-unshare/--new-session/--deny-write/--deny-read/--allow-read/--allow-write/--)"
            ),
            ArgError::MissingSeparator => write!(f, "missing `--` separator before the command to exec"),
            ArgError::EmptyArgv => write!(f, "`--` was given but no command followed it"),
        }
    }
}

impl std::error::Error for ArgError {}

/// Parses `tome-shim`'s argv per this module's top doc comment. `args`
/// excludes `argv[0]` (the program name) — callers pass
/// `std::env::args().skip(1)`, same as every other `main()` in this
/// workspace that hand-parses its own argv.
///
/// Flags may appear in any order and any number of times before the
/// separator (last one wins for `--port`/`--sock`; see
/// `last_value_wins_when_a_flag_is_repeated` below) — this is a trusted,
/// programmatically-built argv, not a human-facing CLI, so there is no
/// value in refusing a harmless repeat. The FIRST bare `--` ends flag
/// parsing unconditionally: every token after it — even one spelled
/// `--port` or `--self-unshare` — is captured into `argv` completely
/// verbatim, never reinterpreted as a flag of this parser's own (a real
/// invocation's trailing command is typically `zsh -l -c '<agent
/// command>'`, and that agent command's own text is attacker/user-
/// influenced in a way this binary's own flags are not — it must never be
/// able to inject a flag into ITS OWN wrapper by choosing its words).
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<ShimArgs, ArgError> {
    let mut iter = args.into_iter();
    let mut port: Option<u16> = None;
    let mut sock: Option<PathBuf> = None;
    let mut self_unshare = false;
    let mut new_session = false;
    let mut deny_write: Option<PathBuf> = None;
    let mut deny_read: Option<PathBuf> = None;
    let mut allow_read: Vec<PathBuf> = Vec::new();
    let mut allow_write: Vec<PathBuf> = Vec::new();
    let mut argv: Option<Vec<String>> = None;

    while let Some(token) = iter.next() {
        match token.as_str() {
            "--port" => {
                let raw = iter.next().ok_or(ArgError::MissingValue("--port"))?;
                port = Some(
                    raw.parse::<u16>()
                        .map_err(|_| ArgError::InvalidPort(raw.clone()))?,
                );
            }
            "--sock" => {
                let raw = iter.next().ok_or(ArgError::MissingValue("--sock"))?;
                sock = Some(PathBuf::from(raw));
            }
            "--self-unshare" => {
                self_unshare = true;
            }
            "--new-session" => {
                new_session = true;
            }
            "--deny-write" => {
                let raw = iter.next().ok_or(ArgError::MissingValue("--deny-write"))?;
                deny_write = Some(PathBuf::from(raw));
            }
            "--deny-read" => {
                let raw = iter.next().ok_or(ArgError::MissingValue("--deny-read"))?;
                deny_read = Some(PathBuf::from(raw));
            }
            "--allow-read" => {
                let raw = iter.next().ok_or(ArgError::MissingValue("--allow-read"))?;
                allow_read.push(PathBuf::from(raw));
            }
            "--allow-write" => {
                let raw = iter.next().ok_or(ArgError::MissingValue("--allow-write"))?;
                allow_write.push(PathBuf::from(raw));
            }
            "--" => {
                let rest: Vec<String> = iter.collect();
                if rest.is_empty() {
                    return Err(ArgError::EmptyArgv);
                }
                argv = Some(rest);
                break;
            }
            other => {
                return Err(ArgError::UnknownFlag(other.to_string()));
            }
        }
    }

    let port = port.ok_or(ArgError::MissingFlag("--port"))?;
    let sock = sock.ok_or(ArgError::MissingFlag("--sock"))?;
    let argv = argv.ok_or(ArgError::MissingSeparator)?;

    Ok(ShimArgs {
        port,
        sock,
        self_unshare,
        new_session,
        deny_write,
        deny_read,
        allow_read,
        allow_write,
        argv,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_full_contract_with_port_sock_and_trailing_argv() {
        let parsed = parse_args(v(&[
            "--port",
            "54321",
            "--sock",
            "/run/tome/proxy.sock",
            "--",
            "zsh",
            "-l",
            "-c",
            "claude",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            ShimArgs {
                port: 54321,
                sock: PathBuf::from("/run/tome/proxy.sock"),
                self_unshare: false,
                new_session: false,
                deny_write: None,
                deny_read: None,
                allow_read: Vec::new(),
                allow_write: Vec::new(),
                argv: v(&["zsh", "-l", "-c", "claude"]),
            }
        );
    }

    // ---- --new-session / --deny-write / --deny-read (rung-2-only flags) ----

    #[test]
    fn new_session_deny_write_deny_read_default_to_absent() {
        let parsed = parse_args(v(&["--port", "1", "--sock", "/s", "--", "true"])).unwrap();
        assert!(!parsed.new_session);
        assert_eq!(parsed.deny_write, None);
        assert_eq!(parsed.deny_read, None);
    }

    #[test]
    fn new_session_is_a_bare_boolean_flag_like_self_unshare() {
        let parsed = parse_args(v(&[
            "--port",
            "1",
            "--sock",
            "/s",
            "--new-session",
            "--",
            "true",
        ]))
        .unwrap();
        assert!(parsed.new_session);
    }

    #[test]
    fn parses_deny_write_and_deny_read_with_their_path_values() {
        let parsed = parse_args(v(&[
            "--port",
            "1",
            "--sock",
            "/s",
            "--deny-write",
            "/home/tester/.config/tome",
            "--deny-read",
            "/home/tester/.config/tome/egress-auth.json",
            "--",
            "true",
        ]))
        .unwrap();
        assert_eq!(
            parsed.deny_write,
            Some(PathBuf::from("/home/tester/.config/tome"))
        );
        assert_eq!(
            parsed.deny_read,
            Some(PathBuf::from("/home/tester/.config/tome/egress-auth.json"))
        );
        assert!(parsed.allow_read.is_empty());
        assert!(parsed.allow_write.is_empty());
    }

    #[test]
    fn parses_repeatable_allow_read_and_allow_write_flags_in_order() {
        // F-02: the Landlock allow-set rides the wire as repeatable flags
        // emitted by egress::linux::build_self_unshare_argv. Order is
        // preserved (it is not observable by the enforcer, but pinning it
        // keeps the two sides of the wire contract honest).
        let parsed = parse_args(v(&[
            "--port",
            "1",
            "--sock",
            "/s",
            "--allow-read",
            "/usr",
            "--allow-read",
            "/etc",
            "--allow-write",
            "/tmp",
            "--allow-write",
            "/ws",
            "--",
            "true",
        ]))
        .unwrap();
        assert_eq!(
            parsed.allow_read,
            vec![PathBuf::from("/usr"), PathBuf::from("/etc")]
        );
        assert_eq!(
            parsed.allow_write,
            vec![PathBuf::from("/tmp"), PathBuf::from("/ws")]
        );
    }

    #[test]
    fn errors_when_allow_read_or_allow_write_is_the_last_token_with_no_value() {
        assert_eq!(
            parse_args(v(&["--port", "1", "--sock", "/s", "--allow-read"])),
            Err(ArgError::MissingValue("--allow-read"))
        );
        assert_eq!(
            parse_args(v(&["--port", "1", "--sock", "/s", "--allow-write"])),
            Err(ArgError::MissingValue("--allow-write"))
        );
    }

    #[test]
    fn deny_write_and_deny_read_are_independent_either_may_appear_alone() {
        let write_only = parse_args(v(&[
            "--port",
            "1",
            "--sock",
            "/s",
            "--deny-write",
            "/cfg",
            "--",
            "true",
        ]))
        .unwrap();
        assert_eq!(write_only.deny_write, Some(PathBuf::from("/cfg")));
        assert_eq!(write_only.deny_read, None);

        let read_only = parse_args(v(&[
            "--port",
            "1",
            "--sock",
            "/s",
            "--deny-read",
            "/cfg/auth.json",
            "--",
            "true",
        ]))
        .unwrap();
        assert_eq!(read_only.deny_write, None);
        assert_eq!(read_only.deny_read, Some(PathBuf::from("/cfg/auth.json")));
    }

    #[test]
    fn errors_when_deny_write_or_deny_read_is_the_last_token_with_no_value() {
        assert_eq!(
            parse_args(v(&["--port", "1", "--sock", "/s", "--deny-write"])),
            Err(ArgError::MissingValue("--deny-write"))
        );
        assert_eq!(
            parse_args(v(&["--port", "1", "--sock", "/s", "--deny-read"])),
            Err(ArgError::MissingValue("--deny-read"))
        );
    }

    #[test]
    fn parses_the_exact_argv_shape_build_self_unshare_argv_emits_new_session_variant() {
        // Pinned literally against egress::linux::build_self_unshare_argv's
        // own headless-true output shape (main crate, cross-crate contract
        // — see this file's own top doc comment) so a change to either
        // side's flag vocabulary is caught here too, not only by the
        // cross-crate test living in the main crate's own suite.
        let parsed = parse_args(v(&[
            "--self-unshare",
            "--new-session",
            "--port",
            "54321",
            "--sock",
            "/run/user/1000/tome/pane-pty-42.sock",
            "--deny-write",
            "/home/tester/.config/tome",
            "--deny-read",
            "/home/tester/.config/tome/egress-auth.json",
            "--",
            "claude",
            "--flow-node",
        ]))
        .unwrap();
        assert!(parsed.self_unshare);
        assert!(parsed.new_session);
        assert_eq!(parsed.port, 54321);
        assert_eq!(
            parsed.sock,
            PathBuf::from("/run/user/1000/tome/pane-pty-42.sock")
        );
        assert_eq!(
            parsed.deny_write,
            Some(PathBuf::from("/home/tester/.config/tome"))
        );
        assert_eq!(
            parsed.deny_read,
            Some(PathBuf::from("/home/tester/.config/tome/egress-auth.json"))
        );
        assert!(parsed.allow_read.is_empty());
        assert!(parsed.allow_write.is_empty());
        assert_eq!(parsed.argv, v(&["claude", "--flow-node"]));
    }

    #[test]
    fn self_unshare_flag_is_optional_and_defaults_to_false() {
        let parsed = parse_args(v(&["--port", "1", "--sock", "/s", "--", "true"])).unwrap();
        assert!(!parsed.self_unshare);
    }

    #[test]
    fn self_unshare_flag_can_appear_in_any_position_before_the_separator() {
        let leading = parse_args(v(&[
            "--self-unshare",
            "--port",
            "1",
            "--sock",
            "/s",
            "--",
            "true",
        ]))
        .unwrap();
        let trailing = parse_args(v(&[
            "--port",
            "1",
            "--sock",
            "/s",
            "--self-unshare",
            "--",
            "true",
        ]))
        .unwrap();
        let middle = parse_args(v(&[
            "--port",
            "1",
            "--self-unshare",
            "--sock",
            "/s",
            "--",
            "true",
        ]))
        .unwrap();
        for parsed in [leading, trailing, middle] {
            assert!(parsed.self_unshare);
            assert_eq!(parsed.port, 1);
            assert_eq!(parsed.sock, PathBuf::from("/s"));
        }
    }

    #[test]
    fn errors_when_port_is_missing() {
        assert_eq!(
            parse_args(v(&["--sock", "/s", "--", "true"])),
            Err(ArgError::MissingFlag("--port"))
        );
    }

    #[test]
    fn errors_when_sock_is_missing() {
        assert_eq!(
            parse_args(v(&["--port", "1", "--", "true"])),
            Err(ArgError::MissingFlag("--sock"))
        );
    }

    #[test]
    fn errors_when_port_is_out_of_u16_range_or_non_numeric() {
        for bad in ["not-a-number", "-1", "65536", "99999", "", "3.14"] {
            let result = parse_args(v(&["--port", bad, "--sock", "/s", "--", "true"]));
            assert_eq!(
                result,
                Err(ArgError::InvalidPort(bad.to_string())),
                "port={bad:?}"
            );
        }
    }

    #[test]
    fn accepts_the_full_u16_boundary_values() {
        assert_eq!(
            parse_args(v(&["--port", "0", "--sock", "/s", "--", "true"]))
                .unwrap()
                .port,
            0
        );
        assert_eq!(
            parse_args(v(&["--port", "65535", "--sock", "/s", "--", "true"]))
                .unwrap()
                .port,
            65535
        );
    }

    #[test]
    fn errors_when_a_flag_is_the_last_token_with_no_value() {
        assert_eq!(
            parse_args(v(&["--port"])),
            Err(ArgError::MissingValue("--port"))
        );
        assert_eq!(
            parse_args(v(&["--port", "1", "--sock"])),
            Err(ArgError::MissingValue("--sock"))
        );
    }

    #[test]
    fn errors_when_the_separator_is_missing_entirely() {
        assert_eq!(
            parse_args(v(&["--port", "1", "--sock", "/s"])),
            Err(ArgError::MissingSeparator)
        );
    }

    #[test]
    fn a_completely_empty_argv_reports_the_first_missing_flag_checked_not_the_separator() {
        // Every required thing (--port, --sock, the separator) is absent
        // at once here — parse_args checks them in a fixed order
        // (port, then sock, then separator) and reports only the first
        // one it finds missing, rather than every reason at once. Pinned
        // explicitly so this ordering reads as a deliberate, tested
        // choice rather than an accident a future edit could silently
        // flip.
        assert_eq!(parse_args(v(&[])), Err(ArgError::MissingFlag("--port")));
    }

    #[test]
    fn errors_when_argv_after_the_separator_is_empty() {
        assert_eq!(
            parse_args(v(&["--port", "1", "--sock", "/s", "--"])),
            Err(ArgError::EmptyArgv)
        );
    }

    #[test]
    fn argv_after_the_separator_is_captured_verbatim_even_if_it_looks_like_a_shim_flag() {
        // The agent command line is untrusted-ish text; it must never be
        // able to inject a flag into tome-shim's OWN wrapper just by
        // containing a substring like "--port" or "--self-unshare".
        let parsed = parse_args(v(&[
            "--port",
            "1",
            "--sock",
            "/s",
            "--",
            "zsh",
            "-l",
            "-c",
            "echo --self-unshare --port 9999 --sock /evil",
        ]))
        .unwrap();
        assert_eq!(
            parsed.argv,
            v(&[
                "zsh",
                "-l",
                "-c",
                "echo --self-unshare --port 9999 --sock /evil"
            ])
        );
        assert_eq!(parsed.port, 1);
        assert!(!parsed.self_unshare);
    }

    #[test]
    fn a_bare_double_dash_inside_argv_after_the_first_one_is_kept_literally() {
        let parsed = parse_args(v(&[
            "--port", "1", "--sock", "/s", "--", "cmd", "--", "more",
        ]))
        .unwrap();
        assert_eq!(parsed.argv, v(&["cmd", "--", "more"]));
    }

    #[test]
    fn unknown_flag_before_the_separator_is_rejected() {
        assert_eq!(
            parse_args(v(&["--bogus", "--port", "1", "--sock", "/s", "--", "true"])),
            Err(ArgError::UnknownFlag("--bogus".to_string()))
        );
    }

    #[test]
    fn last_value_wins_when_a_flag_is_repeated() {
        let parsed = parse_args(v(&[
            "--port", "1", "--port", "2", "--sock", "/a", "--sock", "/b", "--", "true",
        ]))
        .unwrap();
        assert_eq!(parsed.port, 2);
        assert_eq!(parsed.sock, PathBuf::from("/b"));
    }

    #[test]
    fn display_messages_name_the_flag_or_reason_involved() {
        assert_eq!(
            ArgError::MissingFlag("--port").to_string(),
            "missing required flag --port"
        );
        assert_eq!(
            ArgError::MissingValue("--sock").to_string(),
            "--sock requires a value"
        );
        assert!(ArgError::InvalidPort("x".to_string())
            .to_string()
            .contains("--port"));
        assert!(ArgError::UnknownFlag("--wat".to_string())
            .to_string()
            .contains("--wat"));
        assert!(ArgError::MissingSeparator.to_string().contains("--"));
        assert!(ArgError::EmptyArgv.to_string().contains("--"));
    }
}
