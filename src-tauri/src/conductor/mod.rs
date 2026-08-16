//! The assistant tool loop and consent gates — port of
//! `src/main/conductor.js` (430 LOC): gives the assistant chat eyes and
//! hands over the workspace via 13 tools, tracks per-pane scrollback +
//! consent, and never runs a command unless the user flipped "assistant may
//! run commands" on.
//!
//! - [`state::Conductor`] — the live session state (pane meta, scrollback
//!   rings, read consent, the renderer's pane snapshot, `allowRun`, the
//!   chat-abort registry). One instance lives at `AppState.conductor`;
//!   `Conductor::new()` in tests, matching `pty::Registry`/
//!   `airgap::AirgapState`/`flow::Runner`'s own testing shape — see that
//!   module's doc comment for why this can't be a process-wide `static`
//!   the way `conductor.js`'s module-level `let`s are.
//! - [`env::ConductorEnv`] — the injected side-effect seam (`send`,
//!   `log_event`, `can_open_file`, `write_pty`, `roots`, `stream_chat`,
//!   `resolve_path`, `resolve_write`, `skills_root`, `run_command`,
//!   `gate_question`), mirroring `conductor.js`'s own `init(opts)` DI shape
//!   plus the net-new fields this port needs that JS didn't (`stream_chat`,
//!   `resolve_path`, `resolve_write`, `skills_root`, `run_command`,
//!   `gate_question` — see that module's doc comment).
//! - [`tools`] — the 13 tool JSON schemas, `runTool` dispatch, and the two
//!   text sanitizers `conductor.js` imports from `shared/terminal-text.js`.
//! - [`chat`] — the bounded tool loop (`runChat`) and `abortChat`.
//!
//! `ipc::chat::chat_send` is this module's real caller for the loop;
//! `ipc::conductor::conductor_allow_run`/`conductor_allow_read` are the
//! consent-gate setters' real callers; `ipc::panes::panes_sync` feeds
//! `Conductor::set_panes`. See [`state`]'s module doc comment for the one
//! piece of `conductor.js`'s wiring NOT yet connected in this tree (the
//! pty-lifecycle hooks `register`/`record`/`mark_exited`/`forget`) and why.

pub mod chat;
pub mod env;
pub mod state;
pub mod tools;

#[cfg(test)]
mod tests;

pub use state::Conductor;
