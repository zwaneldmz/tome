//! Linux sandbox wrap assembly + fallback ladder (Phase 4, slice L2) — the
//! Linux analog of `egress::seatbelt`. Where `seatbelt.rs` builds an SBPL
//! *profile string* for `sandbox-exec` to interpret, Linux has no single
//! interpreter: the enforcement primitive is a **fresh network namespace**
//! (`unshare(CLONE_NEWNET)`, deny-all egress by construction — see THE
//! DESIGN in the phase-4 plan), created either by `bwrap` (preferred) or by
//! `tome-shim` unsharing itself (fallback). This module assembles the
//! **argv** that invokes either mechanism, plus the pure decision logic for
//! which one to use — it does not itself create a namespace, bind a
//! socket, drop a capability, or exec anything.
//!
//! ## POLICY vs MECHANISM (why everything here compiles on macOS)
//!
//! Every item in this file is either:
//! - **Pure argv/path assembly** — [`build_bwrap_argv`], [`build_self_unshare_argv`],
//!   [`pane_socket_path`], [`auth_file_path`] — plain string/`Vec`/`PathBuf`
//!   building with no syscalls, OS-unconditional, unit-tested on every host
//!   this crate builds on (including this one, macOS).
//! - **A pure decision** — [`decide_sandbox_strategy`] — a 2-input, 3-way
//!   `match` with no I/O at all.
//! - **Pure parsing** of already-read file contents — [`parse_unprivileged_userns_clone`],
//!   [`parse_max_user_namespaces`], [`resolve_userns_allowed`] — and a pure,
//!   PATH-scan search — [`find_executable_on_path`] — that only touches the
//!   filesystem to check whether a *given* candidate path exists (no
//!   Linux-specific syscalls; `std::fs::metadata` + Unix permission bits
//!   work identically on macOS, which is how the tests below exercise it
//!   for real, on this host, without `#[cfg]`).
//! - **The one genuinely `#[cfg(target_os = "linux")]` layer** —
//!   [`probe_bwrap_present`], [`probe_userns_allowed`], [`probe_sandbox_strategy`]
//!   — thin wrappers that read the *real* `$PATH` / `/proc/sys/...` and feed
//!   the pure functions above. See "Verification boundary" below before
//!   trusting these three compile — this crate's local gates do not prove it.
//!
//! **Nothing here is MECHANISM.** No `unshare()`, no `mount()`/bind-mount,
//! no `PR_SET_PDEATHSIG`, no capability drop, no `execve`, no
//! `std::process::Command::spawn`. THE DESIGN assigns all of that to
//! `tome-shim` (its own crate, cross-checked separately — see "Verification
//! boundary") and to the integration layer that actually spawns the argv
//! this module builds — `ipc::pty::pty_create`'s Linux branch, a
//! **different slice's file** (Phase 4/slice L3, landed after this one):
//! this module only ever hands that integrator a `Vec<String>` and a
//! [`SandboxStrategy`] to act on; it does not itself call
//! `std::process::Command::spawn` or anything else that would make argv
//! assembly become a real spawn.
//!
//! ## Verification boundary (read before trusting the `#[cfg(target_os = "linux")]` probes)
//!
//! This slice's three gates are: `cargo check` (native, whole app — this
//! host is macOS), `cargo check -p tome-shim --target
//! x86_64-unknown-linux-gnu` (cross-check, but scoped to the **tome-shim**
//! crate, which never includes this file), and `cargo test --lib
//! egress::linux` (native — same macOS host as the first gate). **None of
//! the three ever type-checks [`probe_bwrap_present`], [`probe_userns_allowed`],
//! or [`probe_sandbox_strategy`]**: the native gates strip
//! `#[cfg(target_os = "linux")]` items before type-checking ever runs, and
//! the cross-check gate looks at a sibling crate that does not contain
//! them. Every non-trivial piece of logic those three functions touch
//! ([`find_executable_on_path`], [`resolve_userns_allowed`],
//! [`decide_sandbox_strategy`]) is deliberately factored out into
//! OS-unconditional functions specifically so it IS proven by this slice's
//! gates — the three `#[cfg(linux)]` wrappers are intentionally reduced to
//! "read a real file/env var, hand the string to an already-tested pure
//! function." That reduces, but does not eliminate, the risk: a first real
//! Linux compile (CI or a real Ubuntu/Fedora box, both **out of this
//! slice's scope**) is still the first time these three functions
//! themselves are type-checked at all. Treat them as "written to compile
//! cleanly," not "proven to compile" — this is the honest boundary the
//! phase-4 task brief asks every slice to report rather than paper over.
//!
//! ## The fallback ladder, and why rung 2 has no `--tmpfs`
//!
//! Rung 1 (`bwrap`) unshares **user + net + mount** namespaces together
//! (`--unshare-user --unshare-net`, and bwrap *always* creates a fresh
//! mount namespace as part of its own sandboxing — that is what makes
//! `--dev-bind`/`--bind`/`--tmpfs` meaningful at all): the loopback bridge
//! socket is reached by *bind-mounting* the host's real socket to a fixed
//! in-sandbox path (`/run/tome/proxy.sock`, [`CONTAINER_PROXY_SOCK_PATH`]),
//! and the app config dir is hidden by *replacing* it with a fresh tmpfs.
//!
//! Rung 2 (self-unshare) is `tome-shim` calling `unshare(CLONE_NEWUSER |
//! CLONE_NEWNET)` **on itself** — deliberately NOT `CLONE_NEWNS` (mount).
//! Without a fresh mount namespace there is no bind-mount trick available
//! and nothing to tmpfs over: the host's real filesystem, at its real
//! paths, is what the sandboxed process still sees. That is exactly why
//! THE DESIGN pairs rung 2 with **Landlock** file rules instead of
//! `--tmpfs`: Landlock denies specific paths per-process without needing a
//! namespace at all. It is also why [`build_self_unshare_argv`] passes the
//! loopback bridge socket's **real host path** (no `/run/tome/proxy.sock`
//! remap — there is no mount namespace to remap it into).
//!
//! Rung 3 is refusal — see [`decide_sandbox_strategy`]'s doc comment for
//! why a heuristic "yes" from rung 2's own preflight check is not the same
//! thing as rung 2 actually succeeding at spawn time.

// Every item below has its own #[cfg(test)] coverage, but — same as
// seatbelt.rs/proxy.rs/mod.rs's own module-level allows — nothing in this
// tree calls any of it yet: the integrator that takes a `SandboxStrategy` +
// assembled argv and actually spawns a gapped Linux pane is a different
// slice's file (`ipc::pty::pty_create`, explicitly out of this slice's
// scope). One allow here rather than scattering `#[allow(dead_code)]` over
// every item.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

// ---- constants ----

/// The bubblewrap binary this module looks for on `$PATH`
/// ([`find_executable_on_path`]/[`probe_bwrap_present`]) and invokes as
/// argv[0] in [`build_bwrap_argv`]. Debian, Ubuntu, and Fedora all package
/// it under this exact name.
pub const BWRAP_BIN: &str = "bwrap";

/// The fixed path `tome-shim` binds the loopback bridge's proxy socket to
/// **inside** the fresh mount namespace bwrap creates — `--bind <host
/// path> <this path>` in [`build_bwrap_argv`], matching THE DESIGN's
/// bwrap invocation exactly. Safe to hardcode as one literal even with
/// several gapped panes running concurrently: bubblewrap creates a
/// *private* mount namespace on every invocation (implicit — there is no
/// `--unshare-mount` flag to opt out of it), so each pane's view of
/// `/run/tome/proxy.sock` is its own and never collides with another
/// pane's. Rung 2 (self-unshare) does NOT use this constant — see the
/// module doc comment's "fallback ladder" section for why.
pub const CONTAINER_PROXY_SOCK_PATH: &str = "/run/tome/proxy.sock";

/// Directory name nested under `$XDG_RUNTIME_DIR` (or the fallback root)
/// that holds every pane's loopback-bridge unix socket. See
/// [`pane_socket_path`].
const PANE_SOCKET_DIR_NAME: &str = "tome";

/// Same filename `egress::seatbelt::seatbelt_profile` denies read of on
/// macOS. Duplicated here as its own named constant rather than imported
/// from `seatbelt.rs` — a sibling module this slice does not own, and
/// which itself only ever inlines the literal once with no exported
/// constant of its own to import. See [`auth_file_path`].
const AUTH_FILE_NAME: &str = "egress-auth.json";

