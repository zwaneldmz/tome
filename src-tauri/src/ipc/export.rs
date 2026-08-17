//! Export destination consent + the runs-pane Export action's IPC surface —
//! guarded `#[tauri::command]` wrappers over `crate::export` (file CRUD +
//! hashing + transports) and `crate::flow::runner` (run resolution for
//! `runs_export`). See `crate::export`'s module doc comment for the security
//! rationale this file only threads through: every renderer-supplied string
//! reaching a transport is either a `destinationId` (resolved against an
//! `export_consent`-vetted, hash-verified record) or a `localPath` (a
//! native-picker-driven folder) — never a bare host/url/target the renderer
//! typed directly into an export call.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

use crate::state::AppState;
use crate::{events, export, flow, lock_gate};

/// Same resolution every other command in this crate uses
/// (`ipc::store::store_get`, `ipc::events::events_list`, ...).
fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

/// `export:destinations` (no args) — every consented destination, minus its
/// bearer token (presence only; see `export::public_view`'s doc comment),
/// sorted by label for a stable list.
#[tauri::command]
pub async fn export_destinations(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "export:destinations")?;
    let dir = app_data_dir(&app)?;
    let store = tokio::task::spawn_blocking(move || export::load(&dir))
        .await
        .map_err(|e| e.to_string())?;
    let mut list: Vec<Value> = store
        .destinations
        .iter()
        .map(|(id, dest)| export::public_view(id, dest))
        .collect();
    list.sort_by(|a, b| {
        let by_label = a["label"]
            .as_str()
            .unwrap_or("")
            .cmp(b["label"].as_str().unwrap_or(""));
        by_label.then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    Ok(Value::Array(list))
}

/// `export:consent` (`{ id?, kind, label, url?, method?, authBearer?,
/// target?, tool? }`) — add (no `id`, or an `id` not already present) or
/// update (an `id` that IS already present) one destination. Canonicalizes
/// and hashes via `export::canonicalize` (the renderer never supplies a
/// hash — see that function's doc comment), then persists the whole store.
/// Resolves `{ ok: true, id }`; refuses (an `Err`, not a `{ok:false}` shape
/// — `preferences.js`'s `openAddDestinationModal` catches and toasts it) on
/// a bad shape (empty label, missing url/target, an unrecognized method or
/// tool).
#[tauri::command]
#[allow(clippy::too_many_arguments)] // one field per persisted record column — see export::canonicalize's own signature
pub async fn export_consent(
    app: AppHandle,
    state: State<'_, AppState>,
    id: Option<String>,
    kind: String,
    label: String,
    url: Option<String>,
    method: Option<String>,
    auth_bearer: Option<String>,
    target: Option<String>,
    tool: Option<String>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "export:consent")?;
    let record = export::canonicalize(
        &kind,
        &label,
        url.as_deref(),
        method.as_deref(),
        auth_bearer.as_deref(),
        target.as_deref(),
        tool.as_deref(),
    )?;
    let record_label = record.label().to_string();
    let dir = app_data_dir(&app)?;
    let dest_id = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let mut store = export::load(&dir);
        let dest_id = match id.filter(|i| store.destinations.contains_key(i)) {
            Some(existing) => existing,
            None => export::new_destination_id(&store.destinations),
        };
        store.destinations.insert(dest_id.clone(), record);
        export::save(&dir, &store)?;
        Ok(dest_id)
    })
    .await
    .map_err(|e| e.to_string())??;
    events::log_event(
        &app,
        "export:consent",
        vec![("kind", json!(kind)), ("label", json!(record_label))],
    );
    Ok(json!({"ok": true, "id": dest_id}))
}

