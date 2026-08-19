//! PTY lifecycle. Ports `src/main/index.js`'s `pty:*` handlers (spawn via
//! `portable-pty`, the 4ms/64KB output batcher, explicit kill/reap) plus
//! `src/main/lib/{agent-spawn,agent-env,pty-authority,custom-agents}.js`
//! for spawn vetting. `pty:data` streams to the renderer over a Tauri
//! Channel per pane; `pty:exit` still goes out on the global event bus
//! (`app.emit`, via the `on_exit` closure `pty_create` hands
//! `crate::pty::Registry::spawn_terminal`/`spawn_raw`) — that split matches
//! the already-committed renderer contract (`tome-ipc.js`'s separate
//! `onData`/`onExit` wiring; see `crate::pty`'s module doc comment for why
//! sending `pty:exit` down the Channel instead would silently break it).
//!
//! `pty_write`/`pty_resize` are thin wrappers over `crate::pty::Registry`
//! (`state.pty`) — Phase 2 slice P1's work (see that module's doc comment
//! for the batcher/reader/kill mechanism). `pty_kill` additionally tears
//! down the pane's egress proxy (`ipc::egress::close_pane_and_proxy`),
//! matching `index.js`'s `ipcMain.on('pty:kill', ...)` calling
//! `egress.closePane(id)` immediately rather than waiting for the killed
//! process's own exit event.
//!
//! `pty_create` below is this phase's (Phase 3, Task A4) integration:
//! reconciles Phase 2's PTY mechanism + spawn-policy ports
//! (`crate::agent_spawn`, `crate::custom_agents`, `crate::pty_authority`,
//! `crate::agent_env`, `crate::login_env`) with the real egress
//! enforcement — `crate::egress::proxy::PaneProxy` (the live loopback
//! CONNECT/HTTP proxy) and, on macOS, an actual `sandbox-exec` wrap built
//! from `crate::egress::seatbelt::seatbelt_profile`. This CLOSES Phase 2's
//! interim gap: every resolved agent is no longer refused outright, and a
//! gapped pane's egress is no longer merely logged-and-ignored — it is
//! enforced.
//!
//! **Phase 4, slice L3 addendum**: the paragraph above described Phase 3's
//! landing, when a gapped pane on any OS other than macOS was still
//! refused outright (real Linux enforcement didn't exist yet). This slice
//! REPLACES that fail-closed stub with the real thing: `egress::linux`'s
//! fallback ladder (bwrap, then `tome-shim` self-unsharing, then an
//! actionable refusal — never a silent unenforced spawn) now backs the
//! Linux branch the exact same way `sandbox-exec` backs macOS's. See "Fail-
//! closed rules this file enforces" below and [`resolve_gapped_spawn`].
//!
//! ## The unified spawn path, and why it now covers agents too
//!
//! A single code path spawns EVERY pane — agent or plain terminal, gapped
//! or not — mirroring `createPty`'s own structure exactly (it never
//! branches into two separate spawn mechanisms; only `agentCmd`,
//! `resolveAgentSecrets()`, and the proxy/sandbox wrap are conditional on
//! `isAgent`/`gapped`). [`build_pty_command`] builds the `CommandBuilder`
//! either way (`agent_cmd: None` for a plain login shell, `Some(cmd)` for
//! `-c <cmd>`); [`pane_env`] builds the environment either way (secrets
//! only when `is_agent`, proxy vars only when gapped) via
//! `agent_env::compose_agent_env`.
//!
//! ## Fail-closed rules this file enforces (TOME-001/002, non-negotiable)
//!
//! - **Every ungapped pane spawn — agent OR plain terminal — needs a fresh
//!   re-auth ceremony once a passphrase is configured.** This is NOT
//!   agent-specific: `pty_authority::unrestricted_spawn_needs_reauth`'s own
//!   signature takes no `is_agent` parameter, because an ungapped pty is
//!   "an unsandboxed shell with the user's full privileges and open
//!   network access" (`src/renderer/lock.js`'s own re-auth-prompt copy)
//!   regardless of what's running inside it. See [`evaluate_reauth`].
//! - **A gapped pane is only ever spawned behind a REAL enforcement
//!   mechanism — never silently unenforced.** macOS gets `sandbox-exec` +
//!   the seatbelt profile (Phase 3). Linux gets `egress::linux`'s fallback
//!   ladder (Phase 4, this slice): bubblewrap when it's on `$PATH`,
//!   otherwise `tome-shim` self-unsharing a fresh user+network namespace,
//!   otherwise an actionable refusal — never a silent degrade to open
//!   egress. Any OTHER OS (a build target this app doesn't actually ship
//!   for — the rewrite plan's locked decisions name exactly macOS + Linux)
//!   also refuses outright. See [`resolve_gapped_spawn`] and
//!   [`build_linux_wrap_argv`]. This whole three-way rule is the exact
//!   TOME-001 hole (`sandbox = null` off-darwin in the Electron original,
//!   proxy env vars set but nothing enforcing them) this rewrite exists to
//!   close — Linux enforcement landing here is what finally makes the air
//!   gap real on both shipping targets, not just macOS.
//! - **A pane's proxy is created BEFORE the process spawns, and torn down
//!   if the spawn then fails** — mirrors `createPty`'s own
//!   `catch (err) { egress.closePane(id); throw err }": a proxy that
//!   came up must never outlive a failed spawn as an orphaned, useless
//!   listener.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use portable_pty::{CommandBuilder, PtySize};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::agent_spawn::{self, AgentEntry};
use crate::ipc::auth::ceil_seconds;
use crate::ipc::egress::{close_pane_and_proxy, create_gapped_pane_proxy};
use crate::{
    agent_env, brain, custom_agents, egress, eventlog, events, lock_gate, login_env, pty_authority,
    state::AppState, store,
};

/// Wire shape of `pty:create`'s options object. `tome-ipc.js`'s
/// `pty.create: (opts) => { const ch = new Channel(); ...; return
/// call('pty_create', { opts, onData: ch }) }` forwards the renderer's
/// `opts` object verbatim — see `src/renderer/panels/terminal.js`'s
/// `tome.pty.create({ id, kind, cwd, egress, ws, model, auth })` for the
/// actual call site — which `src/main/index.js`'s handler destructures as
/// `{ id, kind, cwd, egress: gapped, ws, model, auth }` (line 633).
///
/// `ws`/`model` are accepted here — so a real renderer payload always
/// deserializes cleanly, and the struct documents the full wire contract.
/// `ws`, when present, drives [`resolve_brain_env`]'s `TOME_BRAIN`/
/// `TOME_CORE_VAULT` wiring below (`brain::ensureBrain`/`brain::coreInfo`
/// in the JS original) — unconditionally on `is_agent`/`gapped`, matching
/// `buildAgentEnv`'s own `if (ws) { ... }` block, which runs for a plain
/// terminal pane on an open workspace exactly the same as for an agent
/// pane. `model`/`auth` are wired below too (model pinning via
/// `agent_spawn::build_agent_spawn_from`; `auth` is the TOME-001 re-auth
/// ceremony's credential payload).
#[derive(Debug, Deserialize)]
pub struct PtyCreateOpts {
    pub id: String,
    pub kind: String,
    pub cwd: Option<String>,
    pub egress: Option<bool>,
    pub ws: Option<String>,
    pub model: Option<String>,
    pub auth: Option<Value>,
}

/// How a gapped pane's command line gets wrapped, once its `PaneProxy` is
/// already up — the two shapes [`build_pty_command`] knows how to splice
/// in. Built by [`resolve_gapped_spawn`]'s two enforced branches
/// ([`GappedSpawnDecision::Sandbox`]/[`GappedSpawnDecision::Linux`]),
/// consumed by [`build_pty_command`].
enum SandboxWrap {
    /// macOS: `sandbox-exec -p <profile> <cmd> <args...>` — a simple
    /// PREFIX. `spawnArgs = [...sandbox.args, spawnCmd, ...spawnArgs];
    /// spawnCmd = sandbox.cmd` in the JS original: the plain login-shell
    /// invocation (`build_pty_command`'s own [`login_shell_argv`]) is
    /// appended after `args`, and `cmd` becomes the new spawn target.
    Prefix { cmd: String, args: Vec<String> },
    /// Linux: the ENTIRE argv, already fully assembled by
    /// `egress::linux::build_bwrap_argv`/`build_self_unshare_argv` (argv[0]
    /// is `bwrap` or the resolved `tome-shim` path; the trailing element is
    /// this same pane's [`login_shell_argv`], threaded in as
    /// `GappedSpawnSpec::inner_argv` — see [`build_linux_wrap_argv`]).
    /// Unlike `Prefix`, there is nothing left to append: bwrap's/
    /// `tome-shim`'s own calling convention already embeds the inner
    /// command at the end, so `build_pty_command` just splits this
    /// straight into `argv[0]`/`argv[1..]` and uses it verbatim.
    Full { argv: Vec<String> },
}

