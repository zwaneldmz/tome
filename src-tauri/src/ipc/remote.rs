//! Remote run visibility (plan §Flow products pipeline, phase 3) IPC
//! surface — guarded `#[tauri::command]` wrappers over `crate::remote`
//! (consent-gated source records, ssh argv/parsing/transport). See that
//! module's doc comment for the security rationale this file only threads
//! through: every renderer-supplied string reaching `ssh` is either a
//! `sourceId` (resolved against a `remote_consent`-vetted, hash-verified
//! record) or a `flow`/`runId` (safe-segment-validated before
//! interpolation) — never a bare host/path the renderer typed directly.

use std::path::PathBuf;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

use crate::state::AppState;
use crate::{events, lock_gate, remote};

/// Same resolution every other command in this crate uses
/// (`ipc::export::app_data_dir`, `ipc::schedules::app_data_dir`, ...).
fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

/// Loads the store and finds one source by id — the shared first step of
/// `remote_runs`/`remote_run_detail`, both of which need the SAME resolve-
/// then-verify sequence before ever touching the network.
async fn resolve_source(
    app: &AppHandle,
    source_id: String,
) -> Result<remote::RemoteSource, String> {
    let dir = app_data_dir(app)?;
    let source = tokio::task::spawn_blocking(move || {
        remote::load(&dir)
            .sources
            .into_iter()
            .find(|s| s.id == source_id)
    })
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "no such remote source".to_string())?;
    // Recompute + compare immediately before use — never trust a record
    // that no longer reads as it did when remote_consent hashed it (see
    // RemoteSource::verify's doc comment).
    source.verify()?;
    Ok(source)
}

/// `remote:sources` (no args) — every consented source, minus its internal
/// integrity hash (`remote::public_view`'s doc comment), sorted by label
/// for a stable list.
#[tauri::command]
pub async fn remote_sources(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "remote:sources")?;
    let dir = app_data_dir(&app)?;
    let store = tokio::task::spawn_blocking(move || remote::load(&dir))
        .await
        .map_err(|e| e.to_string())?;
    let mut list: Vec<Value> = store.sources.iter().map(remote::public_view).collect();
    list.sort_by(|a, b| {
        let by_label = a["label"]
            .as_str()
            .unwrap_or("")
            .cmp(b["label"].as_str().unwrap_or(""));
        by_label.then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    Ok(Value::Array(list))
}

/// `remote:consent` (`{ id?, label, host, repoPath }`) — add (no `id`, or
/// an `id` not already present) or update (an `id` that IS already
/// present) one remote source. Mints a fresh id via `remote::new_source_id`
/// before hashing (the id is covered by the hash — see `crate::remote`'s
/// module doc comment), so an update reuses the same id and therefore
/// produces a genuinely new hash for the changed fields, exactly like
/// `export_consent`'s update path. Resolves `{ ok: true, id }`; refuses on
/// a bad shape (empty label/host, a non-absolute repoPath).
#[tauri::command]
pub async fn remote_consent(
    app: AppHandle,
    state: State<'_, AppState>,
    id: Option<String>,
    label: String,
    host: String,
    repo_path: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "remote:consent")?;
    let dir = app_data_dir(&app)?;
    let record_label = label.clone();
    let source_id = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let mut store = remote::load(&dir);
        let source_id = id
            .filter(|i| store.sources.iter().any(|s| &s.id == i))
            .unwrap_or_else(|| remote::new_source_id(&store.sources));
        let record = remote::canonicalize(&source_id, &label, &host, &repo_path)?;
        store.sources.retain(|s| s.id != source_id);
        store.sources.push(record);
        remote::save(&dir, &store)?;
        Ok(source_id)
    })
    .await
    .map_err(|e| e.to_string())??;
    events::log_event(
        &app,
        "remote:consent",
        vec![("id", json!(source_id)), ("label", json!(record_label))],
    );
    Ok(json!({"ok": true, "id": source_id}))
}

/// `remote:revoke` (`{ id }`). Always `{ ok: true }`, even for an id with
/// nothing to revoke — same idempotent, no-such-id-error shape
/// `export_revoke`/`airgap::revoke_repo_allowlist` use for the same reason.
#[tauri::command]
pub async fn remote_revoke(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "remote:revoke")?;
    let dir = app_data_dir(&app)?;
    let removed_label = tokio::task::spawn_blocking({
        let id = id.clone();
        move || -> Result<Option<String>, String> {
            let mut store = remote::load(&dir);
            let removed = store
                .sources
                .iter()
                .position(|s| s.id == id)
                .map(|i| store.sources.remove(i).label);
            remote::save(&dir, &store)?;
            Ok(removed)
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    if let Some(label) = removed_label {
        events::log_event(
            &app,
            "remote:revoke",
            vec![("id", json!(id)), ("label", json!(label))],
        );
    }
    Ok(json!({"ok": true}))
}

/// `remote:runs` (`{ sourceId }`) — a flattened, best-effort list of every
/// run entry found under the remote host's `.tome/flows/*/runs-index.json`.
/// Fetched fresh over `ssh` on every call: `panels/runs.js` only calls this
/// on pane open or its own Refresh button — NEVER a background poll, so
/// there is no push counterpart (`remote:runs` has no `onChanged`).
#[tauri::command]
pub async fn remote_runs(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "remote:runs")?;
    let source = resolve_source(&app, source_id).await?;
    let runs = remote::fetch_remote_runs(&source.host, &source.repo_path).await?;
    events::log_event(
        &app,
        "remote:runs",
        vec![("id", json!(source.id)), ("host", json!(source.host))],
    );
    Ok(json!(runs))
}

/// `remote:runDetail` (`{ sourceId, flow, runId }`) — one run's `run.json`
/// plus (if the run has been promoted) its `manifest.json`, fetched fresh
/// over `ssh`. `flow`/`runId` are safe-segment-validated by
/// `remote::fetch_remote_run_detail` before either ever reaches a shell
/// string — see that function's doc comment.
#[tauri::command]
pub async fn remote_run_detail(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: String,
    flow: String,
    run_id: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "remote:runDetail")?;
    let source = resolve_source(&app, source_id).await?;
    let detail =
        remote::fetch_remote_run_detail(&source.host, &source.repo_path, &flow, &run_id).await?;
    events::log_event(
        &app,
        "remote:runDetail",
        vec![
            ("id", json!(source.id)),
            ("flow", json!(flow)),
            ("runId", json!(run_id)),
        ],
    );
    Ok(detail)
}

#[cfg(test)]
mod tests {
    // Every command above is a thin wrapper: validation, hashing, argv
    // construction, and parsing all live in — and are exhaustively covered
    // by — `crate::remote`'s own #[cfg(test)] suite, and
    // `lock_gate::guard`'s wiring is covered by
    // `lock_gate::tests::channel_table_matches_lib_rs_registration`, which
    // proves these five commands are registered under the exact
    // wire-channel strings `CHANNEL_OF_COMMAND` pins. A `#[tauri::command]`
    // fn cannot be called directly without a live `AppHandle`/`State` (this
    // crate enables no `tauri` `test` feature — see `confine.rs`'s doc
    // comment on the same constraint), so there is nothing hermetic left to
    // unit test in this file itself.
    #[allow(unused_imports)]
    use super::*;
}
