//! opencode integration — API keys, auth/subscription status, and model
//! choice for the `opencode` agent CLI. Reads/writes opencode's own files,
//! never a Tome-side mirror:
//!
//! - credentials: `~/.local/share/opencode/auth.json` (opencode's
//!   credential store — `opencode providers list` reads exactly this file;
//!   an `"api"` entry is an API key, `"oauth"` is a logged-in subscription).
//!   [`set_key`] writes `{ "<id>": { "type": "api", "key": "…" } }`
//!   preserving every other entry; keys are write-only from Tome's side —
//!   [`status`] reports only `type`s, never key material.
//! - provider rows + default model: `~/.config/opencode/opencode.json`
//!   (the config opencode itself reads; `provider.<id>.options.apiKey` is
//!   an in-config key alternative some users prefer — reported as
//!   `hasKey` only).
//!
//! Both paths honor `XDG_CONFIG_HOME`/`XDG_DATA_HOME` when set, matching
//! opencode's own resolution. All file work is synchronous and small —
//! callers wrap it in `spawn_blocking`, the same discipline as
//! `store.rs`/`brain.rs` call sites.
//!
//! ## Process spawning
//!
//! `models()` runs `opencode models [provider]` with the login-shell PATH
//! (GUI-launched Tome inherits a minimal PATH), a 20s timeout, and an
//! output cap. `status()` probes `opencode --version` the same way. Never
//! a shell — args arrays only.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const MODELS_TIMEOUT: Duration = Duration::from_secs(20);
const MODELS_CAP: usize = 50_000;

/// `~/.config/opencode/opencode.json` (or `$XDG_CONFIG_HOME/opencode/...`).
pub fn config_file() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::home_dir().unwrap_or_default().join(".config"))
        .join("opencode")
        .join("opencode.json")
}

/// `~/.local/share/opencode/auth.json` (or `$XDG_DATA_HOME/opencode/...`).
pub fn auth_file() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::home_dir().unwrap_or_default().join(".local/share"))
        .join("opencode")
        .join("auth.json")
}

/// One credential slot, keyed by provider id. `cred_type` is `"api"` /
/// `"oauth"` / whatever opencode stored — reported, never guessed at.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Credential {
    pub id: String,
    pub cred_type: String,
}

/// Everything the Settings section renders. Keys are NEVER serialized
/// anywhere in this struct — `has_key` is the only key material signal.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub installed: bool,
    pub version: Option<String>,
    pub reason: Option<String>,
    pub auth: Vec<Credential>,
    /// Provider ids configured in opencode.json's `provider` map.
    pub providers: Vec<String>,
    /// Which of those providers carry an `options.apiKey` in config.
    pub providers_with_key: Vec<String>,
    pub default_model: Option<String>,
}

/// Reads auth.json (never returns key material).
pub fn read_auth(path: &Path) -> Vec<Credential> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text)
    else {
        return Vec::new();
    };
    let mut out: Vec<Credential> = map
        .into_iter()
        .map(|(id, v)| {
            let cred_type = v
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            Credential { id, cred_type }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Sets (or replaces) an API credential for `id` in auth.json, preserving
/// every other entry. `key.is_empty()` REMOVES the entry instead (same
/// write-only contract as Tome's chat vault: empty string clears the
/// slot). Writes 0600.
pub fn set_key(path: &Path, id: &str, key: &str) -> Result<(), String> {
    let mut map = load_json_object(path).unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        map.remove(id);
    } else {
        map.insert(
            id.to_string(),
            serde_json::json!({ "type": "api", "key": key }),
        );
    }
    save_json_object(path, map)
}

/// The provider ids named in opencode.json's `provider` map, and which of
/// them carry an `options.apiKey` — the in-config alternative to auth.json.
pub fn read_config_providers(path: &Path) -> (Vec<String>, Vec<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (Vec::new(), Vec::new());
    };
    let Some(providers) = json.get("provider").and_then(serde_json::Value::as_object) else {
        return (Vec::new(), Vec::new());
    };
    let mut ids: Vec<String> = providers.keys().cloned().collect();
    ids.sort();
    let with_key: Vec<String> = providers
        .iter()
        .filter(|(_, v)| {
            v.get("options")
                .and_then(|o| o.get("apiKey"))
                .and_then(serde_json::Value::as_str)
                .map(|k| !k.is_empty())
                .unwrap_or(false)
        })
        .map(|(id, _)| id.clone())
        .collect();
    (ids, with_key)
}

/// Reads opencode.json's top-level `model` (the default the CLI picks when
/// nothing pins one).
pub fn read_default_model(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let json = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    json.get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

/// Sets opencode.json's top-level `model`, preserving everything else.
/// `model.is_empty()` removes the key (back to the CLI's own default).
pub fn set_default_model(path: &Path, model: &str) -> Result<(), String> {
    let mut json = load_json_object(path).unwrap_or_default();
    let model = model.trim();
    if model.is_empty() {
        json.remove("model");
    } else {
        json.insert(
            "model".to_string(),
            serde_json::Value::String(model.to_string()),
        );
    }
    save_json_object(path, json)
}

/// `opencode --version` with a short timeout. Never fails the command —
/// unavailability is data (the Settings section renders an install hint).
pub async fn probe() -> (bool, Option<String>, Option<String>) {
    match tokio::process::Command::new("opencode")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env("PATH", &crate::login_env::login_env().await.path)
        .spawn()
    {
        Ok(child) => {
            let out = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()).await;
            match out {
                Ok(Ok(out)) if out.status.success() => (
                    true,
                    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
                    None,
                ),
                Ok(Ok(out)) => (
                    false,
                    None,
                    Some(format!("opencode --version exited {}", out.status.code().unwrap_or(-1))),
                ),
                Ok(Err(e)) => (false, None, Some(e.to_string())),
                Err(_) => (false, None, Some("opencode --version timed out".to_string())),
            }
        }
        Err(e) => (false, None, Some(format!("opencode not found ({e})"))),
    }
}