/// Which real OS `pty_create` is actually running on — a plain enum rather
/// than a loose `bool`/`bool` pair specifically so [`resolve_gapped_spawn`]
/// cannot be called with the nonsensical "both true" state two independent
/// booleans would allow. The one real call site builds this from
/// `cfg!(target_os = ...)`; `#[cfg(test)]` builds it directly to exercise
/// every branch on whichever host this crate's own tests happen to run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostOs {
    MacOs,
    Linux,
    /// A build target this app doesn't actually ship for (the rewrite
    /// plan's locked decisions name exactly macOS + Linux) — kept as a
    /// real, reachable variant rather than a `panic!`/`unreachable!` so
    /// [`resolve_gapped_spawn`] stays a total function no possible host
    /// triple can crash.
    Other,
}

/// What `pty_create` does for a gapped pane, once its `PaneProxy` is
/// already up — never constructed for an ungapped pane (which needs
/// neither).
enum GappedSpawnDecision {
    /// macOS: wrap the spawn in `sandbox-exec -p <profile>`. The literal
    /// path, not `egress::seatbelt::SANDBOX_EXEC_PATH` (that const is
    /// `#[cfg(target_os = "macos")]`-gated for a good reason of its own —
    /// see that module's doc comment — but this function is deliberately
    /// OS-unconditional so `#[cfg(test)]` can exercise both branches on
    /// any host; the literal and the const are byte-identical on the one
    /// OS where either is ever actually used).
    Sandbox {
        cmd: &'static str,
        args: Vec<String>,
    },
    /// Linux: `egress::linux`'s fallback-ladder verdict for THIS host —
    /// `Bwrap`/`SelfUnshare` (real enforcement, argv still to be built —
    /// see [`build_linux_wrap_argv`]) or `Refuse { reason }` (bwrap absent
    /// AND no usable userns fallback; an actionable message, never a
    /// silent unenforced spawn).
    Linux(egress::linux::SandboxStrategy),
    /// Any OS other than macOS or Linux — refuse rather than spawn a
    /// gapped pane with nothing actually enforcing its proxy env vars (the
    /// exact TOME-001 hole this rewrite exists to close). Distinct from
    /// `Linux`'s own `Refuse` variant only in WHICH refusal message an
    /// integrator shows; both are fail-closed by construction.
    RefuseUnsupportedOs,
}

/// Pure decision core behind the gapped-pane fail-closed rule (see this
/// module's doc comment). `host_os`/`linux_strategy` are parameters (not
/// computed inside this function) so `#[cfg(test)]` can exercise every
/// branch — including Linux's three fallback-ladder outcomes — on
/// whichever host this crate's own tests happen to run on: the ONE real
/// call site (`pty_create`) builds `host_os` from `cfg!(target_os = ...)`
/// and `linux_strategy` from [`current_linux_sandbox_strategy`] (the
/// `#[cfg(target_os = "linux")]`-gated real-environment probe, or an inert
/// placeholder on every other host — see that function's own doc comment
/// for why passing a placeholder here is always safe: this function only
/// ever reads `linux_strategy` inside the `HostOs::Linux` arm, which is
/// only ever reached when the app is ACTUALLY running on Linux).
///
/// `proxy_port` is the pane's already-bound loopback proxy port. The macOS
/// arm needs it because the seatbelt profile must name it (F-01 — the old
/// `localhost:*` carve-out let a gapped pane reach every host-local
/// service directly), which is also why the caller creates the proxy
/// BEFORE calling this for a macOS spawn. `app_data_dir` likewise feeds
/// the profile's config-dir confinement rules (F-03); `home_dir` names the
/// Docker-socket deny paths (the container-runtime escape). All three are
/// ignored by the Linux and `Other` arms — a macOS-only input shape, same
/// as `seatbelt_profile` in the pre-F-01 signature — but threading them
/// through one function keeps the three-way decision testable as a single
/// pure call rather than splitting macOS handling out of it.
///
/// Linux's own fallback-ladder DECISION (bwrap vs. self-unshare vs.
/// refuse) is not re-implemented here — `egress::linux::decide_sandbox_
/// strategy`/`probe_sandbox_strategy` already own that, fully tested in
/// their own module. This function's job is narrower: given the verdict,
/// decide what `pty_create` should DO with it, on par with the macOS
/// branch's own `Sandbox { cmd, args }`.
fn resolve_gapped_spawn(
    host_os: HostOs,
    app_data_dir: &Path,
    proxy_port: u16,
    home_dir: &Path,
    linux_strategy: egress::linux::SandboxStrategy,
) -> GappedSpawnDecision {
    match host_os {
        HostOs::MacOs => GappedSpawnDecision::Sandbox {
            cmd: "/usr/bin/sandbox-exec",
            args: vec![
                "-p".to_string(),
                egress::seatbelt::seatbelt_profile(app_data_dir, proxy_port, home_dir),
            ],
        },
        HostOs::Linux => GappedSpawnDecision::Linux(linux_strategy),
        HostOs::Other => GappedSpawnDecision::RefuseUnsupportedOs,
    }
}

/// The real fallback-ladder verdict for THIS host: `egress::linux::
/// probe_sandbox_strategy()`'s real `$PATH` scan + `/proc/sys/...` reads on
/// Linux. See the module doc comment on [`resolve_gapped_spawn`] for why a
/// tiny wrapper exists here rather than calling `probe_sandbox_strategy()`
/// directly at the one call site: it keeps this file's OWN `#[cfg(target_os
/// = "linux")]` split to exactly one spot instead of threading a `#[cfg]`
/// through `pty_create`'s body.
///
/// Verification boundary (same honest caveat `egress::linux`'s own module
/// doc comment states, worth restating at this integration's real call
/// site): this crate's native `cargo check`/`cargo test` gates run on
/// macOS and therefore never type-check this `#[cfg(target_os = "linux")]`
/// arm at all. It compiles cross-checked (`cargo check -p tome-shim
/// --target x86_64-unknown-linux-gnu` proves the SIBLING crate's Linux ABI
/// usage; THIS call — a plain, dependency-free function call into
/// `egress::linux`, itself already cross-check-covered by that same
/// sibling-crate boundary's absence... see note below) but has never
/// actually run on Linux. Concretely: **this specific line is not
/// re-verified by any of this slice's own local gates** — the whole `tome`
/// package (not just `tome-shim`) is never cross-checked for
/// `x86_64-unknown-linux-gnu` by this task's gate list, only natively
/// checked on macOS (where this arm is `#[cfg]`d away entirely) — so the
/// new `.github/workflows/linux-sandbox.yml` job's own `cargo build
/// --workspace` on a real ubuntu runner is the FIRST time this line is
/// even type-checked, and its `#[ignore]`d integration tests are the first
/// time it actually runs. Never claim this "works" from this repo's own
/// local state — only that it compiles for Linux and is CI-gated.
#[cfg(target_os = "linux")]
fn current_linux_sandbox_strategy() -> egress::linux::SandboxStrategy {
    egress::linux::probe_sandbox_strategy()
}

/// Inert placeholder for every host that isn't Linux — [`resolve_gapped_spawn`]
/// only ever reads this parameter inside its `HostOs::Linux` arm, which
/// `pty_create`'s one real call site only ever reaches when
/// `cfg!(target_os = "linux")` is actually true. An empty reason string
/// (rather than, say, `egress::linux::INSTALL_BUBBLEWRAP_HINT`) makes that
/// "never actually read" property visible at a glance in any debug output,
/// instead of printing a plausible-looking message that never applies.
#[cfg(not(target_os = "linux"))]
fn current_linux_sandbox_strategy() -> egress::linux::SandboxStrategy {
    egress::linux::SandboxStrategy::Refuse {
        reason: String::new(),
    }
}

