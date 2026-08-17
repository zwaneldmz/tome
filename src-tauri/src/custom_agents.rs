//! Custom agent CLIs — user-declared pane kinds that widen the spawn
//! allowlist by explicit user consent. Moved into the `tome-flow` workspace
//! crate alongside [`crate::agent_spawn`] by plan step 2.1: already
//! tauri-free by design, and paired tightly enough with `agent_spawn.rs`
//! (its non-test code depends on `agent_spawn::{AgentEntry, AGENTS}`;
//! `agent_spawn.rs`'s own `#[cfg(test)] mod tests` exercises their
//! interplay directly) that splitting the two across the crate boundary
//! would have meant either a reverse `tome_lib` dependency or rewriting a
//! pinned test suite — neither of which a mechanical, behavior-unchanged
//! extraction allows. Not part of the slice brief's own module list, but
//! verified tauri-free the same way every other moved module was. Re-
//! exported here at the original path so `ipc::pty`'s existing
//! `custom_agents::merge_agents` call keeps compiling unchanged. See
//! `tome_flow::custom_agents`'s own doc comment for the real module
//! documentation.

pub use tome_flow::custom_agents::*;
