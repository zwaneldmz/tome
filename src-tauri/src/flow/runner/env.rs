//! The injected environment seam `flow::runner`'s scheduling core is driven
//! through — direct translation of `flow-runner.js`'s `init(opts)` module-
//! level closures (`canOpenFile`, `buildAgentEnv`, `closeAgentEnv`,
//! `airgapDefault`, `logEvent`, `spawn`) plus one Rust-only addition
//! (`push`, standing in for the `win` parameter `startRun(flowPath, win)`
//! closes over in JS) into a plain `Clone` struct of `Arc<dyn Fn>`s. Rust
//! has no module-level `let` to reassign the way `flow-runner.js` does, so
//! [`RunnerEnv`] is built fresh per call instead — [`production_env`] for
//! real `runs:*` commands, a hand-built one with fake closures for tests
//! (mirroring the JS suite's own `install()` helper).
//!
//! ## Where this deliberately reimplements rather than reuses
//!
//! `index.js`'s real `buildAgentEnv` is ONE function shared verbatim by
//! `createPty` and `flow-runner.js` (its own comment: "Factored out of
//! createPty because background flow runs spawn agents too … a duplicated
//! copy … is how [a sandbox gap] happens six months from now"). This
//! module's [`build_production_agent_env`] cannot literally be that same
//! function: the equivalent Rust logic lives inside `ipc::pty::pty_create`
//! as several PRIVATE functions (`resolve_gapped_spawn`, `pane_env`, …),
//! and `ipc/pty.rs` is a different slice this task's brief explicitly says
//! not to touch. What IS shared, and reused directly rather than
//! reimplemented, is every lower-level PUBLIC primitive both paths are
//! built from: `agent_env::compose_agent_env`, `login_env::login_env`,
//! `airgap::seatbelt::seatbelt_profile`, every `airgap::linux` builder, and
//! `ipc::airgap::create_gapped_pane_proxy`/`close_pane_and_proxy` (both
//! `pub(crate)`, reachable from here). The one genuinely NEW piece —
//! `GappedSpawnSpec::headless: true` and `SandboxWrap::Full` for Linux —
//! is new because `ipc::pty::pty_create` has never had a headless caller
//! before this slice (see that file's own doc comment, which anticipates
//! exactly this: "the day a flow spawn path lands, IT decides this
//! independently").

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use super::spawn::{SpawnOutcome, SpawnRequest};
use crate::state::AppState;

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
    /// `(pane_id, gapped, inner_argv) -> Result<BuiltEnv, reason>` — mirrors
    /// `buildAgentEnv({ paneId, agent: true, gapped, ws: undefined })`.
    /// `inner_argv` (the node's own already-resolved `[cmd, ...args]`) is a
    /// Rust-only addition to the JS shape — see this module's doc comment
    /// on why the Linux wrap needs it up front, unlike JS's macOS-only
    /// `{cmd,args}` prefix shape.
    pub build_agent_env:
        Arc<dyn Fn(String, bool, Vec<String>) -> BoxFuture<Result<BuiltEnv, String>> + Send + Sync>,
    /// Tears a node's pane-scoped proxy down — mirrors `airgap.closePane`.
    pub close_agent_env: Arc<dyn Fn(&str) + Send + Sync>,
    /// The same air-gap default a freshly spawned pane would read.
    pub airgap_default: Arc<dyn Fn() -> BoxFuture<bool> + Send + Sync>,
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
    /// production, see [`production_env`]). A field on the env rather
    /// than a bare constant so tests can shrink it to a few milliseconds
    /// and exercise the real escalation logic (`cancel_run`'s
    /// `arm_kill_timer`) against a REAL clock in real time, without a new
    /// Cargo dependency on tokio's `test-util` virtual-clock feature
    /// (this slice does not own `Cargo.toml` — see this crate's other
    /// duplication notes for the same constraint applied elsewhere).
    pub kill_grace: std::time::Duration,
}

// ---- production wiring ----

