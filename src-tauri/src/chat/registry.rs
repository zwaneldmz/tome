//! The data-driven provider registry (docs/PLAN-chat-provider-registry-
//! rework.md §4.1–4.4): six built-in provider rows bundled as JSON
//! (`resources/providers.default.json`, embedded with `include_str!`), a
//! user `Overlay` of model/region/added/hidden choices, and the pure
//! `merge`/`resolve`/`vet_base_url` functions that turn the two layers
//! into one ready-to-stream provider. This is the rung-2 replacement for
//! `providers.rs`'s compile-time `CHAT_PROVIDERS` table — that module
//! keeps compiling alongside this one until the `ipc::chat` rewrite
//! (slice 2 of the plan's build order) deletes it.
//!
//! ## Purity discipline
//!
//! This module is pure data + functions: no filesystem, no `std::env`, no
//! keyring, no clock — everything ambient is a parameter (`&[ProviderRow]`
//! for the table, [`KeySource`] for keys). Same discipline as
//! `providers.rs`'s `resolve_chat_provider`, for the same reasons: `cargo
//! test` runs tests in parallel, so ambient env/file access from test
//! threads is either a flakiness hazard (`set_var` races) or a real side
//! effect on the developer's machine (keychain prompts, store writes).
//! The impure shell — loading the overlay from the store,
//! `TOME_PROVIDERS_FILE` swapping the defaults layer, the vault,
//! `login_env()` for shell-sourced keys — belongs to `ipc::chat` (slice
//! 2), which implements [`KeySource`] and threads the result in.
//!
//! ## The two load-bearing lines
//!
//! - **`merge`'s region gate.** A region override is honored ONLY when its
//!   value byte-equals one of that row's compiled-in `alternates` — a
//!   built-in's key can only ever be sent to a host named in the signed
//!   binary, no matter what the overlay file says. The same reservation
//!   covers row identity: an `added` row may never reuse a built-in id, or
//!   hiding a built-in and "resurrecting" its id with a foreign base_url
//!   would redirect the key the user pasted for that built-in.
//! - **`resolve` has no ambient fallback.** `pick` names a row; the
//!   environment may fill the selected row's key, it may never select the
//!   row. (The old resolver's "first row that happens to have a key"
//!   behavior is exactly how a stray `OPENAI_API_KEY` in someone's
//!   `.zshrc` silently chose the provider.)

// Landed ahead of its consumer: the `ipc::chat` rewrite (slice 2) is
// this module's production caller; until then only its own tests
// exercise it. Same transitional allow as confine.rs/pty.rs carried.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// Which wire dialect a provider speaks — the JSON `"wire"` field's
/// `"openai"` / `"anthropic"` strings. Kept distinct from [`Auth`]
/// because they vary independently (GLM's Anthropic-Messages shim speaks
/// the anthropic wire while the main row speaks openai; VS Code Copilot's
/// `apiType` is the same observation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wire {
    OpenAi,
    Anthropic,
}

/// Which header carries the API key — the JSON `"auth"` field's `"bearer"`
/// / `"x-api-key"` strings. Per ROW, not per wire: routers and shims
/// regularly accept one dialect's messages with the other dialect's auth
/// header, so the table states it explicitly instead of deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Auth {
    #[default]
    Bearer,
    XApiKey,
}

/// One regional endpoint a provider row may be pinned to. `baseUrl` here
/// is data the user picks from (the Settings region dropdown); the only
/// values `merge` will ever act on are the ones compiled into a built-in
/// row's `alternates`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alternate {
    pub label: String,
    pub base_url: String,
}

