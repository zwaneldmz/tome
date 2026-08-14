//! The conductor's 7 tools — direct port of `conductor.js`'s `TOOLS`
//! (Anthropic-shaped `input_schema` defs), `runTool` dispatch, and the two
//! text sanitizers (`stripAnsi`/`stripControlChars`) it imports from
//! `src/shared/terminal-text.js`. That shared module itself stays JS (the
//! renderer's own `panes.js` still needs it for the identical no-auto-submit
//! guard); this is a from-scratch Rust reading of the same two regexes,
//! scoped to this crate's one consumer — the same "shared/** stays JS,
//! narrow Rust port of just what this side needs" shape `flow::model`'s doc
//! comment already establishes for `flow-model.js`.
//!
//! `read_flow`/`draft_flow` call `flow::tools::read_flow_tool`/
//! `draft_flow_tool` directly — no duplication (task brief: "consume
//! flow::tools").

use std::path::Path;

use regex::Regex;
use serde_json::{json, Value};

use super::env::ConductorEnv;
use super::state::Conductor;
use crate::flow;

// ================= text sanitizers (shared/terminal-text.js) =================

/// CSI + OSC + stray escapes + control chars (keeps `\n`/`\t`) — verbatim
/// translation of `stripAnsi`'s four chained `.replace`s.
pub(crate) fn strip_ansi(s: &str) -> String {
    static OSC: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static CSI: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static ESC: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static CTRL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let osc = OSC.get_or_init(|| Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").expect("static pattern is valid"));
    let csi = CSI.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("static pattern is valid"));
    let esc = ESC.get_or_init(|| Regex::new(r"\x1b[@-_]").expect("static pattern is valid"));
    let ctrl = CTRL.get_or_init(|| Regex::new(r"[\x00-\x08\x0b-\x1f\x7f]").expect("static pattern is valid"));
    let s = osc.replace_all(s, "");
    let s = csi.replace_all(&s, "");
    let s = esc.replace_all(&s, "");
    ctrl.replace_all(&s, "").into_owned()
}

/// With auto-run off, model-typed text must stay un-submitted — strips the
/// control chars that would submit or signal on their own. Tab (`\x09`)
/// survives; unlike [`strip_ansi`]'s control class, this one also strips
/// `\n` (`\x0a`) — the JS original's two regexes deliberately differ by
/// exactly that one boundary (`\x0b` vs `\x0a`), ported verbatim.
pub(crate) fn strip_control_chars(s: &str) -> String {
    static CTRL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let ctrl = CTRL.get_or_init(|| Regex::new(r"[\x00-\x08\x0a-\x1f\x7f]").expect("static pattern is valid"));
    ctrl.replace_all(s, "").into_owned()
}

// ================= dynamic TOOLS / SYSTEM text =================

fn agent_kinds_text(agent_ids: &[String]) -> String {
    agent_ids.join(", ")
}

/// The `open_pane` half of `OPENABLE_KINDS_DESCRIPTION`, rebuilt off the
/// current agent list — `openableKindsDescription()`.
fn openable_kinds_description(agent_ids: &[String]) -> String {
    let mut kinds = vec!["terminal".to_string()];
    kinds.extend(agent_ids.iter().cloned());
    kinds.extend(["chat", "brain", "flow", "runs"].iter().map(|s| s.to_string()));
    let quoted: Vec<String> = kinds.iter().map(|k| format!("'{k}'")).collect();
    format!("kind is one of: {}.", quoted.join(", "))
}

/// `draftFlowDescription()`, verbatim text with the agent-kinds parenthetical
/// interpolated.
fn draft_flow_description(agent_ids: &[String]) -> String {
    format!(
        "Create or overwrite a flow at .tome/flows/<name>.flow.json; a flow pane opens and live-updates as you refine it. `flow` is the whole document: {{version: 1, name, nodes: [], edges: []}}. Node: {{id, kind, name, instructions, expects, produces, inputs: [{{name}}], outputs: [{{name}}], x, y, model?}} — kind is \"terminal\" or an agent CLI ({}); give every node a unique short id like \"n1\"; omit x/y for auto-layout. Edge: {{id, from, to, fromOutput, toInput}} joining an output port name to an input port name. Structural errors are refused outright; contract warnings come back for you to raise with the user. Only call this after the user agrees to start (or change) a draft.",
        agent_kinds_text(agent_ids)
    )
}

