//! Builds the headless [`RunnerEnv`] `run_cmd::run` drives
//! `tome_flow::flow::runner::start_run` through — this crate's analogue of
//! the main crate's `flow_env.rs::production_env`, rebuilt from scratch
//! because `flow_env.rs` reaches `tauri::AppHandle`/`crate::state::AppState`
//! at almost every step and this binary has no Tauri dependency at all
//! (see `docs/remote-runner.md`'s "why no webview stack on a server"
//! note). What IS reused, directly and without reimplementation, is every
//! lower-level PUBLIC primitive `flow_env.rs` itself is built from:
//! [`tome_flow::agent_env::compose_agent_env`], [`tome_flow::login_env::login_env`],
//! [`tome_flow::egress::seatbelt::seatbelt_profile`], every
//! `tome_flow::egress::linux` builder, and
//! [`tome_flow::egress::proxy::PaneProxy`] — this file's own job is just
//! wiring those together without a `tauri::AppHandle` in the loop, the
//! same "reuse the primitives, rebuild the glue" split that module's own
//! doc comment describes for its own construction.
//!
//! ## Always gapped — frozen, not read from anywhere
//!
//! [`build`]'s `egress_default` closure always resolves to `true`. There
//! is no store, no lock screen, and no human to ask for a scheduled or
//! remote-triggered run — the project's own non-negotiable is explicit
//! that a background/scheduled agent spawn is ALWAYS gapped, and the
//! desktop app's own scheduler makes the identical choice for the same
//! reason (`src-tauri/src/schedule.rs::SCHEDULED_RUN_EGRESS`, "named and
//! tested on its own... rather than an inline `true`... so the property
//! ... is one grep away"). `tome-runner` has no OTHER kind of run at all —
//! every invocation is exactly this one, unattended, case — so this is
//! not a default that could be overridden by a future flag; there is no
//! flag, and there must never be one that flips this to `false`.
//!
//! ## Per-node proxy lifetime
//!
//! A gapped node's [`tome_flow::egress::proxy::PaneProxy`] must outlive the
//! node's own spawned process — its `Drop` impl tears the listener (and
//! any live tunnels) down. The desktop app keeps every pane's proxy alive
//! inside `AppState`, torn down by `ipc::egress::close_pane_and_proxy`
//! (called from `close_agent_env`). This binary has no `AppState`, so
//! [`ProxyRegistry`] is this file's own minimal stand-in: `build_agent_env`
//! inserts the freshly spawned proxy keyed by pane id before returning,
//! and the `close_agent_env` closure removes (and therefore drops) it —
//! `tome_flow::flow::runner::launch` already calls `close_agent_env` for
//! every node exactly once, on every exit path (success, failure, or
//! cancel), so this registry never needs its own cleanup sweep.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use tome_flow::agent_env::AgentEnvExtras;
use tome_flow::egress::proxy::{BlockedEvent, PaneProxy};
use tome_flow::flow::runner::env::{BoxFuture, BuiltEnv, RunnerEnv, SandboxWrap};

use crate::{egress_config, events, home};

/// Live per-pane egress proxies this process has spawned, keyed by pane
/// id — see this module's doc comment. `Default`-constructed once per
/// `run` invocation ([`build`]'s own `Arc`), shared between the
/// `build_agent_env`/`close_agent_env` closures.
#[derive(Default)]
struct ProxyRegistry(Mutex<HashMap<String, PaneProxy>>);

impl ProxyRegistry {
    fn insert(&self, pane_id: String, proxy: PaneProxy) {
        self.0
            .lock()
            .expect("ProxyRegistry mutex poisoned")
            .insert(pane_id, proxy);
    }

    /// Drops (and therefore shuts down) the pane's proxy, if any is still
    /// registered. A pane that was never gapped (or whose `build_agent_env`
    /// call failed before a proxy was ever spawned) has nothing here —
    /// silently a no-op, same as `EgressState::close_pane`'s own contract
    /// for an unknown id.
    fn close(&self, pane_id: &str) {
        self.0
            .lock()
            .expect("ProxyRegistry mutex poisoned")
            .remove(pane_id);
    }
}

