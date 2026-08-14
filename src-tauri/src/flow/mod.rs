//! Background flow runs — Phase 5b. Ports `src/main/flow-runner.js`
//! (run.json, process groups, cancel), the run-plan scheduling half of
//! `src/shared/flow-run-plan.js`, `src/main/lib/flow-tools.js` (the
//! conductor's `read_flow`/`draft_flow`), and `src/main/lib/flow-confine.js`.
//! `src/shared/flow-model.js` itself stays renderer JS — see
//! [`model`]'s doc comment for the from-scratch subset ported here.
//!
//! - [`confine`] — realpath confinement for the absolute managed paths this
//!   module builds itself (run directories, log files, flow documents).
//! - [`model`] — the flow document shape, `validateFlow`,
//!   `composeBootstrapPrompt`, `flowRoot`.
//! - [`run_plan`] — the DAG scheduling core: layers, ready-node selection,
//!   failure/cancellation propagation.
//! - [`tools`] — `read_flow`/`draft_flow`, the conductor's model-driven
//!   flow reads/writes. `flow::tools::read_flow_tool`/`draft_flow_tool` are
//!   the clean entry points the conductor (a later slice) calls.
//! - [`runner`] — the live background-run engine: [`Runner`] is the
//!   registry (`AppState.flow`); [`runner::start_run`]/
//!   [`runner::cancel_run`]/[`runner::kill_all`]/[`runner::snapshot_all`]
//!   are its free-function API, driven through an injected
//!   [`runner::env::RunnerEnv`] seam so tests never spawn a real agent.

pub mod confine;
pub mod model;
pub mod run_plan;
pub mod runner;
pub mod tools;

pub use runner::Runner;
