//! Thin `#[tauri::command]` wrappers, one file per domain. Every command in
//! the Phase 1 surface (see `crate::lock_gate::CHANNEL_OF_COMMAND`) exists
//! in exactly one of these files — either as a `stub_command!` invocation
//! (below) or, for the rare command that must do something real even in
//! Phase 1 (`app::app_quit_ready`, for the quit handshake), a normal
//! hand-written `#[tauri::command]` fn.
//!
//! `lib.rs`'s `generate_handler!` call lists every one of them by path.
//! Nothing in this file needs to change when a later slice fills in a
//! domain's bodies — that's purely an edit to the one `ipc/<domain>.rs`
//! file that owns it.

pub mod agents;
pub mod airgap;
pub mod app;
pub mod auth;
pub mod brain;
pub mod chat;
pub mod conductor;
pub mod dialog;
pub mod doc;
pub mod events;
pub mod fmt;
pub mod fs;
pub mod git;
pub mod lsp;
pub mod mentor;
pub mod panes;
pub mod popout;
pub mod pty;
pub mod review;
pub mod runs;
pub mod shell;
pub mod skills;
pub mod store;
pub mod stt;
pub mod theme;

/// Declares one Phase 1 stub command named `$name`: it calls
/// `lock_gate::guard` with the exact Electron wire channel `$channel` (from
/// `src/preload/index.js`), then returns `Err("unimplemented: <name>")`.
///
/// To graduate a stub to a real implementation: delete its `stub_command!`
/// line and write a normal `#[tauri::command] pub async fn` in its place —
/// see `ipc::app::app_quit_ready` for a worked example, including the
/// `lock_gate::guard` call every command (stub or real) must keep making
/// first. Nothing outside that one domain file needs to change; `lib.rs`'s
/// `generate_handler!` resolves the same path either way.
macro_rules! stub_command {
    ($name:ident, $channel:literal) => {
        #[tauri::command]
        pub async fn $name(
            state: tauri::State<'_, crate::state::AppState>,
        ) -> Result<serde_json::Value, String> {
            crate::lock_gate::guard(&state, $channel)?;
            Err(concat!("unimplemented: ", stringify!($name)).to_string())
        }
    };
}
pub(crate) use stub_command;