/// `path.resolve(p)`-style lexical containment check — mirrors the main
/// crate's own `flow_env.rs::can_open_flow`, with a single fixed `root`
/// (derived once via [`tome_flow::flow::model::flow_root`], see [`build`])
/// standing in for that function's `AppState.open_folders` list: this
/// binary has no multi-folder "open workspace" concept, only the one
/// flow's own root. Worth being honest about in a doc comment: `root` is
/// DERIVED from the very `flow_path` this closure's one real caller
/// (`start_run`) checks it against, so for that call this is close to a
/// tautology rather than an independent trust boundary — the genuinely
/// load-bearing confinement for anything this process actually reads or
/// writes is `tome_flow::flow::confine::confine_real_abs`, applied deeper
/// inside `start_run`/`runner::launch` itself (real, symlink-resolving
/// containment, not lexical). This closure exists so `RunnerEnv`'s shape
/// stays the one documented seam every caller in this codebase builds —
/// see `flow::runner::env::RunnerEnv::can_open_file`'s own doc comment.
fn can_open_flow(root: &Path, p: &Path) -> bool {
    if p.as_os_str().is_empty() {
        return false;
    }
    home::lexical_resolve(p).starts_with(root)
}

/// Real-environment fallback-ladder verdict — duplicated wrapper around
/// `tome_flow::egress::linux::probe_sandbox_strategy` for the identical
/// reason the main crate's `flow_env.rs::current_linux_sandbox_strategy`
/// exists: that function is `#[cfg(target_os = "linux")]`-gated INSIDE
/// `tome-flow` (it reads real `/proc`/`$PATH` state), so it does not even
/// compile on macOS — this wrapper is what lets the REST of this file
/// call one OS-unconditional function instead of `#[cfg]`-gating every
/// call site. `pub(crate)` so `run_cmd`'s own fail-closed precheck (before
/// `start_run` is ever called — see this crate's top-level doc comment)
/// and this module's per-node `build_agent_env` both read the exact same
/// verdict function, rather than two copies that could in principle
/// diverge.
#[cfg(target_os = "linux")]
pub(crate) fn linux_sandbox_strategy() -> tome_flow::egress::linux::SandboxStrategy {
    tome_flow::egress::linux::probe_sandbox_strategy()
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn linux_sandbox_strategy() -> tome_flow::egress::linux::SandboxStrategy {
    tome_flow::egress::linux::SandboxStrategy::Refuse {
        reason: String::new(),
    }
}

/// Resolves the `tome-shim` binary's absolute path: a sibling of this
/// process's own executable. Unlike the desktop app's `flow_env.rs::resolve_shim_path`
/// (which resolves a Tauri-bundled sidecar, suffixed with the target
/// triple in a release build — see that function's own doc comment), a
/// server-side checkout builds both binaries with plain `cargo build
/// --workspace`, landing them in the same `target/{debug,release}/`
/// directory — so a bare sibling lookup, no triple suffix, is the right
/// (and simpler) equivalent here. See `docs/remote-runner.md`'s
/// prerequisites section.
fn resolve_shim_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| {
        format!("resolve tome-shim: could not determine this process's own binary path: {e}")
    })?;
    let dir = exe.parent().ok_or_else(|| {
        "resolve tome-shim: this process's own binary path has no parent directory".to_string()
    })?;
    let candidate = dir.join("tome-shim");
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(format!(
            "resolve tome-shim: no \"tome-shim\" binary found next to {} — build the whole workspace (`cargo build --workspace`) so both land in the same target directory",
            dir.display()
        ))
    }
}

