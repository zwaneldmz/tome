//! The environment every pty (agent or plain terminal) is built from.
//! `AGENT_ENV_ALLOWLIST`/`build_agent_base_env` port
//! `src/main/lib/agent-env.js` 1:1, pinned by `test/agent-env.test.js`
//! (ported below as `#[cfg(test)] mod tests`). `AGENT_SECRET_KEYS` ports
//! the provider-credential allowlist out of `index.js`'s
//! `AGENT_SECRET_KEYS` (~index.js:226–244) — the actual login-shell
//! shell-out AND the harvest/filtering of its output belong to
//! `login_env.rs` (a different slice's file, and the sole consumer of
//! this constant: it imports [`AGENT_SECRET_KEYS`] from here rather than
//! keeping its own copy — see its module doc comment's "INTEGRATION
//! NOTE" — and does its own parsing+filtering in one pass over raw shell
//! stdout, which is why that half isn't duplicated here too; this module
//! has no process-spawning capability of its own, by design — see the
//! module-level "Pure modules: no Tauri deps" constraint). `compose_agent_env`/
//! `AgentEnvExtras` port the pure layering half of `index.js`'s
//! `buildAgentEnv` (~index.js:667–701) — see that function's doc comment
//! below for exactly which half, and why: unlike the three functions
//! above, `buildAgentEnv` has no dedicated vitest file of its own (it's
//! untested in isolation, only exercised indirectly through
//! `createPty`), so this part is this port's own synthesis of the task's
//! brief rather than a line-for-line spec port — flagged here once
//! rather than at every site below.
//!
//! Before `buildAgentBaseEnv` existed, `buildAgentEnv` spread the ENTIRE
//! main-process environment into every pty — agent or plain terminal,
//! gapped or not — before adding provider secrets on top (TOME-007). Any
//! launch-time value sitting in Tome's own env (a screenshot path, a
//! profiling flag, a stray credential) was therefore readable by every
//! agent CLI. Keep the base allowlist to what a shell/CLI needs to behave
//! like a normal terminal: locale, terminal capabilities, and enough
//! identity/path info to find binaries and a home directory. Provider
//! credentials are layered on separately, by exact key, never by
//! widening this list.

// `build_agent_base_env`/`AGENT_ENV_ALLOWLIST` are covered end-to-end by
// this file's own #[cfg(test)] suite. `AGENT_SECRET_KEYS` already has a
// real caller (`login_env.rs`'s `compute`, via `crate::agent_env::
// AGENT_SECRET_KEYS`). `compose_agent_env`/`AgentEnvExtras` are still
// unused outside tests until the PTY integration slice
// (`ipc::pty::pty_create`, a different slice's file) wires them in. One
// module-level allow here, same rationale as `confine.rs`'s (see that
// module's top doc comment), rather than scattering
// `#[allow(dead_code)]` over individual items.
#![allow(dead_code)]

use std::collections::HashMap;

/// Exact-match keys `build_agent_base_env` copies through unconditionally.
pub const AGENT_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TZ",
    "TMPDIR",
    "TERM",
    "COLORTERM",
];

/// Whole families of locale/desktop-integration variables rather than one
/// key each (`LC_ALL`, `LC_CTYPE`, `LC_COLLATE`, …; `XDG_CONFIG_HOME`,
/// `XDG_CACHE_HOME`, …) — same least-privilege intent as
/// `AGENT_ENV_ALLOWLIST`, just prefix-matched because the exact set
/// varies by OS/desktop environment.
const AGENT_ENV_PREFIXES: &[&str] = &["LC_", "XDG_"];