/// `export:revoke` (`{ id }`). Always `{ ok: true }`, even for an id with
/// nothing to revoke — same idempotent, no-such-id-error shape
/// `airgap::revoke_repo_allowlist` uses for the same reason (a revoke that
/// races a second revoke, or a client that already knows the id is gone,
/// should not have to special-case it).
#[tauri::command]
pub async fn export_revoke(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "export:revoke")?;
    let dir = app_data_dir(&app)?;
    let removed_label = tokio::task::spawn_blocking({
        let id = id.clone();
        move || -> Result<Option<String>, String> {
            let mut store = export::load(&dir);
            let removed = store
                .destinations
                .remove(&id)
                .map(|d| d.label().to_string());
            export::save(&dir, &store)?;
            Ok(removed)
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    if let Some(label) = removed_label {
        events::log_event(
            &app,
            "export:revoke",
            vec![("id", json!(id)), ("label", json!(label))],
        );
    }
    Ok(json!({"ok": true}))
}

/// `runs:export` (`{ id, destinationId?, localPath? }`). Resolves the run
/// from the in-memory runner registry (`flow::runner::snapshot_all`) — never
/// from a renderer-supplied path fragment — and only exports a SETTLED
/// (`status === "done"`) run's promoted-products directory,
/// `<root>/.tome/flows/<flow>/out/<runId>/`. The products module (a parallel
/// slice) is the eventual owner of writing into that directory; this command
/// only depends on the path convention, and deliberately never imports that
/// module (see `crate::export`'s module doc comment). Exactly one of
/// `destination_id`/`local_path` must be given: the renderer may NEVER pass
/// a host/url/target directly, only a consented destination id
/// (`export_destinations`) or a dialog-picked local folder
/// (`tome.pickFolder`).
#[tauri::command]
pub async fn runs_export(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    destination_id: Option<String>,
    local_path: Option<String>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "runs:export")?;

    let snapshot = flow::runner::snapshot_all(&state.flow);
    let run = snapshot
        .as_array()
        .and_then(|runs| runs.iter().find(|r| r["id"] == id))
        .ok_or_else(|| "no such run".to_string())?;
    if run["status"].as_str() != Some("done") {
        return Err("this run has not finished yet".to_string());
    }
    let root = run["root"]
        .as_str()
        .ok_or_else(|| "run has no root".to_string())?;
    let flow_name = run["flow"]
        .as_str()
        .ok_or_else(|| "run has no flow name".to_string())?;
    let source_dir = Path::new(root)
        .join(".tome")
        .join("flows")
        .join(flow_name)
        .join("out")
        .join(&id);
    if !source_dir.is_dir() {
        return Err("this run has no exported products yet".to_string());
    }

    match (destination_id, local_path) {
        (Some(dest_id), None) => {
            let dir = app_data_dir(&app)?;
            let store = tokio::task::spawn_blocking(move || export::load(&dir))
                .await
                .map_err(|e| e.to_string())?;
            let dest = store
                .destinations
                .get(&dest_id)
                .ok_or_else(|| "no such export destination".to_string())?;
            // Recompute + compare immediately before use — never trust a
            // record that no longer reads as it did when export_consent
            // hashed it (see export::Destination::verify's doc comment).
            dest.verify()?;
            export::run_transport(dest, &source_dir, &id).await?;
        }
        (None, Some(local_path)) => {
            let dest_dir = PathBuf::from(local_path);
            tokio::task::spawn_blocking(move || export::copy_to_local(&source_dir, &dest_dir))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        }
        (None, None) => return Err("export needs a destination or a folder".to_string()),
        (Some(_), Some(_)) => {
            return Err("export needs exactly one of destinationId or localPath".to_string())
        }
    }

    events::log_event(&app, "runs:export", vec![("id", json!(id))]);
    Ok(json!({"ok": true}))
}

#[cfg(test)]
mod tests {
    // Every command above is a thin wrapper: input validation and hashing
    // live in `export::canonicalize`/`export::Destination::verify` (covered
    // by `export.rs`'s own #[cfg(test)] suite), run resolution delegates to
    // `flow::runner::snapshot_all` (covered by that module's own tests), and
    // `lock_gate::guard`'s wiring is covered by
    // `lock_gate::tests::channel_table_matches_lib_rs_registration`, which
    // proves these four commands are registered under the exact wire-channel
    // strings `CHANNEL_OF_COMMAND` pins. A `#[tauri::command]` fn cannot be
    // called directly without a live `AppHandle`/`State` (this crate enables
    // no `tauri` `test` feature — see `confine.rs`'s doc comment on the same
    // constraint), so there is nothing hermetic left to unit test in this
    // file itself.
    #[allow(unused_imports)]
    use super::*;
}
