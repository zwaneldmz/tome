//! Chat provider migration (plan §4.5): folds the Electron-era provider
//! state into the registry's overlay + vault. NOT part of `migrate.rs`
//! (the Electron userData copier): that one early-returns unless
//! `electron_user_data_dir()` resolves, and would never fire for an
//! existing Tauri user. This one runs on every boot, once — the
//! `"migrated": 1` marker in the overlay short-circuits it afterwards.
//!
//! The seven rules, in order:
//! 1. `chat-provider` ∈ {kimi, glm, claude, deepseek, custom} → no action
//!    (ids are unchanged by design — the whole "don't rename ids"
//!    decision exists to make this rule a no-op).
//! 2. `chat-provider` == "deepseek-flash" → rewrite the pick to
//!    "deepseek" and pin `overlay.model["deepseek"] = "deepseek-v4-flash"`
//!    (the flash variant became a model of the deepseek row).
//! 3. `chat-model` non-empty → fold into
//!    `overlay.model[<current chat-provider>]`; delete chat-model.json.
//! 4. `chat-provider` == "glm" and `ZHIPU_API_KEY` set and `ZAI_API_KEY`
//!    unset → pin `overlay.region["glm"]` to the China platform. A working
//!    China-platform user must not be silently moved to api.z.ai, where
//!    their key 401s.
//! 5. `custom-provider.json` parses → append an `added` row id "custom"
//!    (so a stored pick of "custom" still resolves), move the key into the
//!    vault under "custom", verify the read-back, then unlink the file —
//!    the plaintext key stops existing on disk without the user doing
//!    anything. Write-then-verify-then-unlink, unlink last: the unlink is
//!    the only irreversible step, and a post-marker sweep below keeps it
//!    idempotent across a crash between the overlay save and the unlink.
//! 6. `REQUESTY_API_KEY` present → report `requesty_notice` (the renderer
//!    offers a pre-filled Requesty row; nothing is added or selected
//!    automatically).
//! 7. `TOME_CHAT_BASE_URL` + `TOME_CHAT_MODEL` both set → append an
//!    `added` row id "imported-env" (never auto-selected).
//!
//! Reads of the three legacy store keys use `store::get(&dir, key,
//! /* locked = */ false)`: the locked parameter is a renderer-request
//! policy gate (`store_keys::is_store_key_allowed`), not a filesystem
//! lock — main owns the store. A naive "read while locked → Null →
//! silently wipe the user's choice" failure is exactly what passing
//! `false` avoids.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use crate::chat::registry::{Auth, ProviderRow, Wire};
use crate::chat::{overlay, vault};
use crate::store;

/// What the migration did, for the caller's side effects (the requesty
/// notice event; a vault-snapshot refresh when keys moved).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub moved_keys: bool,
    pub requesty_notice: bool,
}

/// The only post-marker work: the custom-provider unlink sweep (see the
/// module doc comment's crash-window note). Returns the plaintext file's
/// path when it still exists but its key already round-trips through the
/// vault — the caller unlinks it.
fn custom_unlink_sweep(dir: &Path, vault: &vault::Vault) -> Option<std::path::PathBuf> {
    let path = dir.join("custom-provider.json");
    let cp = read_custom(&path)?;
    if vault
        .load()
        .0
        .get("custom")
        .is_some_and(|k| k == &cp.api_key)
    {
        Some(path)
    } else {
        None
    }
}