/// One provider table row. Field names are camelCase on the wire to match
/// the bundled JSON; everything except the row's identity (`id`, `label`,
/// `wire`, `baseUrl`, `model`) defaults when absent so a hand-edited
/// overlay `added` row stays loadable across schema growth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRow {
    pub id: String,
    pub label: String,
    pub wire: Wire,
    #[serde(default)]
    pub auth: Auth,
    /// Always concrete — no SDK-fallback `None` like the old table's
    /// anthropic row; the shipped JSON names every host.
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub models: Vec<String>,
    /// Iteration-3 discovery hook (plan delta 4): set a `/models` URL and
    /// a later slice can fetch the model list; `None` in every shipped
    /// row, zero behavior today.
    #[serde(default)]
    pub models_url: Option<String>,
    #[serde(default)]
    pub alternates: Vec<Alternate>,
    /// Env var names that may hold this row's key, in order — GLM accepts
    /// both `ZAI_API_KEY` and `ZHIPU_API_KEY`.
    #[serde(default)]
    pub key_env: Vec<String>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// Empty in every shipped row — the old GA-fallbacks 400 came from
    /// attaching betas unconditionally; a row now opts in explicitly.
    #[serde(default)]
    pub betas: Vec<String>,
    #[serde(default)]
    pub builtin: bool,
}

/// The user's choices layered over the bundled table: model overrides,
/// region pins, user-added rows, hidden built-ins, and the one-time
/// migration marker. All fields default so an overlay file written by an
/// older build (or a fresh `{}`) round-trips.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Overlay {
    #[serde(default)]
    pub model: BTreeMap<String, String>,
    #[serde(default)]
    pub region: BTreeMap<String, String>,
    #[serde(default)]
    pub added: Vec<ProviderRow>,
    #[serde(default)]
    pub hidden: Vec<String>,
    #[serde(default)]
    pub migrated: Option<u64>,
}

/// Where a resolved key came from. Reported to the UI (so a card can say
/// "key from your shell") and never carries the key itself. Serialized as
/// `{"kind": ..., "name": ...}` with a lowercase kind; the nameless kinds
/// (`Keychain`, `File`) serialize `name` as `null` and accept a null or
/// absent name on the way back.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyOrigin {
    Keychain,
    File,
    Shell(String),
    Env(String),
}

#[derive(Serialize)]
struct KeyOriginRaw<'a> {
    kind: &'a str,
    name: Option<&'a str>,
}

impl Serialize for KeyOrigin {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let raw = match self {
            KeyOrigin::Keychain => KeyOriginRaw {
                kind: "keychain",
                name: None,
            },
            KeyOrigin::File => KeyOriginRaw {
                kind: "file",
                name: None,
            },
            KeyOrigin::Shell(name) => KeyOriginRaw {
                kind: "shell",
                name: Some(name),
            },
            KeyOrigin::Env(name) => KeyOriginRaw {
                kind: "env",
                name: Some(name),
            },
        };
        raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for KeyOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct KeyOriginRaw {
            kind: String,
            #[serde(default)]
            name: Option<String>,
        }
        let raw = KeyOriginRaw::deserialize(deserializer)?;
        match (raw.kind.as_str(), raw.name) {
            ("keychain", None) => Ok(KeyOrigin::Keychain),
            ("file", None) => Ok(KeyOrigin::File),
            ("shell", Some(name)) => Ok(KeyOrigin::Shell(name)),
            ("env", Some(name)) => Ok(KeyOrigin::Env(name)),
            ("keychain" | "file", Some(_)) => Err(serde::de::Error::custom(format!(
                "invalid KeyOrigin: kind \"{}\" carries no name",
                raw.kind
            ))),
            ("shell" | "env", None) => Err(serde::de::Error::custom(format!(
                "invalid KeyOrigin: kind \"{}\" requires a name",
                raw.kind
            ))),
            (kind, _) => Err(serde::de::Error::unknown_variant(
                kind,
                &["keychain", "file", "shell", "env"],
            )),
        }
    }
}

