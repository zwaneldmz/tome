//! `run <flow.json>` — the whole point of this binary: execute one flow to
//! completion with no desktop app, no renderer, and no human at the
//! keyboard, then exit with a status that reflects what happened. Every
//! piece that actually does the work — DAG scheduling, the live run
//! registry, realpath confinement, the fail-closed output contract,
//! product promotion, the manifest and `runs-index.json` — comes from
//! [`tome_flow::flow::runner::start_run`] unmodified; this module's only
//! job is building the [`tome_flow::flow::runner::env::RunnerEnv`] seam
//! ([`crate::runner_env::build`]) and waiting for the run it starts to
//! settle.
//!
//! Exit code contract (this slice's brief, verbatim): `0` the run
//! finished `"done"`; `1` it settled `"failed"` or `"canceled"` (it
//! STARTED — a run id was minted — but did not succeed); `2` a usage or
//! configuration problem kept it from starting at all: bad argv (handled
//! in `main.rs`, before this module ever runs), an unresolvable `$HOME`,
//! the Linux sandbox ladder resolving to refuse, or `start_run` itself
//! refusing the flow (not a flow file, a cycle, an unsupported node kind,
//! a path outside the flow's own root) — every one of these happens
//! BEFORE a run id exists, so there is nothing an operator could
//! meaningfully "cancel" or call "failed."

use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use crate::{home, runner_env};

/// Poll interval for [`run`]'s completion loop. `tome_flow::flow::runner::start_run`
/// resolves once the first layer is LAUNCHED, not once the whole run
/// SETTLES (nodes finish in the background, driven by their own exit
/// handlers) — polling `snapshot_all` until the run's own status leaves
/// `"running"` is the same pattern `tome_flow`'s own end-to-end test suite
/// uses to observe a real run settle (`flow::runner::tests::settled`),
/// just with a production-scaled interval: an agent CLI run is measured
/// in seconds to minutes, so 500ms costs nothing and never meaningfully
/// delays this process's own exit.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// The settled-status half of this module's exit code contract (see the
/// module doc comment): `"done"` maps to `0`, everything else — `"failed"`,
/// `"canceled"`, or any status this binary doesn't otherwise recognize — to
/// `1`. Pulled out as its own pure function, the same way `schedule_cmd.rs`
/// isolates every one of ITS own decisions (`unit_stem`, `service_unit`,
/// `plan`, ...) from the filesystem/process code around them, specifically
/// so this mapping is unit-testable with a plain string in, int out —
/// [`run`]'s only real caller of a real agent CLI (`runner_env::build`
/// wires the genuine `spawn::spawn_process`, not a test stub), so this was
/// otherwise the one branch of the exit code contract every `cargo test -p
/// tome-runner` run left completely unexercised.
fn exit_code_for_status(status: &str) -> i32 {
    if status == "done" {
        0
    } else {
        1
    }
}

/// `run <flow.json>`'s full body — see this module's doc comment for the
/// exit code contract.
pub async fn run(flow_path: &Path) -> i32 {
    let Some(home_dir) = home::home_dir() else {
        eprintln!(
            "tome-runner: $HOME is not set — cannot resolve ~/.config/tome-runner or ~/.local/state/tome-runner"
        );
        return 2;
    };
    let config_dir = home::config_dir(&home_dir);
    let state_dir = home::state_dir(&home_dir);

    // Fail closed BEFORE touching the runner at all — every run this
    // binary starts is gapped (see runner_env's own doc comment on
    // "always gapped"), so "the sandbox ladder refuses" and "this run
    // cannot happen at all" are the same fact on Linux. `cfg!(...)` here
    // (a runtime check that still compiles on every target) rather than
    // `#[cfg(target_os = "linux")]` on this whole block: macOS has no
    // refusal rung at all (the seatbelt profile is always available — see
    // `runner_env`'s macOS branch), so there is nothing to precheck there.
    if cfg!(target_os = "linux") {
        if let tome_flow::egress::linux::SandboxStrategy::Refuse { reason } =
            runner_env::linux_sandbox_strategy()
        {
            eprintln!("tome-runner: {reason}");
            return 2;
        }
    }

    let flow_path_string = flow_path.to_string_lossy().into_owned();
    // `build` only ever borrows this string (see its own doc comment: it
    // resolves `flow_root` into an owned `PathBuf` up front and never
    // holds the borrow past that call) — `flow_path_string` is still
    // fully ours to move into `start_run` right after.
    let env = runner_env::build(&flow_path_string, config_dir, state_dir);
    let runs = Arc::new(tome_flow::flow::Runner::new());

    let started = tome_flow::flow::runner::start_run(runs.clone(), env, flow_path_string).await;
    let Some(id) = started
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        let msg = started
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("could not start the run");
        eprintln!("tome-runner: {msg}");
        return 2;
    };
    eprintln!("tome-runner: started run {id}");

    loop {
        let snapshot = tome_flow::flow::runner::snapshot_all(&runs);
        let status = snapshot
            .as_array()
            .and_then(|runs| runs.iter().find(|r| r["id"] == json!(id)))
            .and_then(|r| r["status"].as_str())
            .unwrap_or("running")
            .to_string();
        if status != "running" {
            eprintln!("tome-runner: run {id} {status}");
            return exit_code_for_status(&status);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- exit_code_for_status (pure — see the function's own doc comment
    // on why this is the one branch of the exit code contract a scripted
    // end-to-end run of this binary can't reach) ----

    #[test]
    fn exit_code_for_status_maps_done_to_zero() {
        assert_eq!(exit_code_for_status("done"), 0);
    }

    #[test]
    fn exit_code_for_status_maps_failed_to_one() {
        assert_eq!(exit_code_for_status("failed"), 1);
    }

    #[test]
    fn exit_code_for_status_maps_canceled_to_one() {
        assert_eq!(exit_code_for_status("canceled"), 1);
    }

    #[test]
    fn exit_code_for_status_maps_an_unrecognized_status_to_one() {
        assert_eq!(exit_code_for_status("mystery"), 1);
    }

    #[tokio::test]
    async fn run_reports_a_missing_flow_file_as_exit_2() {
        // Exercises the whole path down to start_run's own "could not
        // read flow" refusal — no $HOME requirement bypassed, so this
        // only runs meaningfully where $HOME is set (true for every CI
        // runner and every real dev machine this workspace targets).
        if home::home_dir().is_none() {
            return;
        }
        let code = run(Path::new("/definitely/not/a/real/flow-xyz.flow.json")).await;
        assert_eq!(code, 2);
    }
}