/// Builds the bwrap/self-unshare argv for whichever non-refusing rung
/// `strategy` names, from an already-fully-populated `spec`. Pure — no
/// syscalls, no I/O, just delegating to `egress::linux`'s own already-
/// tested pure builders — so this stays `#[cfg(test)]`-able on every host,
/// same as everything else in this file's gapped-spawn decision layer.
///
/// Total, not a `panic!`/`unreachable!`, even though `pty_create`'s one
/// real call site only ever reaches this AFTER already handling
/// `SandboxStrategy::Refuse` itself (see that function's body): a
/// `Refuse` input here still returns its `reason` as an `Err` rather than
/// crashing, so this function can never be made to panic by a future call
/// site that forgets that precondition.
fn build_linux_wrap_argv(
    strategy: &egress::linux::SandboxStrategy,
    spec: &egress::linux::GappedSpawnSpec,
) -> Result<Vec<String>, String> {
    match strategy {
        egress::linux::SandboxStrategy::Bwrap => Ok(egress::linux::build_bwrap_argv(spec)),
        egress::linux::SandboxStrategy::SelfUnshare => {
            Ok(egress::linux::build_self_unshare_argv(spec))
        }
        egress::linux::SandboxStrategy::Refuse { reason } => Err(reason.clone()),
    }
}

/// The bare login-shell invocation every pane's command line embeds
/// somewhere — `[shell, "-l"]` for a plain terminal, `[shell, "-l", "-c",
/// cmd]` for an agent. ONE shared builder for every wrap shape
/// [`build_pty_command`] assembles: `SandboxWrap::None`/`Prefix` splice
/// this in directly, and Linux's `SandboxWrap::Full` (built earlier, in
/// `pty_create`'s gapped-spawn setup, before the sandbox is even chosen)
/// uses this SAME vector as `egress::linux::GappedSpawnSpec::inner_argv` —
/// so a gapped Linux pane's agent line is never a second,
/// independently-written copy of this "-l"/"-c" shape that could drift
/// from the ungapped/macOS one.
///
/// Uses the harvested login shell (`login_env::login_env().await.shell`)
/// at every real call site, NOT a hardcoded `zsh` — THE DESIGN's own
/// example line (`... -- zsh -l -c '<agent>'`) is illustrative, not a
/// literal requirement to hardcode: this codebase already generalizes "the
/// login shell" for exactly this reason (`resolve_shell`'s own doc comment
/// in `login_env.rs` — zsh is macOS's default, but not every Linux
/// distro's), and a gapped Linux pane should run the SAME shell an
/// ungapped one on the same box would, for the same reasons.
fn login_shell_argv(shell: &str, agent_cmd: Option<&str>) -> Vec<String> {
    let mut argv = vec![shell.to_string()];
    match agent_cmd {
        Some(cmd) => argv.extend(["-l".to_string(), "-c".to_string(), cmd.to_string()]),
        None => argv.push("-l".to_string()),
    }
    argv
}

/// Resolves the absolute path to the `tome-shim` sidecar this process
/// should hand to bwrap/self-unshare as the in-sandbox helper binary. Two
/// branches, matching how this binary actually lands on disk in each case
/// (`tauri.conf.json`'s `bundle.externalBin` entry + `scripts/
/// build-sidecar.sh` are the other two pieces of this same contract):
///
/// - **Dev** (`tauri::is_dev()`): a plain `cargo build -p tome-shim`
///   output, landing in the SAME shared workspace target directory as this
///   very running binary — `target/debug/tome-shim` next to
///   `target/debug/tome`. This generalizes one step further than the
///   literal "target/debug" the phase-4 plan names: it reuses whichever
///   directory [`tauri::utils::platform::current_exe`] is ACTUALLY running
///   from (via [`shim_path_in`]) rather than hardcoding a profile name, so
///   a `--release` dev run resolves correctly too. Requires a developer to
///   have run `cargo build -p tome-shim` at least once — this function
///   does not build it; see this module's own notes on that gap.
/// - **Bundled**: Tauri's `externalBin` mechanism copies the sidecar into
///   the installed app's own binary directory, KEEPING its build-time
///   target-triple suffix in the filename (`tome-shim-<target-triple>`) —
///   that suffix is what lets the same naming scheme serve every platform
///   Tauri could bundle this app for, even though today only Linux ever
///   actually spawns it.
///
/// Uses `tauri::utils::platform::current_exe`/`target_triple` rather than
/// `std::env::current_exe` directly: Tauri's version additionally
/// canonicalizes the path and rejects a macOS symlink per its own doc
/// comment — exactly the hardening a sidecar lookup (a security-relevant
/// "which binary is about to run inside the sandbox" decision) should
/// want. No new Cargo dependency: `tauri::utils` is already this crate's
/// existing `tauri` dependency re-exporting `tauri_utils` (same crate
/// `tauri::is_dev()`, already used elsewhere in this codebase, comes
/// from).
fn resolve_shim_path() -> Result<PathBuf, String> {
    let exe = tauri::utils::platform::current_exe().map_err(|e| {
        format!(
            "resolve tome-shim sidecar: could not determine this process's own binary path: {e}"
        )
    })?;
    let dir = exe.parent().ok_or_else(|| {
        "resolve tome-shim sidecar: this process's own binary path has no parent directory"
            .to_string()
    })?;
    if tauri::is_dev() {
        Ok(shim_path_in(dir, None))
    } else {
        let triple = tauri::utils::platform::target_triple().map_err(|e| {
            format!(
                "resolve tome-shim sidecar: could not determine this platform's target triple: {e}"
            )
        })?;
        Ok(shim_path_in(dir, Some(&triple)))
    }
}

/// Pure half of [`resolve_shim_path`] — plain path joining, OS-unconditional
/// and touching neither the filesystem nor any Tauri/platform API, so it
/// (unlike its caller) carries its own `#[cfg(test)]` coverage.
fn shim_path_in(dir: &Path, target_triple: Option<&str>) -> PathBuf {
    match target_triple {
        Some(triple) => dir.join(format!("tome-shim-{triple}")),
        None => dir.join("tome-shim"),
    }
}

/// The TOME-001 re-auth ceremony's three possible outcomes — see this
/// module's doc comment and [`evaluate_reauth`]. `pub(crate)`: reused
/// verbatim (not duplicated) by `ipc::runs::runs_start`'s own re-auth
/// ceremony for background flow runs — the second, independent
/// unrestricted-spawn path this same TOME-001 rule applies to.
pub(crate) enum ReauthOutcome {
    /// No `auth` payload arrived at all — the renderer's first attempt,
    /// before it has collected anything. No failure recorded.
    NeedsCredentials,
    /// A payload arrived but didn't verify. Caller records a failure.
    Rejected,
    /// A payload arrived and verified. Caller records a success and
    /// proceeds to spawn.
    Verified,
}

/// Pure decision core of the re-auth ceremony's outcome — everything AFTER
/// the caller has already computed whether a credential payload was
/// supplied (`opts.auth.is_some()`) and, if so, whether it actually
/// verified (`auth.totp_active() ? auth.verify_totp(...) :
/// auth.verify_passphrase(...)`, evaluated once at the call site — kept
/// out of this function so it needs no live `AuthLock` to test). Mirrors
/// the JS original's exact shape: `const ok = auth && (... ? verifyTotp(...)
/// : verifyPassphrase(...)); if (!ok) { if (auth) recordFailure(...); return
/// { reauth: true, error: auth ? '...' : null } }`.
pub(crate) fn evaluate_reauth(payload_supplied: bool, verified: bool) -> ReauthOutcome {
    if !payload_supplied {
        ReauthOutcome::NeedsCredentials
    } else if verified {
        ReauthOutcome::Verified
    } else {
        ReauthOutcome::Rejected
    }
}

