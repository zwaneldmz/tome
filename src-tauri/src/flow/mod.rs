//! Background flow runs — Phase 5b, ported here originally from
//! `src/main/flow-runner.js`/`flow-run-plan.js`/`flow-confine.js` and
//! `src/main/lib/flow-tools.js`. Plan step 2.1 extracted every tauri-free
//! piece of this module ([`confine`], [`model`], [`run_plan`], [`runner`],
//! [`products`]) into the `tome-flow` workspace crate, re-exported below at
//! their original paths so every existing call site in this crate
//! (`ipc/pty.rs`, `ipc/runs.rs`, `ipc/schedules.rs`, `menu.rs`, `schedule.rs`,
//! the products hook, …) keeps compiling unchanged.
//!
//! - [`tools`] is the one submodule that stayed here: the conductor's
//!   `read_flow`/`draft_flow` tool surface is tauri-free too, but is not
//!   part of this slice's extraction — it reaches [`confine`]/[`model`]
//!   the same way any other file in this crate does, through this module's
//!   own re-exports.
//! - [`runner::env::RunnerEnv`]'s PRODUCTION wiring (`production_env`,
//!   `frozen_airgap_default`) is not re-exported at its old
//!   `runner::env::*` path — that half of the original `runner/env.rs`
//!   reaches `tauri::AppHandle`/`AppState` and could never move into
//!   `tome-flow`, so it lives on in this crate as [`crate::flow_env`]
//!   instead (a flat rename, not a re-export: `ipc::runs`/`ipc::schedules`/
//!   `schedule.rs` call `crate::flow_env::production_env` directly). The
//!   seam TYPES (`RunnerEnv`/`SandboxWrap`/`BuiltEnv`/`BoxFuture`) still
//!   live at `runner::env::*` exactly as before, via the blanket
//!   `pub use tome_flow::flow::runner` below.

// `products`/`run_plan` have no DIRECT caller in this crate today — the
// one real caller of each (`runner::spawn_promotion`, `runner::start_run`)
// reaches them through `tome-flow`'s own internal `super::{products,
// run_plan}`, invisible from here — so plain rustc would flag this
// re-export `unused_imports` the way it never flagged the pre-extraction
// `pub mod products;`/`pub mod run_plan;` (a `mod` declaration compiles
// its contents regardless of whether anything references the module by
// name; a `use`/re-export does not get that same pass). Kept anyway for
// API-shape parity with every module this slice's brief named — the same
// "kept even though nothing calls it yet" posture `agent_spawn.rs`'s and
// `custom_agents.rs`'s own `#![allow(dead_code)]` take.
#[allow(unused_imports)]
pub use tome_flow::flow::{confine, model, products, run_plan, runner};
pub mod tools;

pub use runner::Runner;