/// `path.resolve(p)`'s single-argument behaviour — duplicated from
/// `confine.rs`'s private `resolve1`/`normalize_lexically` (that module
/// does not export a lexical-only, `open_folders`-aware predicate matching
/// `isConfinedPath`, and this slice does not own `confine.rs` to add one —
/// see this module's doc comment on the same constraint applying to
/// `ipc::pty`). Small and self-contained enough that duplicating it costs
/// far less than widening a file this task's brief says not to touch.
fn lexical_resolve(p: &Path) -> PathBuf {
    use std::path::Component;
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut root = PathBuf::new();
    let mut stack: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in abs.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => root.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                stack.pop();
            }
            Component::Normal(seg) => stack.push(seg),
        }
    }
    root.extend(stack);
    root
}

/// Production `can_open_file` — mirrors `index.js`'s `isConfinedPath`
/// (lexical half only, matching `canOpenFile`'s own contract).
fn can_open_flow(state: &AppState, p: &Path) -> bool {
    let folders_synced = *state
        .folders_synced
        .read()
        .expect("AppState.folders_synced lock poisoned");
    if !folders_synced || p.as_os_str().is_empty() {
        return false;
    }
    let abs = lexical_resolve(p);
    let open_folders = state
        .open_folders
        .read()
        .expect("AppState.open_folders lock poisoned");
    open_folders.iter().any(|f| abs.starts_with(f))
}

/// Resolves the `tome-shim` sidecar path — duplicated from `ipc::pty`'s
/// private `resolve_shim_path`/`shim_path_in` (same constraint as
/// `lexical_resolve` above: that module is a different slice, not this
/// one's to widen).
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

fn shim_path_in(dir: &Path, target_triple: Option<&str>) -> PathBuf {
    match target_triple {
        Some(triple) => dir.join(format!("tome-shim-{triple}")),
        None => dir.join("tome-shim"),
    }
}

/// Real-environment fallback-ladder verdict — duplicated wrapper around
/// `airgap::linux::probe_sandbox_strategy` for the same reason
/// `ipc::pty::current_linux_sandbox_strategy` exists there and is private.
/// See `airgap::linux`'s own "Verification boundary" doc comment: this
/// specific line is never type-checked by this crate's native macOS gates.
#[cfg(target_os = "linux")]
fn current_linux_sandbox_strategy() -> crate::airgap::linux::SandboxStrategy {
    crate::airgap::linux::probe_sandbox_strategy()
}
#[cfg(not(target_os = "linux"))]
fn current_linux_sandbox_strategy() -> crate::airgap::linux::SandboxStrategy {
    crate::airgap::linux::SandboxStrategy::Refuse {
        reason: String::new(),
    }
}

