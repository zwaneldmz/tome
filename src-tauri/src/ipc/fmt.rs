//! Prettier formatting. Per the plan, real formatting runs in a renderer Web
//! Worker (`src/renderer/fmt-worker.js`, `prettier/standalone` + language
//! plugins — no Rust formatter, since running Prettier's own logic through a
//! different engine would drift from its actual output). The phase 5a-docs
//! task wired `tome-ipc.js`'s `fs.format` straight to that worker — no Tauri
//! round-trip — so in the running app this command is never actually
//! invoked; it survives as a safe fallback and for wire-shape parity with
//! `lock_gate::CHANNEL_OF_COMMAND`'s registration (that table's own doc
//! comment covers why a channel stays listed independent of whether
//! anything still calls its command body).

use serde_json::Value;
use tauri::State;

use crate::{lock_gate, state::AppState};

/// Always takes the JS handler's "no parser for this file type" branch
/// (`if (!info.inferredParser) return null`): the real formatting work is
/// the renderer worker described in this file's module doc comment.
/// Returning `null` unconditionally is the same everyday path a file with
/// no registered Prettier parser already took under the Electron original
/// — the editor saves the content as typed instead of failing the save
/// outright — which is why this stays a real body rather than the usual
/// `Err("unimplemented: ...")` stub: an error here would regress every
/// save-with-format-on-save into a failure the moment anything calls this
/// command directly, rather than just being the unreached dead code it is
/// today.
#[tauri::command]
pub async fn fmt_format(
    state: State<'_, AppState>,
    path: String,
    content: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "fmt:format")?;
    let _ = (path, content);
    Ok(Value::Null)
}
