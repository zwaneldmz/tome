//! `mentor_answer` — the user's reply to a mentor-mode comprehension gate.
//! Completes the pending gate that the `gate_question` tool registered (via
//! `conductor::env::gate_question_impl`), letting the paused tool loop
//! resume. The payload carries the user's answers (or a `skip` flag); both
//! are folded into a single `{ answers, skip }` value handed back to the
//! waiting tool result.
//!
//! `mentor_judge` — the renderer's LLM-judged commit/push gate. A one-shot
//! completion grades the user's free-text explanation against the change
//! context, returning `{ pass, feedback }` (feedback empty on a pass).

use serde_json::{json, Value};
use tauri::{AppHandle, State};

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

/// `mentor:judge` — grade a free-text explanation against the change context
/// via a one-shot completion. Returns `{ pass, feedback }`; `feedback` is
/// empty on a pass, otherwise the model's "FAIL: …" explanation with the
/// leading `FAIL:` prefix stripped.
#[tauri::command]
pub async fn mentor_judge(
    app: AppHandle,
    state: State<'_, AppState>,
    answer: String,
    context: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "mentor:judge")?;
    let system = "You grade whether a developer's explanation shows real understanding of a code change. Reply with EXACTLY the word PASS, or FAIL: followed by one short sentence of what they missed. Nothing else.";
    let messages = vec![json!({ "role": "user", "content": format!("The change:\n{context}\n\nTheir explanation:\n{answer}") })];
    let text = crate::ipc::chat::complete_once(&app, &state, &messages, system).await?;
    let trimmed = text.trim();
    let pass = trimmed.to_ascii_uppercase().starts_with("PASS");
    let feedback = if pass {
        String::new()
    } else if let Some(rest) = trimmed
        .strip_prefix("FAIL:")
        .or_else(|| trimmed.strip_prefix("fail:"))
    {
        rest.trim().to_string()
    } else {
        trimmed.to_string()
    };
    Ok(json!({ "pass": pass, "feedback": feedback }))
}
