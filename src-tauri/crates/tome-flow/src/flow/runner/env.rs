//! The injected environment seam `flow::runner`'s scheduling core is driven
//! through — direct translation of `flow-runner.js`'s `init(opts)` module-
//! level closures (`canOpenFile`, `buildAgentEnv`, `closeAgentEnv`,
//! `egressDefault`, `logEvent`, `spawn`) plus one Rust-only addition
//! (`push`, standing in for the `win` parameter `startRun(flowPath, win)`
//! closes over in JS) into a plain `Clone` struct of `Arc<dyn Fn>`s. Rust
//! has no module-level `let` to reassign the way `flow-runner.js` does, so
//! [`RunnerEnv`] is built fresh per call instead — a hand-built one with
//! fake closures for tests (mirroring the JS suite's own `install()`
//! helper), a real one for `runs:*` commands.
//!
//! ## Where this file's contents end, and why
//!
//! This module holds only the SEAM TYPES — [`RunnerEnv`] itself,
//! [`SandboxWrap`], [`BuiltEnv`], [`BoxFuture`] — with no knowledge of how
//! a real `RunnerEnv` gets built. Before plan step 2.1's `tome-flow`
//! extraction, this file also carried that production wiring
//! (`production_env`, `frozen_egress_default`, and every private helper
//! `production_env` calls: `can_open_flow`, `lexical_resolve`,
//! `resolve_shim_path`, `shim_path_in`, `current_linux_sandbox_strategy`,
//! `build_production_agent_env`) — all of it reaching `tauri::AppHandle`
//! or `crate::state::AppState` one way or another, which is exactly what
//! this crate cannot depend on. That half now lives in the `tome` crate
//! itself, as `flow_env.rs` (`tome_lib::flow_env`) — see that module's own
//! doc comment for the reuse-vs-reimplement rationale that used to live
//! here, and for `frozen_egress_default`'s TOCTOU-closing contract.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use super::spawn::{SpawnOutcome, SpawnRequest};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// How a gapped node's argv gets wrapped, once its `PaneProxy` is up —
/// mirrors `ipc::pty::pty_create`'s own `SandboxWrap` (that enum is private
/// to `ipc/pty.rs`; this is an independent copy of the same two shapes, not
/// a reuse of it — see this module's doc comment).
#[derive(Clone)]
pub enum SandboxWrap {
    /// macOS: `sandbox-exec -p <profile>` — a PREFIX. The caller appends
    /// the node's own `[cmd, ...args]` after `args` and spawns `cmd` in
    /// place of the node's own.
    Prefix { cmd: String, args: Vec<String> },
    /// Linux: the ENTIRE argv, already fully assembled (bwrap or
    /// `tome-shim --self-unshare`) with the node's own `[cmd, ...args]`
    /// embedded as its trailing `inner_argv`. Nothing left to append.
    Full { argv: Vec<String> },
}

pub struct BuiltEnv {
    pub env: Vec<(String, String)>,
    pub sandbox: Option<SandboxWrap>,
}

/// Every seam `flow::runner`'s scheduling core reads instead of touching
/// the OS/filesystem/Tauri directly. See this module's doc comment.
#[derive(Clone)]
pub struct RunnerEnv {
    /// LEXICAL confinement only (never resolves a symlink) — mirrors
    /// `flow-runner.js`'s own `canOpenFile` contract exactly (its own
    /// comment: "a LEXICAL check — it never resolves a symlink"; the REAL,
    /// symlink-safe confinement is `flow::confine::confine_real_abs`,
    /// applied at every later sink).
    pub can_open_file: Arc<dyn Fn(&Path) -> bool + Send + Sync>,
    /// `(pane_id, gapped, inner_argv, workspace_root) -> Result<BuiltEnv, reason>` —
    /// mirrors `buildAgentEnv({ paneId, agent: true, gapped, ws: undefined })`.
    /// `inner_argv` (the node's own already-resolved `[cmd, ...args]`) is a
    /// Rust-only addition to the JS shape — see this module's doc comment
    /// on why the Linux wrap needs it up front, unlike JS's macOS-only
    /// `{cmd,args}` prefix shape. `workspace_root` is the node's workspace
    /// root (the runner's `SpawnRequest.cwd`), needed by the Linux
    /// curated-mount allow-list.
    pub build_agent_env: Arc<
        dyn Fn(String, bool, Vec<String>, std::path::PathBuf) -> BoxFuture<Result<BuiltEnv, String>>
            + Send
            + Sync,
    >,
    /// Tears a node's pane-scoped proxy down — mirrors `egress.closePane`.
    pub close_agent_env: Arc<dyn Fn(&str) + Send + Sync>,
    /// The same egress default a freshly spawned pane would read.
    pub egress_default: Arc<dyn Fn() -> BoxFuture<bool> + Send + Sync>,
    /// Persistent event log — mirrors `events.logEvent`.
    pub log_event: Arc<dyn Fn(&str, Vec<(String, Value)>) + Send + Sync>,
    /// The `runs:changed` push — mirrors `win?.webContents.send('runs:changed', snapshotAll())`.
    /// A Rust-only field: JS closes over `win` per `startRun` call instead;
    /// folding it into the env is simpler here and every real call site
    /// only ever has one window to reach anyway.
    pub push: Arc<dyn Fn(Value) + Send + Sync>,
    /// The process-spawn backend — mirrors `spawn: childSpawn`.
    pub spawn: Arc<dyn Fn(SpawnRequest) -> SpawnOutcome + Send + Sync>,
    /// SIGTERM-to-SIGKILL escalation grace period — mirrors
    /// `flow-runner.js`'s `KILL_GRACE_MS` constant (5000ms in
    /// production, see `tome_lib::flow_env::production_env`). A field on
    /// the env rather than a bare constant so tests can shrink it to a few
    /// milliseconds and exercise the real escalation logic (`cancel_run`'s
    /// `arm_kill_timer`) against a REAL clock in real time, without a new
    /// Cargo dependency on tokio's `test-util` virtual-clock feature.
    pub kill_grace: std::time::Duration,
}
