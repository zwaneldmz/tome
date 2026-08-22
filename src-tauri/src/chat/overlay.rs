//! The impure half of the provider registry (plan §4.5): loads the
//! bundled defaults layer (or a `TOME_PROVIDERS_FILE` replacement),
//! loads/saves the user `Overlay` (`chat-providers.json`, 0600 — main's
//! own file, reserved in `store_keys.rs` so the renderer can never read
//! or write it through the generic store), and merges the two into the
//! row list every command consumes.
//!
//! `registry.rs` stays pure (no filesystem, no `std::env`) — this module
//! is the shell around it. It exists instead of using `store::get`/
//! `store::set` because those functions REFUSE reserved key names by
//! design (`is_store_key_allowed`), and the overlay file is exactly a
//! reserved, main-owned file: main reads/writes it directly here, the
//! same shape `egress`/`export`/`schedules` use for their own files.
//!
//! `TOME_PROVIDERS_FILE=/path/to/providers.json` replaces the bundled
//! DEFAULTS layer only (the old `TOME_CHAT_BASE_URL`/`TOME_CHAT_MODEL`/
//! `TOME_CHAT_WIRE` env overrides are deleted — see the plan's precedence
//! section): more powerful (it can define a whole test registry), safer
//! (it cannot cross wires or attach `ANTHROPIC_API_KEY` to a non-Anthropic
//! host), and it never touches the user overlay.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::chat::registry::{self, Overlay, ProviderRow};

/// The overlay file's name under `app_data_dir` — reserved, main-owned.
pub const OVERLAY_FILE: &str = "chat-providers.json";

/// The bundled defaults layer, or a `TOME_PROVIDERS_FILE` replacement.
/// An unreadable/unparsable override falls back to the bundled table
/// (silently — this is a dev/CI affordance, and a broken dev override
/// must not brick every chat command; the file's absence is the normal
/// production state).
pub fn load_defaults() -> Vec<ProviderRow> {
    match std::env::var_os("TOME_PROVIDERS_FILE") {
        Some(path) => load_defaults_from(Some(Path::new(&path))),
        None => load_defaults_from(None),
    }
}

/// [`load_defaults`] with the override path threaded in, so tests never
/// touch `std::env` (parallel `cargo test` discipline — see
/// `registry.rs`'s module doc comment for why that matters).
pub fn load_defaults_from(override_path: Option<&Path>) -> Vec<ProviderRow> {
    if let Some(path) = override_path {
        if let Some(rows) = fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<ProviderRow>>(&text).ok())
        {
            return rows;
        }
    }
    registry::default_rows()
}

/// The user overlay from `dir/chat-providers.json` — missing or corrupt
/// reads as the default (an empty overlay is "user hasn't chosen anything",
/// never an error to report to the user).
pub fn load_overlay(dir: &Path) -> Overlay {
    fs::read_to_string(dir.join(OVERLAY_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Overlay>(&text).ok())
        .unwrap_or_default()
}

/// Persist the overlay (mkdir -p parent, 0600 — `store.rs`'s write shape).
pub fn save_overlay(dir: &Path, ov: &Overlay) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(OVERLAY_FILE);
    let blob = serde_json::to_string_pretty(ov).map_err(|e| e.to_string())?;
    fs::write(&path, blob).map_err(|e| e.to_string())?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    Ok(())
}

/// Defaults merged with the user overlay — the row list every chat
/// command resolves against. Small file reads; callers doing async work
/// wrap this in `spawn_blocking` like every other store read.
pub fn load_rows(dir: &Path) -> Vec<ProviderRow> {
    registry::merge(&load_defaults(), &load_overlay(dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn overlay_round_trips_through_save_and_load() {
        let dir = tempdir().unwrap();
        let mut ov = Overlay::default();
        ov.model
            .insert("glm".to_string(), "glm-5-turbo".to_string());
        ov.hidden.push("openai".to_string());

        save_overlay(dir.path(), &ov).unwrap();
        assert_eq!(load_overlay(dir.path()), ov);

        let mode = fs::metadata(dir.path().join(OVERLAY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_missing_or_corrupt_overlay_reads_as_the_default() {
        let dir = tempdir().unwrap();
        assert_eq!(load_overlay(dir.path()), Overlay::default());
        fs::write(dir.path().join(OVERLAY_FILE), "{corrupt").unwrap();
        assert_eq!(load_overlay(dir.path()), Overlay::default());
    }

    #[test]
    fn load_rows_merges_defaults_with_the_overlay() {
        let dir = tempdir().unwrap();
        let mut ov = Overlay::default();
        ov.model
            .insert("deepseek".to_string(), "deepseek-v4-flash".to_string());
        ov.region
            .insert("kimi".to_string(), "https://api.moonshot.cn/v1".to_string());
        save_overlay(dir.path(), &ov).unwrap();

        let rows = load_rows(dir.path());
        let kimi = rows.iter().find(|r| r.id == "kimi").unwrap();
        let deepseek = rows.iter().find(|r| r.id == "deepseek").unwrap();
        assert_eq!(kimi.base_url, "https://api.moonshot.cn/v1");
        assert_eq!(deepseek.model, "deepseek-v4-flash");
    }

    #[test]
    fn load_defaults_from_uses_the_override_file_when_valid() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-providers.json");
        let rows = vec![ProviderRow {
            id: "testai".to_string(),
            label: "Test AI".to_string(),
            wire: registry::Wire::OpenAi,
            auth: registry::Auth::Bearer,
            base_url: "http://localhost:9999/v1".to_string(),
            model: "test-model".to_string(),
            models: vec!["test-model".to_string()],
            models_url: None,
            alternates: vec![],
            key_env: vec!["TEST_API_KEY".to_string()],
            max_output_tokens: None,
            betas: vec![],
            builtin: false,
        }];
        fs::write(&path, serde_json::to_string(&rows).unwrap()).unwrap();

        let loaded = load_defaults_from(Some(&path));
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "testai");
    }

    #[test]
    fn load_defaults_from_falls_back_to_bundled_when_the_override_is_broken() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("junk.json");
        fs::write(&path, "not json").unwrap();
        let loaded = load_defaults_from(Some(&path));
        assert_eq!(loaded.len(), 6);
    }
}