/// Rung 3's refusal message — see [`decide_sandbox_strategy`]. Actionable
/// (names the real package + both apt/dnf install commands the plan's own
/// verification targets, Ubuntu 24.04 and Fedora, would use) rather than a
/// bare "sandbox unavailable": THE DESIGN is explicit that this path must
/// never silently degrade to open egress, so the user needs to know
/// exactly what to do next.
pub const INSTALL_BUBBLEWRAP_HINT: &str =
    "Linux sandbox unavailable: bubblewrap (bwrap) is not installed, and this \
system does not allow unprivileged user namespaces as a fallback, so Tome \
cannot enforce the egress for a gapped pane. Install bubblewrap — e.g. \
`sudo apt install bubblewrap` (Debian/Ubuntu) or `sudo dnf install bubblewrap` \
(Fedora) — then try again, or run this pane ungapped (requires re-authentication).";

// ---- gapped-spawn inputs (shared by both argv builders) ----

/// Every input either argv builder below needs. One shared struct rather
/// than two near-identical ones: [`build_bwrap_argv`] and
/// [`build_self_unshare_argv`] consume the exact same set of facts about
/// the pane being spawned, differing only in how they arrange them (and,
/// for the socket path, whether it needs a bind-mount remap at all — see
/// the module doc comment).
///
/// `pane_id` does not appear verbatim in either builder's output argv —
/// both encode it *indirectly*, via `host_socket_path` (expected to be
/// [`pane_socket_path`]'s output for this same id; see
/// `bwrap_argv_socket_bind_traces_back_to_the_spec_pane_id_via_the_path_helper`
/// below for the round-trip this implies). It is still a first-class field
/// here — not folded away — because it is a natural, load-bearing part of
/// "which pane is this," useful to a future integrator for logging/
/// diagnostics even where it never lands in a shell argv.
///
/// `allow_read`/`allow_write` are the Landlock allow-set for rung 2
/// (F-02 — the pentest's Linux file-confinement finding): the roots a
/// self-unshared pane may read (broadly) and write (narrowly) beneath.
/// Emitted by [`build_self_unshare_argv`] as repeatable `--allow-read`/
/// `--allow-write` flags and enforced by `tome-shim` via Landlock; the
/// app config dir appears in NEITHER set, which is what hides the store
/// and `egress-auth.json` from the pane. Rung 1 (bwrap) ignores both
/// fields — `--tmpfs` already replaces the config dir there.
#[derive(Debug, Clone, PartialEq)]
pub struct GappedSpawnSpec {
    pub pane_id: String,
    /// The loopback bridge's TCP port — `P` in THE DESIGN, identical to
    /// the port `HTTP_PROXY=http://127.0.0.1:P` already names on macOS
    /// (see `agent_env::compose_agent_env`'s `PROXY_VAR_NAMES` layering).
    pub proxy_port: u16,
    /// The proxy's unix socket, at its REAL host path (typically
    /// [`pane_socket_path`]'s output). [`build_bwrap_argv`] bind-mounts
    /// this to [`CONTAINER_PROXY_SOCK_PATH`] inside the sandbox;
    /// [`build_self_unshare_argv`] passes it through unchanged (no mount
    /// namespace to remap it into).
    pub host_socket_path: PathBuf,
    /// The app config directory to deny the sandboxed process write (and,
    /// via [`auth_file_path`], the auth file read) access to —
    /// [`build_bwrap_argv`] replaces it with a fresh tmpfs;
    /// [`build_self_unshare_argv`] passes it to `tome-shim` as a Landlock
    /// deny target instead (see the module doc comment).
    pub app_config_dir: PathBuf,
    /// Resolved, absolute path to the `tome-shim` binary (its sidecar
    /// resolution is the integrator's job, not this module's).
    pub shim_path: PathBuf,
    /// The command to run once inside the sandbox, already fully shaped by
    /// the caller: an interactive pane passes `["zsh", "-l", "-c",
    /// "<agent line>"]` (mirrors `ipc::pty`'s existing
    /// `build_pty_command`); a headless flow node passes its argv
    /// directly, no shell — "Linux nodes get same bwrap wrap (argv direct,
    /// no shell)" per THE DESIGN. This module does not itself decide that
    /// shape; it only appends whatever it is given, verbatim, after the
    /// trailing `--`.
    pub inner_argv: Vec<String>,
    /// True for a headless flow-runner node, false for an interactive PTY
    /// pane. Threads through to a `--new-session` flag on both ladder
    /// rungs — see [`build_bwrap_argv`]'s doc comment for why this is a
    /// deliberate reading of "whether headless" beyond THE DESIGN's one
    /// literal (interactive-only) example line, flagged as such rather
    /// than silently invented.
    pub headless: bool,
    /// Landlock read-allow roots for rung 2 — see the field-doc block
    /// above. Expected to be [`default_landlock_allow_set`]'s output (plus
    /// the login shell's PATH entries, added by the caller).
    pub allow_read: Vec<PathBuf>,
    /// Landlock write-allow roots for rung 2 — narrower than `allow_read`.
    pub allow_write: Vec<PathBuf>,
}

// ---- rung 1: bwrap ----

/// Assembles the exact `bwrap` argv THE DESIGN specifies for a gapped
/// pane. Pure string/`Vec` construction — no shell is ever involved in
/// running the result (a future integrator execs `argv[0]` with `argv[1..]`
/// directly, for example, via `portable_pty::CommandBuilder`, the same way
/// `ipc::pty::build_pty_command` already wraps `sandbox-exec` on macOS —
/// see that function's doc comment for the parallel), so no element here
/// is ever shell-quoted or needs to be: a path containing a space is just
/// one `Vec` element, not a token boundary.
///
/// Flag order matches THE DESIGN's own invocation left to right, plus
/// this module's one deviation (`--tmpfs /run`, see its call-site comment
/// below): `--unshare-user --unshare-net --die-with-parent
/// [--new-session] --cap-add cap_net_admin --dev-bind / / --tmpfs /run
/// --bind <host> <container> --tmpfs <config-dir> -- <shim> --port <P>
/// --sock <container> -- <inner...>`. The relative order of
/// `--unshare-user`/`--unshare-net`/`--die-with-parent`/`--cap-add`/
/// (`--new-session`) does not matter to bwrap itself (none of them are
/// filesystem operations, which ARE order-sensitive — `--dev-bind / /`
/// must precede the narrower `--tmpfs`/`--bind` ops so those apply ON
/// TOP of the whole-root bind, not the other way around, and `--tmpfs
/// /run` must precede the `/run/tome/proxy.sock` bind so the socket's
/// destination parent exists — and is mkdir-able — when the bind is
/// set up); a fixed order is still pinned by this module's own tests
/// below purely so a future edit that reorders them is a visible,
/// deliberate diff rather than a silent one.
///
/// ### `--new-session` and `headless`
///
/// THE DESIGN's literal bwrap line has no `--new-session` — it shows only
/// the interactive case (`... -- zsh -l -c '<agent>'`). This builder adds
/// it when `spec.headless` is true, and omits it otherwise. This is a
/// deliberate interpretation, not literal spec text — flagged as a
/// deviation for review — chosen because bubblewrap's own documentation
/// recommends `--new-session` (`setsid()` before exec) for any sandboxed
/// process that does not need interactive terminal job control: without
/// it, a compromised sandboxed process can `ioctl(TIOCSTI, ...)` on its
/// controlling terminal to inject keystrokes back into whatever is reading
/// that terminal outside the sandbox. An interactive pane genuinely needs
/// job control (Ctrl-C/Ctrl-Z through its own pty) so it keeps the default
/// session; a headless flow node has no interactive terminal need at all,
/// so it gets the strictly-more-locked-down flag. This gives `headless` —
/// listed in this slice's brief as one of the pure builder's own inputs,
/// distinct from the already-shaped `inner_argv` — a real, tested effect
/// on the assembled argv rather than being accepted and silently ignored.
pub fn build_bwrap_argv(spec: &GappedSpawnSpec) -> Vec<String> {
    let mut argv = vec![BWRAP_BIN.to_string()];
    argv.push("--unshare-user".to_string());
    argv.push("--unshare-net".to_string());
    argv.push("--die-with-parent".to_string());
    if spec.headless {
        argv.push("--new-session".to_string());
    }
    argv.push("--cap-add".to_string());
    argv.push("cap_net_admin".to_string());
    argv.push("--dev-bind".to_string());
    argv.push("/".to_string());
    argv.push("/".to_string());
    // A fresh, world-writable tmpfs over /run, BEFORE the proxy-socket
    // bind below. Not in THE DESIGN's literal bwrap line — flagged as a
    // deviation, and load-bearing: this process is UNPRIVILEGED, so the
    // uid_map bwrap writes maps the caller's real uid (not root), and the
    // sandboxed setup phase runs without CAP_DAC_OVERRIDE (bwrap clears
    // the whole bounding set except --cap-add'd caps; cap_net_admin is
    // for the shim's loopback ioctl, not filesystem access). The bind's
    // destination parent /run/tome must then be CREATED inside the
    // namespace — but the --dev-bind'd host /run is root-owned 0755, so
    // mkdir fails EACCES ("bwrap: Can't mkdir parents for
    // /run/tome/proxy.sock: Permission denied") — reproduced and
    // strace-verified against bwrap 0.9.0 as an unprivileged user, and
    // the exact failure the linux-sandbox CI job's first real run hit.
    // Root callers never saw it (root maps to root; CAP_DAC_OVERRIDE).
    // A tmpfs /run is created fresh (and owned by the mapped uid) on
    // every invocation, so /run/tome under it is mkdir-able. It also
    // hides whatever else lived in the host's /run from the sandboxed
    // process — a small confinement bonus, not a regression: nothing a
    // gapped pane legitimately needs lives in /run (its proxy socket is
    // the one thing, and the very next op binds it back in).
    argv.push("--tmpfs".to_string());
    argv.push("/run".to_string());
    argv.push("--bind".to_string());
    argv.push(spec.host_socket_path.display().to_string());
    argv.push(CONTAINER_PROXY_SOCK_PATH.to_string());
    argv.push("--tmpfs".to_string());
    argv.push(spec.app_config_dir.display().to_string());
    argv.push("--".to_string());
    argv.push(spec.shim_path.display().to_string());
    argv.push("--port".to_string());
    argv.push(spec.proxy_port.to_string());
    argv.push("--sock".to_string());
    argv.push(CONTAINER_PROXY_SOCK_PATH.to_string());
    argv.push("--".to_string());
    argv.extend(spec.inner_argv.iter().cloned());
    argv
}

