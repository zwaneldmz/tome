//! Assistant chat provider resolution — ports `src/shared/chat-providers.js`
//! (the `CHAT_PROVIDERS` table + `DEFAULT_CHAT_PROVIDER`) and
//! `resolveChatProvider` from `src/main/lib/chat-client.js`. Names, wire
//! shapes, endpoints, and default model ids only — no secrets live in this
//! file; callers hand in whatever `env`/`secrets` maps they already
//! resolved (`std::env::vars()` and `login_env::login_env().await.secrets`
//! at the one real call site, `ipc::chat`).
//!
//! [`resolve_chat_provider`] is a PURE function of its four parameters —
//! unlike the JS original, which reads `process.env` directly inside the
//! function body. Two reasons: (1) `cargo test` runs tests in parallel by
//! default, and `std::env::set_var`/`remove_var` mutating real process env
//! vars from concurrent tests is a real flakiness hazard the JS suite
//! sidesteps only because vitest's single-threaded-per-file model
//! serializes access to `process.env` in a way Rust's test runner does not;
//! (2) it matches this crate's own established idiom for porting a JS
//! function that reads ambient state (see e.g. `login_env::
//! default_shell_for_platform` taking `os: &str` instead of reading
//! `std::env::consts::OS`, or `ipc::pty::pane_env` taking `process_env: &
//! HashMap<String, String>`). The impure shell — gathering `std::env::
//! vars()`, awaiting `login_env::login_env()`, and reading the two store
//! keys — lives in `ipc::chat`, same split as everywhere else in this
//! codebase.
//!
//! ## One deliberate behavioral deviation: the env-override anthropic-wire
//! branch's API key
//!
//! The JS original's env-override + anthropic-wire branch builds `opts: {
//! baseURL: envBase || undefined }` — no `apiKey` field at all. That's not
//! a bug there: `opts` becomes the Anthropic SDK constructor's argument
//! (`new Anthropic(opts)`, in `index.js`'s `chat:send` handler), and the
//! SDK itself falls back to reading `process.env.ANTHROPIC_API_KEY`
//! internally whenever `apiKey` is omitted from its constructor options.
//! This Rust port hand-rolls the Anthropic wire (`chat::sse::
//! stream_anthropic`) — there is no SDK underneath it to supply that
//! fallback — so [`resolve_chat_provider`] resolves `ANTHROPIC_API_KEY`
//! explicitly in this branch too (same precedence as every other branch:
//! secrets map, then env), preserving the FUNCTIONAL behavior (successful
//! auth against `api.anthropic.com`) rather than the literal JS `opts`
//! shape (which relied on an SDK crutch this port doesn't have). Flagged
//! here since no vitest assertion exercises `p.opts.apiKey` for this
//! specific branch — only `p.wire` is checked there.

use std::collections::HashMap;

/// Which wire dialect a provider speaks — `'openai' | 'anthropic'` in the
/// JS original's `wire` string field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    OpenAi,
    Anthropic,
}

/// One entry of `src/shared/chat-providers.js`'s `CHAT_PROVIDERS` map. A
/// `&'static` struct array, not a `HashMap` — `Object.entries(CHAT_PROVIDERS)`
/// iterates in the object literal's definition order (kimi, glm, claude),
/// which `ipc::chat::chat_providers`'s own `providers` response array
/// preserves; a plain array trivially preserves that order without pulling
/// in an order-preserving map type for a 3-entry, compile-time-fixed table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderEntry {
    pub id: &'static str,
    pub label: &'static str,
    pub wire: Wire,
    /// `None` for the anthropic wire (`baseURL: null` in the JS source —
    /// "the SDK knows its own endpoint"); `chat::sse::stream_anthropic`
    /// falls back to the real API's base URL itself when this is `None`.
    pub base_url: Option<&'static str>,
    pub model: &'static str,
    pub key_env: &'static str,
}

