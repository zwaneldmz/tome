//! Prettier formatting. Per the plan, this becomes a renderer Web Worker
//! running `prettier/standalone` (no Rust formatter — semantics would
//! drift from running Prettier's own logic through a different engine),
//! so this command exists only for wire-shape parity during the
//! coexistence period.

use serde_json::Value;
use tauri::State;

use crate::{lock_gate, state::AppState};

/// Always takes the JS handler's "no parser for this file type" branch
/// (`if (!info.inferredParser) return null`): real formatting is Phase-5,
/// renderer-side work. Returning `null` unconditionally is the same
/// everyday path a file with no registered Prettier parser already takes
/// today — the editor saves the content as typed instead of failing the
/// save outright, which is why this is a real body rather than the usual
/// `Err("unimplemented: ...")` stub (an error here would regress every
/// save-with-format-on-save into a failure, not just silently skip
/// formatting).
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
