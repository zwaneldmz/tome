//! Agent roster commands: built-in list, vetted custom-agent list, and the
//! "custom-agents changed" nudge. Ports `src/main/lib/custom-agents.js` and
//! the `AGENTS` constant usage in `src/main/index.js`.

use tauri::State;

use crate::ipc::stub_command;
use crate::{lock_gate, state::AppState};

stub_command!(agents_changed, "agents:changed");

/// Mirrors `src/shared/pane-kinds.js`'s `AGENTS` constant. Re-exported
/// from `agent_spawn.rs` (the Phase 2 spawn-policy slice's canonical copy
/// — see that module's doc comment for why `src/shared/**` itself can't
/// be the source) rather than duplicated here a second time, so this
/// stays the one Rust-side copy of the list, consumed by both commands
/// below and — via this exact re-exported path — by `menu.rs`'s "New
/// Pane" submenu (`use crate::ipc::agents::AGENTS`, unchanged).
pub use crate::agent_spawn::AGENTS;

/// Mirrors `agents:list`'s built-in half of `mergeAgents(AGENTS, await
/// readStore('custom-agents'))` (`src/main/lib/custom-agents.js`) — each
/// built-in normalizes to `{id: name, bin: name, custom: false}`, and the
/// availability probe's own shape for a non-custom entry is exactly `{name,
/// available}` (`{ name: a.id, available: !err, ...(a.custom ? {label,
/// custom:true} : {}) }` — the spread contributes nothing when `a.custom`
/// is `false`).
///
/// Two things the JS handler does are deferred, not ported: the actual
/// `command -v <bin>` availability probe over the login shell (needs the
/// Phase 2 login-env harvest this slice does not own — see the plan's "PTY
/// + spawn policy" phase) and merging in custom agents from the
/// `custom-agents` store key (needs `store.rs`'s real `readStore`, a
/// different slice's file). Every built-in is reported `available: true`
/// unconditionally rather than `false`: `pty_create` is itself still
/// `Err("unimplemented")` this phase (see `ipc::pty`), so nothing here can
/// actually be launched either way, and `false` would only hide the
/// "+ pane" menu entries for no corresponding safety benefit.
#[tauri::command]
pub async fn agents_list(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "agents:list")?;
    let agents: Vec<_> = AGENTS
        .iter()
        .map(|name| serde_json::json!({ "name": name, "available": true }))
        .collect();
    Ok(serde_json::json!(agents))
}

/// Mirrors `agents:customs`'s shape for "no custom agents yet" — an empty
/// list. Custom-agent vetting needs `store.rs`'s real `readStore` for the
/// `custom-agents` key; see `agents_list`'s doc comment for the same
/// deferral and why it's safe (nothing can spawn a custom agent this phase
/// either).
#[tauri::command]
pub async fn agents_customs(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "agents:customs")?;
    Ok(serde_json::json!([]))
}