/// `systemPrompt()`, verbatim text — the agent-kinds parenthetical is the
/// only dynamic segment.
pub(crate) fn system_prompt_text(agent_ids: &[String]) -> String {
    format!(
        "You are the assistant pane inside Tome, a desktop coding harness whose grid holds terminal panes, agent CLI panes ({}), editors, documents, and note vaults. You have tools to inspect and drive the workspace: list panes, read a terminal\u{2019}s recent output, type into a terminal, open new panes or files. Use them whenever the user refers to other panes (\"what is claude doing\", \"run the tests over there\", \"open a terminal\"). type_in_terminal only submits when the user has enabled auto-run; otherwise the text is left for them to press Enter on — say so when it happens. Your replies may be read aloud, so keep them focused, brief, and speakable. Plain text only — no markdown tables. When the user wants to design a workflow, act as a flow architect. Flows are graphs of agent nodes saved as .tome/flows/<name>.flow.json; you shape them with read_flow and draft_flow. Restate the goal in one sentence, then ask one question at a time — never a questionnaire. Draft early and refine as you go: once the user agrees to start, call draft_flow as soon as a shape exists, then say what you added and what you assumed. Every node needs instructions, expects, and produces; a blank contract is a question to ask, not a field to invent, so challenge vagueness and voice every warning draft_flow returns. Never overwrite a flow you did not draft in this conversation without asking. You cannot run flows: when the user approves the final shape, say it is saved and that they press Run on the flow pane.",
        agent_kinds_text(agent_ids)
    )
}

/// `TOOLS`, rebuilt fresh from `agent_ids` — see [`Conductor::tools`].
pub(crate) fn tool_schemas(agent_ids: &[String]) -> Vec<Value> {
    vec![
        json!({
            "name": "list_panes",
            "description": "List every open pane in the workspace grid: id, tab title, and for terminal/agent panes the CLI kind, working directory, and whether the process is still alive.",
            "input_schema": { "type": "object", "properties": {} },
        }),
        json!({
            "name": "read_terminal",
            "description": "Read the recent output (scrollback tail) of a terminal or agent pane, ANSI-stripped. Use list_panes first to find the pane id.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pane_id": { "type": "string", "description": "pane id from list_panes" },
                    "lines": { "type": "number", "description": "how many trailing lines (default 60)" },
                },
                "required": ["pane_id"],
            },
        }),
        json!({
            "name": "type_in_terminal",
            "description": "Type text into a terminal or agent pane. Set press_enter to also submit it — that only takes effect when the user has enabled \"assistant may run commands\"; otherwise the text is left in the prompt for the user to review and submit.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pane_id": { "type": "string" },
                    "text": { "type": "string" },
                    "press_enter": { "type": "boolean" },
                },
                "required": ["pane_id", "text"],
            },
        }),
        json!({
            "name": "open_pane",
            // `rebuildPrompts`'s `t.description = 'Open a new pane in the
            // grid. ' + openableKindsDescription()` — the leading sentence
            // is NOT part of `openableKindsDescription()` itself (that
            // function is also conceptually "the open_pane half of
            // OPENABLE_KINDS_DESCRIPTION" per its own JS doc comment), so it
            // must be prepended here, at the one call site that builds the
            // tool's actual description.
            "description": format!("Open a new pane in the grid. {}", openable_kinds_description(agent_ids)),
            "input_schema": {
                "type": "object",
                "properties": { "kind": { "type": "string" } },
                "required": ["kind"],
            },
        }),
        json!({
            "name": "open_file",
            "description": "Open a file from disk in an editor/viewer pane (absolute path).",
            "input_schema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            },
        }),
        json!({
            "name": "read_flow",
            "description": "Read a saved flow. Without a name, lists the flows that exist in the workspace. With a name, returns the raw JSON of .tome/flows/<name>.flow.json.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "flow name without the .flow.json suffix" },
                    "root": { "type": "string", "description": "workspace folder (only needed when several are open)" },
                },
            },
        }),
        json!({
            "name": "draft_flow",
            "description": draft_flow_description(agent_ids),
            "input_schema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "flow name without the .flow.json suffix" },
                    "flow": { "type": "object", "description": "the entire flow document" },
                    "root": { "type": "string", "description": "workspace folder (only needed when several are open)" },
                },
                "required": ["name", "flow"],
            },
        }),
    ]
}