/// A provider ready for `sse::stream_chat`: concrete host, key, origin,
/// model, and per-row limits. No `Option` fields — by the time this
/// exists, everything is decided (that is the point of the type).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProvider {
    pub id: String,
    pub label: String,
    pub wire: Wire,
    pub auth: Auth,
    /// Always concrete, trailing `/` trimmed.
    pub base_url: String,
    pub api_key: String,
    pub key_origin: KeyOrigin,
    pub model: String,
    pub max_output_tokens: u64,
    pub betas: Vec<String>,
}

/// [`resolve`]'s result — one of exactly four states, never a mix: a
/// provider ready to stream, the picked row with no key (naming the row
/// and the env vars that would satisfy it), a picked id that isn't in the
/// table (never silently coerced to another provider), or no pick at all
/// (fresh install: "pick a provider", not a broken default).
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Ready(ResolvedProvider),
    NoKey {
        id: String,
        label: String,
        key_env: Vec<String>,
    },
    Unknown {
        id: String,
    },
    NoneChosen,
}

/// The ambient half of resolution, injected so [`resolve`] stays pure:
/// implementations walk their own key ladder (vault first, then
/// `login_env().secrets`, then process env — slice 2's `ipc::chat`).
/// Returns the key AND its [`KeyOrigin`] so the caller can report where a
/// key came from without ever handing the key to the UI.
pub trait KeySource {
    fn key_for(&self, row: &ProviderRow) -> Option<(String, KeyOrigin)>;
}

const DEFAULT_PROVIDERS_JSON: &str = include_str!("../../resources/providers.default.json");

/// The bundled table — six rows, in the JSON's order (kimi, glm,
/// deepseek, claude, openai, openrouter). Verified by test; the `expect`
/// panic is a build-integrity failure, not a runtime state ("bundled data
/// that cannot fail to parse" is only true because the test says so).
pub fn default_rows() -> Vec<ProviderRow> {
    serde_json::from_str(DEFAULT_PROVIDERS_JSON)
        .expect("bundled providers.default.json must parse as a Vec<ProviderRow>")
}

/// Layer `ov` over `defaults`: drop hidden ids, apply model overrides
/// (trimmed, non-empty only), honor a region pin ONLY when its value is
/// one of that row's compiled-in alternates (the security line — a
/// built-in's key can only ever be sent to a host in the signed binary),
/// then append the overlay's added rows. Two defenses on `added`: a row
/// whose id collides with a default's is dropped (built-in ids are
/// reserved, so an overlay can neither shadow a visible built-in nor
/// resurrect a hidden one with a foreign base_url), and a row whose
/// `base_url` fails [`vet_base_url`] is dropped — vetting happens at
/// upsert AND at load, so a hand-edited overlay file can never aim a key
/// at an unvetted host.
pub fn merge(defaults: &[ProviderRow], ov: &Overlay) -> Vec<ProviderRow> {
    let mut rows = Vec::with_capacity(defaults.len() + ov.added.len());
    for row in defaults {
        if ov.hidden.iter().any(|id| id == &row.id) {
            continue;
        }
        let mut row = row.clone();
        if let Some(model) = ov.model.get(&row.id) {
            let trimmed = model.trim();
            if !trimmed.is_empty() {
                row.model = trimmed.to_string();
            }
        }
        if let Some(region) = ov.region.get(&row.id) {
            if row.alternates.iter().any(|a| &a.base_url == region) {
                row.base_url = region.clone();
            }
        }
        rows.push(row);
    }
    rows.extend(
        ov.added
            .iter()
            .filter(|row| !defaults.iter().any(|d| d.id == row.id))
            .filter(|row| vet_base_url(&row.base_url).is_ok())
            .cloned(),
    );
    rows
}