/// Run the migration once. `secrets` is `login_env().secrets` (the
/// ZHIPU/ZAI check must see the login shell, not just process env —
/// that is where a China-platform user's key lives); `env` is process
/// env (the TOME_CHAT_* and REQUESTY presence checks). Idempotent: with
/// the marker present, only the custom unlink sweep runs.
pub fn run(
    dir: &Path,
    secrets: &HashMap<String, String>,
    env: &HashMap<String, String>,
    vault: &vault::Vault,
) -> Report {
    let mut report = Report::default();

    let mut ov = overlay::load_overlay(dir);
    if ov.migrated.is_some() {
        if let Some(path) = custom_unlink_sweep(dir, vault) {
            let _ = fs::remove_file(path);
        }
        return report;
    }

    // ---- rule 1 + 2: the stored pick (ids unchanged, deepseek-flash folds) ----
    let stored = store::get(dir, "chat-provider", false);
    let mut pick: Option<String> = stored
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if pick.as_deref() == Some("deepseek-flash") {
        ov.model
            .entry("deepseek".to_string())
            .or_insert_with(|| "deepseek-v4-flash".to_string());
        pick = Some("deepseek".to_string());
        let _ = store::set(dir, "chat-provider", &json!("deepseek"), false);
    }

    // ---- rule 3: the global chat-model scalar folds per-provider ----
    let model = store::get(dir, "chat-model", false);
    if let Some(m) = model.as_str().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(p) = &pick {
            if !p.is_empty() {
                ov.model.entry(p.clone()).or_insert_with(|| m.to_string());
            }
        }
        let _ = fs::remove_file(dir.join("chat-model.json"));
    }

    // ---- rule 4: a working China-platform GLM user stays on the China host ----
    if pick.as_deref() == Some("glm")
        && truthy(secrets, env, "ZHIPU_API_KEY")
        && !truthy(secrets, env, "ZAI_API_KEY")
    {
        ov.region.insert(
            "glm".to_string(),
            "https://open.bigmodel.cn/api/paas/v4".to_string(),
        );
    }

    // ---- rule 5: the one pasteable slot becomes an added row + vault key ----
    let custom_path = dir.join("custom-provider.json");
    if let Some(cp) = read_custom(&custom_path) {
        if !ov.added.iter().any(|r| r.id == "custom") {
            ov.added.push(custom_row(&cp));
        }
        let mut map = vault.load().0;
        map.insert("custom".to_string(), cp.api_key.clone());
        if vault.save(&map).is_ok() {
            // write-then-verify; the unlink happens after the overlay
            // save below (last irreversible step last).
            if vault
                .load()
                .0
                .get("custom")
                .is_some_and(|k| k == &cp.api_key)
            {
                report.moved_keys = true;
            }
        }
        // On verification failure the plaintext file STAYS — losing the
        // key would be strictly worse than keeping the old state.
    }

    // ---- rule 6: Requesty becomes an offer, never a row ----
    if truthy(secrets, env, "REQUESTY_API_KEY") {
        report.requesty_notice = true;
    }

    // ---- rule 7: the deleted TOME_CHAT_* override imports as a row ----
    let env_base = env
        .get("TOME_CHAT_BASE_URL")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let env_model = env
        .get("TOME_CHAT_MODEL")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let (Some(base), Some(model)) = (env_base, env_model) {
        if !ov.added.iter().any(|r| r.id == "imported-env") {
            ov.added.push(imported_env_row(
                &base,
                &model,
                env.get("TOME_CHAT_WIRE").map(String::as_str),
            ));
        }
    }

    // ---- commit: marker + overlay, then the (only) irreversible step ----
    ov.migrated = Some(1);
    let _ = overlay::save_overlay(dir, &ov);
    if report.moved_keys {
        let _ = fs::remove_file(custom_path);
    }

    report
}

/// `login.secrets[key] || process.env[key]`, empty string falsy — the
/// presence check rules 4 and 6 share. The old Requesty branch read env
/// BEFORE secrets (reversed vs every other lookup); a migration presence
/// check has no ordering to get wrong, so secrets first like everything
/// else.
fn truthy(secrets: &HashMap<String, String>, env: &HashMap<String, String>, name: &str) -> bool {
    truthy_env(secrets, name) || truthy_env(env, name)
}

fn truthy_env(map: &HashMap<String, String>, name: &str) -> bool {
    map.get(name).is_some_and(|v| !v.trim().is_empty())
}

/// Parses a stored `custom-provider` value — the old
/// `parse_custom_provider` contract: `label`, `baseUrl`, `model`, `key`
/// all non-empty strings, `wire` "anthropic" explicitly, else openai.
/// Lenient on unknown fields.
struct CustomProvider {
    label: String,
    wire: Wire,
    base_url: String,
    api_key: String,
    model: String,
}

fn read_custom(path: &Path) -> Option<CustomProvider> {
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let obj = value.as_object()?;
    let label = obj.get("label")?.as_str()?.trim().to_string();
    let base_url = obj.get("baseUrl")?.as_str()?.trim().to_string();
    let model = obj.get("model")?.as_str()?.trim().to_string();
    let api_key = obj.get("key")?.as_str()?.trim().to_string();
    if label.is_empty() || base_url.is_empty() || model.is_empty() || api_key.is_empty() {
        return None;
    }
    let wire = match obj.get("wire").and_then(Value::as_str) {
        Some("anthropic") => Wire::Anthropic,
        _ => Wire::OpenAi,
    };
    Some(CustomProvider {
        label,
        wire,
        base_url,
        api_key,
        model,
    })
}

