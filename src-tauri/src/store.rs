//! JSON key-value store: one `<app_data_dir>/<key>.json` file per key,
//! written 0600. Ports `src/main/index.js`'s `store:get`/`store:set`
//! handlers (verified at lines 987-1008 of the current tree — see
//! `readStore`/the `store:get`/`store:set` `ipcMain.handle` calls), gated by
//! `store_keys`'s key vetting.
//!
//! `ipc::store::store_get`/`store_set` are the callers; they resolve
//! `app_data_dir` and the current `AppState.locked` snapshot and hand both
//! down to [`get`]/[`set`] here, which are plain sync functions over `&Path`
//! so they're directly unit-testable with `tempfile` (no `AppHandle`
//! needed).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::Value;

use crate::store_keys::{is_store_key_allowed, is_valid_store_key};

/// Mirrors `index.js`'s `readStore(key)`: `store:get` never throws in the
/// Electron original — a disallowed key (bad shape, reserved, or
/// locked-out) and a missing/corrupt file both resolve to `null`, not a
/// rejection. `dir` is the resolved `app_data_dir` (the Tauri analog of
/// Electron's `app.getPath('userData')`); `locked` is the caller's current
/// `AppState.locked` snapshot.
pub fn get(dir: &Path, key: &str, locked: bool) -> Value {
    if !is_store_key_allowed(key, locked) {
        return Value::Null;
    }
    // `is_store_key_allowed` already enforces KEY_SHAPE (via
    // `is_valid_store_key`), which forbids '/', '.', and every other
    // path-traversal character — this join can only ever land inside `dir`.
    let path = dir.join(format!("{key}.json"));
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

/// Mirrors `index.js`'s `store:set` handler body: shape/reservation check
/// first (Electron's inline `vetKey()`, `throw new Error('Bad store
/// key.')`), then the lock check (`throw new Error('Locked.')`), then
/// `mkdir -p` + pretty-printed JSON + `chmod 0600`. The two error strings
/// are returned verbatim so the renderer's normalized `Error(message)` (see
/// `tome-ipc.js`'s `call()`, which turns a command `Err(String)` into a real
/// thrown `Error`) reads identically to the Electron original's
/// `err.message`.
///
/// One deliberate addition beyond the JS original: `create_dir_all(dir)`
/// runs unconditionally here too, same as JS's explicit `await
/// mkdir(storeDir, { recursive: true })` — Electron guarantees `userData`
/// already exists by the time any handler runs (it creates the directory
/// itself before `whenReady` fires), Tauri makes no such guarantee for
/// `app_data_dir`, so this keeps the existing `mkdir -p` rather than
/// dropping it.
pub fn set(dir: &Path, key: &str, value: &Value, locked: bool) -> Result<(), String> {
    if !is_valid_store_key(key) {
        return Err("Bad store key.".to_string());
    }
    if !is_store_key_allowed(key, locked) {
        return Err("Locked.".to_string());
    }
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{key}.json"));
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| e.to_string())?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn get_returns_null_for_missing_file() {
        let dir = tempdir();
        assert_eq!(get(dir.path(), "workspaces", false), Value::Null);
    }

    #[test]
    fn set_then_get_round_trips() {
        let dir = tempdir();
        let value = json!({"a": 1, "b": [1, 2, 3]});
        set(dir.path(), "workspaces", &value, false).unwrap();
        assert_eq!(get(dir.path(), "workspaces", false), value);
    }

    #[test]
    fn set_rejects_bad_shape_before_touching_disk() {
        let dir = tempdir();
        let err = set(dir.path(), "UPPER", &json!(1), false).unwrap_err();
        assert_eq!(err, "Bad store key.");
        assert!(!dir.path().join("UPPER.json").exists());
    }

    #[test]
    fn set_rejects_reserved_key_as_bad_shape() {
        // vetKey() in the JS original doesn't distinguish "reserved" from
        // "malformed" — both are isValidStoreKey() == false, both throw the
        // same 'Bad store key.' message.
        let dir = tempdir();
        let err = set(dir.path(), "airgap-auth", &json!(1), false).unwrap_err();
        assert_eq!(err, "Bad store key.");
    }

    #[test]
    fn set_rejects_traversal_key() {
        let dir = tempdir();
        let err = set(dir.path(), "../escape", &json!(1), false).unwrap_err();
        assert_eq!(err, "Bad store key.");
    }

    #[test]
    fn set_rejects_while_locked_for_non_lockscreen_key() {
        let dir = tempdir();
        let err = set(dir.path(), "workspaces", &json!(1), true).unwrap_err();
        assert_eq!(err, "Locked.");
        assert!(!dir.path().join("workspaces.json").exists());
    }

    #[test]
    fn set_allows_theme_while_locked() {
        let dir = tempdir();
        set(dir.path(), "theme", &json!("dark"), true).unwrap();
        assert_eq!(get(dir.path(), "theme", true), json!("dark"));
    }

    #[test]
    fn get_returns_null_for_disallowed_key_even_if_file_exists_on_disk() {
        // A key written while unlocked (or migrated in), read back while
        // locked: index.js's readStore() returns null without ever opening
        // the file — the gate is purely key-based, checked before touching
        // fs, matching the JS control flow exactly (not just its net
        // effect).
        let dir = tempdir();
        set(dir.path(), "workspaces", &json!({"x": 1}), false).unwrap();
        assert_eq!(get(dir.path(), "workspaces", true), Value::Null);
    }

    #[test]
    fn get_returns_null_for_corrupt_json() {
        let dir = tempdir();
        fs::write(dir.path().join("workspaces.json"), b"{not json").unwrap();
        assert_eq!(get(dir.path(), "workspaces", false), Value::Null);
    }

    #[test]
    fn set_writes_0600_permissions() {
        let dir = tempdir();
        set(dir.path(), "workspaces", &json!({}), false).unwrap();
        let mode = fs::metadata(dir.path().join("workspaces.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn set_creates_the_store_dir_if_missing() {
        let dir = tempdir();
        let nested = dir.path().join("nested/deeper");
        set(&nested, "workspaces", &json!(1), false).unwrap();
        assert!(nested.join("workspaces.json").exists());
    }
}
