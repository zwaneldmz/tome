//! Skills catalog IPC. Thin command wrappers over `crate::skills` — see that
//! module's doc comment for the loader/parser and `default_root`'s dev-vs-
//! packaged root split.

use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::state::AppState;
use crate::{lock_gate, skills};

/// `skills:list` — the sorted skills catalog. A missing/unresolvable root
/// collapses to `[]` rather than erroring (a not-yet-installed skills dir is
/// a normal first-run state).
#[tauri::command]
pub async fn skills_list(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "skills:list")?;
    let Some(root) = skills::default_root(&app) else {
        return Ok(json!([]));
    };
    Ok(serde_json::to_value(skills::list(&root)).unwrap_or_else(|_| json!([])))
}

/// `skills:read` — one skill's metadata plus its markdown body.
#[tauri::command]
pub async fn skills_read(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "skills:read")?;
    let Some(root) = skills::default_root(&app) else {
        return Err("skills directory unavailable".to_string());
    };
    match skills::read(&root, &name) {
        Some((skill, body)) => Ok(json!({
            "name": skill.name,
            "description": skill.description,
            "body": body,
        })),
        None => Err(format!("skill not found: {name}")),
    }
}
