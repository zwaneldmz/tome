//! The conductor's 17 tools — direct port of `conductor.js`'s `TOOLS`
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

use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};

use super::env::ConductorEnv;
use super::state::Conductor;
use crate::flow;
use crate::skills;

// ================= text sanitizers (shared/terminal-text.js) =================

/// CSI + OSC + stray escapes + control chars (keeps `\n`/`\t`) — verbatim
/// translation of `stripAnsi`'s four chained `.replace`s.
pub(crate) fn strip_ansi(s: &str) -> String {
    static OSC: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static CSI: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static ESC: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static CTRL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let osc = OSC.get_or_init(|| {
        Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").expect("static pattern is valid")
    });
    let csi = CSI
        .get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("static pattern is valid"));
    let esc = ESC.get_or_init(|| Regex::new(r"\x1b[@-_]").expect("static pattern is valid"));
    let ctrl = CTRL
        .get_or_init(|| Regex::new(r"[\x00-\x08\x0b-\x1f\x7f]").expect("static pattern is valid"));
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
    let ctrl = CTRL
        .get_or_init(|| Regex::new(r"[\x00-\x08\x0a-\x1f\x7f]").expect("static pattern is valid"));
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
    kinds.extend(
        ["chat", "brain", "graphify", "flow", "runs"]
            .iter()
            .map(|s| s.to_string()),
    );
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
        "You are the assistant pane inside Tome, a desktop coding harness whose grid holds terminal panes, agent CLI panes ({}), editors, documents, and note vaults. You have tools to inspect and drive the workspace: list panes, read a terminal\u{2019}s recent output, type into a terminal, open new panes or files. Use them whenever the user refers to other panes (\"what is claude doing\", \"run the tests over there\", \"open a terminal\"). When the workspace's code graph has been built (the Code graph pane's Build button), prefer graph_query, graph_path, and graph_explain for structure questions — where something lives, what connects to what — instead of reading many files; if graphify is missing or the graph is not built, those tools say so. type_in_terminal only submits when the user has enabled auto-run; otherwise the text is left for them to press Enter on — say so when it happens. Your replies may be read aloud, so keep them focused, brief, and speakable. Plain text only — no markdown tables. When the user wants to design a workflow, act as a flow architect. Flows are graphs of agent nodes saved as .tome/flows/<name>.flow.json; you shape them with read_flow and draft_flow. Restate the goal in one sentence, then ask one question at a time — never a questionnaire. Draft early and refine as you go: once the user agrees to start, call draft_flow as soon as a shape exists, then say what you added and what you assumed. Every node needs instructions, expects, and produces; a blank contract is a question to ask, not a field to invent, so challenge vagueness and voice every warning draft_flow returns. Never overwrite a flow you did not draft in this conversation without asking. You cannot run flows: when the user approves the final shape, say it is saved and that they press Run on the flow pane.",
        agent_kinds_text(agent_ids)
    )
}

/// `mentorSystemPrompt()`, the teaching persona chosen when `chat_send` is
/// called with `verbose: true`. Plain text, brief, and speakable — the
/// assistant's replies may be read aloud. Interpolates the same
/// agent-kinds parenthetical as [`system_prompt_text`]. When `gate` is true
/// (the default) the prompt includes the "write a failing test + call
/// gate_question; do not implement until judged" instruction; when false
/// that instruction is omitted but the rest of the teacher persona stays.
pub(crate) fn mentor_prompt_text(agent_ids: &[String], gate: bool) -> String {
    let base = format!(
        "You are the mentor persona of the assistant pane inside Tome, a desktop coding harness whose grid holds terminal panes, agent CLI panes ({}), editors, documents, and note vaults. You are teaching the user, not just doing their work for them: explain what you are about to do and why before you do it, work one step at a time, and check the user's understanding as you go — pause to confirm they follow each step before moving on. Use list_skills to see what skills are available and read_skill to load one when it applies (for example teach, tdd, grill-me, grilling, to-spec, to-questionnaire, code-review, diagnosing-bugs, ask-matt) — read a skill before relying on it. ",
        agent_kinds_text(agent_ids)
    );
    const GATE_INSTRUCTION: &str = "When the user asks you to implement a feature or make a significant change, first write a failing test that captures the requirement with write_file, then call gate_question with 1-3 comprehension questions about that test; do NOT implement until gate_question returns the user's answers, then judge whether they understand. If they skipped or answered poorly, explain what was missing before implementing. ";
    const TAIL: &str =
        "Keep your replies focused, brief, and speakable — plain text only, no markdown tables.";
    if gate {
        format!("{base}{GATE_INSTRUCTION}{TAIL}")
    } else {
        format!("{base}{TAIL}")
    }
}