// ---- rung 2: self-unshare fallback ----

/// `<app_config_dir>/egress-auth.json` — the same join `seatbelt.rs`'s
/// `seatbelt_profile` computes inline for its `(deny file-read* (literal
/// ...))` rule, factored out here as a named, independently testable
/// function since [`build_self_unshare_argv`] needs the identical path for
/// its own, Landlock-flavored deny-read target.
pub fn auth_file_path(app_config_dir: &Path) -> PathBuf {
    app_config_dir.join(AUTH_FILE_NAME)
}

/// The Landlock allow-set a rung-2 pane spawns with (F-02). Landlock is an
/// allow-list LSM: a ruleset HANDLES a set of access rights, then grants
/// them per-path via `PathBeneath` rules, and handled-but-ungranted rights
/// are denied — there is no "deny this one subtree" rule. So the safe
/// posture is a whitelist: read broadly beneath the roots an agent
/// legitimately needs, write narrowly beneath the subset it may modify,
/// and put the app config dir in NEITHER set (which transitively makes the
/// store and `egress-auth.json` unreadable and unwritable without a
/// per-file rule).
///
/// This is deliberately conservative — the design doc
/// (`docs/LINUX-LANDLOCK-DESIGN.md`) flags that an unverified whitelist
/// breaks agent CLIs worse than an honestly-absent one, so the allow set
/// here is the documented first cut, and the shim FAILS OPEN (egress-only,
/// stderr NOTE) when Landlock itself is unavailable rather than refusing
/// the pane. Known, documented limitations of this set: `~/.claude.json`
/// (claude's home-root config file) and `~/.ssh` are NOT writable/readable
/// — a pane that needs either uses an ungapped spawn. CI integration tests
/// (`linux_sandbox_integration_tests.rs`) assert the three load-bearing
/// properties: auth file unreadable, config dir unwritable, workspace +
/// `/tmp` still writable.
///
/// `path_entries` is the login shell's harvested PATH, split on `:` — every
/// entry is a directory the user trusts to hold executables, so each gets
/// read (and execute) access; without this, an agent binary installed in
/// `~/.local/bin` or `~/.opencode/bin` could not be exec'd at all.
pub fn default_landlock_allow_set(
    cwd: &Path,
    home: &Path,
    brain: Option<&Path>,
    path_entries: &[PathBuf],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    // Read broadly: system roots (everything the login shell, dynamic
    // loader, and agent CLIs need to exec from), the workspace, the brain
    // vault, the agent CLIs' own config dirs, and kernel interfaces.
    let mut allow_read: Vec<PathBuf> = [
        "/usr", "/etc", "/bin", "/lib", "/lib64", "/opt", "/sbin", "/proc", "/sys", "/dev", "/tmp",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    // Write narrowly: the workspace, the brain vault, /tmp, /dev (device
    // nodes like /dev/null take writes), and the agent CLIs' own config
    // dirs. NOT home — the config dir lives under it, and Landlock has no
    // way to except it back out.
    let mut allow_write: Vec<PathBuf> = vec![PathBuf::from("/tmp"), PathBuf::from("/dev")];

    allow_read.push(cwd.to_path_buf());
    allow_write.push(cwd.to_path_buf());

    if let Some(brain) = brain {
        allow_read.push(brain.to_path_buf());
        allow_write.push(brain.to_path_buf());
    }

    // Agent config dirs — the same three the design doc names. Also
    // ~/.cache, which every real agent CLI writes through.
    for rel in [".claude", ".cache", ".config/opencode", ".config/pi"] {
        let path = home.join(rel);
        allow_read.push(path.clone());
        allow_write.push(path);
    }

    // Every harvested PATH entry gets read+execute so agent binaries
    // installed outside the system roots still resolve.
    for entry in path_entries {
        if !entry.as_os_str().is_empty() {
            allow_read.push(entry.clone());
        }
    }

    (allow_read, allow_write)
}

/// Assembles the argv for rung 2 of the fallback ladder: `tome-shim`
/// invoked directly (no `bwrap` wrapping it — there IS no bwrap on this
/// system, that is why this rung exists), told to unshare itself. No
/// bind-mount remap ([`CONTAINER_PROXY_SOCK_PATH`] is a bwrap-rung-only
/// concept — see the module doc comment's "fallback ladder" section for
/// why): `--sock` here carries `spec.host_socket_path` verbatim, the same
/// real path the host proxy actually bound. `--deny-write`/`--deny-read`
/// are this module's own choice of flag names for the Landlock rules THE
/// DESIGN assigns to this rung (`tome-shim`'s own argument parsing does
/// not exist yet — that is MECHANISM, a different slice's/file's job; see
/// the module doc comment) — chosen to name the two access modes Landlock
/// distinguishes (`LANDLOCK_ACCESS_FS_WRITE_FILE`-family vs.
/// `LANDLOCK_ACCESS_FS_READ_FILE`), mirroring `seatbelt_profile`'s own
/// split between a subtree write-deny (the whole config dir) and a single
/// literal read-deny (just the auth file).
///
/// `--new-session` follows the identical `headless` rule
/// [`build_bwrap_argv`] uses, for the identical reason — see that
/// function's doc comment. Placed as a `tome-shim`-level flag (not a
/// bwrap-level one, since there is no bwrap on this rung): a real
/// implementation of `tome-shim --self-unshare` would call `setsid()`
/// itself before `exec`, using the SAME flag name as bwrap's own
/// `--new-session` deliberately, so the two ladder rungs present one
/// consistent flag vocabulary to whatever reads them later, even though
/// they are two different programs' argv.
pub fn build_self_unshare_argv(spec: &GappedSpawnSpec) -> Vec<String> {
    let mut argv = vec![spec.shim_path.display().to_string()];
    argv.push("--self-unshare".to_string());
    if spec.headless {
        argv.push("--new-session".to_string());
    }
    argv.push("--port".to_string());
    argv.push(spec.proxy_port.to_string());
    argv.push("--sock".to_string());
    argv.push(spec.host_socket_path.display().to_string());
    argv.push("--deny-write".to_string());
    argv.push(spec.app_config_dir.display().to_string());
    argv.push("--deny-read".to_string());
    argv.push(auth_file_path(&spec.app_config_dir).display().to_string());
    // F-02: the Landlock allow-set. `--deny-write`/`--deny-read` above stay
    // for wire compatibility and name the EXCLUDED roots; these name the
    // INCLUDED ones (Landlock is an allow-list). The config dir appears in
    // neither set, so exclusion is implicit — but the shim still receives
    // the deny paths and logs a NOTE if Landlock can't be applied.
    for path in &spec.allow_read {
        argv.push("--allow-read".to_string());
        argv.push(path.display().to_string());
    }
    for path in &spec.allow_write {
        argv.push("--allow-write".to_string());
        argv.push(path.display().to_string());
    }
    argv.push("--".to_string());
    argv.extend(spec.inner_argv.iter().cloned());
    argv
}

// ---- fallback-ladder decision ----

/// Which mechanism a gapped Linux pane spawn should use — the pure result
/// of [`decide_sandbox_strategy`]. Never constructed to silently mean "run
/// unenforced": every variant is either a real enforcement mechanism or an
/// explicit refusal, by construction — there is no fourth "just run it
/// open" case to accidentally fall into, which is the exact TOME-001 hole
/// (`sandbox = null` off-darwin in the Electron original) this whole rung
/// exists to close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxStrategy {
    /// Rung 1 — bwrap is on `$PATH`. Always preferred when available: a
    /// real mount namespace plus a distro-shipped AppArmor profile beats
    /// rung 2's Landlock-only confinement (see THE DESIGN: "raw `unshare`
    /// is blocked by Ubuntu 23.10+ AppArmor for unprofiled binaries, but
    /// distro bwrap ships with an AppArmor profile").
    Bwrap,
    /// Rung 2 — no bwrap, but this system is believed to allow
    /// unprivileged user namespace creation. "Believed" is doing real work
    /// in that sentence — see this function's own doc comment below.
    SelfUnshare,
    /// Rung 3 — neither mechanism is available. Carries an actionable
    /// message ([`INSTALL_BUBBLEWRAP_HINT`] in production) rather than a
    /// bare boolean: THE DESIGN requires refusing loudly, not degrading
    /// silently.
    Refuse { reason: String },
}

