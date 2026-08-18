//! `tome-runner`: a headless binary that runs one flow to completion on a
//! server — a git checkout with no Tauri app, no renderer, and no human
//! clicking "Allow" anywhere in the loop (see `docs/remote-runner.md` for
//! the full operational picture, including why agent credentials on that
//! server are the server owner's own responsibility, not this binary's).
//! Two subcommands:
//!
//! - `run <flow.json>` executes one flow to completion and exits with a
//!   status that reflects the result (see [`run_cmd`]'s own doc comment
//!   for the exact exit-code contract).
//! - `schedule install <flow.json> --on-calendar <expr> [--unit-dir <dir>]`
//!   writes a `systemd --user` service+timer pair that calls `run` on a
//!   calendar schedule (see [`schedule_cmd`]).
//!
//! Argv is hand-parsed in [`cli`] — this workspace has no `clap`
//! dependency, and this slice's grant doesn't add one; `crates/tome-shim/
//! src/args.rs` is the precedent this module follows (see `cli`'s own doc
//! comment).
//!
//! ## Why this binary carries no Tauri dependency
//!
//! Every other consumer of `tome-flow`'s `RunnerEnv` seam in this
//! workspace (`src-tauri/src/flow_env.rs`) is built from an
//! `AppHandle`/`AppState` — a live desktop app with a webview, an IPC
//! surface, a lock screen. `tome-runner` runs on a headless server with
//! none of that: pulling in Tauri (and everything it drags in — a
//! renderer bundle, a windowing stack, an IPC command surface with its
//! own attack surface) would add a large amount of code this binary would
//! never exercise, purely to reach a handful of functions this crate
//! already gets directly from [`tome_flow`]. `runner_env.rs` rebuilds
//! exactly the wiring this binary needs from `tome_flow`'s own public
//! primitives instead — see that module's own doc comment.
//!
//! ## The egress and lock gate are never weakened here
//!
//! This binary has no lock screen to bypass (there is nothing to
//! authenticate to — it is not interactive), so the project's "nothing
//! spawns while locked" invariant has no analog to weaken. What DOES
//! apply, and is enforced identically: every flow node this binary spawns
//! is gapped, unconditionally (`runner_env::build`'s `egress_default`
//! is frozen `true` — see that module's doc comment), and a gapped node's
//! egress allowlist is read from exactly one place, a file under the
//! SERVER OWNER's own `$HOME` that nothing in the repo checkout can reach
//! or edit (`egress_config`'s own doc comment spells out why this is a
//! prompt-injection line, not a convenience choice).

mod cli;
mod egress_config;
mod events;
mod home;
mod run_cmd;
mod runner_env;
mod schedule_cmd;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse_args(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tome-runner: {e}\n\n{}", cli::USAGE);
            std::process::exit(2);
        }
    };

    let exit_code = match command {
        cli::Command::Run { flow_path } => run_cmd::run(&flow_path).await,
        cli::Command::ScheduleInstall {
            flow_path,
            on_calendar,
            unit_dir,
        } => schedule_cmd::run(&flow_path, &on_calendar, unit_dir),
    };
    std::process::exit(exit_code);
}