/// Builds the `CommandBuilder` for EVERY pane this phase spawns — agent or
/// plain terminal, gapped or not. `agent_cmd` is
/// `agent_spawn::build_agent_spawn_from`'s output (`None` for `terminal`,
/// `Some(cmd)` otherwise); `sandbox`, when present, wraps the whole thing
/// per [`SandboxWrap`]'s own two shapes — `Prefix` (macOS: `index.js`'s own
/// `if (sandbox) { spawnArgs = [...sandbox.args, spawnCmd, ...spawnArgs];
/// spawnCmd = sandbox.cmd }`) or `Full` (Linux: the whole argv is already
/// assembled, nothing to splice).
fn build_pty_command(
    shell: &str,
    agent_cmd: Option<&str>,
    cwd: &Path,
    env: &[(String, String)],
    sandbox: Option<&SandboxWrap>,
) -> CommandBuilder {
    let (spawn_cmd, spawn_args): (String, Vec<String>) = match sandbox {
        None => {
            let argv = login_shell_argv(shell, agent_cmd);
            (argv[0].clone(), argv[1..].to_vec())
        }
        Some(SandboxWrap::Prefix { cmd, args }) => {
            let mut wrapped = args.clone();
            wrapped.extend(login_shell_argv(shell, agent_cmd));
            (cmd.clone(), wrapped)
        }
        Some(SandboxWrap::Full { argv }) => (argv[0].clone(), argv[1..].to_vec()),
    };

    let mut cmd = CommandBuilder::new(&spawn_cmd);
    for a in &spawn_args {
        cmd.arg(a);
    }
    cmd.cwd(cwd);
    // See build_terminal_command's original note (still true here): this
    // process's own environment must never leak into a pty child
    // unfiltered (TOME-007) — `env_clear()` wipes portable-pty's default
    // inherited seed, and every pair in `env` is the ONLY thing the child
    // ends up with.
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

/// `pty:create` (`src/main/index.js` lines 633-787: the `ipcMain.handle`
/// wrapper plus `createPty`). Follows the original's order exactly:
///
/// 1. Resolve `kind` against the built-in + vetted-custom agent list.
///    Anything neither a known agent nor `"terminal"` is a silent no-op.
/// 2. Resolve gapping (`pty_authority::resolve_gapping`) — the renderer may
///    ask for MORE isolation than policy wants, never less (TOME-001).
/// 3. TOME-001 re-auth ceremony, for EVERY ungapped spawn (agent or
///    terminal) once a passphrase is configured — see this module's doc
///    comment. Runs BEFORE resolving the spawn cwd, matching `createPty`'s
///    own order.
/// 4. Resolve the spawn cwd against the open workspace roots.
/// 5. If gapped: `resolve_gapped_spawn` — macOS gets a real `PaneProxy` +
///    `sandbox-exec` wrap; Linux gets a real `PaneProxy` (now ALSO bound
///    to a unix socket — the loopback bridge) + a bwrap/self-unshare wrap;
///    anything else refuses outright, fail-closed.
/// 6. Build the command line + environment (secrets only if agent, proxy
///    vars only if gapped) and spawn via `state.pty.spawn_raw` — the
///    primitive `spawn_terminal` is itself built on, per that function's
///    own doc comment inviting exactly this direct call for the agent
///    path once it existed.
#[tauri::command]
pub async fn pty_create(
    app: AppHandle,
    state: State<'_, AppState>,
    opts: PtyCreateOpts,
    on_data: Channel<Value>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "pty:create")?;
    let locked = *state.locked.read().unwrap();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;

    let customs = {
        let customs_dir = dir.clone();
        tokio::task::spawn_blocking(move || store::get(&customs_dir, "custom-agents", locked))
            .await
            .map_err(|e| e.to_string())?
    };
    let agents = custom_agents::merge_agents(agent_spawn::AGENTS, &customs);
    let is_agent = is_agent_kind(&agents, &opts.kind);
    if !is_agent && opts.kind != "terminal" {
        return Ok(json!({}));
    }
    // Pure, allocation-only, no I/O — moved ahead of the reauth/cwd/
    // gapped-setup steps below (its ORIGINAL position, right before
    // `build_pty_command`) so the Linux gapped-spawn branch can fold it
    // straight into `GappedSpawnSpec::inner_argv` without a second,
    // independently-written resolution later. Safe to compute even on the
    // early-return paths above/below it — unlike `login_env::login_env()`
    // (a cached, but first-call-EXPENSIVE shell-out, deliberately NOT
    // moved for exactly that reason — see the gapped Linux branch below),
    // this is a plain allowlist lookup with no cost worth guarding.
    let agent_cmd = agent_spawn::build_agent_spawn_from(&agents, &opts.kind, opts.model.as_deref());

    let egress_default = {
        let default_dir = dir.clone();
        tokio::task::spawn_blocking(move || store::get(&default_dir, "egress-default", locked))
            .await
            .map_err(|e| e.to_string())?
    };
    // `!== false` in the JS original: an absent key (`Value::Null`) and
    // anything else but the literal `false` all mean "gap by default".
    let policy_default = egress_default != json!(false);
    let effective_gapped =
        pty_authority::resolve_gapping(opts.egress.unwrap_or(false), policy_default);

    // ---- TOME-001 re-auth ceremony (before resolving cwd — matches
    // createPty's own order) ----
    if !effective_gapped {
        let auth_configured = {
            let guard = state.auth.lock().expect("AppState.auth lock poisoned");
            guard
                .as_ref()
                .map(|a| a.status().configured)
                .unwrap_or(false)
        };
        if pty_authority::unrestricted_spawn_needs_reauth(effective_gapped, auth_configured) {
            let mut guard = state.auth.lock().expect("AppState.auth lock poisoned");
            let auth = guard
                .as_mut()
                .ok_or_else(|| "auth: not initialized".to_string())?;
            let wait = auth.throttle_retry_in("pty:unrestricted");
            if wait > 0 {
                return Ok(json!({
                    "reauth": true,
                    "error": format!("Too many attempts. Wait {}s.", ceil_seconds(wait)),
                }));
            }
            let payload_supplied = opts.auth.is_some();
            let verified = payload_supplied && {
                let payload = opts
                    .auth
                    .as_ref()
                    .expect("payload_supplied just checked Some");
                if auth.totp_active() {
                    payload
                        .get("code")
                        .and_then(Value::as_str)
                        .is_some_and(|c| auth.verify_totp(c))
                } else {
                    payload
                        .get("passphrase")
                        .and_then(Value::as_str)
                        .is_some_and(|p| auth.verify_passphrase(p))
                }
            };
            match evaluate_reauth(payload_supplied, verified) {
                ReauthOutcome::NeedsCredentials => {
                    return Ok(json!({"reauth": true, "error": Value::Null}));
                }
                ReauthOutcome::Rejected => {
                    auth.record_failure("pty:unrestricted");
                    return Ok(json!({"reauth": true, "error": "Incorrect passphrase or code."}));
                }
                ReauthOutcome::Verified => auth.record_success("pty:unrestricted"),
            }
        }
    }

    let home = std::env::home_dir().unwrap_or_default();
    let open_folders = state.open_folders.read().unwrap().clone();
    // `resolve_spawn_cwd` ends in a `std::fs::metadata` call — run it off
    // this async command's worker thread, same as the two `store::get`
    // calls above. `home` is cloned into the closure (it is ALSO used
    // later, in the Linux Landlock allow-set — F-02).
    let cwd = opts.cwd.clone();
    let spawn_cwd = tokio::task::spawn_blocking({
        let home_for_spawn = home.clone();
        move || pty_authority::resolve_spawn_cwd(cwd.as_deref(), &open_folders, &home_for_spawn)
    })
    .await
    .map_err(|e| e.to_string())?;

    // ---- gapped-pane setup: live proxy + real OS enforcement, or
    // fail-closed refusal — see this module's doc comment ----
    let mut proxy_port: Option<u16> = None;
    let mut sandbox: Option<SandboxWrap> = None;
    if effective_gapped {
        let host_os = if cfg!(target_os = "macos") {
            HostOs::MacOs
        } else if cfg!(target_os = "linux") {
            HostOs::Linux
        } else {
            HostOs::Other
        };
        // F-01: the seatbelt profile must name THIS pane's proxy port (the
        // old `localhost:*` carve-out let a gapped pane reach every
        // host-local service directly), and the port is kernel-assigned at
        // bind time — so macOS creates the proxy FIRST and the profile is
        // built from its real port. If the spawn fails afterward, the
        // existing cleanup (`close_pane_and_proxy`) tears it down.
        if host_os == HostOs::MacOs {
            let proxy = create_gapped_pane_proxy(&app, &state, &opts.id, None)
                .await
                .map_err(|e| e.to_string())?;
            proxy_port = Some(proxy.port());
        }
        match resolve_gapped_spawn(
            host_os,
            &dir,
            proxy_port.unwrap_or(0),
            &home,
            current_linux_sandbox_strategy(),
        ) {
            GappedSpawnDecision::Sandbox { cmd, args } => {
                sandbox = Some(SandboxWrap::Prefix {
                    cmd: cmd.to_string(),
                    args,
                });
            }
            GappedSpawnDecision::Linux(strategy) => {
                // Rung 3: refuse loudly with an actionable message BEFORE
                // touching anything — no proxy created, nothing to tear
                // down. Never a silent unenforced spawn (TOME-001).
                if let egress::linux::SandboxStrategy::Refuse { reason } = &strategy {
                    events::append(
                        &app,
                        eventlog::make_event(
                            "pty:blocked",
                            vec![
                                ("paneId", json!(opts.id)),
                                ("kind", json!(opts.kind)),
                                ("gapped", json!(true)),
                                ("reason", json!(reason)),
                            ],
                            None,
                        ),
                    );
                    return Err(reason.clone());
                }

                // Rung 1 (bwrap) or rung 2 (self-unshare): real enforcement.
                // The loopback bridge's unix socket path — bind-mounted
                // (bwrap) or reached at its real host path (self-unshare;
                // no mount namespace to remap it into, see
                // `egress::linux`'s module doc comment) — must exist and
                // its parent dir must already be `0700` BEFORE `PaneProxy`
                // tries to `UnixListener::bind` it.
                let sock_path = egress::linux::pane_socket_path_from_env(&opts.id).ok_or_else(|| {
                    "gapped pane refused: pane id is not a valid loopback-bridge socket path component".to_string()
                })?;
                if let Some(parent) = sock_path.parent() {
                    egress::linux::ensure_pane_socket_dir(parent).map_err(|e| e.to_string())?;
                }
                let shim_path = resolve_shim_path()?;

                let proxy =
                    create_gapped_pane_proxy(&app, &state, &opts.id, Some(sock_path.clone()))
                        .await
                        .map_err(|e| e.to_string())?;
                proxy_port = Some(proxy.port());

                // Cached (`tokio::sync::OnceCell`) — see `login_env.rs`'s
                // module doc comment. Calling it here (rather than only at
                // its original call site further down) does NOT re-pay the
                // shell-out cost, and does not change WHEN the first-ever
                // pty:create call pays it either: gapped panes never run
                // the TOME-001 reauth ceremony above (see
                // `pty_authority::unrestricted_spawn_needs_reauth`'s own
                // "gapped spawn never needs reauth" contract, pinned in
                // this file's own tests), so there is no early-return path
                // between here and the function's start that this call
                // could newly delay.
                let login = login_env::login_env().await;
                let inner_argv = login_shell_argv(&login.shell, agent_cmd.as_deref());

                // F-02: the Landlock allow-set for rung 2 — the workspace
                // cwd, the brain vault (when a workspace is open), system
                // roots, agent config dirs, and the login shell's PATH
                // entries. The app config dir appears in NEITHER set
                // (Landlock is an allow-list; exclusion is implicit — see
                // `egress::linux::default_landlock_allow_set`).
                let brain_path = match opts.ws.as_deref() {
                    Some(ws) => {
                        let ws = ws.to_string();
                        let root = tokio::task::spawn_blocking(move || brain::ensure_brain(&ws))
                            .await
                            .map_err(|e| e.to_string())??;
                        Some(root)
                    }
                    None => None,
                };
                let path_entries: Vec<PathBuf> = login
                    .path
                    .split(':')
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .collect();
                let (allow_read, allow_write) = egress::linux::default_landlock_allow_set(
                    &spawn_cwd,
                    &home,
                    brain_path.as_deref(),
                    &path_entries,
                );

                let spec = egress::linux::GappedSpawnSpec {
                    pane_id: opts.id.clone(),
                    proxy_port: proxy.port(),
                    host_socket_path: sock_path,
                    app_config_dir: dir.clone(),
                    shim_path,
                    inner_argv,
                    // Every `pty:create` call is an interactive pane — a
                    // headless flow-node spawn path does not exist in this
                    // tree yet (checked: no `src-tauri/src/flow` module,
                    // and `ipc::runs::*` are still Phase-5 stubs), so this
                    // is unconditionally `false` here; the day a flow
                    // spawn path lands, IT decides this independently, the
                    // same way `build_bwrap_argv`'s own doc comment
                    // anticipates.
                    headless: false,
                    allow_read,
                    allow_write,
                };
                // `?` here would be provably safe TODAY (the only
                // `Refuse` case was already handled above, before the
                // proxy existed, and `build_linux_wrap_argv` has no other
                // failure mode) — but this module's own doc comment
                // promises "a pane's proxy is created BEFORE the process
                // spawns, and torn down if the spawn then fails" as a
                // standing invariant, not a today-only fact, so this stays
                // explicit rather than relying on a future reader/refactor
                // to re-derive why a bare `?` would still be safe here.
                let argv = match build_linux_wrap_argv(&strategy, &spec) {
                    Ok(argv) => argv,
                    Err(reason) => {
                        close_pane_and_proxy(&app, &state, &opts.id);
                        return Err(reason);
                    }
                };
                sandbox = Some(SandboxWrap::Full { argv });
            }
            GappedSpawnDecision::RefuseUnsupportedOs => {
                events::append(
                    &app,
                    eventlog::make_event(
                        "pty:blocked",
                        vec![
                            ("paneId", json!(opts.id)),
                            ("kind", json!(opts.kind)),
                            ("gapped", json!(true)),
                            (
                                "reason",
                                json!("gapped panes are only supported on macOS and Linux"),
                            ),
                        ],
                        None,
                    ),
                );
                return Err(
                    "gapped panes are only supported on macOS and Linux — refusing to spawn unenforced on this OS"
                        .to_string(),
                );
            }
        }
    }

    let login = login_env::login_env().await;
    let process_env: HashMap<String, String> = std::env::vars().collect();
    let secrets = if is_agent {
        login.secrets.clone()
    } else {
        HashMap::new()
    };
    // `if (ws) { env.TOME_BRAIN = await brain.ensureBrain(ws); ... }` —
    // unconditional on is_agent/gapped, matching buildAgentEnv's own order
    // (see PtyCreateOpts's doc comment on `ws`).
    let (brain_path, core_vault_root) = match opts.ws.clone() {
        Some(ws) => {
            let dir_for_brain = dir.clone();
            tokio::task::spawn_blocking(move || resolve_brain_env(&ws, &dir_for_brain, locked))
                .await
                .map_err(|e| e.to_string())??
        }
        None => (None, None),
    };
    let extras = agent_env::AgentEnvExtras {
        is_agent,
        secrets,
        brain_path,
        core_vault_root,
        proxy_port,
    };
    let env = pane_env(&process_env, &login.path, &extras);

    let cmd = build_pty_command(
        &login.shell,
        agent_cmd.as_deref(),
        &spawn_cwd,
        &env,
        sandbox.as_ref(),
    );
    let size = PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };

    let exit_id = opts.id.clone();
    let exit_app = app.clone();
    // The per-chunk scrollback tap `conductor.js`'s `p.onData` performs
    // (`conductor.record(id, data)`) — the feed for `read_terminal`'s
    // consent-gated scrollback. `Arc<Conductor>` (see `AppState.conductor`)
    // so this closure can outlive the command on the batcher task. `record`
    // is a no-op until `register` (below, after a successful spawn) opens the
    // pane's ring; the batcher's 4ms window makes any first output strictly
    // later than that register call.
    let tap_conductor = state.conductor.clone();
    let tap_id = opts.id.clone();
    let tap: crate::pty::DataTap =
        std::sync::Arc::new(move |data: &str| tap_conductor.record(&tap_id, data));
    let spawn_result = state
        .pty
        .spawn_raw(
            opts.id.clone(),
            cmd,
            size,
            on_data,
            Some(tap),
            move |exit_code| {
                let _ = exit_app.emit("pty:exit", json!({"id": exit_id, "exitCode": exit_code}));
                let exit_state = exit_app.state::<AppState>();
                // Mirrors index.js's `p.onExit(({ exitCode }) => { ...;
                // conductor.markExited(id); egress.closePane(id); ... })` —
                // markExited BEFORE closePane, same order.
                exit_state.conductor.mark_exited(&exit_id);
                close_pane_and_proxy(&exit_app, &exit_state, &exit_id);
            },
        )
        .await;

    if let Err(err) = spawn_result {
        // The proxy came up (if gapped) before the spawn attempt — a
        // failed spawn must not strand it listening on loopback.
        close_pane_and_proxy(&app, &state, &opts.id);
        return Err(err);
    }
    // Mirrors index.js's `ptys.set(id, p); conductor.register(id, { kind,
    // cwd: spawnCwd, egress: effectiveGapped })` — registered only once the
    // real spawn has succeeded, so the conductor's tools (list_panes'
    // enrichment, read_terminal's consent gate) can see this pane. `record`
    // (the per-chunk scrollback tap conductor.js's `p.onData` also calls)
    // has no equivalent call site here: `Registry::spawn_raw` hands the raw
    // data stream straight to the `on_data` Tauri Channel with no Rust-side
    // interception point, and adding one is a `pty.rs` (Phase 2) reader-loop
    // change outside this slice's safe blast radius — left as a follow-up
    // (see `conductor::state`'s module doc comment).
    state.conductor.register(
        &opts.id,
        &opts.kind,
        &spawn_cwd.to_string_lossy(),
        effective_gapped,
    );
    Ok(json!({}))
}