/// The real `build_agent_env` for one headless flow node — see this
/// module's doc comment for exactly what's reused vs. rebuilt. `gapped`
/// is always `true` in this binary's own production wiring (see [`build`]'s
/// `egress_default`), but this still handles `false` correctly rather
/// than panicking, matching `flow_env.rs::build_production_agent_env`'s
/// identical shape for the same branch.
async fn build_agent_env(
    pane_id: &str,
    gapped: bool,
    inner_argv: Vec<String>,
    config_dir: &Path,
    state_dir: &Path,
    registry: &Arc<ProxyRegistry>,
) -> Result<BuiltEnv, String> {
    let login = tome_flow::login_env::login_env().await;
    let mut process_env: HashMap<String, String> = std::env::vars().collect();
    process_env.insert("PATH".to_string(), login.path.clone());
    let mut extras = AgentEnvExtras {
        is_agent: true,
        secrets: login.secrets.clone(),
        ..Default::default()
    };

    if !gapped {
        let env = tome_flow::agent_env::compose_agent_env(&process_env, &extras)
            .into_iter()
            .collect();
        return Ok(BuiltEnv { env, sandbox: None });
    }

    let allowed = egress_config::load_allowed(config_dir);
    let on_blocked = {
        let state_dir = state_dir.to_path_buf();
        move |evt: BlockedEvent| {
            // Only the coalesced signal is persisted — mirrors the
            // desktop app's own split (`egress::proxy`'s module doc
            // comment: `Coalesced` -> the persistent log, uncoalesced
            // `Attempt` -> a live push only). This binary has no live
            // push to fan `Attempt` out to (`RunnerEnv::push` is a
            // no-op here — see `build`), so it is simply dropped.
            if let BlockedEvent::Coalesced { host, count } = evt {
                events::append(
                    &state_dir,
                    "egress:blocked",
                    vec![
                        ("host".to_string(), json!(host)),
                        ("count".to_string(), json!(count)),
                    ],
                );
            }
        }
    };

    if cfg!(target_os = "macos") {
        let proxy = PaneProxy::spawn(allowed, None, on_blocked)
            .await
            .map_err(|e| e.to_string())?;
        extras.proxy_port = Some(proxy.port());
        let env = tome_flow::agent_env::compose_agent_env(&process_env, &extras)
            .into_iter()
            .collect();
        let profile = tome_flow::egress::seatbelt::seatbelt_profile(config_dir);
        registry.insert(pane_id.to_string(), proxy);
        return Ok(BuiltEnv {
            env,
            sandbox: Some(SandboxWrap::Prefix {
                cmd: "/usr/bin/sandbox-exec".to_string(),
                args: vec!["-p".to_string(), profile],
            }),
        });
    }

    if cfg!(target_os = "linux") {
        let strategy = linux_sandbox_strategy();
        if let tome_flow::egress::linux::SandboxStrategy::Refuse { reason } = &strategy {
            // Fail closed before anything is created — mirrors
            // `flow_env.rs`'s identical ordering (no proxy to tear down
            // on this path). `run_cmd`'s own precheck already refuses
            // the whole invocation before `start_run` is ever called
            // when the ladder resolves to Refuse (see this crate's
            // top-level doc comment); this is the defense-in-depth copy
            // for the vanishingly unlikely window where the environment
            // changes between that precheck and this call.
            return Err(reason.clone());
        }
        let sock_path = tome_flow::egress::linux::pane_socket_path_from_env(pane_id)
            .ok_or_else(|| {
                "gapped flow node refused: pane id is not a valid loopback-bridge socket path component"
                    .to_string()
            })?;
        if let Some(parent) = sock_path.parent() {
            tome_flow::egress::linux::ensure_pane_socket_dir(parent).map_err(|e| e.to_string())?;
        }
        let shim_path = resolve_shim_path()?;
        let proxy = PaneProxy::spawn(allowed, Some(sock_path.clone()), on_blocked)
            .await
            .map_err(|e| e.to_string())?;
        extras.proxy_port = Some(proxy.port());
        let env = tome_flow::agent_env::compose_agent_env(&process_env, &extras)
            .into_iter()
            .collect();

        let spec = tome_flow::egress::linux::GappedSpawnSpec {
            pane_id: pane_id.to_string(),
            proxy_port: proxy.port(),
            host_socket_path: sock_path,
            app_config_dir: config_dir.to_path_buf(),
            shim_path,
            inner_argv,
            // tome-runner only ever spawns headless flow nodes — there is
            // no interactive pty pane path in this binary at all.
            headless: true,
        };
        let argv = match &strategy {
            tome_flow::egress::linux::SandboxStrategy::Bwrap => {
                tome_flow::egress::linux::build_bwrap_argv(&spec)
            }
            tome_flow::egress::linux::SandboxStrategy::SelfUnshare => {
                tome_flow::egress::linux::build_self_unshare_argv(&spec)
            }
            tome_flow::egress::linux::SandboxStrategy::Refuse { .. } => {
                unreachable!("Refuse handled above")
            }
        };
        registry.insert(pane_id.to_string(), proxy);
        return Ok(BuiltEnv {
            env,
            sandbox: Some(SandboxWrap::Full { argv }),
        });
    }

    Err("gapped flow nodes are only supported on macOS and Linux — refusing to spawn unenforced on this OS".to_string())
}

