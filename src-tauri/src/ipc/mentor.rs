//! `mentor_answer` — the user's reply to a mentor-mode comprehension gate.
//! Completes the pending gate that the `gate_question` tool registered (via
//! `conductor::env::gate_question_impl`), letting the paused tool loop
//! resume. The payload carries the user's answers (or a `skip` flag); both
//! are folded into a single `{ answers, skip }` value handed back to the
//! waiting tool result.

use serde_json::{json, Value};
use tauri::State;

use crate::state::AppState;
use crate::lock_gate;

/// `mentor:answer` — complete the gate with the given `id`. Returns
/// `{ ok: bool }`, where `false` means there was no pending gate with that
/// id (already answered, timed out, or never existed).
#[tauri::command]
pub async fn mentor_answer(
    state: State<'_, AppState>,
    id: String,
    answers: Option<Value>,
    skip: Option<bool>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "mentor:answer")?;
    let value = json!({ "answers": answers.unwrap_or(Value::Null), "skip": skip.unwrap_or(false) });
    let ok = state.mentor.answer(&id, value);
    Ok(json!({ "ok": ok }))
}