/// The real `buildAgentEnv` for a headless flow node — see this module's
/// doc comment for exactly what is reused vs. reimplemented here.
/// `inner_argv` is the node's own already-resolved `[cmd, ...args]`
/// (`agent_spawn::build_headless_spawn`'s output) — threaded straight into
/// the Linux wrap's `GappedSpawnSpec::inner_argv` with `headless: true`
/// (THE first headless caller of `airgap::linux` in this tree).
async fn build_production_agent_env(
    app: &AppHandle,
    pane_id: &str,
    gapped: bool,
    inner_argv: Vec<String>,
) -> Result<BuiltEnv, String> {
    let login = crate::login_env::login_env().await;
    let mut process_env: std::collections::HashMap<String, String> = std::env::vars().collect();
    process_env.insert("PATH".to_string(), login.path.clone());
    let mut extras = crate::agent_env::AgentEnvExtras {
        is_agent: true,
        secrets: login.secrets.clone(),
        ..Default::default()
    };

    if !gapped {
        let env = crate::agent_env::compose_agent_env(&process_env, &extras)
            .into_iter()
            .collect();
        return Ok(BuiltEnv { env, sandbox: None });
    }

    let state = app.state::<AppState>();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;

    if cfg!(target_os = "macos") {
        let proxy = crate::ipc::airgap::create_gapped_pane_proxy(app, state.inner(), pane_id, None)
            .await
            .map_err(|e| e.to_string())?;
        extras.proxy_port = Some(proxy.port());
        let env = crate::agent_env::compose_agent_env(&process_env, &extras)
            .into_iter()
            .collect();
        let profile = crate::airgap::seatbelt::seatbelt_profile(&dir);
        return Ok(BuiltEnv {
            env,
            sandbox: Some(SandboxWrap::Prefix {
                cmd: "/usr/bin/sandbox-exec".to_string(),
                args: vec!["-p".to_string(), profile],
            }),
        });
    }

    if cfg!(target_os = "linux") {
        let strategy = current_linux_sandbox_strategy();
        if let crate::airgap::linux::SandboxStrategy::Refuse { reason } = &strategy {
            // Fail closed BEFORE anything is created — no proxy to tear
            // down, matching pty_create's own rung-3 refusal ordering.
            return Err(reason.clone());
        }
        let sock_path = crate::airgap::linux::pane_socket_path_from_env(pane_id)
            .ok_or_else(|| "gapped flow node refused: pane id is not a valid loopback-bridge socket path component".to_string())?;
        if let Some(parent) = sock_path.parent() {
            crate::airgap::linux::ensure_pane_socket_dir(parent).map_err(|e| e.to_string())?;
        }
        let shim_path = resolve_shim_path()?;
        let proxy = crate::ipc::airgap::create_gapped_pane_proxy(
            app,
            state.inner(),
            pane_id,
            Some(sock_path.clone()),
        )
        .await
        .map_err(|e| e.to_string())?;
        extras.proxy_port = Some(proxy.port());
        let env = crate::agent_env::compose_agent_env(&process_env, &extras)
            .into_iter()
            .collect();

        let spec = crate::airgap::linux::GappedSpawnSpec {
            pane_id: pane_id.to_string(),
            proxy_port: proxy.port(),
            host_socket_path: sock_path,
            app_config_dir: dir,
            shim_path,
            inner_argv,
            // The flag ipc::pty.rs's own doc comment anticipated: THIS is
            // the headless spawn path landing.
            headless: true,
        };
        let argv = match &strategy {
            crate::airgap::linux::SandboxStrategy::Bwrap => {
                crate::airgap::linux::build_bwrap_argv(&spec)
            }
            crate::airgap::linux::SandboxStrategy::SelfUnshare => {
                crate::airgap::linux::build_self_unshare_argv(&spec)
            }
            crate::airgap::linux::SandboxStrategy::Refuse { .. } => {
                unreachable!("Refuse handled above")
            }
        };
        return Ok(BuiltEnv {
            env,
            sandbox: Some(SandboxWrap::Full { argv }),
        });
    }

    // Any OS other than macOS/Linux — refuse rather than spawn a gapped
    // node with nothing enforcing its proxy env vars (the exact TOME-001
    // hole this rewrite exists to close), same rule pty_create enforces.
    Err("gapped flow nodes are only supported on macOS and Linux — refusing to spawn unenforced on this OS".to_string())
}

/// Freezes an already-resolved `airgap-default` reading into the same
/// closure shape [`RunnerEnv::airgap_default`] expects, so every future call
/// returns that ONE value rather than re-reading the store. Used by
/// `ipc::runs::runs_start`'s own TOME-001 re-auth gate (mirroring
/// `ipc::pty::pty_create`'s identical ceremony): the gate resolves
/// `airgap_default` ONCE to decide whether a fresh passphrase/TOTP is
/// required, then freezes that value into the `RunnerEnv` it hands to
/// [`start_run`] — so the gate's decision and every node this run actually
/// spawns are PROVABLY looking at the same resolved gapped state, not two
/// independent store reads a concurrent `store_set("airgap-default", ...)`
/// could race apart (a compromised renderer racing its own `store_set`
/// between the gate's read and `start_run`'s otherwise-separate internal
/// read must not be able to make the gate see "gapped" while nodes spawn
/// ungapped).
///
/// [`start_run`]: super::start_run
pub fn frozen_airgap_default(value: bool) -> Arc<dyn Fn() -> BoxFuture<bool> + Send + Sync> {
    Arc::new(move || Box::pin(async move { value }) as BoxFuture<bool>)
}

