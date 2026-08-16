//! Usage review report command. Thin wrapper over `crate::review`: guards
//! with the `review:generate` wire channel, then hands the `AppHandle` to
//! `review::generate_report` (which resolves the provider and makes the
//! one-shot LLM call) and shapes the result into `{ report }`.

use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::state::AppState;

/// `review:generate` (no args) -> `{ report }`. The report is the
/// accumulated markdown text from the one-shot provider call; any provider
/// or network failure surfaces as a plain `Err(String)` the renderer's
/// `call()` normalizes into a thrown `Error`.
#[tauri::command]
pub async fn review_generate(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    crate::lock_gate::guard(&state, "review:generate")?;
    let report = crate::review::generate_report(&app).await?;
    Ok(json!({ "report": report }))
}