pub const CHAT_PROVIDERS: &[ProviderEntry] = &[
    ProviderEntry {
        id: "kimi",
        label: "Kimi (Moonshot)",
        wire: Wire::OpenAi,
        base_url: Some("https://api.moonshot.ai/v1"),
        // The provider's own model id — kept verbatim from the JS source's
        // own comment: user-overridable in Preferences or TOME_CHAT_MODEL.
        model: "kimi-k3",
        key_env: "MOONSHOT_API_KEY",
    },
    ProviderEntry {
        id: "glm",
        label: "GLM (Zhipu)",
        wire: Wire::OpenAi,
        base_url: Some("https://open.bigmodel.cn/api/paas/v4"),
        model: "glm-5.2",
        key_env: "ZHIPU_API_KEY",
    },
    ProviderEntry {
        id: "claude",
        label: "Claude (Anthropic)",
        wire: Wire::Anthropic,
        base_url: None,
        model: "claude-opus-5",
        key_env: "ANTHROPIC_API_KEY",
    },
];

pub const DEFAULT_CHAT_PROVIDER: &str = "kimi";

/// Requesty routes Claude via vertex/bedrock; bare `anthropic/*` model ids
/// 403 unless the key's Model Library approves them — verbatim from the JS
/// module doc comment.
const REQUESTY_MODEL: &str = "vertex/claude-opus-4-8@eu";
const REQUESTY_BASE: &str = "https://router.requesty.ai";

pub fn provider_entry(id: &str) -> Option<&'static ProviderEntry> {
    CHAT_PROVIDERS.iter().find(|p| p.id == id)
}

fn claude_entry() -> &'static ProviderEntry {
    provider_entry("claude").expect("CHAT_PROVIDERS always has a \"claude\" entry")
}

/// `CHAT_PROVIDERS[stored] ? stored : DEFAULT_CHAT_PROVIDER` — the id
/// resolution shared by [`resolve_chat_provider`]'s store-backed branch and
/// `ipc::chat::chat_providers`'s `active` field (the JS original computes
/// this exact expression independently in both `resolveChatProvider` and
/// the `chat:providers` handler; this port gives it one name instead of
/// two copies).
pub fn active_provider_id(stored: Option<&str>) -> &'static str {
    stored
        .and_then(provider_entry)
        .map(|e| e.id)
        .unwrap_or(DEFAULT_CHAT_PROVIDER)
}

/// `!!(secrets[keyEnv] || process.env[keyEnv])` — the `chat:providers`
/// response's per-provider `keySet` boolean. Never returns the key itself.
pub fn key_is_set(
    secrets: &HashMap<String, String>,
    env: &HashMap<String, String>,
    key_env: &str,
) -> bool {
    truthy_lookup(secrets, key_env)
        .or_else(|| truthy_lookup(env, key_env))
        .is_some()
}

/// A provider ready for `chat::sse::stream_chat` — the JS original's
/// resolved-provider object shape, minus the field
/// [`ProviderResolution::KeyMissing`] carries instead.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProvider {
    pub id: String,
    pub label: String,
    pub wire: Wire,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: String,
    /// Gates `ipc::chat::chat_send`'s betas/fallbacks attachment. Mirrors
    /// `entry.wire === 'anthropic'` in the JS original's store-backed
    /// branch; explicitly forced `false` in the env-override and Requesty
    /// branches regardless of wire — routers 400 on Anthropic-only beta
    /// args (verbatim JS comment).
    pub beta: bool,
}

/// [`resolve_chat_provider`]'s result — the JS original either returns a
/// full resolved-provider object OR `{ keyMissing: entry, id }` (returned
/// mid-function, before `wire`/`opts`/`model`/`beta` are ever computed),
/// never a mix of both. Modeled as an enum rather than an
/// `Option`-plus-error-field so a caller cannot accidentally read
/// `.model`/`.wire` off a `KeyMissing` result — no such fields exist on
/// that variant.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderResolution {
    Ready(ResolvedProvider),
    KeyMissing {
        entry: &'static ProviderEntry,
        id: String,
    },
}

/// `!!s` in JS — `Some` only for a genuinely non-empty string. JS's `||`
/// chains (`secrets[k] || process.env[k]`, `envBase || envModel`, …) treat
/// `""` as falsy exactly like `null`/`undefined`; this is that check,
/// applied uniformly everywhere [`resolve_chat_provider`] reads one of its
/// map/string inputs.
fn truthy(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.is_empty())
}

fn truthy_lookup<'a>(map: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    truthy(map.get(key).map(String::as_str))
}

