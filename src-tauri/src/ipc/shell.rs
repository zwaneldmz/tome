//! "Open in default app" (Finder/Explorer/etc reveal-or-open), confined the
//! same way fs commands are. Ports `src/main/index.js`'s `shell:openPath`
//! handler via `tauri-plugin-opener`, after a `crate::confine` check.

use std::path::Path;

use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use crate::{confine, lock_gate, state::AppState};

/// Same two-branch message `index.js`'s top-level `confinementError`
/// helper builds (`` `${what}: path is outside the open workspace folders`
/// `` / `` `${what}: workspace folders have not been reported yet` ``).
/// Duplicated locally rather than added to `confine.rs` — that module is
/// owned by a parallel slice (S3) implementing `confined_real_path` itself;
/// `shell_open_path` below is the only command in this file that needs the
/// message, so a local copy is the smaller footprint. Worth folding into
/// `confine.rs` proper later, since `index.js`'s `tome://` protocol handler
/// (a different, not-yet-ported command) will want the identical text.
fn confinement_error(what: &str, folders_synced: bool) -> String {
    if folders_synced {
        format!("{what}: path is outside the open workspace folders")
    } else {
        format!("{what}: workspace folders have not been reported yet")
    }
}

/// Mirrors `shell:openPath`'s handler exactly:
///
/// ```js
/// const real = await confinedRealPath(p)
/// return real ? shell.openPath(real) : confinementError('shell:openPath')
/// ```
///
/// `shell.openPath` never throws in Electron — it resolves to `''` on
/// success, an error description on failure — and a confinement refusal
/// returns a STRING for the same reason, not an Error (TOME-001). So unlike
/// most commands here, a "failure" to open the path is still `Ok(<string>)`
/// on the Rust side; `Err` is reserved for `lock_gate::guard` alone, the
/// one real throw path the Electron handler actually had.
///
/// `confine::confined_real_path` (`crate::confine`, S3's slice) has since
/// landed as a real, tested implementation. This command was originally
/// written against only its signature (`fn(&State<AppState>, &Path) ->
/// Result<PathBuf, String>`) while S3's body was still in flight, and needed
/// no follow-up edit once it did: any `Err` from it is treated as "refused"
/// regardless of the message inside, with the confinement message built
/// locally from `AppState.folders_synced` rather than trusted from the
/// callee's error text.
#[tauri::command]
pub async fn shell_open_path(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "shell:openPath")?;
    let real = match confine::confined_real_path(&state, Path::new(&path)) {
        Ok(p) => p,
        Err(_) => {
            let synced = *state
                .folders_synced
                .read()
                .expect("shell_open_path: AppState.folders_synced lock poisoned");
            return Ok(serde_json::json!(confinement_error(
                "shell:openPath",
                synced
            )));
        }
    };
    let result = match app.opener().open_path(real.to_string_lossy(), None::<&str>) {
        Ok(()) => String::new(),
        Err(e) => e.to_string(),
    };
    Ok(serde_json::json!(result))
}