/// Resolve the picked provider against `rows`, asking `keys` for the key.
/// Two steps, nothing ambient: trim `pick` (empty/`None` →
/// [`Resolution::NoneChosen`]); look the id up in `rows` (missing →
/// [`Resolution::Unknown`]); ask `keys` for that row's key (`None` or an
/// empty string → [`Resolution::NoKey`] naming THAT row — never a
/// fall-through to some other row that happens to have a key). Ready
/// carries the row with its base URL's trailing `/` trimmed and
/// `max_output_tokens` defaulted to 64 000 when the row doesn't set one.
pub fn resolve(rows: &[ProviderRow], pick: Option<&str>, keys: &dyn KeySource) -> Resolution {
    let Some(pick) = pick.map(str::trim).filter(|p| !p.is_empty()) else {
        return Resolution::NoneChosen;
    };
    let Some(row) = rows.iter().find(|r| r.id == pick) else {
        return Resolution::Unknown {
            id: pick.to_string(),
        };
    };
    let Some((api_key, key_origin)) =
        keys.key_for(row).filter(|(key, _)| !key.is_empty())
    else {
        return Resolution::NoKey {
            id: row.id.clone(),
            label: row.label.clone(),
            key_env: row.key_env.clone(),
        };
    };
    Resolution::Ready(ResolvedProvider {
        id: row.id.clone(),
        label: row.label.clone(),
        wire: row.wire,
        auth: row.auth,
        base_url: row.base_url.trim_end_matches('/').to_string(),
        api_key,
        key_origin,
        model: row.model.clone(),
        max_output_tokens: row.max_output_tokens.unwrap_or(64_000),
        betas: row.betas.clone(),
    })
}

/// `localhost`, `127.0.0.1`, and `::1` — the last in both renderings
/// `Url::host_str()` can produce for an IPv6 literal ("::1" from some
/// paths, "[::1]" with brackets from others), so brackets are stripped
/// before parsing. `IpAddr::is_loopback` rather than a literal list so
/// the rest of 127/8 and IPv6 loopback forms count as loopback too.
fn is_loopback_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Cloud instance-metadata services and the link-local block they live
/// on. Blocking the whole `169.254.` range (not just the two famous
/// names) closes the "your metadata service on a different address"
/// variant for free.
fn is_blocked_host(host: &str) -> bool {
    host == "169.254.169.254" || host == "metadata.google.internal" || host.starts_with("169.254.")
}