// ================= runTool dispatch =================

/// `runTool(name, input, chatId)` — dispatches to one of the 7 impls below,
/// or `"Unknown tool."` for anything else. Infallible (always returns a
/// `String`): unlike the JS original's `try { out = runTool(...) } catch`,
/// none of these impls can panic on attacker-shaped `input` (every field
/// read is `Option`-guarded), so [`super::chat::run_chat`] does not need
/// its own catch-and-format-as-error branch either — a deliberate, minor
/// simplification over the JS `ok:false` path, noted here since nothing
/// currently exercises it.
pub fn run_tool(c: &Conductor, env: &ConductorEnv, name: &str, input: &Value, chat_id: &str) -> String {
    match name {
        "list_panes" => list_panes(c),
        "read_terminal" => read_terminal(c, env, input),
        "type_in_terminal" => type_in_terminal(c, env, input),
        "open_pane" => open_pane(env, input, chat_id),
        "open_file" => open_file(env, input, chat_id),
        "read_flow" => read_flow(env, input),
        "draft_flow" => draft_flow(env, input, chat_id),
        _ => "Unknown tool.".to_string(),
    }
}

/// `block.input?.pane_id || block.input?.kind || block.input?.path ||
/// block.input?.name || ''` — the `chat:tool`/`conductor:tool` hint.
pub(crate) fn tool_hint(input: &Value) -> String {
    for key in ["pane_id", "kind", "path", "name"] {
        if let Some(s) = input.get(key).and_then(Value::as_str) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

fn list_panes(c: &Conductor) -> String {
    let rows: Vec<Value> = c
        .panes_snapshot()
        .into_iter()
        .map(|p| {
            let id = p.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            match c.meta_of(&id) {
                Some(m) => {
                    let mut obj = p.as_object().cloned().unwrap_or_default();
                    obj.insert("kind".to_string(), json!(m.kind));
                    obj.insert("cwd".to_string(), json!(m.cwd));
                    obj.insert("airgapped".to_string(), json!(m.airgap));
                    // `!m.exited && ptys.has(p.id)` in JS — simplified to
                    // `!m.exited` alone: this crate's pty-liveness registry
                    // (`pty::Registry`) exposes no production-reachable
                    // `contains` check (only a `#[cfg(test)]` one), and by
                    // the time `mark_exited` ever runs, `pty::Registry`'s
                    // own entry for this pane is already gone anyway (see
                    // `pty.rs`'s `reader_loop` — registry removal happens
                    // strictly before its `on_exit` callback fires), so the
                    // two conditions only ever disagree during the brief
                    // window a pane is still spawning — not worth a second
                    // cross-module liveness check for.
                    obj.insert("alive".to_string(), json!(!m.exited));
                    Value::Object(obj)
                }
                None => p,
            }
        })
        .collect();
    serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())
}

fn read_terminal(c: &Conductor, env: &ConductorEnv, input: &Value) -> String {
    let pane_id = input.get("pane_id").and_then(Value::as_str).unwrap_or("");
    let Some(buf) = c.scrollback_of(pane_id) else {
        return "No such terminal pane. Use list_panes.".to_string();
    };
    // Scrollback can hold anything the pane ever printed — default-deny,
    // and an air-gapped pane is refused outright, consent or not (TOME-009).
    if c.meta_of(pane_id).map(|m| m.airgap).unwrap_or(false) {
        return "Refused: air-gapped pane output cannot be disclosed.".to_string();
    }
    if !c.has_read_consent(pane_id) {
        // One-time per-pane consent prompt; fail closed until answered.
        if c.mark_read_requested(pane_id) {
            (env.send)("conductor:readRequest", json!({ "paneId": pane_id }));
        }
        return "Refused: user has not authorized reading this terminal.".to_string();
    }
    let stripped = strip_ansi(&buf);
    let all_lines: Vec<&str> = stripped.split('\n').collect();
    // `Math.min(Math.max(input.lines || 60, 1), 400)` — `|| 60` treats an
    // absent/zero/non-numeric value as "use the default", not just absent.
    let raw = input.get("lines").and_then(Value::as_f64).filter(|n| *n != 0.0).unwrap_or(60.0);
    let want = raw.clamp(1.0, 400.0) as usize;
    let start = all_lines.len().saturating_sub(want);
    let tail = &all_lines[start..];
    // Audit the read like conductor:tool audits a tool call: pane + line
    // count only — never the scrollback content itself.
    (env.log_event)(
        "conductor:read",
        vec![("paneId".to_string(), json!(pane_id)), ("lines".to_string(), json!(tail.len()))],
    );
    let joined = tail.join("\n");
    if joined.is_empty() {
        "(no output yet)".to_string()
    } else {
        joined
    }
}

fn type_in_terminal(c: &Conductor, env: &ConductorEnv, input: &Value) -> String {
    let pane_id = input.get("pane_id").and_then(Value::as_str).unwrap_or("");
    let text_raw = input.get("text").and_then(Value::as_str).unwrap_or("");
    let allow_run = c.allow_run();
    let press_enter = input.get("press_enter").and_then(Value::as_bool).unwrap_or(false);
    let enter = press_enter && allow_run;
    // With auto-run off the text must stay un-submitted, so strip the
    // control chars that would submit or signal on their own.
    let text = if allow_run { text_raw.to_string() } else { strip_control_chars(text_raw) };
    let payload = if enter { format!("{text}\r") } else { text };
    if !(env.write_pty)(pane_id, &payload) {
        return "No such live terminal pane. Use list_panes.".to_string();
    }
    (env.send)("conductor:acted", json!({ "pane": pane_id, "ran": enter }));
    if enter {
        return "Typed and submitted.".to_string();
    }
    if press_enter {
        "Typed, but NOT submitted: auto-run is disabled. The user can press Enter, or enable \"assistant may run commands\" in the \u{ff0b} menu.".to_string()
    } else {
        "Typed (not submitted).".to_string()
    }
}

fn open_pane(env: &ConductorEnv, input: &Value, chat_id: &str) -> String {
    let kind = input.get("kind").and_then(Value::as_str).unwrap_or("");
    (env.send)("conductor:open", json!({ "kind": kind, "source": chat_id }));
    "Requested.".to_string()
}

fn open_file(env: &ConductorEnv, input: &Value, chat_id: &str) -> String {
    let file = input.get("path").and_then(Value::as_str).unwrap_or("");
    // The model must not make main open/parse arbitrary files on disk —
    // only paths inside the open workspace folders or a brain vault.
    if !(env.can_open_file)(Path::new(file)) {
        return "Refused: open_file is confined to the open workspace folders and brain vaults.".to_string();
    }
    (env.send)("conductor:open", json!({ "file": file, "source": chat_id }));
    "Requested.".to_string()
}

fn read_flow(env: &ConductorEnv, input: &Value) -> String {
    let roots = (env.roots)();
    let root_arg = input.get("root").and_then(Value::as_str);
    let name = input.get("name").and_then(Value::as_str);
    flow::tools::read_flow_tool(&roots, root_arg, name)
}

fn draft_flow(env: &ConductorEnv, input: &Value, chat_id: &str) -> String {
    let roots = (env.roots)();
    let root_arg = input.get("root").and_then(Value::as_str);
    let name = input.get("name").and_then(Value::as_str);
    let flow_val = input.get("flow").cloned();
    let result = flow::tools::draft_flow_tool(&roots, root_arg, name, flow_val);
    // Open the pane only on create; overwrites reach the already-open pane
    // through the disk watcher, so re-opening would just churn the grid.
    if let Some(open_path) = &result.open_path {
        (env.send)("conductor:open", json!({ "file": open_path.to_string_lossy(), "source": chat_id }));
    }
    result.text
}