/// `pty:write` (`tome-ipc.js`'s `write: (id, data) => fire('pty_write', {
/// id, data })`) — fire-and-forget from the renderer, so this never
/// surfaces a per-call error; an unknown pane id is a silent no-op, same as
/// the Electron original's `ptys.get(id)?.write(data)`.
#[tauri::command]
pub async fn pty_write(
    state: State<'_, AppState>,
    id: String,
    data: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "pty:write")?;
    state.pty.write(&id, &data);
    Ok(json!({}))
}

/// `pty:resize` (`fire('pty_resize', { id, cols, rows })`) — same
/// no-op-on-unknown-id contract as `pty_write`.
#[tauri::command]
pub async fn pty_resize(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<Value, String> {
    lock_gate::guard(&state, "pty:resize")?;
    state.pty.resize(&id, cols, rows);
    Ok(json!({}))
}

/// `pty:kill` (`fire('pty_kill', { id })`) — signals and reaps the pane's
/// child process (see `crate::pty::Registry::kill`'s doc comment for the
/// full kill/drop/reap sequence and why it doesn't block on the reap
/// itself), THEN tears down its egress proxy immediately — mirrors the JS
/// original's `ptys.get(id)?.kill(); ptys.delete(id); conductor.forget(id);
/// egress.closePane(id)`, which closes the pane's egress the moment a kill
/// is requested rather than waiting for the killed process's own exit
/// event. `on_exit` (fired later, once the killed process is actually
/// reaped — see `crate::pty`'s module doc comment) calls
/// `close_pane_and_proxy` too; that second call is a safe no-op by then
/// (idempotent, matching `closePane`'s own contract). An unknown/
/// already-gone pane id is a safe no-op throughout, same as the Electron
/// original's optional chaining.
#[tauri::command]
pub async fn pty_kill(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "pty:kill")?;
    state.pty.kill(&id).await;
    // Mirrors index.js's `ptys.get(id)?.kill(); ptys.delete(id);
    // conductor.forget(id); egress.closePane(id)` — forget BEFORE
    // closePane, same order; drops meta/scrollback/read-consent/
    // read-requested together so a reopened pane with the same id starts
    // clean rather than inheriting a stale consent grant.
    state.conductor.forget(&id);
    close_pane_and_proxy(&app, &state, &id);
    Ok(json!({}))
}