/// Builds the real `RunnerEnv` a `runs:*` command wires the scheduling core
/// through. Cheap to call per-command (every closure only clones an
/// `AppHandle`, itself `Arc`-backed) — no boot-time `init()` step needed,
/// unlike JS's module-level closures.
pub fn production_env(app: AppHandle) -> RunnerEnv {
    RunnerEnv {
        can_open_file: {
            let app = app.clone();
            Arc::new(move |p: &Path| can_open_flow(app.state::<AppState>().inner(), p))
        },
        build_agent_env: {
            let app = app.clone();
            Arc::new(
                move |pane_id: String, gapped: bool, inner_argv: Vec<String>| {
                    let app = app.clone();
                    Box::pin(async move {
                        build_production_agent_env(&app, &pane_id, gapped, inner_argv).await
                    }) as BoxFuture<Result<BuiltEnv, String>>
                },
            )
        },
        close_agent_env: {
            let app = app.clone();
            Arc::new(move |pane_id: &str| {
                let state = app.state::<AppState>();
                crate::ipc::airgap::close_pane_and_proxy(&app, state.inner(), pane_id);
            })
        },
        airgap_default: {
            let app = app.clone();
            Arc::new(move || {
                let app = app.clone();
                Box::pin(async move {
                    let locked = *app
                        .state::<AppState>()
                        .locked
                        .read()
                        .expect("AppState.locked lock poisoned");
                    let Ok(dir) = app.path().app_data_dir() else {
                        return true;
                    };
                    let value = tokio::task::spawn_blocking(move || {
                        crate::store::get(&dir, "airgap-default", locked)
                    })
                    .await
                    .unwrap_or(Value::Bool(true));
                    // `!== false` in the JS original: absent or anything but
                    // the literal `false` means "gap by default".
                    value != Value::Bool(false)
                }) as BoxFuture<bool>
            })
        },
        log_event: {
            let app = app.clone();
            Arc::new(move |kind: &str, fields: Vec<(String, Value)>| {
                crate::events::log_event(&app, kind, fields);
            })
        },
        push: {
            let app = app.clone();
            Arc::new(move |snapshot: Value| {
                let _ = app.emit("runs:changed", snapshot);
            })
        },
        spawn: Arc::new(super::spawn::spawn_process),
        kill_grace: std::time::Duration::from_millis(5000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_resolve_normalizes_dot_and_dotdot_without_touching_disk() {
        assert_eq!(
            lexical_resolve(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(lexical_resolve(Path::new("/a/./b")), PathBuf::from("/a/b"));
    }

    #[test]
    fn can_open_flow_denies_until_folders_synced() {
        let state = AppState::new();
        *state.open_folders.write().unwrap() = vec![PathBuf::from("/work/proj")];
        assert!(!can_open_flow(&state, Path::new("/work/proj/x.flow.json")));
    }

    #[test]
    fn can_open_flow_allows_a_path_under_an_open_folder_once_synced() {
        let state = AppState::new();
        *state.open_folders.write().unwrap() = vec![PathBuf::from("/work/proj")];
        *state.folders_synced.write().unwrap() = true;
        assert!(can_open_flow(
            &state,
            Path::new("/work/proj/.tome/flows/x.flow.json")
        ));
        assert!(!can_open_flow(&state, Path::new("/elsewhere/x.flow.json")));
    }

    #[tokio::test]
    async fn frozen_airgap_default_always_returns_the_value_it_was_built_with() {
        let gapped = frozen_airgap_default(true);
        assert!((gapped)().await);
        assert!((gapped)().await, "stays frozen across repeated calls");
        let ungapped = frozen_airgap_default(false);
        assert!(!(ungapped)().await);
    }

    #[test]
    fn shim_path_in_appends_the_target_triple_only_when_given() {
        assert_eq!(
            shim_path_in(Path::new("/opt/tome"), None),
            PathBuf::from("/opt/tome/tome-shim")
        );
        assert_eq!(
            shim_path_in(Path::new("/opt/tome"), Some("x86_64-unknown-linux-gnu")),
            PathBuf::from("/opt/tome/tome-shim-x86_64-unknown-linux-gnu")
        );
    }
}