/// Vet a pasted base URL: must parse; must be https, or http on a
/// loopback host (local model servers); must not embed credentials (the
/// key belongs in the key field, not logged in a URL); must not carry a
/// query or fragment (silently dropped by some SDKs, so a pasted
/// "endpoint?token=…" would send requests without it); must not target a
/// cloud metadata/link-local host (SSRF-shaped). Returns the normalized
/// URL with any trailing `/` trimmed — the shape `merge`/`resolve`
/// compare against alternates and the SSE layer concatenates paths onto.
pub fn vet_base_url(raw: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "That is not a valid URL.".to_string())?;
    let Some(host) = url.host_str() else {
        return Err("That is not a valid URL.".to_string());
    };
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(host) => {}
        _ => {
            return Err(
                "Only https:// base URLs are allowed (http:// only for localhost).".to_string(),
            )
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Put the API key in the key field, not in the URL.".to_string());
    }
    if url.query().is_some() {
        return Err("Base URLs cannot carry a query string.".to_string());
    }
    if url.fragment().is_some() {
        return Err("Base URLs cannot carry a fragment.".to_string());
    }
    if is_blocked_host(host) {
        return Err("That host is a link-local or metadata-service address.".to_string());
    }
    Ok(url.to_string().trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ================= bundled JSON =================

    #[test]
    fn bundled_json_has_the_six_ids_in_order() {
        let rows = default_rows();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["kimi", "glm", "deepseek", "claude", "openai", "openrouter"]
        );
    }

    #[test]
    fn glm_defaults_to_the_z_ai_international_base_url() {
        let rows = default_rows();
        let glm = rows.iter().find(|r| r.id == "glm").unwrap();
        assert_eq!(glm.base_url, "https://api.z.ai/api/paas/v4");
    }

    #[test]
    fn deepseek_base_url_is_the_bare_host() {
        // no /v1 — DeepSeek serves both /chat/completions and /models at
        // the root, and the old table's /v1 was wrong
        let rows = default_rows();
        let deepseek = rows.iter().find(|r| r.id == "deepseek").unwrap();
        assert_eq!(deepseek.base_url, "https://api.deepseek.com");
    }

    #[test]
    fn glm_accepts_both_zai_and_zhipu_key_env_names() {
        let rows = default_rows();
        let glm = rows.iter().find(|r| r.id == "glm").unwrap();
        assert_eq!(glm.key_env, vec!["ZAI_API_KEY", "ZHIPU_API_KEY"]);
    }

    #[test]
    fn kimi_alternates_include_the_cn_host() {
        let rows = default_rows();
        let kimi = rows.iter().find(|r| r.id == "kimi").unwrap();
        assert!(kimi
            .alternates
            .iter()
            .any(|a| a.base_url == "https://api.moonshot.cn/v1"));
    }

    #[test]
    fn every_row_is_builtin_with_empty_betas_and_null_optional_fields() {
        for row in default_rows() {
            assert!(row.builtin, "{} must be builtin", row.id);
            assert!(row.betas.is_empty(), "{} betas must be empty", row.id);
            assert!(row.models_url.is_none(), "{} modelsUrl must be null", row.id);
            assert!(
                row.max_output_tokens.is_none(),
                "{} maxOutputTokens must be null",
                row.id
            );
            assert!(!row.base_url.is_empty(), "{} baseUrl must be concrete", row.id);
        }
    }

    #[test]
    fn claude_is_the_only_anthropic_x_api_key_row() {
        for row in default_rows() {
            let expected = row.id == "claude";
            assert_eq!(row.wire == Wire::Anthropic, expected, "{} wire", row.id);
            assert_eq!(row.auth == Auth::XApiKey, expected, "{} auth", row.id);
        }
    }

    #[test]
    fn overlay_defaults_from_an_empty_object() {
        // every field #[serde(default)] — an overlay file written by an
        // older build (or a fresh {}) must still load
        let ov: Overlay = serde_json::from_str("{}").unwrap();
        assert_eq!(ov, Overlay::default());
    }

    // ================= merge =================

    #[test]
    fn merge_applies_model_overrides_trimmed_and_only_when_non_empty() {
        let ov = Overlay {
            model: BTreeMap::from([
                ("kimi".to_string(), "  kimi-k4-preview  ".to_string()),
                ("glm".to_string(), "   ".to_string()),
                ("deepseek".to_string(), String::new()),
            ]),
            ..Default::default()
        };
        let merged = merge(&default_rows(), &ov);
        let by_id = |id: &str| merged.iter().find(|r| r.id == id).unwrap();
        assert_eq!(by_id("kimi").model, "kimi-k4-preview");
        assert_eq!(by_id("glm").model, "glm-5.3");
        assert_eq!(by_id("deepseek").model, "deepseek-v4-pro");
    }

    #[test]
    fn merge_rejects_a_region_not_in_the_rows_compiled_in_alternates() {
        let ov = Overlay {
            region: BTreeMap::from([(
                "glm".to_string(),
                "https://evil.example.com/v1".to_string(),
            )]),
            ..Default::default()
        };
        let merged = merge(&default_rows(), &ov);
        let glm = merged.iter().find(|r| r.id == "glm").unwrap();
        assert_eq!(glm.base_url, "https://api.z.ai/api/paas/v4");
    }

    #[test]
    fn merge_honors_a_region_matching_a_compiled_in_alternate() {
        let ov = Overlay {
            region: BTreeMap::from([(
                "glm".to_string(),
                "https://open.bigmodel.cn/api/paas/v4".to_string(),
            )]),
            ..Default::default()
        };
        let merged = merge(&default_rows(), &ov);
        let glm = merged.iter().find(|r| r.id == "glm").unwrap();
        assert_eq!(glm.base_url, "https://open.bigmodel.cn/api/paas/v4");
    }

    #[test]
    fn merge_drops_hidden_rows() {
        let ov = Overlay {
            hidden: vec!["deepseek".to_string()],
            ..Default::default()
        };
        let merged = merge(&default_rows(), &ov);
        assert_eq!(merged.len(), 5);
        assert!(merged.iter().all(|r| r.id != "deepseek"));
    }

    fn synthetic_row(id: &str) -> ProviderRow {
        ProviderRow {
            id: id.to_string(),
            label: format!("{id} (custom)"),
            wire: Wire::OpenAi,
            auth: Auth::Bearer,
            base_url: format!("https://{id}.example.com/v1"),
            model: format!("{id}-model"),
            models: vec![format!("{id}-model")],
            models_url: None,
            alternates: vec![],
            key_env: vec![format!("{}_API_KEY", id.to_uppercase())],
            max_output_tokens: None,
            betas: vec![],
            builtin: false,
        }
    }

    #[test]
    fn merge_appends_added_rows_after_the_builtins() {
        let ov = Overlay {
            added: vec![synthetic_row("myai")],
            ..Default::default()
        };
        let merged = merge(&default_rows(), &ov);
        assert_eq!(merged.len(), 7);
        assert_eq!(merged.last().unwrap().id, "myai");
    }

    #[test]
    fn merge_never_lets_an_added_row_reuse_a_builtin_id() {
        // the hijack shape: hide the real glm, "resurrect" the id with a
        // foreign base_url — built-in ids are reserved either way
        let mut fake = synthetic_row("glm");
        fake.base_url = "https://evil.example.com/v1".to_string();
        let ov = Overlay {
            hidden: vec!["glm".to_string()],
            added: vec![fake],
            ..Default::default()
        };
        let merged = merge(&default_rows(), &ov);
        assert!(
            merged.iter().all(|r| r.id != "glm"),
            "a hidden builtin must not resurrect via an added row"
        );
    }

    #[test]
    fn merge_drops_added_rows_with_an_unvetted_base_url() {
        // vet-on-load (defense in depth on top of the upsert vet): a
        // hand-edited overlay file must never aim a key at a plain-http
        // public host, a metadata address, or an embedded credential.
        for bad in [
            "http://evil.example.com/v1",
            "https://user:pw@host.example/v1",
            "https://169.254.169.254/v1",
            "not a url",
        ] {
            let mut fake = synthetic_row("badrow");
            fake.base_url = bad.to_string();
            let merged = merge(
                &default_rows(),
                &Overlay {
                    added: vec![fake],
                    ..Default::default()
                },
            );
            assert!(
                merged.iter().all(|r| r.id != "badrow"),
                "an unvetted base_url must drop the row: {bad}"
            );
        }
        // the legit local-model shape survives (http + loopback)
        let mut local = synthetic_row("local");
        local.base_url = "http://localhost:9999/v1".to_string();
        let merged = merge(
            &default_rows(),
            &Overlay {
                added: vec![local],
                ..Default::default()
            },
        );
        assert!(merged.iter().any(|r| r.id == "local"));
    }

    // ================= resolve =================

    struct FakeKeys(HashMap<String, String>);

    impl KeySource for FakeKeys {
        fn key_for(&self, row: &ProviderRow) -> Option<(String, KeyOrigin)> {
            self.0
                .get(&row.id)
                .cloned()
                .map(|key| (key, KeyOrigin::Shell("zsh".to_string())))
        }
    }

    #[test]
    fn resolve_none_or_blank_pick_is_none_chosen() {
        let keys = FakeKeys(HashMap::from([("glm".to_string(), "k".to_string())]));
        assert_eq!(resolve(&default_rows(), None, &keys), Resolution::NoneChosen);
        assert_eq!(
            resolve(&default_rows(), Some(""), &keys),
            Resolution::NoneChosen
        );
        assert_eq!(
            resolve(&default_rows(), Some("   "), &keys),
            Resolution::NoneChosen
        );
    }

    #[test]
    fn resolve_an_unknown_id_is_unknown_not_a_fallback() {
        let keys = FakeKeys(HashMap::from([("glm".to_string(), "k".to_string())]));
        assert_eq!(
            resolve(&default_rows(), Some("bogus"), &keys),
            Resolution::Unknown {
                id: "bogus".to_string()
            }
        );
    }

    #[test]
    fn resolve_a_missing_key_names_that_row_with_its_env_names() {
        let keys = FakeKeys(HashMap::new());
        assert_eq!(
            resolve(&default_rows(), Some("glm"), &keys),
            Resolution::NoKey {
                id: "glm".to_string(),
                label: "GLM (Z.ai)".to_string(),
                key_env: vec!["ZAI_API_KEY".to_string(), "ZHIPU_API_KEY".to_string()],
            }
        );
    }

    #[test]
    fn resolve_never_falls_through_to_a_row_that_has_a_key() {
        // a stray OPENAI_API_KEY must not rescue a picked-but-keyless glm
        let keys = FakeKeys(HashMap::from([("openai".to_string(), "sk-x".to_string())]));
        match resolve(&default_rows(), Some("glm"), &keys) {
            Resolution::NoKey { id, .. } => assert_eq!(id, "glm"),
            other => panic!("expected NoKey, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ready_defaults_max_output_tokens_and_trims_the_base_url() {
        let keys = FakeKeys(HashMap::from([("claude".to_string(), "sk-ant".to_string())]));
        let mut rows = default_rows();
        rows.iter_mut()
            .find(|r| r.id == "claude")
            .unwrap()
            .base_url = "https://api.anthropic.com/".to_string();

        let res = resolve(&rows, Some(" claude "), &keys);
        let Resolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.id, "claude");
        assert_eq!(p.label, "Claude (Anthropic)");
        assert_eq!(p.wire, Wire::Anthropic);
        assert_eq!(p.auth, Auth::XApiKey);
        assert_eq!(p.base_url, "https://api.anthropic.com");
        assert_eq!(p.api_key, "sk-ant");
        assert!(matches!(p.key_origin, KeyOrigin::Shell(ref n) if n == "zsh"));
        assert_eq!(p.model, "claude-opus-5");
        assert_eq!(p.max_output_tokens, 64_000);
        assert!(p.betas.is_empty());
    }

    #[test]
    fn resolve_honors_a_rows_explicit_max_output_tokens() {
        let keys = FakeKeys(HashMap::from([("glm".to_string(), "k".to_string())]));
        let mut rows = default_rows();
        rows.iter_mut()
            .find(|r| r.id == "glm")
            .unwrap()
            .max_output_tokens = Some(8_192);
        let res = resolve(&rows, Some("glm"), &keys);
        let Resolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.max_output_tokens, 8_192);
    }

    #[test]
    fn resolve_treats_an_empty_key_as_missing() {
        // "empty string falsy at every rung" — a set-but-empty env var or
        // vault slot is no key at all
        let keys = FakeKeys(HashMap::from([("glm".to_string(), String::new())]));
        match resolve(&default_rows(), Some("glm"), &keys) {
            Resolution::NoKey { id, .. } => assert_eq!(id, "glm"),
            other => panic!("expected NoKey, got {other:?}"),
        }
    }

    #[test]
    fn resolve_finds_added_rows_by_their_own_id() {
        let rows = merge(
            &default_rows(),
            &Overlay {
                added: vec![synthetic_row("myai")],
                ..Default::default()
            },
        );
        let keys = FakeKeys(HashMap::from([("myai".to_string(), "mk".to_string())]));
        let res = resolve(&rows, Some("myai"), &keys);
        let Resolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.id, "myai");
        assert_eq!(p.base_url, "https://myai.example.com/v1");
    }

    // ================= KeyOrigin serde =================

    #[test]
    fn key_origin_round_trips_through_serde_for_all_four_variants() {
        for origin in [
            KeyOrigin::Keychain,
            KeyOrigin::File,
            KeyOrigin::Shell("zsh".to_string()),
            KeyOrigin::Env("OPENAI_API_KEY".to_string()),
        ] {
            let json = serde_json::to_value(&origin).unwrap();
            let back: KeyOrigin = serde_json::from_value(json).unwrap();
            assert_eq!(back, origin);
        }
    }

    #[test]
    fn key_origin_serializes_name_as_null_for_the_nameless_kinds() {
        assert_eq!(
            serde_json::to_value(KeyOrigin::Keychain).unwrap(),
            serde_json::json!({"kind": "keychain", "name": null})
        );
        assert_eq!(
            serde_json::to_value(KeyOrigin::File).unwrap(),
            serde_json::json!({"kind": "file", "name": null})
        );
        assert_eq!(
            serde_json::to_value(KeyOrigin::Shell("zsh".to_string())).unwrap(),
            serde_json::json!({"kind": "shell", "name": "zsh"})
        );
    }

    #[test]
    fn key_origin_accepts_an_absent_or_null_name_for_the_nameless_kinds() {
        assert_eq!(
            serde_json::from_value::<KeyOrigin>(serde_json::json!({"kind": "keychain"})).unwrap(),
            KeyOrigin::Keychain
        );
        assert_eq!(
            serde_json::from_value::<KeyOrigin>(serde_json::json!({"kind": "file", "name": null}))
                .unwrap(),
            KeyOrigin::File
        );
        // a named kind without its name is malformed, not silently defaulted
        assert!(
            serde_json::from_value::<KeyOrigin>(serde_json::json!({"kind": "shell"})).is_err()
        );
    }

    // ================= vet_base_url =================

    #[test]
    fn vet_accepts_https_urls() {
        assert_eq!(
            vet_base_url("https://api.example.com/v1").unwrap(),
            "https://api.example.com/v1"
        );
    }

    #[test]
    fn vet_accepts_http_only_for_loopback_hosts() {
        assert_eq!(
            vet_base_url("http://localhost:1234/v1").unwrap(),
            "http://localhost:1234/v1"
        );
        assert_eq!(
            vet_base_url("http://127.0.0.1:8080/v1").unwrap(),
            "http://127.0.0.1:8080/v1"
        );
        // IPv6 literal: Url::host_str() renders it as "::1" (brackets stripped)
        assert_eq!(
            vet_base_url("http://[::1]:9000/v1").unwrap(),
            "http://[::1]:9000/v1"
        );
    }

    #[test]
    fn vet_rejects_plain_http_to_a_public_host() {
        assert!(vet_base_url("http://example.com/v1").is_err());
    }

    #[test]
    fn vet_rejects_credentials_embedded_in_the_url() {
        let err = vet_base_url("https://user:pw@api.example.com/v1").unwrap_err();
        assert_eq!(err, "Put the API key in the key field, not in the URL.");
    }

    #[test]
    fn vet_rejects_query_strings_and_fragments() {
        assert!(vet_base_url("https://api.example.com/v1?token=x").is_err());
        assert!(vet_base_url("https://api.example.com/v1#frag").is_err());
    }

    #[test]
    fn vet_rejects_cloud_metadata_and_link_local_hosts() {
        assert!(vet_base_url("https://169.254.169.254/v1").is_err());
        assert!(vet_base_url("https://metadata.google.internal/v1").is_err());
        assert!(vet_base_url("https://169.254.1.1/v1").is_err());
    }

    #[test]
    fn vet_rejects_things_that_are_not_urls() {
        assert_eq!(
            vet_base_url("not a url"),
            Err("That is not a valid URL.".to_string())
        );
    }

    #[test]
    fn vet_trims_a_trailing_slash() {
        assert_eq!(
            vet_base_url("https://api.example.com/v1/").unwrap(),
            "https://api.example.com/v1"
        );
    }
}