/// Direct port of `resolveChatProvider` (`src/main/lib/chat-client.js`).
/// See the module doc comment for the one deliberate behavioral deviation
/// (env-override + anthropic-wire's `api_key`) and for why this is a pure
/// function of four parameters rather than reading `process.env`/an async
/// `readStore` callback directly.
///
/// Precedence, verbatim from the JS original:
/// 1. `TOME_CHAT_BASE_URL`/`TOME_CHAT_MODEL` env override — anthropic wire
///    iff `TOME_CHAT_WIRE == "anthropic"` or the base URL's host is
///    `api.anthropic.com`, else openai wire.
/// 2. `REQUESTY_API_KEY` (env, then secrets — note the reversed precedence
///    from every other key lookup below) → the Requesty router, hardcoded
///    vertex model, anthropic wire, beta forced off.
/// 3. The stored `chat-provider` preference (validated against
///    [`CHAT_PROVIDERS`], default [`DEFAULT_CHAT_PROVIDER`]) with an
///    optional stored `chat-model` override —
///    [`ProviderResolution::KeyMissing`] if that provider's key isn't in
///    `secrets` or `env`.
pub fn resolve_chat_provider(
    env: &HashMap<String, String>,
    secrets: &HashMap<String, String>,
    stored_provider: Option<&str>,
    stored_model: Option<&str>,
) -> ProviderResolution {
    let env_base = truthy_lookup(env, "TOME_CHAT_BASE_URL");
    let env_model = truthy_lookup(env, "TOME_CHAT_MODEL");
    if env_base.is_some() || env_model.is_some() {
        let env_host = env_base
            .and_then(|b| reqwest::Url::parse(b).ok())
            .and_then(|u| u.host_str().map(str::to_string));
        let anthropic_wire = truthy_lookup(env, "TOME_CHAT_WIRE") == Some("anthropic")
            || env_host.as_deref() == Some("api.anthropic.com");
        // Same resolution the openai-wire sub-branch below uses for its
        // apiKey — see the module doc comment's deviation note.
        let api_key = truthy_lookup(secrets, "ANTHROPIC_API_KEY")
            .or_else(|| truthy_lookup(env, "ANTHROPIC_API_KEY"))
            .map(str::to_string);
        let base_url = if anthropic_wire {
            env_base.map(str::to_string)
        } else {
            Some(env_base.unwrap_or("https://api.anthropic.com").to_string())
        };
        return ProviderResolution::Ready(ResolvedProvider {
            id: "env".to_string(),
            label: "Custom endpoint (TOME_CHAT_BASE_URL/TOME_CHAT_MODEL)".to_string(),
            wire: if anthropic_wire {
                Wire::Anthropic
            } else {
                Wire::OpenAi
            },
            base_url,
            api_key,
            // Falls back to the claude model id regardless of which wire
            // was chosen — a quirk of the JS source (`CHAT_PROVIDERS.claude.model`
            // unconditionally), ported verbatim rather than "fixed".
            model: env_model.unwrap_or(claude_entry().model).to_string(),
            beta: false,
        });
    }

    if let Some(req_key) = truthy_lookup(env, "REQUESTY_API_KEY")
        .or_else(|| truthy_lookup(secrets, "REQUESTY_API_KEY"))
    {
        return ProviderResolution::Ready(ResolvedProvider {
            id: "requesty".to_string(),
            label: "Requesty router".to_string(),
            wire: Wire::Anthropic,
            base_url: Some(REQUESTY_BASE.to_string()),
            api_key: Some(req_key.to_string()),
            model: REQUESTY_MODEL.to_string(),
            beta: false,
        });
    }

    let id = active_provider_id(stored_provider);
    let entry =
        provider_entry(id).expect("active_provider_id always returns a valid CHAT_PROVIDERS id");
    let Some(api_key) =
        truthy_lookup(secrets, entry.key_env).or_else(|| truthy_lookup(env, entry.key_env))
    else {
        return ProviderResolution::KeyMissing {
            entry,
            id: id.to_string(),
        };
    };
    let model = stored_model
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(entry.model);
    ProviderResolution::Ready(ResolvedProvider {
        id: id.to_string(),
        label: entry.label.to_string(),
        wire: entry.wire,
        base_url: if matches!(entry.wire, Wire::OpenAi) {
            entry.base_url.map(str::to_string)
        } else {
            None
        },
        api_key: Some(api_key.to_string()),
        model: model.to_string(),
        beta: matches!(entry.wire, Wire::Anthropic),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ================= resolveChatProvider — ported from
    // test/chat-providers.test.js's `describe('resolveChatProvider', ...)` =================

    #[test]
    fn env_override_wins_over_everything_openai_wire_by_default() {
        let env = map(&[
            ("TOME_CHAT_BASE_URL", "http://localhost:1234/v1"),
            ("TOME_CHAT_MODEL", "local-model"),
            ("REQUESTY_API_KEY", "rq"),
        ]);
        let secrets = map(&[("ANTHROPIC_API_KEY", "sk")]);
        let res = resolve_chat_provider(&env, &secrets, Some("claude"), None);
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.wire, Wire::OpenAi);
        assert_eq!(p.base_url.as_deref(), Some("http://localhost:1234/v1"));
        assert_eq!(p.model, "local-model");
        assert!(!p.beta);
    }

    #[test]
    fn env_override_on_api_anthropic_com_hostname_is_anthropic_wire() {
        let env = map(&[("TOME_CHAT_BASE_URL", "https://api.anthropic.com")]);
        let res = resolve_chat_provider(&env, &HashMap::new(), None, None);
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.wire, Wire::Anthropic);
    }

    #[test]
    fn env_override_with_explicit_tome_chat_wire_is_anthropic_regardless_of_hostname() {
        let env = map(&[
            ("TOME_CHAT_BASE_URL", "http://localhost:9999"),
            ("TOME_CHAT_WIRE", "anthropic"),
        ]);
        let res = resolve_chat_provider(&env, &HashMap::new(), None, None);
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.wire, Wire::Anthropic);
    }

    #[test]
    fn env_override_anthropic_wire_still_resolves_an_api_key_secrets_over_env() {
        // The one deliberate deviation from the JS opts shape — see the
        // module doc comment. Functional parity (auth actually succeeds)
        // matters more here than a literal copy of a shape that relied on
        // the Anthropic SDK's own implicit env fallback.
        let env_only = map(&[
            ("TOME_CHAT_BASE_URL", "https://api.anthropic.com"),
            ("ANTHROPIC_API_KEY", "env-key"),
        ]);
        let res = resolve_chat_provider(&env_only, &HashMap::new(), None, None);
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.api_key.as_deref(), Some("env-key"));

        let secrets = map(&[("ANTHROPIC_API_KEY", "secret-key")]);
        let res2 = resolve_chat_provider(&env_only, &secrets, None, None);
        let ProviderResolution::Ready(p2) = res2 else {
            panic!("expected Ready, got {res2:?}")
        };
        assert_eq!(p2.api_key.as_deref(), Some("secret-key"));
    }

    #[test]
    fn requesty_key_beats_the_store_and_keeps_the_vertex_model_verbatim() {
        let env = map(&[("REQUESTY_API_KEY", "rq-key")]);
        let res = resolve_chat_provider(&env, &HashMap::new(), Some("glm"), None);
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.wire, Wire::Anthropic);
        assert_eq!(p.api_key.as_deref(), Some("rq-key"));
        assert_eq!(p.base_url.as_deref(), Some(REQUESTY_BASE));
        assert_eq!(p.model, REQUESTY_MODEL);
        assert!(!p.beta);
    }

    #[test]
    fn requesty_env_key_beats_a_requesty_secret_too() {
        // Unlike every other key lookup in this module, REQUESTY_API_KEY
        // checks env BEFORE secrets — verbatim JS precedence.
        let env = map(&[("REQUESTY_API_KEY", "env-rq")]);
        let secrets = map(&[("REQUESTY_API_KEY", "secret-rq")]);
        let res = resolve_chat_provider(&env, &secrets, None, None);
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.api_key.as_deref(), Some("env-rq"));
    }

    #[test]
    fn store_provider_and_login_shell_key_resolves_that_provider() {
        let secrets = map(&[("ZHIPU_API_KEY", "z-key")]);
        let res = resolve_chat_provider(&HashMap::new(), &secrets, Some("glm"), Some("glm-custom"));
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.id, "glm");
        assert_eq!(p.wire, Wire::OpenAi);
        assert_eq!(
            p.base_url.as_deref(),
            provider_entry("glm").unwrap().base_url
        );
        assert_eq!(p.api_key.as_deref(), Some("z-key"));
        assert_eq!(p.model, "glm-custom");
    }

    #[test]
    fn defaults_to_kimi_with_its_default_model_when_the_store_is_empty() {
        let secrets = map(&[("MOONSHOT_API_KEY", "m-key")]);
        let res = resolve_chat_provider(&HashMap::new(), &secrets, None, None);
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.id, DEFAULT_CHAT_PROVIDER);
        assert_eq!(p.model, provider_entry("kimi").unwrap().model);
        assert_eq!(
            p.base_url.as_deref(),
            provider_entry("kimi").unwrap().base_url
        );
    }

    #[test]
    fn an_invalid_stored_provider_falls_back_to_the_default() {
        let secrets = map(&[("MOONSHOT_API_KEY", "m-key")]);
        let res = resolve_chat_provider(&HashMap::new(), &secrets, Some("bogus"), None);
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.id, DEFAULT_CHAT_PROVIDER);
    }

    #[test]
    fn missing_key_resolves_to_key_missing_naming_the_provider_entry() {
        let res = resolve_chat_provider(&HashMap::new(), &HashMap::new(), None, None);
        let ProviderResolution::KeyMissing { entry, id } = res else {
            panic!("expected KeyMissing, got {res:?}")
        };
        assert_eq!(entry.id, "kimi");
        assert_eq!(id, "kimi");
    }

    #[test]
    fn claude_resolves_anthropic_wire_with_beta_on() {
        let secrets = map(&[("ANTHROPIC_API_KEY", "a-key")]);
        let res = resolve_chat_provider(&HashMap::new(), &secrets, Some("claude"), None);
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.wire, Wire::Anthropic);
        assert!(p.beta);
        assert_eq!(p.model, provider_entry("claude").unwrap().model);
    }

    #[test]
    fn a_blank_stored_model_override_falls_back_to_the_entrys_default() {
        let secrets = map(&[("MOONSHOT_API_KEY", "m-key")]);
        let res = resolve_chat_provider(&HashMap::new(), &secrets, None, Some("   "));
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.model, provider_entry("kimi").unwrap().model);
    }

    #[test]
    fn a_stored_model_override_is_trimmed() {
        let secrets = map(&[("MOONSHOT_API_KEY", "m-key")]);
        let res = resolve_chat_provider(&HashMap::new(), &secrets, None, Some("  custom-id  "));
        let ProviderResolution::Ready(p) = res else {
            panic!("expected Ready, got {res:?}")
        };
        assert_eq!(p.model, "custom-id");
    }

    // ================= key_is_set / active_provider_id — used directly by
    // ipc::chat::chat_providers, not exercised through resolveChatProvider =================

    #[test]
    fn key_is_set_true_from_either_secrets_or_env_false_otherwise() {
        let secrets = map(&[("MOONSHOT_API_KEY", "x")]);
        let env = map(&[("ZHIPU_API_KEY", "y")]);
        assert!(key_is_set(&secrets, &HashMap::new(), "MOONSHOT_API_KEY"));
        assert!(key_is_set(&HashMap::new(), &env, "ZHIPU_API_KEY"));
        assert!(!key_is_set(
            &HashMap::new(),
            &HashMap::new(),
            "MOONSHOT_API_KEY"
        ));
    }

    #[test]
    fn key_is_set_treats_an_empty_string_value_as_not_set() {
        let secrets = map(&[("MOONSHOT_API_KEY", "")]);
        assert!(!key_is_set(&secrets, &HashMap::new(), "MOONSHOT_API_KEY"));
    }

    #[test]
    fn active_provider_id_falls_back_to_default_for_none_or_invalid() {
        assert_eq!(active_provider_id(None), DEFAULT_CHAT_PROVIDER);
        assert_eq!(active_provider_id(Some("bogus")), DEFAULT_CHAT_PROVIDER);
        assert_eq!(active_provider_id(Some("glm")), "glm");
    }

    // ================= CHAT_PROVIDERS table shape =================

    #[test]
    fn chat_providers_has_exactly_kimi_glm_claude_in_that_order() {
        let ids: Vec<&str> = CHAT_PROVIDERS.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec!["kimi", "glm", "claude"]);
    }

    #[test]
    fn only_claude_speaks_the_anthropic_wire() {
        for p in CHAT_PROVIDERS {
            assert_eq!(p.wire == Wire::Anthropic, p.id == "claude");
        }
    }
}