/// This phase's central security predicate: true for ANY resolved agent —
/// built-in or vetted custom — in `agents`; false for a plain terminal or an
/// unrecognized kind. Split out from `pty_create`'s body so the fail-closed
/// decision is unit-testable without a live `AppHandle`/`State`.
fn is_agent_kind(agents: &[AgentEntry], kind: &str) -> bool {
    agents.iter().any(|a| a.id == kind)
}

/// Resolves `TOME_CORE_VAULT` for a workspace-scoped pane, given the
/// already-resolved `core-vault` store value — ports the second half of
/// `index.js`'s `if (ws) { ...; const info = await brain.coreInfo(...); if
/// (info.configured) env.TOME_CORE_VAULT = info.root }`. Split out from
/// [`resolve_brain_env`] (which additionally calls `brain::ensure_brain`,
/// a real `$HOME`-writing side effect — see that function's own doc
/// comment) specifically so this half is unit-testable hermetically, with
/// no real home directory touched.
fn resolve_core_vault_root(core_vault_store_value: Option<&str>) -> Option<String> {
    brain::core_info(core_vault_store_value)
        .configured_root()
        .map(str::to_string)
}

/// Resolves `TOME_BRAIN`/`TOME_CORE_VAULT` for a workspace-scoped pane —
/// ports the `if (ws) { ... }` block of `index.js`'s `buildAgentEnv`
/// (~index.js:678-682) verbatim: `ensureBrain` always runs (creating the
/// vault directory + seeding `AGENTS.md` as a side effect) whenever `ws`
/// is present, regardless of `is_agent`/`gapped` — a plain terminal pane
/// on an open workspace gets `$TOME_BRAIN` too, same as an agent pane. A
/// failure here (`ensure_brain`'s mkdir/write can fail) propagates and
/// fails the whole `pty:create` call, matching the JS original: nothing
/// wraps `buildAgentEnv`'s `await brain.ensureBrain(ws)` in a try/catch,
/// so a throw there rejects `createPty` entirely.
///
/// Not unit-tested directly: `brain::ensure_brain` resolves its vault root
/// off the REAL `$HOME` (see that function's own doc comment on why
/// `brain.rs`'s own test suite avoids calling it directly too — there is
/// no way to override that path for a hermetic test without changing
/// `brain::ensure_brain`'s signature, out of scope for this fix). The
/// `TOME_CORE_VAULT` half has no such coupling and IS unit-tested — see
/// [`resolve_core_vault_root`].
fn resolve_brain_env(
    ws: &str,
    dir: &Path,
    locked: bool,
) -> Result<(Option<String>, Option<String>), String> {
    let brain_path = brain::ensure_brain(ws)?.to_string_lossy().into_owned();
    let core_vault = store::get(dir, "core-vault", locked);
    let core_root = core_vault.as_str().map(str::to_string);
    Ok((
        Some(brain_path),
        resolve_core_vault_root(core_root.as_deref()),
    ))
}