/// Pure: copies only allowlisted keys (exact match, or one of the
/// prefixes above) out of `process_env`. Callers layer overrides,
/// secrets, and workspace vars onto the returned map afterward (see
/// `compose_agent_env` below).
pub fn build_agent_base_env(process_env: &HashMap<String, String>) -> HashMap<String, String> {
    process_env
        .iter()
        .filter(|(k, _)| {
            AGENT_ENV_ALLOWLIST.contains(&k.as_str())
                || AGENT_ENV_PREFIXES.iter().any(|p| k.starts_with(p))
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Forward only the credentials the supported chat/agent providers
/// actually consume — ports `index.js`'s `AGENT_SECRET_KEYS` (new
/// provider? add its key here, same as the JS original's own comment
/// says). Same `.zshrc` blind spot as `PATH` motivates harvesting these
/// from an interactive login shell rather than the app's own process
/// env: agent CLIs are spawned with `-l -c`, which never reads `.zshrc`,
/// so keys exported there would otherwise be invisible.
pub const AGENT_SECRET_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "REQUESTY_API_KEY",
    "OPENROUTER_API_KEY",
    "DEEPSEEK_API_KEY",
    "MOONSHOT_API_KEY",
    "ZHIPU_API_KEY",
    "GROQ_API_KEY",
    "MISTRAL_API_KEY",
    "XAI_API_KEY",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    // bedrock
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
];

/// Already-resolved inputs `compose_agent_env` layers onto the base env —
/// everything `index.js`'s `buildAgentEnv` awaits before it can build the
/// final map, threaded in instead of fetched here because this module has
/// no I/O capability (no Tauri deps: no store reads, no login-shell
/// shell-out, no airgap proxy). The integration slice that owns the real
/// `pty_create` wiring resolves each field (`login_env`'s cached harvest,
/// `brain::ensure_brain`, `airgap::create_pane_proxy`) and passes the
/// results through here.
#[derive(Debug, Clone, Default)]
pub struct AgentEnvExtras {
    /// `true` for an agent pane (built-in or vetted custom), `false` for
    /// a plain terminal — gates whether `secrets` is applied at all,
    /// mirroring `if (agent) Object.assign(env, await
    /// resolveAgentSecrets())`.
    pub is_agent: bool,
    /// Provider credentials already resolved by the caller — in practice
    /// `login_env::login_env().await.secrets` (or left empty for a
    /// terminal pane, where it is ignored anyway regardless).
    pub secrets: HashMap<String, String>,
    /// `TOME_BRAIN`, set when a workspace is open (`brain::ensureBrain(ws)`'s
    /// result in the JS original).
    pub brain_path: Option<String>,
    /// `TOME_CORE_VAULT`, set when a workspace is open AND a core vault is
    /// configured (`brain::coreInfo(...).configured` in the JS original).
    pub core_vault_root: Option<String>,
    /// The per-pane proxy's port, when the pane is gapped
    /// (`airgap::createPaneProxy`'s result in the JS original) — `None`
    /// for an ungapped pane, matching `if (!gapped) return { env, sandbox:
    /// null }` short-circuiting before the proxy vars are ever set. (The
    /// seatbelt `sandbox` wrap itself is a separate concern the JS
    /// original returns alongside `env` — out of scope for a pure env-map
    /// builder, and not ported here.)
    pub proxy_port: Option<u16>,
}

const PROXY_VAR_NAMES: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "ALL_PROXY",
];

/// Ports the environment-LAYERING half of `index.js`'s `buildAgentEnv` —
/// the pure composition, once every value it would otherwise `await` is
/// already in hand (see `AgentEnvExtras`). Order matches the original
/// exactly: allowlisted base, then the fixed `TERM`/`COLORTERM` pair
/// (unconditionally overwriting whatever `TERM` the allowlist may have
/// carried through from the launching shell — deliberate, not a bug: the
/// pane's pty is created with `name: "xterm-256color"`, so its reported
/// terminal type must match what it was actually created with), then
/// provider secrets for agent panes only, then the brain vars, then the
/// proxy vars for gapped panes only.
pub fn compose_agent_env(
    process_env: &HashMap<String, String>,
    extras: &AgentEnvExtras,
) -> HashMap<String, String> {
    let mut env = build_agent_base_env(process_env);
    env.insert("TERM".to_string(), "xterm-256color".to_string());
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    if extras.is_agent {
        for (k, v) in &extras.secrets {
            env.insert(k.clone(), v.clone());
        }
    }
    if let Some(brain) = &extras.brain_path {
        env.insert("TOME_BRAIN".to_string(), brain.clone());
    }
    if let Some(vault) = &extras.core_vault_root {
        env.insert("TOME_CORE_VAULT".to_string(), vault.clone());
    }
    if let Some(port) = extras.proxy_port {
        let proxy = format!("http://127.0.0.1:{port}");
        for name in PROXY_VAR_NAMES {
            env.insert((*name).to_string(), proxy.clone());
        }
        env.insert("NO_PROXY".to_string(), "localhost,127.0.0.1".to_string());
        env.insert("no_proxy".to_string(), "localhost,127.0.0.1".to_string());
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentinel_env() -> HashMap<String, String> {
        [
            ("PATH", "/usr/bin:/bin:/opt/homebrew/bin"),
            ("HOME", "/Users/tester"),
            ("USER", "tester"),
            ("LOGNAME", "tester"),
            ("SHELL", "/bin/zsh"),
            ("LANG", "en_US.UTF-8"),
            ("TZ", "America/New_York"),
            ("TMPDIR", "/tmp"),
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("LC_ALL", "en_US.UTF-8"),
            ("LC_CTYPE", "en_US.UTF-8"),
            ("XDG_CONFIG_HOME", "/Users/tester/.config"),
            ("XDG_DATA_HOME", "/Users/tester/.local/share"),
            // Sentinels: must never survive into an agent's environment
            // via the base spread — only a caller explicitly assigning
            // into `AgentEnvExtras.secrets` (in practice, `login_env.rs`'s
            // harvest) may add a provider credential, and only for agent
            // (not terminal) panes.
            ("TOME_SHOT", "/Users/tester/Desktop/shot.png"),
            ("TOME_PROFILE", "1"),
            ("SUPER_SECRET_TOKEN", "placeholder-value-must-not-leak"),
            ("GITHUB_TOKEN", "placeholder-value-must-not-leak"),
            ("AWS_SECRET_ACCESS_KEY", "placeholder-value-must-not-leak"), // provider creds come from the login shell, not here
            ("NPM_TOKEN", "placeholder-value-must-not-leak"),
            ("DIGITALOCEAN_TOKEN", "placeholder-value-must-not-leak"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    // ---- build_agent_base_env ----

    #[test]
    fn keeps_path_home_and_the_other_exact_allowlisted_keys() {
        let sentinel = sentinel_env();
        let result = build_agent_base_env(&sentinel);
        for key in [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            "LANG",
            "TZ",
            "TMPDIR",
            "TERM",
            "COLORTERM",
        ] {
            assert_eq!(
                result.get(key),
                sentinel.get(key),
                "{key} should survive unchanged"
            );
        }
    }

    #[test]
    fn keeps_lc_and_xdg_prefix_matched_keys() {
        let sentinel = sentinel_env();
        let result = build_agent_base_env(&sentinel);
        for key in ["LC_ALL", "LC_CTYPE", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
            assert_eq!(
                result.get(key),
                sentinel.get(key),
                "{key} should survive unchanged"
            );
        }
    }

    #[test]
    fn drops_every_sentinel_secret() {
        // The acceptance test this (TOME-007) closes.
        let result = build_agent_base_env(&sentinel_env());
        for key in [
            "TOME_SHOT",
            "TOME_PROFILE",
            "SUPER_SECRET_TOKEN",
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "NPM_TOKEN",
            "DIGITALOCEAN_TOKEN",
        ] {
            assert!(
                !result.contains_key(key),
                "{key} must not survive the base env spread"
            );
        }
    }

    #[test]
    fn does_not_prefix_match_a_key_that_merely_contains_lc_or_xdg_mid_string() {
        let env: HashMap<String, String> =
            [("MYLC_FOO", "x"), ("FOO_XDG_BAR", "y"), ("PATH", "/bin")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        let result = build_agent_base_env(&env);
        assert!(!result.contains_key("MYLC_FOO"));
        assert!(!result.contains_key("FOO_XDG_BAR"));
        assert_eq!(result.get("PATH"), Some(&"/bin".to_string()));
    }

    #[test]
    fn returns_a_fresh_map_and_does_not_mutate_the_input() {
        let sentinel = sentinel_env();
        let before = sentinel.clone();
        let _ = build_agent_base_env(&sentinel);
        assert_eq!(sentinel, before);
    }

    #[test]
    fn handles_a_missing_or_empty_environment_without_panicking() {
        assert!(build_agent_base_env(&HashMap::new()).is_empty());
    }

    #[test]
    fn the_allowlist_itself_contains_exactly_the_documented_exact_match_keys() {
        let mut got: Vec<&str> = AGENT_ENV_ALLOWLIST.to_vec();
        got.sort_unstable();
        let mut want = vec![
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            "LANG",
            "TZ",
            "TMPDIR",
            "TERM",
            "COLORTERM",
        ];
        want.sort_unstable();
        assert_eq!(got, want);
    }

    // ---- AGENT_SECRET_KEYS ----

    #[test]
    fn agent_secret_keys_lists_every_supported_provider_credential() {
        let mut got = AGENT_SECRET_KEYS.to_vec();
        got.sort_unstable();
        let mut want = vec![
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "REQUESTY_API_KEY",
            "OPENROUTER_API_KEY",
            "DEEPSEEK_API_KEY",
            "MOONSHOT_API_KEY",
            "ZHIPU_API_KEY",
            "GROQ_API_KEY",
            "MISTRAL_API_KEY",
            "XAI_API_KEY",
            "GOOGLE_API_KEY",
            "GEMINI_API_KEY",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
        ];
        want.sort_unstable();
        assert_eq!(got, want);
    }

    // ---- compose_agent_env ----

    fn base_process_env() -> HashMap<String, String> {
        [
            ("PATH", "/usr/bin"),
            ("HOME", "/Users/tester"),
            ("TOME_SHOT", "/leak.png"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn always_sets_the_fixed_term_and_colorterm_pair_overriding_any_inherited_term() {
        let mut process_env = base_process_env();
        process_env.insert("TERM".to_string(), "dumb".to_string()); // whatever the launching shell had
        let env = compose_agent_env(&process_env, &AgentEnvExtras::default());
        assert_eq!(env.get("TERM"), Some(&"xterm-256color".to_string()));
        assert_eq!(env.get("COLORTERM"), Some(&"truecolor".to_string()));
    }

    #[test]
    fn applies_secrets_only_for_an_agent_pane() {
        let secrets: HashMap<String, String> = [("ANTHROPIC_API_KEY", "sk-ant-x")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        let terminal_extras = AgentEnvExtras {
            is_agent: false,
            secrets: secrets.clone(),
            ..Default::default()
        };
        let terminal_env = compose_agent_env(&base_process_env(), &terminal_extras);
        assert!(!terminal_env.contains_key("ANTHROPIC_API_KEY"));

        let agent_extras = AgentEnvExtras {
            is_agent: true,
            secrets,
            ..Default::default()
        };
        let agent_env = compose_agent_env(&base_process_env(), &agent_extras);
        assert_eq!(
            agent_env.get("ANTHROPIC_API_KEY"),
            Some(&"sk-ant-x".to_string())
        );
    }

    #[test]
    fn sets_brain_and_core_vault_vars_only_when_present() {
        let no_ws = compose_agent_env(&base_process_env(), &AgentEnvExtras::default());
        assert!(!no_ws.contains_key("TOME_BRAIN"));
        assert!(!no_ws.contains_key("TOME_CORE_VAULT"));

        let with_ws = compose_agent_env(
            &base_process_env(),
            &AgentEnvExtras {
                brain_path: Some("/Users/tester/Tome/Brains/proj".to_string()),
                core_vault_root: Some("/Users/tester/Tome/Brains/core".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(
            with_ws.get("TOME_BRAIN"),
            Some(&"/Users/tester/Tome/Brains/proj".to_string())
        );
        assert_eq!(
            with_ws.get("TOME_CORE_VAULT"),
            Some(&"/Users/tester/Tome/Brains/core".to_string())
        );
    }

    #[test]
    fn sets_every_proxy_var_only_for_a_gapped_pane_with_a_port() {
        let ungapped = compose_agent_env(&base_process_env(), &AgentEnvExtras::default());
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "ALL_PROXY",
            "NO_PROXY",
            "no_proxy",
        ] {
            assert!(
                !ungapped.contains_key(name),
                "{name} must be absent for an ungapped pane"
            );
        }

        let gapped = compose_agent_env(
            &base_process_env(),
            &AgentEnvExtras {
                proxy_port: Some(54321),
                ..Default::default()
            },
        );
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "ALL_PROXY",
        ] {
            assert_eq!(
                gapped.get(name),
                Some(&"http://127.0.0.1:54321".to_string()),
                "{name}"
            );
        }
        assert_eq!(
            gapped.get("NO_PROXY"),
            Some(&"localhost,127.0.0.1".to_string())
        );
        assert_eq!(
            gapped.get("no_proxy"),
            Some(&"localhost,127.0.0.1".to_string())
        );
    }

    #[test]
    fn the_base_allowlist_still_applies_inside_compose() {
        let env = compose_agent_env(&base_process_env(), &AgentEnvExtras::default());
        assert!(
            !env.contains_key("TOME_SHOT"),
            "compose_agent_env must not widen the base allowlist"
        );
        assert_eq!(env.get("PATH"), Some(&"/usr/bin".to_string()));
    }
}