fn custom_row(cp: &CustomProvider) -> ProviderRow {
    ProviderRow {
        id: "custom".to_string(),
        label: cp.label.clone(),
        wire: cp.wire,
        // The old custom anthropic wire always sent x-api-key; openai
        // always Bearer — preserve both so the migrated row authenticates
        // exactly like the old custom-provider path did.
        auth: if cp.wire == Wire::Anthropic {
            Auth::XApiKey
        } else {
            Auth::Bearer
        },
        base_url: cp.base_url.trim_end_matches('/').to_string(),
        model: cp.model.clone(),
        models: vec![cp.model.clone()],
        models_url: None,
        alternates: vec![],
        key_env: vec![],
        max_output_tokens: None,
        betas: vec![],
        builtin: false,
    }
}

/// The deleted env-override branch, preserved as a row: same wire
/// heuristic (`TOME_CHAT_WIRE == "anthropic"` or the host is
/// api.anthropic.com), same `ANTHROPIC_API_KEY` ladder the old branch
/// used for its key, never auto-selected.
fn imported_env_row(base: &str, model: &str, wire_env: Option<&str>) -> ProviderRow {
    let host_is_anthropic = reqwest::Url::parse(base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .is_some_and(|h| h == "api.anthropic.com");
    let anthropic = wire_env == Some("anthropic") || host_is_anthropic;
    ProviderRow {
        id: "imported-env".to_string(),
        label: "Imported from TOME_CHAT_*".to_string(),
        wire: if anthropic {
            Wire::Anthropic
        } else {
            Wire::OpenAi
        },
        auth: if anthropic {
            Auth::XApiKey
        } else {
            Auth::Bearer
        },
        base_url: base.trim_end_matches('/').to_string(),
        model: model.to_string(),
        models: vec![model.to_string()],
        models_url: None,
        alternates: vec![],
        key_env: vec!["ANTHROPIC_API_KEY".to_string()],
        max_output_tokens: None,
        betas: vec![],
        builtin: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::registry::Overlay;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// In-memory vault fake (mirrors vault.rs's own MemoryIo): the real
    /// keyring must never run under `cargo test`.
    struct MemoryIo(Mutex<Option<String>>);

    impl vault::SecretIo for MemoryIo {
        fn get(&self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
        fn set(&self, secret: &str) -> bool {
            *self.0.lock().unwrap() = Some(secret.to_string());
            true
        }
    }

    fn test_vault(dir: &Path) -> vault::Vault {
        vault::Vault::with_io(dir, Box::new(MemoryIo(Mutex::new(None))))
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn store_set(dir: &Path, key: &str, v: &Value) {
        store::set(dir, key, v, false).unwrap();
    }

    fn custom_json() -> Value {
        json!({
            "label": "My endpoint",
            "baseUrl": "https://api.example.com/v1",
            "model": "some-model",
            "key": "sk-secret",
            "wire": "openai",
        })
    }

    // ================= rule 1 + marker =================

    #[test]
    fn a_fresh_dir_marks_migrated_and_adds_nothing() {
        let dir = tempdir().unwrap();
        let v = test_vault(dir.path());
        let report = run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        assert!(!report.moved_keys && !report.requesty_notice);
        let ov = overlay::load_overlay(dir.path());
        assert_eq!(ov.migrated, Some(1));
        assert!(ov.added.is_empty());
        assert!(ov.model.is_empty());
    }

    #[test]
    fn an_unchanged_id_pick_is_left_alone() {
        let dir = tempdir().unwrap();
        store_set(dir.path(), "chat-provider", &json!("glm"));
        let v = test_vault(dir.path());
        run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        assert_eq!(store::get(dir.path(), "chat-provider", false), json!("glm"));
        let ov = overlay::load_overlay(dir.path());
        assert!(ov.region.is_empty());
    }

    // ================= rule 2: deepseek-flash folds =================

    #[test]
    fn deepseek_flash_rewrites_the_pick_and_pins_the_model() {
        let dir = tempdir().unwrap();
        store_set(dir.path(), "chat-provider", &json!("deepseek-flash"));
        let v = test_vault(dir.path());
        run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        assert_eq!(
            store::get(dir.path(), "chat-provider", false),
            json!("deepseek")
        );
        let ov = overlay::load_overlay(dir.path());
        assert_eq!(ov.model.get("deepseek").unwrap(), "deepseek-v4-flash");
    }

    // ================= rule 3: chat-model folds =================

    #[test]
    fn a_stored_model_folds_into_the_picked_providers_override_and_deletes_the_file() {
        let dir = tempdir().unwrap();
        store_set(dir.path(), "chat-provider", &json!("kimi"));
        store_set(dir.path(), "chat-model", &json!("kimi-k4-preview"));
        let v = test_vault(dir.path());
        run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        let ov = overlay::load_overlay(dir.path());
        assert_eq!(ov.model.get("kimi").unwrap(), "kimi-k4-preview");
        assert!(!dir.path().join("chat-model.json").exists());
    }

    #[test]
    fn a_stored_model_with_no_pick_is_dropped_not_lost_forever() {
        // No pick to attribute the scalar to — the file is deleted (the
        // global scalar is dead either way) and the overlay gains nothing.
        let dir = tempdir().unwrap();
        store_set(dir.path(), "chat-model", &json!("orphan-model"));
        let v = test_vault(dir.path());
        run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        let ov = overlay::load_overlay(dir.path());
        assert!(ov.model.is_empty());
        assert!(!dir.path().join("chat-model.json").exists());
    }

    // ================= rule 4: glm China-platform pin =================

    #[test]
    fn glm_china_platform_user_is_pinned_to_the_cn_host() {
        let dir = tempdir().unwrap();
        store_set(dir.path(), "chat-provider", &json!("glm"));
        let secrets = env(&[("ZHIPU_API_KEY", "zhipu-secret")]);
        let v = test_vault(dir.path());
        run(dir.path(), &secrets, &HashMap::new(), &v);
        let ov = overlay::load_overlay(dir.path());
        assert_eq!(
            ov.region.get("glm").unwrap(),
            "https://open.bigmodel.cn/api/paas/v4"
        );
    }

    #[test]
    fn glm_with_both_keys_or_neither_is_not_pinned() {
        let dir = tempdir().unwrap();
        store_set(dir.path(), "chat-provider", &json!("glm"));
        let both = env(&[("ZHIPU_API_KEY", "z"), ("ZAI_API_KEY", "a")]);
        let v = test_vault(dir.path());
        run(dir.path(), &both, &HashMap::new(), &v);
        let ov = overlay::load_overlay(dir.path());
        assert!(ov.region.is_empty());
    }

    #[test]
    fn glm_zhipu_in_process_env_only_still_counts() {
        let dir = tempdir().unwrap();
        store_set(dir.path(), "chat-provider", &json!("glm"));
        let envmap = env(&[("ZHIPU_API_KEY", "z")]);
        let v = test_vault(dir.path());
        run(dir.path(), &HashMap::new(), &envmap, &v);
        let ov = overlay::load_overlay(dir.path());
        assert!(ov.region.contains_key("glm"));
    }

    #[test]
    fn an_empty_zhipu_key_is_unset() {
        let dir = tempdir().unwrap();
        store_set(dir.path(), "chat-provider", &json!("glm"));
        let secrets = env(&[("ZHIPU_API_KEY", "  ")]);
        let v = test_vault(dir.path());
        run(dir.path(), &secrets, &HashMap::new(), &v);
        assert!(overlay::load_overlay(dir.path()).region.is_empty());
    }

    // ================= rule 5: custom provider moves to the vault =================

    #[test]
    fn a_custom_provider_becomes_an_added_row_and_its_key_moves_to_the_vault() {
        let dir = tempdir().unwrap();
        store_set(dir.path(), "chat-provider", &json!("custom"));
        fs::write(
            dir.path().join("custom-provider.json"),
            serde_json::to_string(&custom_json()).unwrap(),
        )
        .unwrap();
        let v = test_vault(dir.path());
        let report = run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        assert!(report.moved_keys);

        let ov = overlay::load_overlay(dir.path());
        let row = ov.added.iter().find(|r| r.id == "custom").unwrap();
        assert_eq!(row.label, "My endpoint");
        assert_eq!(row.wire, Wire::OpenAi);
        assert_eq!(row.auth, Auth::Bearer);
        assert_eq!(row.base_url, "https://api.example.com/v1");
        assert_eq!(row.model, "some-model");

        assert_eq!(v.load().0.get("custom").unwrap(), "sk-secret");
        // The plaintext file is gone — that is the point.
        assert!(!dir.path().join("custom-provider.json").exists());
    }

    #[test]
    fn a_custom_anthropic_wire_row_keeps_x_api_key_auth() {
        let dir = tempdir().unwrap();
        let mut cp = custom_json();
        cp["wire"] = json!("anthropic");
        fs::write(
            dir.path().join("custom-provider.json"),
            serde_json::to_string(&cp).unwrap(),
        )
        .unwrap();
        let v = test_vault(dir.path());
        run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        let ov = overlay::load_overlay(dir.path());
        let row = ov.added.iter().find(|r| r.id == "custom").unwrap();
        assert_eq!(row.wire, Wire::Anthropic);
        assert_eq!(row.auth, Auth::XApiKey);
    }

    #[test]
    fn an_incomplete_custom_provider_file_is_left_alone() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("custom-provider.json"), r#"{"label": "x"}"#).unwrap();
        let v = test_vault(dir.path());
        let report = run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        assert!(!report.moved_keys);
        assert!(dir.path().join("custom-provider.json").exists());
    }

    // ================= rule 6: requesty notice =================

    #[test]
    fn a_requesty_key_flags_the_notice_but_adds_nothing() {
        let dir = tempdir().unwrap();
        let secrets = env(&[("REQUESTY_API_KEY", "rq-key")]);
        let v = test_vault(dir.path());
        let report = run(dir.path(), &secrets, &HashMap::new(), &v);
        assert!(report.requesty_notice);
        let ov = overlay::load_overlay(dir.path());
        assert!(ov.added.is_empty());
        assert_eq!(store::get(dir.path(), "chat-provider", false), Value::Null);
    }

    // ================= rule 7: TOME_CHAT_* import =================

    #[test]
    fn tome_chat_env_imports_as_a_row_with_the_anthropic_host_heuristic() {
        let dir = tempdir().unwrap();
        let envmap = env(&[
            ("TOME_CHAT_BASE_URL", "https://api.anthropic.com"),
            ("TOME_CHAT_MODEL", "claude-opus-5"),
        ]);
        let v = test_vault(dir.path());
        run(dir.path(), &HashMap::new(), &envmap, &v);
        let ov = overlay::load_overlay(dir.path());
        let row = ov.added.iter().find(|r| r.id == "imported-env").unwrap();
        assert_eq!(row.wire, Wire::Anthropic);
        assert_eq!(row.auth, Auth::XApiKey);
        assert_eq!(row.key_env, vec!["ANTHROPIC_API_KEY".to_string()]);
        // never auto-selected
        assert_eq!(store::get(dir.path(), "chat-provider", false), Value::Null);
    }

    #[test]
    fn tome_chat_openai_host_imports_on_the_openai_wire() {
        let dir = tempdir().unwrap();
        let envmap = env(&[
            ("TOME_CHAT_BASE_URL", "http://localhost:9999/v1"),
            ("TOME_CHAT_MODEL", "local-model"),
        ]);
        let v = test_vault(dir.path());
        run(dir.path(), &HashMap::new(), &envmap, &v);
        let ov = overlay::load_overlay(dir.path());
        let row = ov.added.iter().find(|r| r.id == "imported-env").unwrap();
        assert_eq!(row.wire, Wire::OpenAi);
        assert_eq!(row.base_url, "http://localhost:9999/v1");
    }

    #[test]
    fn tome_chat_model_alone_does_not_import() {
        let dir = tempdir().unwrap();
        let envmap = env(&[("TOME_CHAT_MODEL", "m")]);
        let v = test_vault(dir.path());
        run(dir.path(), &HashMap::new(), &envmap, &v);
        assert!(overlay::load_overlay(dir.path()).added.is_empty());
    }

    // ================= idempotency =================

    #[test]
    fn the_marker_makes_a_second_run_a_no_op() {
        let dir = tempdir().unwrap();
        store_set(dir.path(), "chat-provider", &json!("deepseek-flash"));
        let v = test_vault(dir.path());
        run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        let ov_after_first = overlay::load_overlay(dir.path());

        // Change state under the migration's nose — the marker must stop
        // a second run from touching anything.
        store_set(dir.path(), "chat-provider", &json!("glm"));
        run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        assert_eq!(overlay::load_overlay(dir.path()), ov_after_first);
        assert_eq!(store::get(dir.path(), "chat-provider", false), json!("glm"));
    }

    #[test]
    fn the_post_marker_sweep_unlinks_a_surviving_custom_file() {
        // Crash-window simulation: the key made it into the vault, the
        // plaintext file survived (as if the unlink never ran).
        let dir = tempdir().unwrap();
        let v = test_vault(dir.path());
        let mut map = HashMap::new();
        map.insert("custom".to_string(), "sk-secret".to_string());
        v.save(&map).unwrap();
        fs::write(
            dir.path().join("custom-provider.json"),
            serde_json::to_string(&custom_json()).unwrap(),
        )
        .unwrap();
        let ov = Overlay {
            migrated: Some(1),
            ..Overlay::default()
        };
        overlay::save_overlay(dir.path(), &ov).unwrap();

        run(dir.path(), &HashMap::new(), &HashMap::new(), &v);
        assert!(!dir.path().join("custom-provider.json").exists());
    }
}