/// `voiceSystemPrompt()`, the persona chosen when `chat_send` is called with
/// `voice: true`. The user is hands-free and the reply is read aloud, so the
/// prompt drives short conversational answers (one to three brief sentences,
/// no lists/code/markdown) while keeping the same tools as the assistant
/// pane — the model still lists panes, reads terminals, types into them, and
/// opens panes/files, saying aloud what it did. Interpolates the same
/// agent-kinds parenthetical as [`system_prompt_text`].
pub(crate) fn voice_prompt_text(agent_ids: &[String]) -> String {
    format!(
        "You are the voice assistant in Tome, a desktop coding harness whose grid holds terminal panes, agent CLI panes ({}), editors, documents, and note vaults. The user is talking to you hands-free and your reply is read aloud, so keep it short and conversational: one to three brief sentences, plain speech only — no lists, no code, no markdown. Acknowledge briefly, then answer. You have the same tools as the assistant pane: list panes, read a terminal's recent output, type into a terminal, open panes or files — use them when the user refers to other panes ('what is claude doing', 'run the tests over there', 'open a terminal') and say aloud what you did. type_in_terminal only submits when the user has enabled auto-run; otherwise say the text was left for them to press Enter.",
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
        json!({
            "name": "write_file",
            "description": "Write a file to disk (absolute path), creating it if needed. Confined to the open workspace folders and brain vaults.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "absolute path to write" },
                    "content": { "type": "string", "description": "the full file contents" },
                },
                "required": ["path", "content"],
            },
        }),
        json!({
            "name": "read_file",
            "description": "Read a file from disk and return its contents (absolute path). Confined to the open workspace folders and brain vaults.",
            "input_schema": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "absolute path to read" } },
                "required": ["path"],
            },
        }),
        json!({
            "name": "run_command",
            "description": "Run a shell command in a working directory. Only runs when the user has enabled \"assistant may run commands\". Confined to the open workspace folders and brain vaults.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "cwd": { "type": "string", "description": "working directory (must be inside an open workspace folder or brain vault)" },
                    "cmd": { "type": "string", "description": "the shell command to run" },
                },
                "required": ["cwd", "cmd"],
            },
        }),
        json!({
            "name": "list_skills",
            "description": "List the skills available in the skills directory (name, description, and relative path).",
            "input_schema": { "type": "object", "properties": {} },
        }),
        json!({
            "name": "read_skill",
            "description": "Read a skill's full SKILL.md body by name (see list_skills for names).",
            "input_schema": {
                "type": "object",
                "properties": { "name": { "type": "string", "description": "skill name from list_skills" } },
                "required": ["name"],
            },
        }),
        json!({
            "name": "graph_query",
            "description": "Ask the workspace's code graph a question (BFS traversal of graphify-out/graph.json). Use it for structure questions — where something lives, what connects two things — instead of reading many files. Only works after the user has built the graph (the Code graph pane's Build button); if graphify is missing or no graph exists, the tool reports that.",
            "input_schema": {
                "type": "object",
                "properties": { "question": { "type": "string", "description": "natural-language question about the codebase structure" } },
                "required": ["question"],
            },
        }),
        json!({
            "name": "graph_path",
            "description": "Shortest dependency path between two nodes in the workspace's code graph (graphify-out/graph.json). Use for 'how does A reach B'.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "source node/symbol name" },
                    "to": { "type": "string", "description": "target node/symbol name" },
                },
                "required": ["from", "to"],
            },
        }),
        json!({
            "name": "graph_explain",
            "description": "Plain-language explanation of a node (function, class, symbol) and its neighbors in the workspace's code graph (graphify-out/graph.json). Use for 'what is X and what touches it'.",
            "input_schema": {
                "type": "object",
                "properties": { "symbol": { "type": "string", "description": "node/symbol name" } },
                "required": ["symbol"],
            },
        }),
        json!({
            "name": "run_agent",
            "description": format!("Run an agent CLI ({}) headless on a single prompt and return its output. Use this to delegate work — investigate, fix, summarize — instead of opening a visible pane: the agent runs sandboxed in the background (no network except model-provider domains) and this tool returns when it finishes (up to 10 minutes). The prompt is the agent's whole task; be specific. Optionally pin a model.", agent_kinds_text(agent_ids)),
            "input_schema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "agent CLI kind (same list as open_pane)" },
                    "prompt": { "type": "string", "description": "the task for the agent — what to do, where, and what to report back" },
                    "model": { "type": "string", "description": "optional model pin (provider/model shape for opencode/pi)" },
                },
                "required": ["kind", "prompt"],
            },
        }),
        json!({
            "name": "gate_question",
            "description": "Pause and ask the user comprehension questions about a test you just wrote; resumes when they answer (or skip). Required to gate implementation on understanding.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "questions": { "type": "array" },
                    "test_code": { "type": "string" },
                    "summary": { "type": "string" },
                },
                "required": ["questions", "test_code", "summary"],
            },
        }),
    ]
}

