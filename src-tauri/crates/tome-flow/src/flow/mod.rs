//! Background flow runs — Phase 5b. Ports `src/main/flow-runner.js`
//! (run.json, process groups, cancel), the run-plan scheduling half of
//! `src/shared/flow-run-plan.js`, and `src/main/lib/flow-confine.js`.
//! `src/shared/flow-model.js` itself stays renderer JS — see
//! [`model`]'s doc comment for the from-scratch subset ported here.
//!
//! This is the tauri-free slice of what was originally one `flow` module
//! inside the main `tome` crate (plan step 2.1's `tome-flow` extraction —
//! see the crate root doc comment for the full membership list and why
//! `tools` specifically stays behind in `tome_lib`, reached from there
//! through the `tome_lib::flow` re-export shim). Everything below was
//! already tauri-free before the extraction; nothing in this module was
//! split the way `runner::env` was.
//!
//! - [`confine`] — realpath confinement for the absolute managed paths this
//!   module builds itself (run directories, log files, flow documents).
//! - [`model`] — the flow document shape, `validateFlow`,
//!   `composeBootstrapPrompt`, `flowRoot`.
//! - [`run_plan`] — the DAG scheduling core: layers, ready-node selection,
//!   failure/cancellation propagation.
//! - [`runner`] — the live background-run engine: [`Runner`] is the
//!   registry (`AppState.flow`); [`runner::start_run`]/
//!   [`runner::cancel_run`]/[`runner::kill_all`]/[`runner::snapshot_all`]
//!   are its free-function API, driven through an injected
//!   [`runner::env::RunnerEnv`] seam so tests never spawn a real agent.
//! - [`products`] — plan step 1.4: once a run settles `"done"`,
//!   `runner::settle_if_done` hands this module a
//!   [`products::PromoteRequest`] and it promotes every terminal node's
//!   declared outputs into a hashed, git-pinned `out/<runId>/` snapshot
//!   plus a capped `runs-index.json` history. Tauri-free by design (see
//!   its own module doc comment) — the extraction boundary this crate now
//!   sits behind.

pub mod confine;
pub mod model;
pub mod products;
pub mod run_plan;
pub mod runner;

pub use runner::Runner;