/// The fallback ladder's pure decision core: `bwrap_present` wins
/// unconditionally when true (rung 1 needs nothing else); otherwise
/// `userns_allowed` decides between rung 2 and rung 3. Exactly the
/// `(bool, bool) -> 3-way enum` shape this slice's brief asks for, with no
/// I/O of its own — [`probe_bwrap_present`]/[`probe_userns_allowed`] (both
/// `#[cfg(target_os = "linux")]`) are what a real caller feeds in.
///
/// **`userns_allowed` is a preflight HEURISTIC, not a guarantee** — this
/// is the one nuance worth stating explicitly, because THE DESIGN's own
/// rung 3 trigger is "EPERM on AppArmor-restricted userns," and AppArmor's
/// unprivileged-userns-restriction (Ubuntu 23.10+) is a *separate*
/// mechanism from the `/proc/sys/...` sysctls [`probe_userns_allowed`]
/// reads: a system can have `unprivileged_userns_clone=1` (sysctl says
/// "yes") while its AppArmor policy still denies `unshare()` to any
/// unconfined binary at the LSM layer (profile says "no"), because the
/// sysctl and the AppArmor restriction are independent gates that both
/// have to say yes. This function cannot see that second gate — nothing
/// short of an actual `unshare()` attempt can (see
/// [`probe_userns_allowed`]'s own doc comment) — so a `SelfUnshare`
/// verdict here is "this rung is worth attempting," not "this rung is
/// proven to work." The real EPERM discovery, and the real fallback to
/// refusal WHEN rung 2 itself then fails, happens inside `tome-shim`'s own
/// attempt at spawn time (MECHANISM — a different slice's file); this
/// function's `SelfUnshare` verdict is the integration layer's cue to try
/// that rung next, not a promise that it succeeds.
pub fn decide_sandbox_strategy(bwrap_present: bool, userns_allowed: bool) -> SandboxStrategy {
    if bwrap_present {
        SandboxStrategy::Bwrap
    } else if userns_allowed {
        SandboxStrategy::SelfUnshare
    } else {
        SandboxStrategy::Refuse {
            reason: INSTALL_BUBBLEWRAP_HINT.to_string(),
        }
    }
}

// ---- bwrap-on-PATH: pure search + linux probe ----

