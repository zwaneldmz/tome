//! The agent half of the pty command line — moved into the `tome-flow`
//! workspace crate by plan step 2.1 (it was already tauri-free: pure
//! allowlist comparison and argv/command-line string building, no
//! `tauri`/`AppState` reach at all). Re-exported here at the original path
//! so every existing call site in this crate (`ipc::pty`, `ipc::agents`,
//! `menu.rs`'s "New Pane" submenu, `conductor::state`, …) keeps compiling
//! unchanged. See `tome_flow::agent_spawn`'s own doc comment for the real
//! module documentation.

pub use tome_flow::agent_spawn::*;