/// Builds the real [`RunnerEnv`] `run_cmd::run` drives `start_run`
/// through. `flow_path` is the exact string `start_run` itself will be
/// called with — [`tome_flow::flow::model::flow_root`] derives this run's
/// confinement root from it once, up front, the same way `start_run`
/// derives its own internal `root` from that identical string.
pub fn build(flow_path: &str, config_dir: PathBuf, state_dir: PathBuf) -> RunnerEnv {
    let root = home::lexical_resolve(Path::new(&tome_flow::flow::model::flow_root(flow_path)));
    let registry: Arc<ProxyRegistry> = Arc::new(ProxyRegistry::default());

    RunnerEnv {
        can_open_file: Arc::new(move |p: &Path| can_open_flow(&root, p)),
        build_agent_env: {
            let registry = registry.clone();
            let config_dir = config_dir.clone();
            let state_dir = state_dir.clone();
            Arc::new(
                move |pane_id: String, gapped: bool, inner_argv: Vec<String>| {
                    let registry = registry.clone();
                    let config_dir = config_dir.clone();
                    let state_dir = state_dir.clone();
                    Box::pin(async move {
                        build_agent_env(
                            &pane_id,
                            gapped,
                            inner_argv,
                            &config_dir,
                            &state_dir,
                            &registry,
                        )
                        .await
                    }) as BoxFuture<Result<BuiltEnv, String>>
                },
            )
        },
        close_agent_env: {
            let registry = registry.clone();
            Arc::new(move |pane_id: &str| registry.close(pane_id))
        },
        // Frozen true — see this module's top doc comment.
        egress_default: Arc::new(|| Box::pin(async { true }) as BoxFuture<bool>),
        log_event: {
            let state_dir = state_dir.clone();
            Arc::new(move |kind: &str, fields: Vec<(String, Value)>| {
                events::append(&state_dir, kind, fields);
            })
        },
        // No renderer, no window — `runs:changed` has nobody to reach.
        push: Arc::new(|_snapshot: Value| {}),
        spawn: Arc::new(tome_flow::flow::runner::spawn::spawn_process),
        kill_grace: std::time::Duration::from_millis(5000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- can_open_flow ----

    #[test]
    fn can_open_flow_rejects_an_empty_path() {
        assert!(!can_open_flow(Path::new("/repo"), Path::new("")));
    }

    #[test]
    fn can_open_flow_accepts_a_path_lexically_under_root() {
        assert!(can_open_flow(
            Path::new("/repo"),
            Path::new("/repo/.tome/flows/x.flow.json")
        ));
    }

    #[test]
    fn can_open_flow_rejects_a_path_outside_root() {
        assert!(!can_open_flow(
            Path::new("/repo"),
            Path::new("/elsewhere/x.flow.json")
        ));
    }

    // ---- linux_sandbox_strategy on a non-Linux build ----

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn linux_sandbox_strategy_refuses_unconditionally_off_linux() {
        assert!(matches!(
            linux_sandbox_strategy(),
            tome_flow::egress::linux::SandboxStrategy::Refuse { .. }
        ));
    }

    // ---- resolve_shim_path ----

    #[test]
    fn resolve_shim_path_errors_with_an_actionable_message_when_absent() {
        // In this test binary's own build output there is no sibling
        // "tome-shim" executable next to the test harness's own exe, so
        // this exercises the real error path end to end.
        match resolve_shim_path() {
            Err(msg) => {
                assert!(msg.contains("tome-shim"));
                assert!(msg.contains("cargo build --workspace"));
            }
            Ok(p) => {
                // Only plausible if a prior full workspace build left a
                // real tome-shim binary sitting in the same directory as
                // this test binary — still a valid outcome, just not the
                // one this test is really trying to pin.
                assert!(p.ends_with("tome-shim"));
            }
        }
    }

    // ---- build (smoke-level: RunnerEnv comes back with a usable spawn fn) ----

    #[tokio::test]
    async fn build_produces_an_egress_default_that_is_always_true() {
        let env = build(
            "/tmp/does-not-need-to-exist/x.flow.json",
            PathBuf::from("/tmp/tome-runner-test-config"),
            PathBuf::from("/tmp/tome-runner-test-state"),
        );
        assert!((env.egress_default)().await);
    }

    #[test]
    fn build_produces_a_can_open_file_confined_to_the_flow_roots_own_root() {
        let env = build(
            "/srv/repo/.tome/flows/nightly.flow.json",
            PathBuf::from("/tmp/tome-runner-test-config"),
            PathBuf::from("/tmp/tome-runner-test-state"),
        );
        assert!((env.can_open_file)(Path::new(
            "/srv/repo/.tome/flows/nightly.flow.json"
        )));
        assert!(!(env.can_open_file)(Path::new("/elsewhere/x.flow.json")));
    }
}