/// Left-to-right `$PATH` search for the first `dir/name` that exists, is a
/// regular file, and (on unix) has at least one executable bit set — the
/// same resolution order a shell uses. Takes the raw PATH string rather
/// than reading the environment itself specifically so it is pure and
/// OS-unconditional: [`probe_bwrap_present`] (the one real caller, `#[cfg(
/// target_os = "linux")]`) is a two-line wrapper around this, and every
/// non-trivial case — precedence, the executable-bit check, a same-named
/// directory, a missing PATH entry — is exercised by this function's own
/// tests below, on this host (macOS), not deferred to a Linux-only,
/// untested probe.
pub fn find_executable_on_path(path_var: &str, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Rung 1's real-environment check: is `bwrap` on this process's actual
/// `$PATH`? See the module doc comment's "Verification boundary" section
/// — this two-line wrapper is the only part of bwrap detection this
/// crate's native/cross-check gates never type-check; all its real logic
/// lives in [`find_executable_on_path`], which they do.
#[cfg(target_os = "linux")]
pub fn probe_bwrap_present() -> bool {
    let path = std::env::var("PATH").unwrap_or_default();
    find_executable_on_path(&path, BWRAP_BIN).is_some()
}

// ---- userns-allowed: pure parsing + linux probe ----

/// Parses `/proc/sys/kernel/unprivileged_userns_clone`'s contents (a
/// Debian/Ubuntu-kernel-patch sysctl — NOT present on every distro, see
/// [`resolve_userns_allowed`]): `"1"` (optionally with trailing
/// whitespace/newline, as a real `/proc` read always has) means
/// unprivileged user namespace creation is allowed, `"0"` means it is
/// disabled. Anything else (missing, or content that parses as neither)
/// is `None` — "I don't know," not "no."
pub fn parse_unprivileged_userns_clone(contents: &str) -> Option<bool> {
    match contents.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// Parses `/proc/sys/user/max_user_namespaces`'s contents — a mainline
/// (not distro-patch-specific) sysctl capping the total number of user
/// namespaces the whole system may have live at once. Its default on a
/// stock kernel is a large positive number (tens of thousands); `0` is the
/// documented way admin/hardening scripts disable user namespaces
/// entirely on distros that lack the `unprivileged_userns_clone` toggle
/// (Fedora/RHEL/Arch). A non-positive value (`0`, or a malformed negative)
/// reads as disallowed; any positive integer reads as allowed; anything
/// that fails to parse as an integer at all is `None`.
pub fn parse_max_user_namespaces(contents: &str) -> Option<bool> {
    contents.trim().parse::<i64>().ok().map(|n| n > 0)
}

/// Combines both sysctls into one best-effort verdict — the "pick the
/// robust one" this slice's brief invites was, on inspection, better
/// answered as "neither file alone is robust across every distro, so
/// prefer the more explicit one and fall back to the other" than by
/// picking a single file:
///
/// 1. `unprivileged_userns_clone`, when its content actually parses, wins
///    outright — it is the more DIRECT signal where it exists at all
///    (Debian/Ubuntu family): an explicit unprivileged-clone toggle, not a
///    resource cap being (ab)used as one.
/// 2. Otherwise, `max_user_namespaces`, when it parses — covers
///    Fedora/RHEL/Arch-family kernels that carry the mainline cap sysctl
///    but not the Debian-patch toggle.
/// 3. If NEITHER file exists or parses, the verdict defaults to `true`
///    (permissive): a kernel modern enough to run this app at all
///    supports user namespaces (mainline since Linux 3.8), and upstream's
///    own default — absent any distro-specific restriction file — is to
///    allow unprivileged creation. A distro that shipped neither
///    restriction knob is read as "never restricted this," not "restricts
///    everything."
///
/// See [`decide_sandbox_strategy`]'s doc comment for the caveat this
/// function's result does NOT cover: AppArmor's independent,
/// non-sysctl-based unprivileged-userns restriction (Ubuntu 23.10+), which
/// can still deny the actual `unshare()` call even when this function
/// returns `true`.
pub fn resolve_userns_allowed(
    unprivileged_userns_clone: Option<&str>,
    max_user_namespaces: Option<&str>,
) -> bool {
    if let Some(v) = unprivileged_userns_clone.and_then(parse_unprivileged_userns_clone) {
        return v;
    }
    if let Some(v) = max_user_namespaces.and_then(parse_max_user_namespaces) {
        return v;
    }
    true
}

/// Rung 2's real-environment preflight check — see
/// [`decide_sandbox_strategy`]'s doc comment for why this is a heuristic,
/// not a guarantee, and the module doc comment's "Verification boundary"
/// for why this specific function is never type-checked by this slice's
/// own gates. A genuinely authoritative answer would attempt the real
/// `unshare(CLONE_NEWUSER)` syscall and observe whether it succeeds — that
/// is MECHANISM (real syscalls needing `libc`/`nix`, exactly the kind of
/// code THE DESIGN and this slice's brief assign to `tome-shim`, a
/// different crate this slice does not own or touch) — so this function
/// deliberately stays in the same read-only-file, no-new-dependency lane
/// as [`probe_bwrap_present`] instead, at the cost of the AppArmor blind
/// spot documented above.
#[cfg(target_os = "linux")]
pub fn probe_userns_allowed() -> bool {
    let a = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone").ok();
    let b = std::fs::read_to_string("/proc/sys/user/max_user_namespaces").ok();
    resolve_userns_allowed(a.as_deref(), b.as_deref())
}

/// Convenience: [`decide_sandbox_strategy`] fed by both real-environment
/// probes above — the one call a future Linux integrator needs to learn
/// which rung to use. `#[cfg(target_os = "linux")]` for the same reason
/// its two inputs are.
#[cfg(target_os = "linux")]
pub fn probe_sandbox_strategy() -> SandboxStrategy {
    decide_sandbox_strategy(probe_bwrap_present(), probe_userns_allowed())
}

// ---- pane unix socket path ----

/// A pane id is only safe to splice into [`pane_socket_path`]'s output as
/// a single path component if it contains no path separator and is not a
/// bare `.`/`..` traversal segment. Every real pane id in this codebase is
/// generator-produced, never user-typed, so this should never reject a
/// legitimate id in practice — but the path this module builds becomes a
/// **bind-mount source for a privileged sandbox invocation**
/// ([`build_bwrap_argv`]'s `--bind`), so validating defensively here, once,
/// costs nothing and rules out an entire path-traversal class outright
/// rather than trusting every future caller to have generated a safe id.
fn is_safe_pane_id_component(pane_id: &str) -> bool {
    !pane_id.is_empty()
        && !pane_id.contains('/')
        && !pane_id.contains('\\')
        && pane_id != "."
        && pane_id != ".."
}

/// Pure construction of a pane's loopback-bridge unix socket path:
/// `<base>/tome/pane-<id>.sock`, where `<base>` is `xdg_runtime_dir` when
/// given (`Some` and non-empty — mirrors shell convention for "unset or
/// empty means absent") or `fallback_dir` otherwise (a real caller passes
/// `std::env::var("XDG_RUNTIME_DIR").ok()` and `std::env::temp_dir()` —
/// see [`pane_socket_path_from_env`]). Returns `None` for a `pane_id` that
/// fails [`is_safe_pane_id_component`] rather than silently building a
/// path that could resolve outside `<base>/tome/` once actually opened —
/// see that function's doc comment.
///
/// Known, deliberately-unhandled boundary: this does not check the
/// resulting path against `sockaddr_un`'s traditional ~108-byte
/// `sun_path` limit. Real pane ids are short generator-produced tokens
/// and `$XDG_RUNTIME_DIR` is normally short (`/run/user/<uid>`), so this
/// is not expected to bite in practice; a future caller with unusually
/// long inputs would see a bind() failure at the mechanism layer rather
/// than a rejection here.
pub fn pane_socket_path(
    xdg_runtime_dir: Option<&str>,
    fallback_dir: &Path,
    pane_id: &str,
) -> Option<PathBuf> {
    if !is_safe_pane_id_component(pane_id) {
        return None;
    }
    let base = match xdg_runtime_dir {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => fallback_dir.to_path_buf(),
    };
    Some(
        base.join(PANE_SOCKET_DIR_NAME)
            .join(format!("pane-{pane_id}.sock")),
    )
}

/// Real-environment convenience over [`pane_socket_path`]: reads
/// `$XDG_RUNTIME_DIR` and falls back to `std::env::temp_dir()` (matching
/// THE DESIGN's "fallback to a temp dir if XDG_RUNTIME_DIR unset").
/// OS-unconditional — reading an environment variable and constructing a
/// `PathBuf` needs no Linux-specific syscall — but deliberately NOT
/// exercised by a `#[cfg(test)]` that mutates `XDG_RUNTIME_DIR` itself:
/// `std::env::set_var` mutates real, process-global state that every
/// concurrently-running test (Rust tests run in parallel by default)
/// shares, so a test doing that would be flaky by construction rather than
/// a meaningful check; [`pane_socket_path`]'s own tests below already
/// cover every branch of this function's actual logic directly, with
/// explicit inputs instead of ambient global state.
pub fn pane_socket_path_from_env(pane_id: &str) -> Option<PathBuf> {
    let xdg = std::env::var("XDG_RUNTIME_DIR").ok();
    pane_socket_path(xdg.as_deref(), &std::env::temp_dir(), pane_id)
}

/// Creates (if needed) and locks down `dir` to `0700` — THE DESIGN's "0700
/// dir" requirement for the directory holding every pane's socket.
/// `#[cfg(unix)]` (not `target_os = "linux"`): unix permission bits are a
/// unix-wide concept, and `proxy.rs`'s own unix-socket support is already
/// gated the same way (see that module's doc comment) — matching that
/// precedent lets this function's tests run for real, on this host
/// (macOS), rather than being deferred to an untested Linux-only probe.
/// `set_permissions` runs AFTER `create_dir_all` specifically so the mode
/// is exact regardless of the process's umask (`create_dir_all`'s own
/// mode is umask-masked; a following `set_permissions` is not).
#[cfg(unix)]
pub fn ensure_pane_socket_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Locks a pane's already-bound socket file down to `0600` — THE DESIGN's
/// "0600 socket" requirement. Not called anywhere in this tree yet: the
/// actual `UnixListener::bind` call lives in `egress::proxy` (a sibling
/// module this slice does not own — see that module's doc comment on
/// `PaneProxy::spawn`'s Linux seam), so this is a small utility for that
/// future integrator to call immediately after binding, kept here because
/// it is the same "0700 dir / 0600 socket" requirement `pane_socket_path`/
/// `ensure_pane_socket_dir` are already responsible for.
#[cfg(unix)]
pub fn secure_pane_socket_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn sample_spec() -> GappedSpawnSpec {
        GappedSpawnSpec {
            pane_id: "pty-42".to_string(),
            proxy_port: 54321,
            host_socket_path: PathBuf::from("/run/user/1000/tome/pane-pty-42.sock"),
            app_config_dir: PathBuf::from("/home/tester/.config/tome"),
            shim_path: PathBuf::from("/opt/tome/bin/tome-shim"),
            inner_argv: s(&["zsh", "-l", "-c", "claude"]),
            headless: false,
            allow_read: vec![
                PathBuf::from("/usr"),
                PathBuf::from("/etc"),
                PathBuf::from("/home/tester/proj"),
            ],
            allow_write: vec![PathBuf::from("/home/tester/proj"), PathBuf::from("/tmp")],
        }
    }

    // ==== build_bwrap_argv ====

    #[test]
    fn build_bwrap_argv_matches_the_design_invocation_exactly_for_an_interactive_pane() {
        let argv = build_bwrap_argv(&sample_spec());
        assert_eq!(
            argv,
            s(&[
                "bwrap",
                "--unshare-user",
                "--unshare-net",
                "--die-with-parent",
                "--cap-add",
                "cap_net_admin",
                "--dev-bind",
                "/",
                "/",
                "--tmpfs",
                "/run",
                "--bind",
                "/run/user/1000/tome/pane-pty-42.sock",
                "/run/tome/proxy.sock",
                "--tmpfs",
                "/home/tester/.config/tome",
                "--",
                "/opt/tome/bin/tome-shim",
                "--port",
                "54321",
                "--sock",
                "/run/tome/proxy.sock",
                "--",
                "zsh",
                "-l",
                "-c",
                "claude",
            ])
        );
    }

    #[test]
    fn build_bwrap_argv_inserts_new_session_right_after_die_with_parent_when_headless() {
        let mut spec = sample_spec();
        spec.headless = true;
        spec.inner_argv = s(&["claude", "--flow-node"]);
        let argv = build_bwrap_argv(&spec);
        assert_eq!(
            argv,
            s(&[
                "bwrap",
                "--unshare-user",
                "--unshare-net",
                "--die-with-parent",
                "--new-session",
                "--cap-add",
                "cap_net_admin",
                "--dev-bind",
                "/",
                "/",
                "--tmpfs",
                "/run",
                "--bind",
                "/run/user/1000/tome/pane-pty-42.sock",
                "/run/tome/proxy.sock",
                "--tmpfs",
                "/home/tester/.config/tome",
                "--",
                "/opt/tome/bin/tome-shim",
                "--port",
                "54321",
                "--sock",
                "/run/tome/proxy.sock",
                "--",
                "claude",
                "--flow-node",
            ])
        );
    }

    #[test]
    fn build_bwrap_argv_omits_new_session_for_an_interactive_pane() {
        let argv = build_bwrap_argv(&sample_spec());
        assert!(!argv.contains(&"--new-session".to_string()));
    }

    #[test]
    fn build_bwrap_argv_uses_bwrap_as_argv0() {
        assert_eq!(build_bwrap_argv(&sample_spec())[0], BWRAP_BIN);
    }

    #[test]
    fn build_bwrap_argv_dev_binds_root_before_the_narrower_bind_and_tmpfs() {
        // Filesystem-operation ordering is load-bearing for bwrap itself
        // (later operations on/under an already-bound path layer on top of
        // it) — pin it explicitly, not just via the one big exact-sequence
        // test above, so a refactor that reorders these specifically fails
        // loudly here even if the big pin is ever updated to match a bad
        // reorder.
        let argv = build_bwrap_argv(&sample_spec());
        let dev_bind = argv.iter().position(|a| a == "--dev-bind").unwrap();
        let bind = argv.iter().position(|a| a == "--bind").unwrap();
        let tmpfs = argv.iter().position(|a| a == "--tmpfs").unwrap();
        assert!(
            dev_bind < bind,
            "--dev-bind / / must precede the narrower --bind"
        );
        assert!(dev_bind < tmpfs, "--dev-bind / / must precede --tmpfs");
    }

    #[test]
    fn build_bwrap_argv_places_the_double_dash_separators_correctly() {
        // Exactly two `--` separators: one ending bwrap's own flags, one
        // ending tome-shim's own flags — everything after the second is
        // the caller's inner_argv, untouched.
        let argv = build_bwrap_argv(&sample_spec());
        let dashes: Vec<usize> = argv
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "--")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(dashes.len(), 2);
        assert_eq!(&argv[dashes[0] + 1], "/opt/tome/bin/tome-shim");
        assert_eq!(&argv[dashes[1] + 1..], ["zsh", "-l", "-c", "claude"]);
    }

    #[test]
    fn build_bwrap_argv_appends_inner_argv_verbatim_including_when_empty() {
        let mut spec = sample_spec();
        spec.inner_argv = Vec::new();
        let argv = build_bwrap_argv(&spec);
        assert_eq!(
            argv.last().unwrap(),
            "--",
            "an empty inner_argv still ends in the trailing separator"
        );

        let mut spec2 = sample_spec();
        spec2.inner_argv = s(&["one", "two", "three"]);
        let argv2 = build_bwrap_argv(&spec2);
        assert_eq!(&argv2[argv2.len() - 3..], ["one", "two", "three"]);
    }

    #[test]
    fn build_bwrap_argv_does_not_split_a_path_containing_spaces() {
        // No shell is ever involved in running this argv (see the
        // function's own doc comment) — a space in a path must survive as
        // ONE Vec element, not become a token boundary.
        let mut spec = sample_spec();
        spec.app_config_dir = PathBuf::from("/home/tester/My Config Dir/tome");
        let argv = build_bwrap_argv(&spec);
        assert!(argv.contains(&"/home/tester/My Config Dir/tome".to_string()));
    }

    #[test]
    fn build_bwrap_argv_uses_the_same_fixed_container_path_for_both_bind_and_sock() {
        let argv = build_bwrap_argv(&sample_spec());
        let bind_idx = argv.iter().position(|a| a == "--bind").unwrap();
        let sock_idx = argv.iter().position(|a| a == "--sock").unwrap();
        assert_eq!(argv[bind_idx + 2], CONTAINER_PROXY_SOCK_PATH);
        assert_eq!(argv[sock_idx + 1], CONTAINER_PROXY_SOCK_PATH);
    }

    #[test]
    fn bwrap_argv_socket_bind_traces_back_to_the_spec_pane_id_via_the_path_helper() {
        // pane_id never appears literally in the argv (see GappedSpawnSpec's
        // doc comment) — this proves the indirect link is real: the same
        // pane_id, run through pane_socket_path, produces the exact source
        // path build_bwrap_argv's --bind uses.
        let pane_id = "flow-node-7";
        let sock =
            pane_socket_path(Some("/run/user/1000"), &PathBuf::from("/tmp"), pane_id).unwrap();
        let mut spec = sample_spec();
        spec.pane_id = pane_id.to_string();
        spec.host_socket_path = sock.clone();
        let argv = build_bwrap_argv(&spec);
        let bind_idx = argv.iter().position(|a| a == "--bind").unwrap();
        assert_eq!(argv[bind_idx + 1], sock.display().to_string());
        assert!(argv[bind_idx + 1].contains("pane-flow-node-7.sock"));
    }

    // ==== build_self_unshare_argv ====

    #[test]
    fn build_self_unshare_argv_matches_the_designed_invocation_exactly() {
        let argv = build_self_unshare_argv(&sample_spec());
        assert_eq!(
            argv,
            s(&[
                "/opt/tome/bin/tome-shim",
                "--self-unshare",
                "--port",
                "54321",
                "--sock",
                "/run/user/1000/tome/pane-pty-42.sock",
                "--deny-write",
                "/home/tester/.config/tome",
                "--deny-read",
                "/home/tester/.config/tome/egress-auth.json",
                "--allow-read",
                "/usr",
                "--allow-read",
                "/etc",
                "--allow-read",
                "/home/tester/proj",
                "--allow-write",
                "/home/tester/proj",
                "--allow-write",
                "/tmp",
                "--",
                "zsh",
                "-l",
                "-c",
                "claude",
            ])
        );
    }

    #[test]
    fn build_self_unshare_argv_inserts_new_session_right_after_self_unshare_when_headless() {
        let mut spec = sample_spec();
        spec.headless = true;
        let argv = build_self_unshare_argv(&spec);
        assert_eq!(argv[1], "--self-unshare");
        assert_eq!(argv[2], "--new-session");
    }

    #[test]
    fn build_self_unshare_argv_omits_new_session_for_an_interactive_pane() {
        let argv = build_self_unshare_argv(&sample_spec());
        assert!(!argv.contains(&"--new-session".to_string()));
    }

    #[test]
    fn build_self_unshare_argv_passes_the_real_host_socket_path_with_no_container_remap() {
        // The defining structural difference from build_bwrap_argv: no
        // /run/tome/proxy.sock anywhere, because there is no mount
        // namespace to remap into (see the module doc comment).
        let argv = build_self_unshare_argv(&sample_spec());
        assert!(!argv.contains(&CONTAINER_PROXY_SOCK_PATH.to_string()));
        let sock_idx = argv.iter().position(|a| a == "--sock").unwrap();
        assert_eq!(argv[sock_idx + 1], "/run/user/1000/tome/pane-pty-42.sock");
    }

    #[test]
    fn build_self_unshare_argv_uses_shim_path_as_argv0_not_bwrap() {
        let argv = build_self_unshare_argv(&sample_spec());
        assert_eq!(argv[0], "/opt/tome/bin/tome-shim");
    }

    #[test]
    fn build_self_unshare_argv_deny_read_targets_are_derived_via_auth_file_path() {
        let spec = sample_spec();
        let argv = build_self_unshare_argv(&spec);
        let deny_read_idx = argv.iter().position(|a| a == "--deny-read").unwrap();
        assert_eq!(
            argv[deny_read_idx + 1],
            auth_file_path(&spec.app_config_dir).display().to_string()
        );
    }

    #[test]
    fn build_self_unshare_argv_appends_inner_argv_verbatim_including_when_empty() {
        let mut spec = sample_spec();
        spec.inner_argv = Vec::new();
        let argv = build_self_unshare_argv(&spec);
        assert_eq!(argv.last().unwrap(), "--");
    }

    // ==== auth_file_path ====

    #[test]
    fn auth_file_path_joins_the_fixed_filename() {
        assert_eq!(
            auth_file_path(&PathBuf::from("/home/tester/.config/tome")),
            PathBuf::from("/home/tester/.config/tome/egress-auth.json")
        );
    }

    // ==== default_landlock_allow_set (F-02) ====

    #[test]
    fn landlock_allow_set_reads_broadly_writes_narrowly_and_excludes_the_config_dir() {
        let home = PathBuf::from("/home/tester");
        let cwd = PathBuf::from("/home/tester/proj");
        let brain = PathBuf::from("/home/tester/Tome/Brains/proj");
        let path_entries = vec![PathBuf::from("/home/tester/.local/bin")];
        let (allow_read, allow_write) =
            default_landlock_allow_set(&cwd, &home, Some(&brain), &path_entries);

        for must_read in [
            "/usr", "/etc", "/bin", "/lib", "/lib64", "/opt", "/sbin", "/proc", "/sys", "/dev",
            "/tmp",
        ] {
            assert!(
                allow_read.contains(&PathBuf::from(must_read)),
                "{must_read} must be read-allowed"
            );
        }
        assert!(allow_read.contains(&cwd));
        assert!(allow_read.contains(&brain));
        assert!(allow_read.contains(&PathBuf::from("/home/tester/.claude")));
        assert!(allow_read.contains(&PathBuf::from("/home/tester/.config/opencode")));
        assert!(allow_read.contains(&PathBuf::from("/home/tester/.config/pi")));
        assert!(allow_read.contains(&PathBuf::from("/home/tester/.cache")));
        assert!(allow_read.contains(&PathBuf::from("/home/tester/.local/bin")));

        assert!(allow_write.contains(&cwd));
        assert!(allow_write.contains(&brain));
        assert!(allow_write.contains(&PathBuf::from("/tmp")));
        assert!(allow_write.contains(&PathBuf::from("/dev")));
        assert!(allow_write.contains(&PathBuf::from("/home/tester/.claude")));

        // The load-bearing exclusion: the config dir (and therefore the
        // store and egress-auth.json) appears in NEITHER set, and home
        // itself — its parent — is not wholesale allowed either, since
        // Landlock has no "except" rule to carve the config dir back out.
        let config = PathBuf::from("/home/tester/.config/tome");
        assert!(!allow_read.contains(&config));
        assert!(!allow_write.contains(&config));
        assert!(!allow_read.contains(&home));
        assert!(!allow_write.contains(&home));
    }

    #[test]
    fn landlock_allow_set_skips_empty_path_entries_and_brain() {
        let home = PathBuf::from("/home/tester");
        let cwd = PathBuf::from("/home/tester/proj");
        let (allow_read, allow_write) = default_landlock_allow_set(
            &cwd,
            &home,
            None,
            &[PathBuf::from(""), PathBuf::from("/a/bin")],
        );
        assert!(!allow_read.contains(&PathBuf::from("")));
        assert!(allow_read.contains(&PathBuf::from("/a/bin")));
        assert!(!allow_write.contains(&PathBuf::from("")));
    }

    // ==== cross-crate contract: tome_shim::args::parse_args accepts what
    // this module builds ====
    //
    // Every test above (and every test in crates/tome-shim/src/args.rs's
    // own suite) only ever exercises ONE side of this wire contract in
    // isolation: this file's tests assert build_bwrap_argv/build_self_
    // unshare_argv's OUTPUT shape; args.rs's own tests assert parse_args's
    // behavior on hand-typed literal token lists that mirror what its
    // author assumed the builders emit. That is exactly how the two sides
    // drifted apart unnoticed once already — build_self_unshare_argv grew
    // `--deny-write`/`--deny-read` while tome-shim's parser still only knew
    // `--port`/`--sock`/`--self-unshare`/`--`, and every one of those
    // isolated tests stayed green throughout, because none of them ever
    // fed one side's REAL output into the other side's REAL code. These
    // two tests do exactly that — see `tome-shim`'s `Cargo.toml`/`lib.rs`
    // for why this crate can depend on that one (dev-only, path
    // dependency; tome-shim depends on nothing from this crate, so this
    // isn't circular) purely to make this possible.

    #[test]
    fn tome_shim_args_parses_the_real_build_self_unshare_argv_output() {
        let spec = sample_spec();
        let argv = build_self_unshare_argv(&spec);
        let (shim_path, rest) = argv
            .split_first()
            .expect("build_self_unshare_argv never returns an empty argv");
        assert_eq!(shim_path, &spec.shim_path.display().to_string());

        // parse_args's own contract: `args` excludes argv[0] (see that
        // function's doc comment) — `rest` here already skips the shim
        // path for exactly that reason.
        let parsed = tome_shim::args::parse_args(rest.iter().cloned()).expect(
            "tome-shim's own parser must accept the exact argv egress::linux builds for it",
        );
        assert!(parsed.self_unshare);
        assert!(!parsed.new_session); // sample_spec()'s headless is false
        assert_eq!(parsed.port, spec.proxy_port);
        assert_eq!(parsed.sock, spec.host_socket_path);
        assert_eq!(parsed.deny_write, Some(spec.app_config_dir.clone()));
        assert_eq!(parsed.deny_read, Some(auth_file_path(&spec.app_config_dir)));
        // F-02: the Landlock allow-set rides the same wire contract.
        assert_eq!(parsed.allow_read, spec.allow_read);
        assert_eq!(parsed.allow_write, spec.allow_write);
        assert_eq!(parsed.argv, spec.inner_argv);
    }

    #[test]
    fn tome_shim_args_parses_the_real_build_self_unshare_argv_output_when_headless() {
        // headless:true is what makes build_self_unshare_argv also emit
        // --new-session (see that function's own doc comment) — a second,
        // independent flag this same incident class could silently break
        // again if only the non-headless shape above were ever checked.
        let mut spec = sample_spec();
        spec.headless = true;
        let argv = build_self_unshare_argv(&spec);
        let (_shim_path, rest) = argv.split_first().unwrap();
        let parsed = tome_shim::args::parse_args(rest.iter().cloned())
            .expect("tome-shim's own parser must accept --new-session too");
        assert!(parsed.self_unshare);
        assert!(parsed.new_session);
    }

    #[test]
    fn tome_shim_args_parses_the_embedded_shim_invocation_inside_build_bwrap_argv() {
        // bwrap's own argv EMBEDS a tome-shim invocation as its tail (from
        // the shim path through the second "--") — that slice is exactly
        // what bwrap itself execs once its own setup completes, so it must
        // ALSO be a valid tome-shim argv, independent of rung 2's own
        // coverage above (rung 1 never sets --self-unshare/--deny-write/
        // --deny-read at all — see the assertions below).
        let spec = sample_spec();
        let argv = build_bwrap_argv(&spec);
        let dashes: Vec<usize> = argv
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "--")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            dashes.len(),
            2,
            "expected exactly two `--` separators in build_bwrap_argv's output"
        );
        let shim_path_idx = dashes[0] + 1;
        assert_eq!(argv[shim_path_idx], spec.shim_path.display().to_string());
        let rest: Vec<String> = argv[shim_path_idx + 1..].to_vec(); // skip the shim path itself (argv[0])

        let parsed = tome_shim::args::parse_args(rest)
            .expect("tome-shim's own parser must accept the embedded invocation build_bwrap_argv assembles for it");
        assert!(!parsed.self_unshare); // rung 1: bwrap already unshared everything
        assert!(!parsed.new_session); // sample_spec()'s headless is false
        assert_eq!(parsed.deny_write, None); // rung 1 hides the config dir via --tmpfs, not this
        assert_eq!(parsed.deny_read, None);
        assert_eq!(parsed.port, spec.proxy_port);
        assert_eq!(parsed.sock, PathBuf::from(CONTAINER_PROXY_SOCK_PATH));
        assert_eq!(parsed.argv, spec.inner_argv);
    }

    // ==== decide_sandbox_strategy — all three branches ====

    #[test]
    fn decide_sandbox_strategy_prefers_bwrap_whenever_present_regardless_of_userns() {
        assert_eq!(decide_sandbox_strategy(true, true), SandboxStrategy::Bwrap);
        assert_eq!(decide_sandbox_strategy(true, false), SandboxStrategy::Bwrap);
    }

    #[test]
    fn decide_sandbox_strategy_falls_back_to_self_unshare_without_bwrap_when_userns_is_allowed() {
        assert_eq!(
            decide_sandbox_strategy(false, true),
            SandboxStrategy::SelfUnshare
        );
    }

    #[test]
    fn decide_sandbox_strategy_refuses_when_neither_mechanism_is_available() {
        match decide_sandbox_strategy(false, false) {
            SandboxStrategy::Refuse { reason } => {
                assert_eq!(reason, INSTALL_BUBBLEWRAP_HINT);
            }
            other => panic!("expected Refuse, got {other:?}"),
        }
    }

    #[test]
    fn refuse_reason_is_actionable_it_names_the_package_and_an_install_command() {
        let SandboxStrategy::Refuse { reason } = decide_sandbox_strategy(false, false) else {
            panic!("expected Refuse");
        };
        assert!(reason.contains("bubblewrap"));
        assert!(
            reason.contains("apt install bubblewrap") || reason.contains("dnf install bubblewrap")
        );
    }

    #[test]
    fn decide_sandbox_strategy_never_produces_a_fourth_silently_open_variant() {
        // Documentation-as-test: every one of the three branches above is
        // covered, and SandboxStrategy has exactly three variants — there
        // is no representable "run it anyway, unenforced" result to fall
        // into by omission. This test exists to fail loudly (a non-
        // exhaustive match below) if a future edit ever adds a fourth.
        for (bwrap, userns) in [(true, true), (true, false), (false, true), (false, false)] {
            match decide_sandbox_strategy(bwrap, userns) {
                SandboxStrategy::Bwrap
                | SandboxStrategy::SelfUnshare
                | SandboxStrategy::Refuse { .. } => {}
            }
        }
    }

    // ==== find_executable_on_path ====

    #[cfg(unix)]
    mod path_scan {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn write_file(path: &Path, executable: bool) {
            std::fs::write(path, b"#!/bin/sh\n").unwrap();
            let mode = if executable { 0o755 } else { 0o644 };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        #[test]
        fn finds_an_executable_in_a_single_path_dir() {
            let dir = tempfile::tempdir().unwrap();
            write_file(&dir.path().join("bwrap"), true);
            let path_var = dir.path().to_string_lossy().to_string();
            assert_eq!(
                find_executable_on_path(&path_var, "bwrap"),
                Some(dir.path().join("bwrap"))
            );
        }

        #[test]
        fn returns_none_when_not_found_anywhere_on_path() {
            let dir = tempfile::tempdir().unwrap();
            let path_var = dir.path().to_string_lossy().to_string();
            assert_eq!(find_executable_on_path(&path_var, "bwrap"), None);
        }

        #[test]
        fn skips_a_non_executable_file_and_finds_a_later_executable_one() {
            let dir1 = tempfile::tempdir().unwrap();
            let dir2 = tempfile::tempdir().unwrap();
            write_file(&dir1.path().join("bwrap"), false); // present, but not executable
            write_file(&dir2.path().join("bwrap"), true);
            let path_var = std::env::join_paths([dir1.path(), dir2.path()]).unwrap();
            assert_eq!(
                find_executable_on_path(&path_var.to_string_lossy(), "bwrap"),
                Some(dir2.path().join("bwrap"))
            );
        }

        #[test]
        fn skips_a_same_named_directory() {
            let dir1 = tempfile::tempdir().unwrap();
            let dir2 = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir1.path().join("bwrap")).unwrap(); // a directory, not a file
            write_file(&dir2.path().join("bwrap"), true);
            let path_var = std::env::join_paths([dir1.path(), dir2.path()]).unwrap();
            assert_eq!(
                find_executable_on_path(&path_var.to_string_lossy(), "bwrap"),
                Some(dir2.path().join("bwrap"))
            );
        }

        #[test]
        fn earlier_path_entries_win_over_later_ones() {
            let dir1 = tempfile::tempdir().unwrap();
            let dir2 = tempfile::tempdir().unwrap();
            write_file(&dir1.path().join("bwrap"), true);
            write_file(&dir2.path().join("bwrap"), true);
            let path_var = std::env::join_paths([dir1.path(), dir2.path()]).unwrap();
            assert_eq!(
                find_executable_on_path(&path_var.to_string_lossy(), "bwrap"),
                Some(dir1.path().join("bwrap")),
                "PATH search must resolve left-to-right, same as a shell"
            );
        }

        #[test]
        fn tolerates_a_nonexistent_directory_in_the_middle_of_path() {
            let dir1 = tempfile::tempdir().unwrap();
            let missing = dir1.path().join("does-not-exist");
            let dir2 = tempfile::tempdir().unwrap();
            write_file(&dir2.path().join("bwrap"), true);
            let path_var = std::env::join_paths([missing, dir2.path().to_path_buf()]).unwrap();
            assert_eq!(
                find_executable_on_path(&path_var.to_string_lossy(), "bwrap"),
                Some(dir2.path().join("bwrap"))
            );
        }
    }

    #[test]
    fn find_executable_on_path_handles_an_empty_path_variable() {
        assert_eq!(find_executable_on_path("", "bwrap"), None);
    }

    // ==== parse_unprivileged_userns_clone ====

    #[test]
    fn parse_unprivileged_userns_clone_reads_one_and_zero() {
        assert_eq!(parse_unprivileged_userns_clone("1"), Some(true));
        assert_eq!(parse_unprivileged_userns_clone("0"), Some(false));
    }

    #[test]
    fn parse_unprivileged_userns_clone_trims_the_trailing_newline_a_real_proc_read_has() {
        assert_eq!(parse_unprivileged_userns_clone("1\n"), Some(true));
        assert_eq!(parse_unprivileged_userns_clone("0\n"), Some(false));
    }

    #[test]
    fn parse_unprivileged_userns_clone_is_none_for_anything_else() {
        assert_eq!(parse_unprivileged_userns_clone(""), None);
        assert_eq!(parse_unprivileged_userns_clone("2"), None);
        assert_eq!(parse_unprivileged_userns_clone("garbage"), None);
    }

    // ==== parse_max_user_namespaces ====

    #[test]
    fn parse_max_user_namespaces_zero_means_disallowed() {
        assert_eq!(parse_max_user_namespaces("0"), Some(false));
        assert_eq!(parse_max_user_namespaces("0\n"), Some(false));
    }

    #[test]
    fn parse_max_user_namespaces_any_positive_value_means_allowed() {
        assert_eq!(parse_max_user_namespaces("15000"), Some(true));
        assert_eq!(parse_max_user_namespaces("1"), Some(true));
    }

    #[test]
    fn parse_max_user_namespaces_negative_or_unparseable_is_handled_without_panicking() {
        assert_eq!(parse_max_user_namespaces("-1"), Some(false));
        assert_eq!(parse_max_user_namespaces("not-a-number"), None);
        assert_eq!(parse_max_user_namespaces(""), None);
    }

    // ==== resolve_userns_allowed ====

    #[test]
    fn resolve_userns_allowed_prefers_unprivileged_userns_clone_when_it_parses() {
        assert!(!resolve_userns_allowed(Some("0"), Some("15000"))); // clone=0 wins even though cap says allowed
        assert!(resolve_userns_allowed(Some("1"), Some("0"))); // clone=1 wins even though cap says disallowed
    }

    #[test]
    fn resolve_userns_allowed_falls_back_to_max_user_namespaces_when_the_first_file_is_absent() {
        assert!(resolve_userns_allowed(None, Some("15000")));
        assert!(!resolve_userns_allowed(None, Some("0")));
    }

    #[test]
    fn resolve_userns_allowed_falls_back_when_the_first_file_exists_but_fails_to_parse() {
        // Present-but-garbage must be treated the same as absent, not as a
        // hard "no" — the fallback chain still runs.
        assert!(resolve_userns_allowed(Some("garbage"), Some("15000")));
        assert!(!resolve_userns_allowed(Some("garbage"), Some("0")));
    }

    #[test]
    fn resolve_userns_allowed_defaults_permissive_when_neither_file_is_readable() {
        assert!(resolve_userns_allowed(None, None));
    }

    // ==== pane_socket_path ====

    #[test]
    fn pane_socket_path_uses_xdg_runtime_dir_when_given() {
        let p = pane_socket_path(Some("/run/user/1000"), &PathBuf::from("/tmp"), "pty-1").unwrap();
        assert_eq!(p, PathBuf::from("/run/user/1000/tome/pane-pty-1.sock"));
    }

    #[test]
    fn pane_socket_path_falls_back_to_the_given_dir_when_xdg_runtime_dir_is_none_or_empty() {
        let via_none = pane_socket_path(None, &PathBuf::from("/tmp/fallback"), "pty-1").unwrap();
        assert_eq!(
            via_none,
            PathBuf::from("/tmp/fallback/tome/pane-pty-1.sock")
        );

        let via_empty =
            pane_socket_path(Some(""), &PathBuf::from("/tmp/fallback"), "pty-1").unwrap();
        assert_eq!(
            via_empty,
            PathBuf::from("/tmp/fallback/tome/pane-pty-1.sock")
        );
    }

    #[test]
    fn pane_socket_path_rejects_unsafe_pane_ids() {
        let base = PathBuf::from("/tmp/fallback");
        for bad in ["", ".", "..", "a/b", "../../etc/passwd", "a\\b"] {
            assert_eq!(
                pane_socket_path(Some("/run/user/1000"), &base, bad),
                None,
                "pane_id={bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn pane_socket_path_accepts_a_realistic_generated_id() {
        assert!(pane_socket_path(
            Some("/run/user/1000"),
            &PathBuf::from("/tmp"),
            "a1b2c3d4-e5f6-7890"
        )
        .is_some());
    }

    // ==== ensure_pane_socket_dir / secure_pane_socket_permissions (real filesystem) ====

    #[cfg(unix)]
    #[test]
    fn ensure_pane_socket_dir_creates_the_directory_at_exactly_0700() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("tome");
        assert!(!target.exists());
        ensure_pane_socket_dir(&target).unwrap();
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "got mode {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_pane_socket_dir_is_idempotent_on_an_existing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("tome");
        ensure_pane_socket_dir(&target).unwrap();
        ensure_pane_socket_dir(&target).unwrap(); // must not error the second time
        assert!(target.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn secure_pane_socket_permissions_locks_a_file_down_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("pane.sock");
        std::fs::write(&sock, b"").unwrap();
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o644)).unwrap(); // start deliberately too open
        secure_pane_socket_permissions(&sock).unwrap();
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got mode {mode:o}");
    }

    // ==== cross-OS compile sanity (mirrors seatbelt.rs's own such test) ====

    #[test]
    fn every_non_cfg_gated_item_in_this_module_compiles_and_runs_on_every_target_os() {
        // Unlike seatbelt.rs (one function), this module has many pure
        // items — this test just exercises one of each KIND (argv
        // builder, decision, parser, path helper) together, so a future
        // edit that accidentally sneaks a target_os="linux" cfg onto one
        // of the supposedly-portable items fails here first, on this host,
        // rather than only being discovered in CI.
        let spec = sample_spec();
        assert!(!build_bwrap_argv(&spec).is_empty());
        assert!(!build_self_unshare_argv(&spec).is_empty());
        assert_eq!(decide_sandbox_strategy(true, true), SandboxStrategy::Bwrap);
        assert_eq!(parse_unprivileged_userns_clone("1"), Some(true));
        assert!(pane_socket_path(None, &PathBuf::from("/tmp"), "x").is_some());
    }
}