// ================= runTool dispatch =================

/// Heuristic "did this tool result read as a failure?" — drives the
/// `chat:tool-done` event's `ok` flag and the audit log's honest `ok`
/// field (the old port logged `true` unconditionally). NOT a contract for
/// the model — it reads the real text — just a UI red/green signal and an
/// audit sanity line. Prefix-based and deliberately small; a new tool that
/// fails with a novel shape needs its prefix added here, and the
/// `is_tool_failure_*` tests below pin every prefix.
pub(crate) fn is_tool_failure(out: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "Refused",
        "Unknown tool.",
        "No workspace folder",
        "cannot run",
        "could not",
        "agent timed out",
        "run_agent needs",
        "graph_query needs",
        "graph_path needs",
        "graph_explain needs",
        "skill not found",
        "exit ",
        "error:",
    ];
    let t = out.trim_start();
    PREFIXES.iter().any(|p| t.starts_with(p))
}

/// `runTool(name, input, chatId)` — dispatches to one of the 17 impls below,
/// or `"Unknown tool."` for anything else. Infallible (always returns a
/// `String`): unlike the JS original's `try { out = runTool(...) } catch`,
/// none of these impls can panic on attacker-shaped `input` (every field
/// read is `Option`-guarded), so [`super::chat::run_chat`] does not need
/// its own catch-and-format-as-error branch either — a deliberate, minor
/// simplification over the JS `ok:false` path, noted here since nothing
/// currently exercises it. `async` so `run_command` can await its process
/// backend; the other arms stay synchronous inside the same fn.
pub async fn run_tool(
    c: &Conductor,
    env: &ConductorEnv,
    name: &str,
    input: &Value,
    chat_id: &str,
) -> String {
    match name {
        "list_panes" => list_panes(c),
        "read_terminal" => read_terminal(c, env, input),
        "type_in_terminal" => type_in_terminal(c, env, input),
        "open_pane" => open_pane(env, input, chat_id),
        "open_file" => open_file(env, input, chat_id),
        "read_flow" => read_flow(env, input),
        "draft_flow" => draft_flow(env, input, chat_id),
        "write_file" => write_file(env, input),
        "read_file" => read_file(env, input),
        "run_command" => run_command(c, env, input).await,
        "list_skills" => list_skills(env, input),
        "read_skill" => read_skill(env, input),
        "graph_query" => graph_query_tool(env, input).await,
        "graph_path" => graph_path_tool(env, input).await,
        "graph_explain" => graph_explain_tool(env, input).await,
        "run_agent" => run_agent_tool(env, input, chat_id).await,
        "gate_question" => gate_question(env, input).await,
        _ => "Unknown tool.".to_string(),
    }
}