/// The env every pane (agent or plain terminal, gapped or not) is spawned
/// with: the current process's environment with `PATH` overridden to the
/// login shell's harvested value FIRST, then run through
/// `agent_env::compose_agent_env`'s allowlist. `extras.is_agent`/
/// `extras.proxy_port` are what make this the same helper for both a
/// gapped agent pane (secrets AND proxy vars) and a plain ungapped
/// terminal (`AgentEnvExtras::default()` — neither).
///
/// Mirrors the JS original's NET EFFECT — `ensureLoginEnv()` mutates
/// `process.env.PATH` in place before `buildAgentBaseEnv(process.env)` ever
/// reads it — without the in-place mutation; see `login_env.rs`'s module
/// doc comment for why this port returns data instead of mutating global
/// process state.
fn pane_env(
    process_env: &HashMap<String, String>,
    login_path: &str,
    extras: &agent_env::AgentEnvExtras,
) -> Vec<(String, String)> {
    let mut base = process_env.clone();
    base.insert("PATH".to_string(), login_path.to_string());
    agent_env::compose_agent_env(&base, extras)
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ================= PtyCreateOpts — the renderer wire shape =================

    #[test]
    fn pty_create_opts_deserializes_the_full_renderer_payload() {
        let raw = json!({
            "id": "pane-1",
            "kind": "claude",
            "cwd": "/work/proj",
            "egress": true,
            "ws": "/work/proj",
            "model": "haiku",
            "auth": { "passphrase": "x" },
        });
        let opts: PtyCreateOpts = serde_json::from_value(raw).unwrap();
        assert_eq!(opts.id, "pane-1");
        assert_eq!(opts.kind, "claude");
        assert_eq!(opts.cwd.as_deref(), Some("/work/proj"));
        assert_eq!(opts.egress, Some(true));
        assert_eq!(opts.ws.as_deref(), Some("/work/proj"));
        assert_eq!(opts.model.as_deref(), Some("haiku"));
        assert!(opts.auth.is_some());
    }

    #[test]
    fn pty_create_opts_tolerates_a_bare_terminal_payload() {
        // panels/terminal.js always spreads every key, but cwd/egress/ws/
        // model/auth are each `undefined` for the common case (a fresh
        // terminal pane, no pinned model, no workspace open, no prior
        // reauth attempt) — JSON.stringify drops an `undefined` property
        // entirely, so the object Tauri actually deserializes can be
        // missing any/all of them.
        let raw = json!({ "id": "pane-2", "kind": "terminal" });
        let opts: PtyCreateOpts = serde_json::from_value(raw).unwrap();
        assert_eq!(opts.cwd, None);
        assert_eq!(opts.egress, None);
        assert_eq!(opts.ws, None);
        assert_eq!(opts.model, None);
        assert!(opts.auth.is_none());
    }

    #[test]
    fn pty_create_opts_rejects_a_payload_missing_the_required_id_or_kind() {
        assert!(serde_json::from_value::<PtyCreateOpts>(json!({ "kind": "terminal" })).is_err());
        assert!(serde_json::from_value::<PtyCreateOpts>(json!({ "id": "pane-3" })).is_err());
    }

    // ================= is_agent_kind — the fail-closed predicate =================

    fn builtins_only() -> Vec<AgentEntry> {
        custom_agents::merge_agents(agent_spawn::AGENTS, &Value::Null)
    }

    #[test]
    fn is_agent_kind_true_for_every_builtin() {
        let agents = builtins_only();
        for kind in agent_spawn::AGENTS {
            assert!(
                is_agent_kind(&agents, kind),
                "{kind} should be an agent kind"
            );
        }
    }

    #[test]
    fn is_agent_kind_true_for_a_vetted_custom_agent() {
        let customs = json!([{ "id": "aider", "label": "Aider", "bin": "aider" }]);
        let agents = custom_agents::merge_agents(agent_spawn::AGENTS, &customs);
        assert!(is_agent_kind(&agents, "aider"));
    }

    #[test]
    fn is_agent_kind_false_for_a_plain_terminal() {
        assert!(!is_agent_kind(&builtins_only(), "terminal"));
    }

    #[test]
    fn is_agent_kind_false_for_an_unrecognized_kind() {
        assert!(!is_agent_kind(&builtins_only(), "some-unknown-kind"));
    }

    #[test]
    fn is_agent_kind_false_for_a_custom_id_that_was_never_vetted_in() {
        assert!(!is_agent_kind(&builtins_only(), "aider"));
    }

    #[test]
    fn is_agent_kind_false_for_a_custom_that_failed_vetting() {
        let customs = json!([{ "id": "evil", "label": "Evil", "bin": "/bin/sh" }]);
        let agents = custom_agents::merge_agents(agent_spawn::AGENTS, &customs);
        assert!(!is_agent_kind(&agents, "evil"));
    }

    // ================= resolve_core_vault_root — TOME_CORE_VAULT half of the
    // TOME_BRAIN wiring (the ensure_brain half is $HOME-coupled — see
    // resolve_brain_env's own doc comment for why that half stays
    // integration-only) =================

    #[test]
    fn resolve_core_vault_root_returns_the_root_when_configured() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        let root_str = tmp.path().to_str().unwrap();
        assert_eq!(
            resolve_core_vault_root(Some(root_str)),
            Some(root_str.to_string())
        );
    }

    #[test]
    fn resolve_core_vault_root_is_none_when_no_core_vault_is_stored() {
        assert_eq!(resolve_core_vault_root(None), None);
    }

    #[test]
    fn resolve_core_vault_root_is_none_for_an_empty_or_unreadable_root() {
        assert_eq!(resolve_core_vault_root(Some("")), None);
        assert_eq!(
            resolve_core_vault_root(Some("/definitely/does/not/exist/core-vault-xyz")),
            None
        );
    }

    // ================= pane_env — the login-shell PATH override + layering ================

    #[test]
    fn pane_env_overrides_path_with_the_login_shell_value() {
        let mut process_env = HashMap::new();
        process_env.insert("PATH".to_string(), "/usr/bin:/bin".to_string()); // launchd's bare PATH
        process_env.insert("HOME".to_string(), "/Users/tester".to_string());
        let harvested = "/usr/bin:/bin:/opt/homebrew/bin:/Users/tester/.local/bin";
        let env = pane_env(
            &process_env,
            harvested,
            &agent_env::AgentEnvExtras::default(),
        );
        let path = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.as_str());
        assert_eq!(path, Some(harvested));
    }

    #[test]
    fn pane_env_never_carries_a_provider_secret_for_a_plain_terminal() {
        let mut process_env = HashMap::new();
        process_env.insert(
            "ANTHROPIC_API_KEY".to_string(),
            "sk-ant-should-not-leak".to_string(),
        );
        let env = pane_env(
            &process_env,
            "/usr/bin",
            &agent_env::AgentEnvExtras::default(),
        );
        assert!(
            env.iter().all(|(k, _)| k != "ANTHROPIC_API_KEY"),
            "pane_env with is_agent:false must never carry a provider credential"
        );
    }

    #[test]
    fn pane_env_carries_secrets_only_when_is_agent_is_set() {
        let mut secrets = HashMap::new();
        secrets.insert("ANTHROPIC_API_KEY".to_string(), "sk-ant-x".to_string());
        let extras = agent_env::AgentEnvExtras {
            is_agent: true,
            secrets,
            ..Default::default()
        };
        let env = pane_env(&HashMap::new(), "/usr/bin", &extras);
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "ANTHROPIC_API_KEY")
                .map(|(_, v)| v.as_str()),
            Some("sk-ant-x")
        );
    }

    #[test]
    fn pane_env_carries_proxy_vars_only_when_gapped() {
        let extras = agent_env::AgentEnvExtras {
            proxy_port: Some(54321),
            ..Default::default()
        };
        let env = pane_env(&HashMap::new(), "/usr/bin", &extras);
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "HTTP_PROXY")
                .map(|(_, v)| v.as_str()),
            Some("http://127.0.0.1:54321")
        );
        let ungapped = pane_env(
            &HashMap::new(),
            "/usr/bin",
            &agent_env::AgentEnvExtras::default(),
        );
        assert!(ungapped.iter().all(|(k, _)| k != "HTTP_PROXY"));
    }

    #[test]
    fn pane_env_still_sets_the_fixed_term_pair() {
        let env = pane_env(
            &HashMap::new(),
            "/usr/bin",
            &agent_env::AgentEnvExtras::default(),
        );
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("TERM"), Some("xterm-256color".to_string()));
        assert_eq!(get("COLORTERM"), Some("truecolor".to_string()));
    }

    // ================= resolve_gapped_spawn — TOME-001's three-way OS rule ================

    fn refuse_strategy() -> egress::linux::SandboxStrategy {
        egress::linux::SandboxStrategy::Refuse {
            reason: "install bubblewrap".to_string(),
        }
    }

    #[test]
    fn resolve_gapped_spawn_wraps_in_sandbox_exec_on_macos_regardless_of_linux_strategy() {
        for linux_strategy in [
            egress::linux::SandboxStrategy::Bwrap,
            egress::linux::SandboxStrategy::SelfUnshare,
            refuse_strategy(),
        ] {
            match resolve_gapped_spawn(
                HostOs::MacOs,
                Path::new("/tmp/tome-test"),
                4321,
                Path::new("/Users/test"),
                linux_strategy,
            ) {
                GappedSpawnDecision::Sandbox { cmd, args } => {
                    assert_eq!(cmd, "/usr/bin/sandbox-exec");
                    // F-01/F-03: the profile is built HERE, from the
                    // pane's real proxy port and the config dir — pinning
                    // both the port-naming loopback rule and the subpath
                    // config-dir confinement. The Docker-socket denies
                    // (container-runtime escape) ride along on the same
                    // profile.
                    assert_eq!(
                        args,
                        vec![
                            "-p".to_string(),
                            egress::seatbelt::seatbelt_profile(
                                Path::new("/tmp/tome-test"),
                                4321,
                                Path::new("/Users/test")
                            )
                        ]
                    );
                    // Pin that the home-dir-derived Docker socket literal
                    // made it into the profile built for the spawn.
                    assert!(args[1].contains(
                        "(deny file-read* (literal \"/Users/test/.docker/run/docker.sock\"))"
                    ));
                }
                GappedSpawnDecision::Linux(_) => panic!("expected Sandbox on macOS, got Linux(_)"),
                GappedSpawnDecision::RefuseUnsupportedOs => {
                    panic!("expected Sandbox on macOS, got RefuseUnsupportedOs")
                }
            }
        }
    }

    #[test]
    fn resolve_gapped_spawn_passes_the_linux_strategy_through_unchanged_on_linux() {
        for linux_strategy in [
            egress::linux::SandboxStrategy::Bwrap,
            egress::linux::SandboxStrategy::SelfUnshare,
            refuse_strategy(),
        ] {
            let decision = resolve_gapped_spawn(
                HostOs::Linux,
                Path::new("/irrelevant-on-linux"),
                0,
                Path::new("/irrelevant-on-linux"),
                linux_strategy.clone(),
            );
            assert!(
                matches!(decision, GappedSpawnDecision::Linux(ref s) if *s == linux_strategy),
                "expected Linux({linux_strategy:?}) passthrough"
            );
        }
    }

    #[test]
    fn resolve_gapped_spawn_refuses_on_any_os_other_than_macos_or_linux() {
        assert!(matches!(
            resolve_gapped_spawn(
                HostOs::Other,
                Path::new("/irrelevant"),
                0,
                Path::new("/irrelevant"),
                refuse_strategy()
            ),
            GappedSpawnDecision::RefuseUnsupportedOs
        ));
        // Even a real Bwrap/SelfUnshare verdict must not leak through on an
        // OS this app doesn't ship a Linux sandbox for — HostOs::Other
        // never reads linux_strategy at all.
        assert!(matches!(
            resolve_gapped_spawn(
                HostOs::Other,
                Path::new("/irrelevant"),
                0,
                Path::new("/irrelevant"),
                egress::linux::SandboxStrategy::Bwrap
            ),
            GappedSpawnDecision::RefuseUnsupportedOs
        ));
    }

    // ================= build_linux_wrap_argv =================

    fn sample_linux_spec() -> egress::linux::GappedSpawnSpec {
        egress::linux::GappedSpawnSpec {
            pane_id: "pty-1".to_string(),
            proxy_port: 54321,
            host_socket_path: PathBuf::from("/run/user/1000/tome/pane-pty-1.sock"),
            app_config_dir: PathBuf::from("/home/tester/.config/tome"),
            shim_path: PathBuf::from("/opt/tome/tome-shim"),
            inner_argv: vec![
                "/bin/zsh".to_string(),
                "-l".to_string(),
                "-c".to_string(),
                "claude".to_string(),
            ],
            headless: false,
            allow_read: vec![PathBuf::from("/usr"), PathBuf::from("/home/tester/proj")],
            allow_write: vec![PathBuf::from("/home/tester/proj"), PathBuf::from("/tmp")],
        }
    }

    #[test]
    fn build_linux_wrap_argv_bwrap_matches_egress_linux_build_bwrap_argv() {
        let spec = sample_linux_spec();
        assert_eq!(
            build_linux_wrap_argv(&egress::linux::SandboxStrategy::Bwrap, &spec).unwrap(),
            egress::linux::build_bwrap_argv(&spec)
        );
    }

    #[test]
    fn build_linux_wrap_argv_self_unshare_matches_egress_linux_build_self_unshare_argv() {
        let spec = sample_linux_spec();
        assert_eq!(
            build_linux_wrap_argv(&egress::linux::SandboxStrategy::SelfUnshare, &spec).unwrap(),
            egress::linux::build_self_unshare_argv(&spec)
        );
    }

    #[test]
    fn build_linux_wrap_argv_refuse_returns_the_reason_as_an_error_rather_than_panicking() {
        let spec = sample_linux_spec();
        let strategy = egress::linux::SandboxStrategy::Refuse {
            reason: "install bubblewrap".to_string(),
        };
        assert_eq!(
            build_linux_wrap_argv(&strategy, &spec),
            Err("install bubblewrap".to_string())
        );
    }

    // ================= login_shell_argv =================

    #[test]
    fn login_shell_argv_is_a_bare_login_shell_with_no_agent_cmd() {
        assert_eq!(
            login_shell_argv("/bin/zsh", None),
            vec!["/bin/zsh".to_string(), "-l".to_string()]
        );
    }

    #[test]
    fn login_shell_argv_runs_the_agent_command_via_dash_c() {
        assert_eq!(
            login_shell_argv("/bin/zsh", Some("claude")),
            vec![
                "/bin/zsh".to_string(),
                "-l".to_string(),
                "-c".to_string(),
                "claude".to_string()
            ]
        );
    }

    // ================= shim_path_in / resolve_shim_path's pure half =================

    #[test]
    fn shim_path_in_dev_mode_has_no_target_triple_suffix() {
        assert_eq!(
            shim_path_in(Path::new("/app/target/debug"), None),
            PathBuf::from("/app/target/debug/tome-shim")
        );
    }

    #[test]
    fn shim_path_in_bundled_mode_appends_the_target_triple() {
        assert_eq!(
            shim_path_in(Path::new("/usr/lib/tome"), Some("x86_64-unknown-linux-gnu")),
            PathBuf::from("/usr/lib/tome/tome-shim-x86_64-unknown-linux-gnu")
        );
    }

    // ================= build_pty_command =================

    #[test]
    fn build_pty_command_is_a_bare_login_shell_for_a_terminal_pane() {
        let cmd = build_pty_command("/bin/sh", None, Path::new("/tmp"), &[], None);
        let argv: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv, vec!["/bin/sh".to_string(), "-l".to_string()]);
        assert_eq!(cmd.get_cwd().unwrap().to_string_lossy(), "/tmp");
    }

    #[test]
    fn build_pty_command_runs_the_agent_command_via_dash_c() {
        let cmd = build_pty_command("/bin/zsh", Some("claude"), Path::new("/work"), &[], None);
        let argv: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            vec![
                "/bin/zsh".to_string(),
                "-l".to_string(),
                "-c".to_string(),
                "claude".to_string()
            ]
        );
    }

    #[test]
    fn build_pty_command_wraps_the_whole_line_in_sandbox_exec_when_gapped() {
        let sandbox = SandboxWrap::Prefix {
            cmd: "/usr/bin/sandbox-exec".to_string(),
            args: vec!["-p".to_string(), "PROFILE".to_string()],
        };
        let cmd = build_pty_command(
            "/bin/zsh",
            Some("claude"),
            Path::new("/work"),
            &[],
            Some(&sandbox),
        );
        let argv: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            vec![
                "/usr/bin/sandbox-exec".to_string(),
                "-p".to_string(),
                "PROFILE".to_string(),
                "/bin/zsh".to_string(),
                "-l".to_string(),
                "-c".to_string(),
                "claude".to_string(),
            ]
        );
    }

    #[test]
    fn build_pty_command_uses_a_full_linux_argv_verbatim_ignoring_shell_and_agent_cmd() {
        // The defining structural difference from Prefix: shell/agent_cmd
        // are NOT consulted at all when sandbox is Full — the whole argv
        // (already ending in this same pane's login_shell_argv, folded in
        // earlier via GappedSpawnSpec::inner_argv) is used exactly as
        // given.
        let argv = vec![
            "bwrap".to_string(),
            "--unshare-user".to_string(),
            "--unshare-net".to_string(),
            "--".to_string(),
            "/opt/tome/tome-shim".to_string(),
        ];
        let sandbox = SandboxWrap::Full { argv: argv.clone() };
        // Deliberately mismatched shell/agent_cmd — must be completely
        // ignored.
        let cmd = build_pty_command(
            "/bin/NEVER-USED",
            Some("also-never-used"),
            Path::new("/work"),
            &[],
            Some(&sandbox),
        );
        let got: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, argv);
    }

    #[test]
    fn build_pty_command_env_is_exactly_what_was_given_not_merged_with_this_process() {
        // Seed a known value into THIS process's env first, so the leak-check
        // below stays meaningful even on a minimal CI container (for example fedora)
        // that sets neither USER nor LOGNAME.
        std::env::set_var("USER", "tome-test-user");
        let env = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/home/x".to_string()),
        ];
        let cmd = build_pty_command("/bin/sh", None, Path::new("/tmp"), &env, None);
        assert_eq!(cmd.get_env("PATH").unwrap().to_string_lossy(), "/usr/bin");
        assert_eq!(cmd.get_env("HOME").unwrap().to_string_lossy(), "/home/x");
        assert!(
            std::env::var("USER").is_ok() || std::env::var("LOGNAME").is_ok(),
            "test precondition: expected USER or LOGNAME to be set in the test process"
        );
        assert!(cmd.get_env("USER").is_none() || std::env::var("USER").is_err());
    }

    // ================= evaluate_reauth — TOME-001's three-way outcome =================

    #[test]
    fn evaluate_reauth_needs_credentials_when_nothing_was_supplied() {
        assert!(matches!(
            evaluate_reauth(false, false),
            ReauthOutcome::NeedsCredentials
        ));
    }

    #[test]
    fn evaluate_reauth_rejects_a_supplied_but_wrong_credential() {
        assert!(matches!(
            evaluate_reauth(true, false),
            ReauthOutcome::Rejected
        ));
    }

    #[test]
    fn evaluate_reauth_accepts_a_verified_credential() {
        assert!(matches!(
            evaluate_reauth(true, true),
            ReauthOutcome::Verified
        ));
    }

    // ================= pty_authority integration sanity (already pinned in
    // pty_authority.rs — these two just prove the wiring above reads them
    // the way pty_create actually calls them) =================

    #[test]
    fn ungapped_spawn_with_configured_auth_needs_the_reauth_ceremony() {
        assert!(pty_authority::unrestricted_spawn_needs_reauth(false, true));
    }

    #[test]
    fn gapped_spawn_never_needs_the_reauth_ceremony_regardless_of_auth_config() {
        assert!(!pty_authority::unrestricted_spawn_needs_reauth(true, true));
        assert!(!pty_authority::unrestricted_spawn_needs_reauth(true, false));
    }
}
