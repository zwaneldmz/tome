//! `tome-flow`: the tauri-free half of the background flow-run engine —
//! plan step 2.1's extraction out of the main `tome` crate (`tome_lib`).
//! Everything below either already had zero `tauri`/`AppState` coupling
//! (verified module by module before this crate existed) or was split so
//! its tauri-free half could move here while its tauri-touching half
//! stayed behind — see [`flow::runner::env`]'s doc comment for that split.
//!
//! Membership, and why each one is here:
//! - [`flow`] — the flow document model, DAG scheduling, realpath
//!   confinement, the live run registry/engine, and finished-run
//!   promotion. `flow::tools` (the conductor's `read_flow`/`draft_flow`
//!   tool surface) is the one sibling that STAYS in `tome_lib` — it is
//!   tauri-free too, but this slice's brief scopes the move to exactly the
//!   modules listed above, and `tools.rs` reaches back into this crate's
//!   `flow::confine`/`flow::model` the same way any other `tome_lib` file
//!   does, through the `tome_lib::flow` re-export shim.
//! - [`agent_spawn`] — the vetted agent-CLI command-line builder (built-ins
//!   + the headless flow-node argv shape).
//! - [`custom_agents`] — user-declared custom agent CLIs, vetted against
//!   [`agent_spawn`]'s allowlist shape. Not in the slice brief's own module
//!   list, but pulled in alongside `agent_spawn` because the two are a
//!   matched pair: `agent_spawn`'s own `#[cfg(test)] mod tests` exercises
//!   their interplay directly (`agent_spawn.rs`'s "interplay with
//!   custom_agents" test section), and `custom_agents.rs`'s non-test code
//!   already depends on `agent_spawn::{AgentEntry, AGENTS}` — splitting the
//!   pair across the crate boundary would have meant either a reverse
//!   `tome_lib` dependency (impossible: `tome_lib` depends on this crate,
//!   not the other way around) or rewriting `agent_spawn.rs`'s pinned test
//!   suite, neither of which "mechanical extraction, behavior unchanged"
//!   allows. Verified tauri-free the same way every other module here was.
//! - [`agent_env`] / [`login_env`] — the pty/headless-node environment
//!   allowlist and the cached login-shell PATH/secrets harvest.
//! - [`egress`] — the egress state machine, host allowlist compiler, pane
//!   proxy, Linux sandbox argv assembly, and macOS seatbelt profile
//!   builder.
//!
//! `tome_lib` re-exports every one of these at its own pre-extraction
//! paths (`crate::flow`, `crate::agent_spawn`, `crate::custom_agents`,
//! `crate::agent_env`, `crate::login_env`, `crate::egress`) via `pub use`
//! shims, so no caller elsewhere in that crate needed to change.

pub mod agent_env;
pub mod agent_spawn;
pub mod custom_agents;
pub mod egress;
pub mod flow;
pub mod login_env;