/// `block.input?.pane_id || block.input?.kind || block.input?.path ||
/// block.input?.name || block.input?.cwd || ''` — the `chat:tool`/
/// `conductor:tool` hint. `cwd` is a Rust-port addition for `run_command`.
pub(crate) fn tool_hint(input: &Value) -> String {
    for key in ["pane_id", "kind", "path", "name", "cwd"] {
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
            let id = p
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            match c.meta_of(&id) {
                Some(m) => {
                    let mut obj = p.as_object().cloned().unwrap_or_default();
                    obj.insert("kind".to_string(), json!(m.kind));
                    obj.insert("cwd".to_string(), json!(m.cwd));
                    obj.insert("egressped".to_string(), json!(m.egress));
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
    // and an gapped pane is refused outright, consent or not (TOME-009).
    if c.meta_of(pane_id).map(|m| m.egress).unwrap_or(false) {
        return "Refused: gapped pane output cannot be disclosed.".to_string();
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
    let raw = input
        .get("lines")
        .and_then(Value::as_f64)
        .filter(|n| *n != 0.0)
        .unwrap_or(60.0);
    let want = raw.clamp(1.0, 400.0) as usize;
    let start = all_lines.len().saturating_sub(want);
    let tail = &all_lines[start..];
    // Audit the read like conductor:tool audits a tool call: pane + line
    // count only — never the scrollback content itself.
    (env.log_event)(
        "conductor:read",
        vec![
            ("paneId".to_string(), json!(pane_id)),
            ("lines".to_string(), json!(tail.len())),
        ],
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
    let press_enter = input
        .get("press_enter")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let enter = press_enter && allow_run;
    // With auto-run off the text must stay un-submitted, so strip the
    // control chars that would submit or signal on their own.
    let text = if allow_run {
        text_raw.to_string()
    } else {
        strip_control_chars(text_raw)
    };
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
    // P2.1 containment-only ceiling: a plain terminal is the one
    // inherently-UNSANDBOXED pane this tool can propose (agent kinds spawn
    // gapped under containment-only — the renderer forces it, and the IPC
    // layer would refuse them otherwise). Refuse to even propose it rather
    // than hand the renderer a request its own backend must reject —
    // defense in depth; `pty_create`'s check is the real wall.
    if kind == "terminal" && (env.containment_only)() {
        return "Refused: containment-only mode is on — unsandboxed terminal panes are disabled."
            .to_string();
    }
    (env.send)("conductor:open", json!({ "kind": kind, "source": chat_id }));
    "Requested.".to_string()
}

fn open_file(env: &ConductorEnv, input: &Value, chat_id: &str) -> String {
    let file = input.get("path").and_then(Value::as_str).unwrap_or("");
    // The model must not make main open/parse arbitrary files on disk —
    // only paths inside the open workspace folders or a brain vault.
    if !(env.can_open_file)(Path::new(file)) {
        return "Refused: open_file is confined to the open workspace folders and brain vaults."
            .to_string();
    }
    (env.send)("conductor:open", json!({ "file": file, "source": chat_id }));
    "Requested.".to_string()
}

/// The workspace root the assistant's workspace-relative tools should
/// operate at: the renderer-synced ACTIVE root when present, else the first
/// open folder. One resolver so every tool agrees on what "the project"
/// means.
fn assistant_root(env: &ConductorEnv) -> Option<String> {
    (env.cwd)().or_else(|| (env.roots)().into_iter().next())
}

/// The roots list flow tools resolve against, ACTIVE ROOT FIRST: a flow
/// with no explicit `root` lands in the workspace you are looking at, not
/// the first folder of the first workspace (the pre-sync default).
fn active_first_roots(env: &ConductorEnv) -> Vec<String> {
    let mut roots = (env.roots)();
    if let Some(cwd) = (env.cwd)() {
        roots.retain(|r| r != &cwd);
        roots.insert(0, cwd);
    }
    roots
}

fn read_flow(env: &ConductorEnv, input: &Value) -> String {
    let roots = active_first_roots(env);
    let root_arg = input.get("root").and_then(Value::as_str);
    let name = input.get("name").and_then(Value::as_str);
    flow::tools::read_flow_tool(&roots, root_arg, name)
}

fn draft_flow(env: &ConductorEnv, input: &Value, chat_id: &str) -> String {
    let roots = active_first_roots(env);
    let root_arg = input.get("root").and_then(Value::as_str);
    let name = input.get("name").and_then(Value::as_str);
    let flow_val = input.get("flow").cloned();
    let result = flow::tools::draft_flow_tool(&roots, root_arg, name, flow_val);
    // Open the pane only on create; overwrites reach the already-open pane
    // through the disk watcher, so re-opening would just churn the grid.
    if let Some(open_path) = &result.open_path {
        (env.send)(
            "conductor:open",
            json!({ "file": open_path.to_string_lossy(), "source": chat_id }),
        );
    }
    result.text
}

/// `read_file`'s returned-content cap, in bytes — a model must not be handed
/// a multi-gigabyte file. Trimmed from the head (kept from the start),
/// snapped down to a UTF-8 char boundary so the truncation never splits a
/// multi-byte sequence.
const READ_CAP: usize = 1_000_000;

fn write_file(env: &ConductorEnv, input: &Value) -> String {
    let path = input.get("path").and_then(Value::as_str).unwrap_or("");
    let content = input.get("content").and_then(Value::as_str).unwrap_or("");
    let resolved = match (env.resolve_write)(Path::new(path)) {
        Ok(p) => p,
        Err(reason) => return format!("Refused: {reason}"),
    };
    if let Err(e) = std::fs::write(&resolved, content) {
        return e.to_string();
    }
    // Audit the write: path only — never the content.
    (env.log_event)(
        "conductor:writeFile",
        vec![("path".to_string(), json!(path))],
    );
    format!("Wrote {} bytes to {}.", content.len(), path)
}

fn read_file(env: &ConductorEnv, input: &Value) -> String {
    let path = input.get("path").and_then(Value::as_str).unwrap_or("");
    let resolved = match (env.resolve_path)(Path::new(path)) {
        Ok(p) => p,
        Err(reason) => return format!("Refused: {reason}"),
    };
    let mut content = match std::fs::read_to_string(&resolved) {
        Ok(s) => s,
        Err(e) => return e.to_string(),
    };
    if content.len() > READ_CAP {
        let mut cut = READ_CAP;
        while cut > 0 && !content.is_char_boundary(cut) {
            cut -= 1;
        }
        content.truncate(cut);
    }
    // Audit the read: path only — never the content.
    (env.log_event)(
        "conductor:readFile",
        vec![("path".to_string(), json!(path))],
    );
    content
}

async fn run_command(c: &Conductor, env: &ConductorEnv, input: &Value) -> String {
    if !c.allow_run() {
        return "Refused: enable \"assistant may run commands\" to let me run commands."
            .to_string();
    }
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or("");
    let cmd = input.get("cmd").and_then(Value::as_str).unwrap_or("");
    // The cwd must be an existing, confined directory — same resolver as
    // `read_file`, since the directory already exists.
    let resolved = match (env.resolve_path)(Path::new(cwd)) {
        Ok(p) => p,
        Err(reason) => return format!("Refused: {reason}"),
    };
    // Audit the run: cwd only — never the command.
    (env.log_event)("conductor:run", vec![("cwd".to_string(), json!(cwd))]);
    let cwd_str = resolved.to_string_lossy().into_owned();
    (env.run_command)(cwd_str.as_str(), cmd)
        .await
        .unwrap_or_else(|e| e)
}

fn list_skills(env: &ConductorEnv, _input: &Value) -> String {
    let Some(root) = (env.skills_root)() else {
        return "Skills directory unavailable.".to_string();
    };
    serde_json::to_string(&skills::list(&root)).unwrap_or_else(|_| "[]".to_string())
}

fn read_skill(env: &ConductorEnv, input: &Value) -> String {
    let Some(root) = (env.skills_root)() else {
        return "Skills directory unavailable.".to_string();
    };
    let name = input.get("name").and_then(Value::as_str).unwrap_or("");
    match skills::read(&root, name) {
        Some((_skill, body)) => body,
        None => format!("skill not found: {name}"),
    }
}

/// `gate_question` — pauses the tool loop until the user answers. The env
/// seam owns the actual register/emit/await work (see
/// `env::gate_question_impl`); this arm just forwards the tool's whole
/// input object and returns the seam's `String` result (or its `Err` text)
/// as the tool result the loop appends to the transcript.
async fn gate_question(env: &ConductorEnv, input: &Value) -> String {
    (env.gate_question)(input.clone())
        .await
        .unwrap_or_else(|e| e)
}

// ================= graph tools (graphify knowledge graph) =================

/// shell-quotes one argv element for `run_command`'s `sh -c` backend —
/// single quotes with the standard `'\''` escape, so a graph question
/// containing quotes or semicolons can never smuggle a second command past
/// the fixed `graphify <subcommand>` prefix. `pub(crate)` for the
/// conductor test suite's injection assertions.
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Shared backend for `graph_query`/`graph_path`/`graph_explain`: runs the
/// fixed `graphify <subcommand> …` argv through `env.run_command` — so
/// confinement, timeout, and output cap are exactly run_command's — with
/// cwd = the first open workspace folder (the only place
/// `graphify-out/graph.json` can live). Deliberately NOT gated on
/// "assistant may run commands": these are read-only graph lookups with a
/// fixed argv, not arbitrary shell.
///
/// PATH is prefixed inline with the login-shell harvest
/// (`crate::login_env`) because `run_command`'s `sh -c` inherits Tome's
/// own PATH, and a GUI-launched Tome has the launchd-minimal one —
/// `~/.local/bin/graphify` would be invisible without it. Direct-calling
/// `login_env` here follows the same precedent `list_skills`/`read_skill`
/// set by calling `crate::skills` directly rather than through the seam.
async fn graph_ask(env: &ConductorEnv, args: &[&str]) -> String {
    let Some(cwd) = assistant_root(env) else {
        return "No workspace folder is open, so there is no graph to ask.".to_string();
    };
    // Audit the lookup: workspace only — the question never enters the log.
    (env.log_event)("conductor:graphAsk", vec![("ws".to_string(), json!(cwd))]);
    let login = crate::login_env::login_env().await;
    let mut cmd = format!("PATH={} graphify", sh_quote(&login.path));
    for a in args {
        cmd.push(' ');
        cmd.push_str(&sh_quote(a));
    }
    (env.run_command)(cwd.as_str(), &cmd)
        .await
        .unwrap_or_else(|e| e)
}

async fn graph_query_tool(env: &ConductorEnv, input: &Value) -> String {
    let q = input.get("question").and_then(Value::as_str).unwrap_or("");
    if q.is_empty() {
        return "graph_query needs a non-empty question.".to_string();
    }
    graph_ask(env, &["query", q]).await
}

async fn graph_path_tool(env: &ConductorEnv, input: &Value) -> String {
    let from = input.get("from").and_then(Value::as_str).unwrap_or("");
    let to = input.get("to").and_then(Value::as_str).unwrap_or("");
    if from.is_empty() || to.is_empty() {
        return "graph_path needs both 'from' and 'to' node names.".to_string();
    }
    graph_ask(env, &["path", from, to]).await
}

async fn graph_explain_tool(env: &ConductorEnv, input: &Value) -> String {
    let s = input.get("symbol").and_then(Value::as_str).unwrap_or("");
    if s.is_empty() {
        return "graph_explain needs a symbol name.".to_string();
    }
    graph_ask(env, &["explain", s]).await
}

// ================= run_agent (headless orchestration) =================

/// `run_agent` — one headless agent run. Vets the kind against the
/// built-in AGENTS (only those have headless templates — a custom CLI
/// names an interactive TUI the backend cannot drive), requires a
/// non-empty prompt, then delegates to the seam. The seam's `Err` is the
/// tool result too (the model reads it and either reports or retries), so
/// nothing here throws.
async fn run_agent_tool(env: &ConductorEnv, input: &Value, chat_id: &str) -> String {
    let kind = input.get("kind").and_then(Value::as_str).unwrap_or("");
    let prompt = input.get("prompt").and_then(Value::as_str).unwrap_or("");
    if kind.is_empty() || prompt.is_empty() {
        return "run_agent needs a non-empty kind and prompt.".to_string();
    }
    if !crate::agent_spawn::AGENTS.contains(&kind) {
        return format!(
            "run_agent: '{kind}' is not a headless-capable agent kind ({}).",
            crate::agent_spawn::AGENTS.join(", ")
        );
    }
    let model = input
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(String::from);
    let Some(cwd) = assistant_root(env) else {
        return "No workspace folder is open, so there is nowhere to run an agent.".to_string();
    };
    // Audit the run: kind and chat only — the prompt never enters the log.
    (env.log_event)(
        "conductor:runAgent",
        vec![
            ("kind".to_string(), json!(kind)),
            ("chatId".to_string(), json!(chat_id)),
        ],
    );
    (env.run_agent)(
        crate::agent_run::RunAgentRequest {
            chat_id: chat_id.to_string(),
            kind: kind.to_string(),
            model,
            prompt: prompt.to_string(),
        },
        PathBuf::from(cwd),
    )
    .await
    .unwrap_or_else(|e| e)
}