/// `opencode models [provider]` — parsed into a deduped list of
/// `provider/model` ids. The CLI prints one model per line (bare id, no
/// decoration) in the versions tested; lines that don't match the
/// provider/model shape are skipped rather than half-parsed.
pub async fn models(provider: Option<&str>) -> Result<Vec<String>, String> {
    let mut cmd = tokio::process::Command::new("opencode");
    cmd.arg("models");
    if let Some(p) = provider.filter(|p| !p.is_empty()) {
        cmd.arg(p);
    }
    let out = tokio::time::timeout(
        MODELS_TIMEOUT,
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .env("PATH", &crate::login_env::login_env().await.path)
            .output(),
    )
    .await
    .map_err(|_| "opencode models timed out".to_string())?
    .map_err(|e| format!("spawn opencode: {e}"))?;
    if !out.status.success() {
        return Err(format!("opencode models exited {}", out.status.code().unwrap_or(-1)));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.len() > MODELS_CAP {
        return Err("opencode models output too large".to_string());
    }
    let mut seen = std::collections::HashSet::new();
    let mut list = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !is_model_id(line) {
            continue;
        }
        if seen.insert(line.to_string()) {
            list.push(line.to_string());
        }
    }
    list.sort();
    Ok(list)
}

/// `provider/model` shape check — the same format the agent-spawn layer
/// vets (lowercase segments, `-._` allowed after the first `/`).
fn is_model_id(s: &str) -> bool {
    let mut segs = s.split('/');
    let Some(first) = segs.next() else { return false };
    if first.is_empty()
        || !first
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return false;
    }
    let mut rest = 0;
    for seg in segs {
        if seg.is_empty()
            || matches!(seg, "." | "..")
            || !seg
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_'))
        {
            return false;
        }
        rest += 1;
    }
    rest > 0
}

// ---- file helpers (pure, testable with injected paths) ----

fn load_json_object(path: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_json_object(
    path: &Path,
    map: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let blob = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
    std::fs::write(path, blob).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_key_round_trips_and_preserves_other_entries() {
        let dir = std::env::temp_dir().join(format!("tome-opencode-{}", std::process::id()));
        let path = dir.join("auth.json");
        let _ = std::fs::remove_dir_all(&dir);
        set_key(&path, "deepseek", "sk-a").unwrap();
        set_key(&path, "eurouter", "sk-b").unwrap();
        assert_eq!(
            read_auth(&path),
            vec![
                Credential { id: "deepseek".to_string(), cred_type: "api".to_string() },
                Credential { id: "eurouter".to_string(), cred_type: "api".to_string() },
            ]
        );
        // empty key clears the slot
        set_key(&path, "deepseek", "").unwrap();
        assert_eq!(
            read_auth(&path),
            vec![Credential { id: "eurouter".to_string(), cred_type: "api".to_string() }]
        );
        // oauth entries survive untouched
        let map = load_json_object(&path).unwrap();
        let mut map = map;
        map.insert("anthropic".to_string(), serde_json::json!({ "type": "oauth", "account": "x" }));
        save_json_object(&path, map).unwrap();
        set_key(&path, "eurouter", "sk-c").unwrap();
        assert_eq!(
            read_auth(&path),
            vec![
                Credential { id: "anthropic".to_string(), cred_type: "oauth".to_string() },
                Credential { id: "eurouter".to_string(), cred_type: "api".to_string() },
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_providers_and_default_model_round_trip() {
        let dir = std::env::temp_dir().join(format!("tome-opencode-cfg-{}", std::process::id()));
        let path = dir.join("opencode.json");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            &path,
            r#"{"provider": {"eurouter": {"options": {"apiKey": "sk-x", "baseURL": "https://x"}}, "lmstudio": {"options": {"baseURL": "http://localhost:1234/v1"}}}}"#,
        )
        .unwrap();
        let (ids, with_key) = read_config_providers(&path);
        assert_eq!(ids, vec!["eurouter".to_string(), "lmstudio".to_string()]);
        assert_eq!(with_key, vec!["eurouter".to_string()]);
        assert_eq!(read_default_model(&path), None);

        set_default_model(&path, "deepseek/deepseek-chat").unwrap();
        assert_eq!(read_default_model(&path), Some("deepseek/deepseek-chat".to_string()));
        // everything else survives
        let (ids, _) = read_config_providers(&path);
        assert_eq!(ids.len(), 2);
        set_default_model(&path, "").unwrap();
        assert_eq!(read_default_model(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_id_shape_matches_the_agent_spawn_vet() {
        for ok in ["deepseek/deepseek-chat", "eurouter/glm-5.2", "lmstudio/openai/gpt-oss-20b"] {
            assert!(is_model_id(ok), "{ok}");
        }
        for bad in ["gpt-5", "A/b", "a/b c", "a//b", "a/", "x/../y", "README.md"] {
            assert!(!is_model_id(bad), "{bad}");
        }
    }
}
