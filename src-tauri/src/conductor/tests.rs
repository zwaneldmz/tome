//! Consent-gate state machine, tool dispatch, and tool-loop tests.
//!
//! The `read_terminal` (TOME-009) and `runChat` abort (TOME-015) blocks
//! below port `test/conductor-security.test.js` FULLY — every `it()` in
//! that file has a matching `#[test]`/`#[tokio::test]` here, same
//! assertions. The `strip_ansi`/`strip_control_chars` block ports
//! `test/conductor.test.js` (which pins `shared/terminal-text.js`, the
//! module `conductor.js` — and this port — sanitizes scrollback/typed text
//! through). Token-budget/loop-limit/clean-end/refusal/stream-error
//! coverage has no direct JS vitest counterpart (the JS suite only ever
//! exercises the abort path against a real fetch mock) but is this task's
//! own explicit brief: "tool dispatch, token budget... with chat + pty
//! injected/faked".
//!
//! Every test builds its own [`Conductor::new`] — see that module's doc
//! comment on why a shared global would flake under `cargo test`'s
//! parallel-by-default execution.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::chat;
use super::env::{BoxFuture, ConductorEnv, OnText};
use super::state::Conductor;
use super::tools::{self, strip_ansi, strip_control_chars};
use crate::chat::sse::{ChatError, NormalizedResponse, Usage};

// ================= fake env plumbing =================

type Sent = Arc<Mutex<Vec<(String, Value)>>>;
type Logged = Arc<Mutex<Vec<(String, Vec<(String, Value)>)>>>;

/// A harmless-default env: `send`/`log_event` record into `Sent`/`Logged`
/// for assertions; `can_open_file` allows everything; `write_pty` reports
/// every pane live; `roots` is empty; `stream_chat` panics if called (tests
/// that need it override that one field via struct-update syntax).
fn fake_env() -> (ConductorEnv, Sent, Logged) {
    let sent: Sent = Arc::new(Mutex::new(Vec::new()));
    let logged: Logged = Arc::new(Mutex::new(Vec::new()));
    let env = ConductorEnv {
        send: {
            let sent = sent.clone();
            Arc::new(move |ch: &str, payload: Value| {
                sent.lock().unwrap().push((ch.to_string(), payload))
            })
        },
        log_event: {
            let logged = logged.clone();
            Arc::new(move |kind: &str, fields: Vec<(String, Value)>| {
                logged.lock().unwrap().push((kind.to_string(), fields))
            })
        },
        can_open_file: Arc::new(|_p: &Path| true),
        write_pty: Arc::new(|_id: &str, _text: &str| true),
        roots: Arc::new(Vec::new),
        cwd: Arc::new(|| None),
        stream_chat: Arc::new(|_, _, _, _: OnText| {
            unimplemented!("stream_chat not faked for this test")
        }),
        resolve_path: Arc::new(|p: &Path| Ok(p.to_path_buf())),
        resolve_write: Arc::new(|p: &Path| Ok(p.to_path_buf())),
        skills_root: Arc::new(|| None),
        run_command: Arc::new(|_cwd: &str, _cmd: &str| {
            unimplemented!("run_command not faked for this test")
        }),
        run_agent: Arc::new(|_req: crate::agent_run::RunAgentRequest, _cwd: PathBuf| {
            unimplemented!("run_agent not faked for this test")
        }),
        gate_question: Arc::new(|_payload: Value| {
            unimplemented!("gate_question not faked for this test")
        }),
    };
    (env, sent, logged)
}

// ================= LIVE conductor probe (ignored — real network) =================
//
// Drives the FULL backend chat path — `chat::run_chat`'s multi-turn loop,
// tool dispatch, and both wire round-trips — against a real
// OpenAI-compatible endpoint. Everything except `stream_chat` is the same
// fake_env the other tests use, so the ONLY live surface is the provider.
// Run manually:
//
//   TOME_PROBE_KEY=sk-… TOME_PROBE_MODEL=glm-5.2 \
//     cargo test --lib conductor::tests::live_conductor_tool_loop_probe -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn live_conductor_tool_loop_probe() {
    use crate::chat::registry::{Auth, KeyOrigin, ResolvedProvider, Wire};
    use crate::chat::sse::{StreamChatArgs, stream_chat};
    use super::chat::run_chat;

    let key = std::env::var("TOME_PROBE_KEY").expect("TOME_PROBE_KEY not set");
    let model = std::env::var("TOME_PROBE_MODEL").unwrap_or_else(|_| "glm-5.2".to_string());
    let provider = ResolvedProvider {
        id: "probe".to_string(),
        label: "probe".to_string(),
        wire: Wire::OpenAi,
        auth: Auth::Bearer,
        base_url: "https://api.eurouter.ai/api/v1".to_string(),
        api_key: key,
        key_origin: KeyOrigin::Env("probe".to_string()),
        model,
        max_output_tokens: 4096,
        betas: vec![],
    };

    let c = Conductor::new();
    let (mut env, sent, _logged) = fake_env();
    env.stream_chat = {
        let provider = provider.clone();
        Arc::new(move |system, messages, tools, on_text: OnText| {
            let provider = provider.clone();
            Box::pin(async move {
                stream_chat(
                    crate::ipc::chat::http_client(),
                    &provider,
                    StreamChatArgs {
                        system: system.as_deref(),
                        messages: &messages,
                        tools: &tools,
                        betas: None,
                        fallbacks: None,
                    },
                    on_text,
                )
                .await
                .map_err(|e| crate::chat::sse::ChatError::Network(e.to_string()))
            })
        })
    };

    let messages = vec![json!({
        "role": "user",
        "content": "Call the list_panes tool now, then call the read_terminal tool, then answer with the count of open panes. Do not ask questions."
    })];
    run_chat(
        &c,
        &env,
        "probe-chat".to_string(),
        Some(
            "You are the Tome assistant probe. Use list_panes when asked about panes."
                .to_string(),
        ),
        messages,
    )
    .await
    .expect("run_chat should not fail");

    let sent = sent.lock().unwrap();
    let tools_called: Vec<&str> = sent
        .iter()
        .filter(|(ch, _)| ch == "chat:tool")
        .map(|(_, p)| p.get("tool").and_then(Value::as_str).unwrap_or("?"))
        .collect();
    let done = sent
        .iter()
        .find(|(ch, _)| ch == "chat:done")
        .map(|(_, p)| p.clone());
    println!("tools called: {tools_called:?}");
    println!("chat:done: {done:?}");
    assert!(
        tools_called.contains(&"list_panes"),
        "the assistant should call list_panes; got {tools_called:?}"
    );
    assert_eq!(
        done.and_then(|d| d.get("error").cloned()),
        Some(Value::Null),
        "chat:done must carry error null (clean end), not a failure"
    );
}

fn sent_channel(sent: &Sent, channel: &str) -> Vec<Value> {
    sent.lock()
        .unwrap()
        .iter()
        .filter(|(ch, _)| ch == channel)
        .map(|(_, p)| p.clone())
        .collect()
}

/// Synchronous wrapper around the now-`async` [`tools::run_tool`], so the
/// many non-async tool-dispatch tests below can keep their existing
/// `#[test]` shape. Each call gets its own current-thread runtime (none of
/// these tests are already inside one, since they are plain `#[test]`s),
/// driving the same fn the async `#[tokio::test]`s await.
fn run_tool(c: &Conductor, env: &ConductorEnv, name: &str, input: &Value, chat_id: &str) -> String {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(tools::run_tool(c, env, name, input, chat_id))
}

/// A `stream_chat` fake that always returns the same canned response,
/// counting how many times it was called.
fn canned_stream(
    resp: NormalizedResponse,
    calls: Arc<AtomicUsize>,
) -> Arc<
    dyn Fn(
            Option<String>,
            Vec<Value>,
            Vec<Value>,
            OnText,
        ) -> BoxFuture<Result<NormalizedResponse, ChatError>>
        + Send
        + Sync,
> {
    Arc::new(move |_system, _messages, _tools, _on_text: OnText| {
        calls.fetch_add(1, Ordering::SeqCst);
        let resp = resp.clone();
        Box::pin(async move { Ok(resp) })
    })
}

fn tool_use_block(id: &str, name: &str) -> Value {
    json!({ "type": "tool_use", "id": id, "name": name, "input": {} })
}

fn tool_use_block_with(id: &str, name: &str, input: Value) -> Value {
    json!({ "type": "tool_use", "id": id, "name": name, "input": input })
}

// ================= read_terminal (TOME-009 pane-scoped consent) =================
// Ports test/conductor-security.test.js's `describe('conductor read_terminal
// (TOME-009 pane-scoped consent)', ...)` block, one #[test] per `it()`.

#[test]
fn read_terminal_refuses_a_registered_pane_with_no_consent_granted() {
    let c = Conductor::new();
    let (env, _sent, _logged) = fake_env();
    c.register("p-noconsent", "terminal", "/tmp", false);
    c.record("p-noconsent", "hello world");
    let out = run_tool(
        &c,
        &env,
        "read_terminal",
        &json!({ "pane_id": "p-noconsent" }),
        "chat-1",
    );
    assert_eq!(
        out,
        "Refused: user has not authorized reading this terminal."
    );
}

#[test]
fn read_terminal_surfaces_a_one_time_consent_prompt_not_one_per_call() {
    let c = Conductor::new();
    let (env, sent, _logged) = fake_env();
    c.register("p-ask", "terminal", "/tmp", false);
    c.record("p-ask", "hello world");
    run_tool(
        &c,
        &env,
        "read_terminal",
        &json!({ "pane_id": "p-ask" }),
        "chat-1",
    );
    run_tool(
        &c,
        &env,
        "read_terminal",
        &json!({ "pane_id": "p-ask" }),
        "chat-1",
    );
    assert_eq!(
        sent_channel(&sent, "conductor:readRequest"),
        vec![json!({ "paneId": "p-ask" })]
    );
}

#[test]
fn read_terminal_never_prompts_for_an_egressped_pane() {
    let c = Conductor::new();
    let (env, sent, _logged) = fake_env();
    c.register("p-air", "terminal", "/tmp", true);
    c.record("p-air", "secret");
    run_tool(
        &c,
        &env,
        "read_terminal",
        &json!({ "pane_id": "p-air" }),
        "chat-1",
    );
    assert!(sent_channel(&sent, "conductor:readRequest").is_empty());
}

#[test]
fn read_terminal_refuses_an_egressped_pane_even_after_consent_is_granted() {
    let c = Conductor::new();
    let (env, _sent, _logged) = fake_env();
    c.register("p-egress", "terminal", "/tmp", true);
    c.record("p-egress", "secret output");
    c.set_read_consent("p-egress", true);
    let out = run_tool(
        &c,
        &env,
        "read_terminal",
        &json!({ "pane_id": "p-egress" }),
        "chat-1",
    );
    assert_eq!(out, "Refused: gapped pane output cannot be disclosed.");
}

#[test]
fn read_terminal_returns_scrollback_once_consented_and_audits_pane_and_count_only() {
    let c = Conductor::new();
    let (env, _sent, logged) = fake_env();
    c.register("p-ok", "terminal", "/tmp", false);
    c.record("p-ok", "line1\nline2\nline3");
    c.set_read_consent("p-ok", true);
    let out = run_tool(
        &c,
        &env,
        "read_terminal",
        &json!({ "pane_id": "p-ok", "lines": 2 }),
        "chat-1",
    );
    assert_eq!(out, "line2\nline3");
    let logs = logged.lock().unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(
        *logs,
        vec![(
            "conductor:read".to_string(),
            vec![
                ("paneId".to_string(), json!("p-ok")),
                ("lines".to_string(), json!(2))
            ]
        )]
    );
}

#[test]
fn read_terminal_revoking_consent_refuses_again() {
    let c = Conductor::new();
    let (env, _sent, _logged) = fake_env();
    c.register("p-revoke", "terminal", "/tmp", false);
    c.record("p-revoke", "x");
    c.set_read_consent("p-revoke", true);
    c.set_read_consent("p-revoke", false);
    let out = run_tool(
        &c,
        &env,
        "read_terminal",
        &json!({ "pane_id": "p-revoke" }),
        "chat-1",
    );
    assert_eq!(
        out,
        "Refused: user has not authorized reading this terminal."
    );
}

#[test]
fn read_terminal_an_unknown_pane_is_refused_for_missing_pane_reasons_not_consent() {
    let c = Conductor::new();
    let (env, _sent, _logged) = fake_env();
    let out = run_tool(
        &c,
        &env,
        "read_terminal",
        &json!({ "pane_id": "never-registered" }),
        "chat-1",
    );
    assert_eq!(out, "No such terminal pane. Use list_panes.");
}

#[test]
fn forgetting_a_pane_clears_its_read_consent_too() {
    let c = Conductor::new();
    let (env, _sent, _logged) = fake_env();
    c.register("p-forget", "terminal", "/tmp", false);
    c.record("p-forget", "data");
    c.set_read_consent("p-forget", true);
    c.forget("p-forget");
    c.register("p-forget", "terminal", "/tmp", false);
    c.record("p-forget", "data-again");
    let out = run_tool(
        &c,
        &env,
        "read_terminal",
        &json!({ "pane_id": "p-forget" }),
        "chat-1",
    );
    assert_eq!(
        out,
        "Refused: user has not authorized reading this terminal."
    );
}

// ================= runChat abort (TOME-015) =================
// Ports test/conductor-security.test.js's `describe('conductor runChat abort
// (TOME-015)', ...)`. The JS test mocks `fetch`; this port injects
// `ConductorEnv.stream_chat` directly instead (this port's own seam for
// exactly this purpose — see env.rs's doc comment) rather than standing up
// a local HTTP server to feed `chat::sse::stream_chat` real bytes.

#[tokio::test]
async fn abort_stops_the_tool_loop_mid_batch_and_emits_one_terminal_done() {
    let c = Arc::new(Conductor::new());
    let (mut env, sent, logged) = fake_env();
    let chat_id = "abort-test-1".to_string();
    let chat_tool_count = Arc::new(AtomicUsize::new(0));
    let stream_calls = Arc::new(AtomicUsize::new(0));

    // Two tool_use blocks in a single assistant turn.
    let resp = NormalizedResponse {
        stop_reason: "tool_use".to_string(),
        content: vec![
            tool_use_block("call_1", "list_panes"),
            tool_use_block("call_2", "list_panes"),
        ],
        usage: Usage {
            input: 0,
            output: 0,
        },
    };
    env.stream_chat = canned_stream(resp, stream_calls.clone());

    // The mock `send` aborts as soon as the FIRST chat:tool event fires —
    // synchronously, so the abort lands before the loop reaches the second
    // block.
    env.send = {
        let sent = sent.clone();
        let chat_tool_count = chat_tool_count.clone();
        let c_for_abort = c.clone();
        let chat_id = chat_id.clone();
        Arc::new(move |ch: &str, payload: Value| {
            sent.lock().unwrap().push((ch.to_string(), payload));
            if ch == "chat:tool" {
                let n = chat_tool_count.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 1 {
                    c_for_abort.abort_chat(&chat_id);
                }
            }
        })
    };

    let result = chat::run_chat(
        &c,
        &env,
        chat_id.clone(),
        Some("sys".to_string()),
        vec![json!({ "role": "user", "content": "do stuff" })],
    )
    .await;
    assert!(result.is_ok());

    // Only the first tool ran — the second never got a chat:tool event, a
    // runTool call, or an audit entry.
    assert_eq!(chat_tool_count.load(Ordering::SeqCst), 1);
    let logs = logged.lock().unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].0, "conductor:tool");
    let fields: std::collections::HashMap<_, _> = logs[0].1.iter().cloned().collect();
    assert_eq!(fields.get("tool"), Some(&json!("list_panes")));
    assert_eq!(fields.get("chatId"), Some(&json!(chat_id)));
    assert_eq!(fields.get("ok"), Some(&json!(true)));
    drop(logs);

    // The loop stopped instead of re-sending the transcript for another turn.
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
    // Exactly one terminal outcome, flagged aborted.
    let done = sent_channel(&sent, "chat:done");
    assert_eq!(
        done,
        vec![json!({ "id": chat_id, "aborted": true, "error": "Stopped." })]
    );
}

// ================= token budget / loop limit / stop-reason handling =================
// No direct JS vitest counterpart — this task's own brief ("token budget...
// with chat + pty injected/faked").

#[tokio::test]
async fn stops_and_reports_once_the_token_budget_is_exceeded() {
    let c = Conductor::new();
    let (mut env, sent, _logged) = fake_env();
    let stream_calls = Arc::new(AtomicUsize::new(0));
    // One tool_use turn whose usage alone exceeds TOKEN_BUDGET (400_000).
    let resp = NormalizedResponse {
        stop_reason: "tool_use".to_string(),
        content: vec![tool_use_block("call_1", "list_panes")],
        usage: Usage {
            input: 300_000,
            output: 200_000,
        },
    };
    env.stream_chat = canned_stream(resp, stream_calls.clone());

    let result = chat::run_chat(
        &c,
        &env,
        "budget-test".to_string(),
        Some("sys".to_string()),
        vec![],
    )
    .await;
    assert!(result.is_ok());
    // Stopped after the ONE over-budget turn — never looped to a second.
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);

    let done = sent_channel(&sent, "chat:done");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["aborted"], json!(false));
    let error = done[0]["error"].as_str().unwrap_or("");
    assert!(
        error.contains("Token budget reached"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn stops_after_max_turns_with_a_loop_limit_message() {
    let c = Conductor::new();
    let (mut env, sent, _logged) = fake_env();
    let stream_calls = Arc::new(AtomicUsize::new(0));
    // Every turn asks for a tool but never stops on its own — tiny usage so
    // the token budget never trips first.
    let resp = NormalizedResponse {
        stop_reason: "tool_use".to_string(),
        content: vec![],
        usage: Usage {
            input: 1,
            output: 1,
        },
    };
    env.stream_chat = canned_stream(resp, stream_calls.clone());

    let result = chat::run_chat(
        &c,
        &env,
        "limit-test".to_string(),
        Some("sys".to_string()),
        vec![],
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(stream_calls.load(Ordering::SeqCst), 12);

    let done = sent_channel(&sent, "chat:done");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["aborted"], json!(false));
    assert_eq!(
        done[0]["error"],
        json!("Tool loop limit reached — ask again to continue.")
    );
}

#[tokio::test]
async fn ends_cleanly_on_a_non_tool_use_stop_reason() {
    let c = Conductor::new();
    let (mut env, sent, _logged) = fake_env();
    let stream_calls = Arc::new(AtomicUsize::new(0));
    let resp = NormalizedResponse {
        stop_reason: "end".to_string(),
        content: vec![json!({ "type": "text", "text": "done talking" })],
        usage: Usage {
            input: 10,
            output: 10,
        },
    };
    env.stream_chat = canned_stream(resp, stream_calls.clone());

    let result = chat::run_chat(
        &c,
        &env,
        "end-test".to_string(),
        Some("sys".to_string()),
        vec![],
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
    let done = sent_channel(&sent, "chat:done");
    assert_eq!(
        done,
        vec![json!({ "id": "end-test", "aborted": false, "error": Value::Null })]
    );
}

#[tokio::test]
async fn reports_a_refusal_stop_reason_as_a_friendly_error() {
    let c = Conductor::new();
    let (mut env, sent, _logged) = fake_env();
    let stream_calls = Arc::new(AtomicUsize::new(0));
    let resp = NormalizedResponse {
        stop_reason: "refusal".to_string(),
        content: vec![],
        usage: Usage {
            input: 1,
            output: 1,
        },
    };
    env.stream_chat = canned_stream(resp, stream_calls.clone());

    let result = chat::run_chat(
        &c,
        &env,
        "refusal-test".to_string(),
        Some("sys".to_string()),
        vec![],
    )
    .await;
    assert!(result.is_ok());
    let done = sent_channel(&sent, "chat:done");
    assert_eq!(
        done,
        vec![
            json!({ "id": "refusal-test", "aborted": false, "error": "Request declined by safety classifiers." })
        ]
    );
}

#[tokio::test]
async fn a_genuine_stream_error_propagates_without_run_chat_emitting_its_own_done() {
    let c = Conductor::new();
    let (mut env, sent, _logged) = fake_env();
    env.stream_chat = Arc::new(|_, _, _, _: OnText| {
        Box::pin(async move { Err(ChatError::Network("boom".to_string())) })
    });

    let result = chat::run_chat(
        &c,
        &env,
        "err-test".to_string(),
        Some("sys".to_string()),
        vec![],
    )
    .await;
    assert_eq!(result, Err(ChatError::Network("boom".to_string())));
    // The caller (ipc::chat::chat_send) is the one that classifies and
    // emits chat:done for a real network failure — run_chat itself must
    // not have sent one.
    assert!(sent_channel(&sent, "chat:done").is_empty());
}

// ================= tool dispatch (the other 5 tools + Unknown) =================

#[test]
fn list_panes_merges_the_renderer_snapshot_with_registered_meta() {
    let c = Conductor::new();
    let (env, _sent, _logged) = fake_env();
    c.set_panes(json!([{ "id": "p1", "title": "one" }, { "id": "p2", "title": "two" }]));
    c.register("p1", "claude", "/work", true);
    // p2 stays unregistered (for example a chat/brain pane) — passed through as-is.
    let out = run_tool(&c, &env, "list_panes", &json!({}), "chat-1");
    let rows: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows[0]["kind"], json!("claude"));
    assert_eq!(rows[0]["cwd"], json!("/work"));
    assert_eq!(rows[0]["egressped"], json!(true));
    assert_eq!(rows[0]["alive"], json!(true));
    assert_eq!(rows[1], json!({ "id": "p2", "title": "two" }));
}

#[test]
fn list_panes_reports_a_marked_exited_pane_as_not_alive() {
    let c = Conductor::new();
    let (env, _sent, _logged) = fake_env();
    c.set_panes(json!([{ "id": "p1", "title": "one" }]));
    c.register("p1", "terminal", "/tmp", false);
    c.mark_exited("p1");
    let out = run_tool(&c, &env, "list_panes", &json!({}), "chat-1");
    let rows: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows[0]["alive"], json!(false));
}

#[test]
fn type_in_terminal_strips_control_chars_and_never_submits_when_auto_run_is_off() {
    let c = Conductor::new();
    let (env, sent, _logged) = fake_env();
    assert!(!c.allow_run());
    let out = run_tool(
        &c,
        &env,
        "type_in_terminal",
        &json!({ "pane_id": "p1", "text": "ls\r", "press_enter": true }),
        "chat-1",
    );
    assert!(out.starts_with("Typed, but NOT submitted"));
    let acted = sent_channel(&sent, "conductor:acted");
    assert_eq!(acted, vec![json!({ "pane": "p1", "ran": false })]);
}

#[test]
fn type_in_terminal_submits_when_auto_run_is_on_and_press_enter_is_set() {
    let c = Conductor::new();
    let (env, sent, _logged) = fake_env();
    c.set_allow_run(true);
    let out = run_tool(
        &c,
        &env,
        "type_in_terminal",
        &json!({ "pane_id": "p1", "text": "ls", "press_enter": true }),
        "chat-1",
    );
    assert_eq!(out, "Typed and submitted.");
    assert_eq!(
        sent_channel(&sent, "conductor:acted"),
        vec![json!({ "pane": "p1", "ran": true })]
    );
}

#[test]
fn type_in_terminal_reports_a_dead_or_unknown_pane() {
    let c = Conductor::new();
    let (mut env, _sent, _logged) = fake_env();
    env.write_pty = Arc::new(|_, _| false);
    let out = run_tool(
        &c,
        &env,
        "type_in_terminal",
        &json!({ "pane_id": "gone", "text": "hi" }),
        "chat-1",
    );
    assert_eq!(out, "No such live terminal pane. Use list_panes.");
}

#[test]
fn open_pane_requests_the_renderer_open_a_new_pane() {
    let c = Conductor::new();
    let (env, sent, _logged) = fake_env();
    let out = run_tool(
        &c,
        &env,
        "open_pane",
        &json!({ "kind": "claude" }),
        "chat-9",
    );
    assert_eq!(out, "Requested.");
    assert_eq!(
        sent_channel(&sent, "conductor:open"),
        vec![json!({ "kind": "claude", "source": "chat-9" })]
    );
}

#[test]
fn open_file_refuses_a_path_outside_the_confined_set() {
    let c = Conductor::new();
    let (mut env, sent, _logged) = fake_env();
    env.can_open_file = Arc::new(|_p: &Path| false);
    let out = run_tool(
        &c,
        &env,
        "open_file",
        &json!({ "path": "/etc/passwd" }),
        "chat-1",
    );
    assert_eq!(
        out,
        "Refused: open_file is confined to the open workspace folders and brain vaults."
    );
    assert!(sent_channel(&sent, "conductor:open").is_empty());
}

#[test]
fn open_file_requests_the_renderer_open_a_confined_path() {
    let c = Conductor::new();
    let (env, sent, _logged) = fake_env();
    let out = run_tool(
        &c,
        &env,
        "open_file",
        &json!({ "path": "/work/proj/README.md" }),
        "chat-1",
    );
    assert_eq!(out, "Requested.");
    assert_eq!(
        sent_channel(&sent, "conductor:open"),
        vec![json!({ "file": "/work/proj/README.md", "source": "chat-1" })]
    );
}

#[test]
fn read_flow_and_draft_flow_round_trip_through_flow_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_string_lossy().into_owned();
    let c = Conductor::new();
    let (mut env, sent, _logged) = fake_env();
    env.roots = {
        let root = root.clone();
        Arc::new(move || vec![root.clone()])
    };

    let list_before = run_tool(&c, &env, "read_flow", &json!({}), "chat-1");
    assert_eq!(list_before, "No flows exist yet.");

    let flow_doc = json!({
        "version": 1, "name": "x",
        "nodes": [{ "id": "n1", "kind": "claude", "inputs": [], "outputs": [] }],
        "edges": [],
    });
    let draft_out = run_tool(
        &c,
        &env,
        "draft_flow",
        &json!({ "name": "pipeline", "flow": flow_doc }),
        "chat-1",
    );
    assert!(draft_out.starts_with("Created \"pipeline\""), "{draft_out}");
    // A newly-created flow asks the renderer to open it.
    assert_eq!(sent_channel(&sent, "conductor:open").len(), 1);

    let read_back = run_tool(
        &c,
        &env,
        "read_flow",
        &json!({ "name": "pipeline" }),
        "chat-1",
    );
    let doc: Value = serde_json::from_str(&read_back).unwrap();
    assert_eq!(doc["name"], json!("pipeline"));
}

#[test]
fn unknown_tool_name_is_reported_as_such() {
    let c = Conductor::new();
    let (env, _sent, _logged) = fake_env();
    assert_eq!(
        run_tool(&c, &env, "delete_everything", &json!({}), "chat-1"),
        "Unknown tool."
    );
}

// ================= dynamic TOOLS / SYSTEM (setAgents) =================

#[test]
fn set_agents_widens_open_panes_kind_description_and_the_system_prompt() {
    let c = Conductor::new();
    let before = c.system_prompt();
    assert!(before.contains("claude"));
    assert!(!before.contains("aider"));

    c.set_agents(&["claude".to_string(), "aider".to_string()]);
    let after = c.system_prompt();
    assert!(after.contains("aider"));

    let open_pane = c
        .tools()
        .into_iter()
        .find(|t| t["name"] == json!("open_pane"))
        .unwrap();
    assert!(open_pane["description"]
        .as_str()
        .unwrap()
        .contains("'aider'"));
}

#[test]
fn set_agents_falls_back_to_the_builtins_on_an_empty_list() {
    let c = Conductor::new();
    c.set_agents(&["only-one".to_string()]);
    c.set_agents(&[]);
    let open_pane = c
        .tools()
        .into_iter()
        .find(|t| t["name"] == json!("open_pane"))
        .unwrap();
    let desc = open_pane["description"].as_str().unwrap();
    assert!(desc.contains("'claude'"));
    assert!(!desc.contains("'only-one'"));
}

#[test]
fn tool_schemas_names_exactly_the_seventeen_tools_in_order() {
    let names: Vec<Value> = tools::tool_schemas(&["claude".to_string()])
        .into_iter()
        .map(|t| t["name"].clone())
        .collect();
    assert_eq!(
        names,
        vec![
            json!("list_panes"),
            json!("read_terminal"),
            json!("type_in_terminal"),
            json!("open_pane"),
            json!("open_file"),
            json!("read_flow"),
            json!("draft_flow"),
            json!("write_file"),
            json!("read_file"),
            json!("run_command"),
            json!("list_skills"),
            json!("read_skill"),
            json!("graph_query"),
            json!("graph_path"),
            json!("graph_explain"),
            json!("run_agent"),
            json!("gate_question"),
        ]
    );
}

// ================= graph tools =================

#[test]
fn is_tool_failure_catches_every_failure_prefix_and_spares_real_output() {
    for bad in [
        "Refused: enable \"assistant may run commands\"",
        "Unknown tool.",
        "No workspace folder is open",
        "cannot run 'terminal' headless",
        "could not prepare the agent environment: seatbelt refused",
        "agent timed out after 600s",
        "run_agent needs a non-empty kind and prompt.",
        "graph_query needs a non-empty question.",
        "skill not found: x",
        "exit 1:\nboom",
        "error: nope",
    ] {
        assert!(tools::is_tool_failure(bad), "{bad:?} must read as failure");
    }
    for ok in ["[]", "{}", "graph built — /ws/graphify-out/graph.json", "the agent output tail"] {
        assert!(!tools::is_tool_failure(ok), "{ok:?} must read as success");
    }
}

#[test]
fn sh_quote_neutralizes_shell_metacharacters() {
    assert_eq!(tools::sh_quote("hello"), "'hello'");
    assert_eq!(tools::sh_quote("a'b"), "'a'\\''b'");
    assert_eq!(tools::sh_quote("x; rm -rf /"), "'x; rm -rf /'");
    assert_eq!(tools::sh_quote("$(whoami)"), "'$(whoami)'");
}

// ================= run_agent (headless orchestration) =================

#[tokio::test]
async fn the_assistant_prefers_the_synced_active_root_over_the_first_open_folder() {
    // The workspace you are LOOKING at wins over workspace #1's first
    // folder — that is the whole point of conductor:cwd.
    let c = Conductor::new();
    let (mut env, _sent, logged) = fake_env();
    env.roots = Arc::new(|| vec!["/first-ws/root".to_string(), "/second-ws/root".to_string()]);
    env.cwd = Arc::new(|| Some("/second-ws/root".to_string()));
    env.run_agent = Arc::new(|_req: crate::agent_run::RunAgentRequest, cwd: PathBuf| {
        Box::pin(async move {
            assert_eq!(cwd, PathBuf::from("/second-ws/root"));
            Ok("out".to_string())
        })
    });
    let out = tools::run_tool(
        &c,
        &env,
        "run_agent",
        &json!({ "kind": "claude", "prompt": "hi" }),
        "c1",
    )
    .await;
    assert_eq!(out, "out");
    {
        let logs = logged.lock().unwrap();
        let run = logs.iter().find(|(k, _)| k == "conductor:runAgent").unwrap();
        assert!(run.1.contains(&("kind".to_string(), json!("claude"))));
    }

    // graph queries resolve the same way: the synced root, not roots[0].
    env.run_agent = Arc::new(|_req, _cwd| unimplemented!());
    env.log_event = {
        let logged = logged.clone();
        Arc::new(move |kind: &str, fields: Vec<(String, Value)>| {
            if kind == "conductor:graphAsk" {
                assert!(fields.contains(&("ws".to_string(), json!("/second-ws/root"))));
            }
            logged.lock().unwrap().push((kind.to_string(), fields))
        })
    };
    env.run_command = Arc::new(|cwd: &str, _cmd: &str| {
        assert_eq!(cwd, "/second-ws/root");
        Box::pin(async move { Ok("".to_string()) })
    });
    let _ = tools::run_tool(&c, &env, "graph_query", &json!({ "question": "x" }), "c1").await;
}

#[tokio::test]
async fn without_a_synced_root_the_assistant_falls_back_to_the_first_open_folder() {
    let c = Conductor::new();
    let (mut env, _sent, _logged) = fake_env();
    env.roots = Arc::new(|| vec!["/only-ws".to_string()]);
    env.cwd = Arc::new(|| None);
    env.run_agent = Arc::new(|_req, cwd: PathBuf| {
        Box::pin(async move {
            assert_eq!(cwd, PathBuf::from("/only-ws"));
            Ok("".to_string())
        })
    });
    let _ = tools::run_tool(
        &c,
        &env,
        "run_agent",
        &json!({ "kind": "pi", "prompt": "hi" }),
        "c1",
    )
    .await;
}

#[tokio::test]
async fn a_refused_tool_emits_chat_tool_done_with_ok_false_and_an_honest_audit() {
    use super::env::BoxFuture;

    let c = Conductor::new();
    let (mut env, sent, logged) = fake_env();
    // run_command refuses when auto-run is off — a deterministic failure
    // that never reaches the env seam. The canned stream returns ONE
    // tool_use then ends, so the loop stops after a single batch.
    let calls = Arc::new(AtomicUsize::new(0));
    let tool_resp = NormalizedResponse {
        stop_reason: "tool_use".to_string(),
        content: vec![tool_use_block_with(
            "t1",
            "run_command",
            json!({ "cwd": "/tmp", "cmd": "ls" }),
        )],
        usage: Usage { input: 1, output: 1 },
    };
    let end_resp = NormalizedResponse {
        stop_reason: "end".to_string(),
        content: vec![json!({ "type": "text", "text": "done" })],
        usage: Usage { input: 1, output: 1 },
    };
    env.stream_chat = {
        let calls = calls.clone();
        Arc::new(move |_system, _messages, _tools, _on_text: OnText| {
            calls.fetch_add(1, Ordering::SeqCst);
            let resp = if calls.load(Ordering::SeqCst) == 1 {
                tool_resp.clone()
            } else {
                end_resp.clone()
            };
            Box::pin(async move { Ok(resp) }) as BoxFuture<Result<NormalizedResponse, ChatError>>
        })
    };

    chat::run_chat(&c, &env, "fail-test".to_string(), Some("sys".to_string()), vec![])
        .await
        .expect("run_chat ok");

    // the completion event carries the honest outcome + timing
    let done_events = sent_channel(&sent, "chat:tool-done");
    assert_eq!(done_events.len(), 1);
    assert_eq!(done_events[0]["tool"], json!("run_command"));
    assert_eq!(done_events[0]["ok"], json!(false));
    assert!(done_events[0]["ms"].as_u64().is_some());

    // and the audit log no longer lies about ok
    let logs = logged.lock().unwrap();
    let fields: std::collections::HashMap<_, _> = logs[0].1.iter().cloned().collect();
    assert_eq!(fields.get("ok"), Some(&json!(false)));
}
#[tokio::test]
async fn run_agent_vets_the_kind_and_requires_a_prompt() {
    let c = Conductor::new();
    let (env, _sent, _logged) = fake_env();

    // unknown kind — refused before the seam is touched
    let out = tools::run_tool(&c, &env, "run_agent", &json!({ "kind": "terminal", "prompt": "hi" }), "c1")
        .await;
    assert!(out.contains("not a headless-capable agent kind"), "{out}");

    // empty prompt — refused
    let out = tools::run_tool(&c, &env, "run_agent", &json!({ "kind": "claude" }), "c1").await;
    assert!(out.contains("non-empty kind and prompt"), "{out}");

    // no workspace — refused (fake_env roots is empty)
    let out = tools::run_tool(&c, &env, "run_agent", &json!({ "kind": "claude", "prompt": "hi" }), "c1")
        .await;
    assert!(out.contains("No workspace folder is open"), "{out}");
}

#[tokio::test]
async fn run_agent_hands_the_seam_the_vetted_request_and_returns_its_output() {
    let c = Conductor::new();
    let (mut env, _sent, logged) = fake_env();
    env.roots = Arc::new(|| vec!["/tmp/ws".to_string()]);
    env.run_agent = Arc::new(|req: crate::agent_run::RunAgentRequest, cwd: PathBuf| {
        Box::pin(async move {
            assert_eq!(req.kind, "claude");
            assert_eq!(req.prompt, "summarize the repo");
            assert_eq!(req.model.as_deref(), Some("haiku"));
            assert_eq!(cwd, PathBuf::from("/tmp/ws"));
            Ok("the agent output tail".to_string())
        })
    });

    let out = tools::run_tool(
        &c,
        &env,
        "run_agent",
        &json!({ "kind": "claude", "prompt": "summarize the repo", "model": "haiku" }),
        "c1",
    )
    .await;
    assert_eq!(out, "the agent output tail");

    // audited with kind + chat id, never the prompt
    let logged = logged.lock().unwrap();
    let run = logged.iter().find(|(k, _)| k == "conductor:runAgent").expect("audit missing");
    let fields = &run.1;
    assert!(fields.contains(&("kind".to_string(), json!("claude"))));
    assert!(fields.contains(&("chatId".to_string(), json!("c1"))));
    let serialized = serde_json::to_string(&fields).unwrap();
    assert!(!serialized.contains("summarize"), "prompt must not enter the audit log");
}

// ================= mentor persona / gate_question =================

#[test]
fn mentor_prompt_text_mentions_gate_and_skills() {
    let text = tools::mentor_prompt_text(&["claude".to_string()], true);
    assert!(
        text.contains("gate_question"),
        "mentor prompt must mention gate_question: {text}"
    );
    assert!(
        text.contains("list_skills"),
        "mentor prompt must mention list_skills: {text}"
    );
}

#[test]
fn mentor_prompt_text_omits_gate_when_disabled() {
    let text = tools::mentor_prompt_text(&["claude".to_string()], false);
    assert!(
        !text.contains("gate_question"),
        "mentor prompt must not mention gate_question when the gate is off: {text}"
    );
    assert!(
        text.contains("list_skills"),
        "mentor prompt must still mention list_skills: {text}"
    );
}

#[test]
fn gate_question_returns_the_answer_value() {
    let c = Conductor::new();
    let (mut env, _sent, _logged) = fake_env();
    env.gate_question = Arc::new(|_payload: Value| {
        Box::pin(async move { Ok("the-answer".to_string()) }) as BoxFuture<Result<String, String>>
    });
    let out = run_tool(
        &c,
        &env,
        "gate_question",
        &json!({ "questions": ["q1"], "test_code": "assert(true)", "summary": "a test" }),
        "chat-1",
    );
    assert_eq!(out, "the-answer");
}

// ================= voice persona =================

#[test]
fn voice_prompt_text_marks_the_voice_assistant_persona() {
    let text = tools::voice_prompt_text(&["claude".to_string()]);
    assert!(!text.is_empty());
    assert!(
        text.contains("voice assistant"),
        "voice prompt must identify the voice assistant: {text}"
    );
    assert!(
        text.contains("read aloud"),
        "voice prompt must mention the reply is read aloud: {text}"
    );
}

#[test]
fn voice_system_prompt_matches_voice_prompt_text_for_the_default_agents() {
    let c = Conductor::new();
    assert_eq!(
        c.voice_system_prompt(),
        tools::voice_prompt_text(&c.agent_ids())
    );
}

// ================= strip_ansi / strip_control_chars =================
// Ports test/conductor.test.js in full.

#[test]
fn strip_ansi_removes_csi_sequences() {
    assert_eq!(strip_ansi("\x1b[1;31mred\x1b[0m plain"), "red plain");
    assert_eq!(strip_ansi("\x1b[2J\x1b[Hhome"), "home");
    assert_eq!(strip_ansi("\x1b[?25lhidden-cursor"), "hidden-cursor");
}

#[test]
fn strip_ansi_removes_osc_sequences_bel_and_st_terminated() {
    assert_eq!(strip_ansi("\x1b]0;window title\x07after"), "after");
    assert_eq!(
        strip_ansi("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"),
        "link"
    );
}

#[test]
fn strip_ansi_removes_stray_escapes_and_control_chars() {
    assert_eq!(strip_ansi("a\x1bM b\x07c\x00d\x7fe"), "a bcde");
}

#[test]
fn strip_ansi_preserves_newline_and_tab() {
    assert_eq!(strip_ansi("line1\nline2\tcol"), "line1\nline2\tcol");
}

#[test]
fn strip_control_chars_removes_every_submission_or_signal_character() {
    assert_eq!(strip_control_chars("ls\r"), "ls");
    assert_eq!(strip_control_chars("ls\n"), "ls");
    assert_eq!(strip_control_chars("a\x03b"), "ab");
    assert_eq!(strip_control_chars("a\x04b"), "ab");
    assert_eq!(strip_control_chars("a\x1bb"), "ab");
    assert_eq!(strip_control_chars("a\x7fb"), "ab");
    assert_eq!(strip_control_chars("a\x00b"), "ab");
}

#[test]
fn strip_control_chars_preserves_tab() {
    assert_eq!(strip_control_chars("git che\t"), "git che\t");
}

#[test]
fn strip_control_chars_strips_cr_lf_out_of_multi_line_smuggle_attempts() {
    assert_eq!(
        strip_control_chars("echo hi\r\nrm -rf ~\r"),
        "echo hirm -rf ~"
    );
}

#[test]
fn strip_control_chars_leaves_plain_text_untouched() {
    assert_eq!(
        strip_control_chars("plain text 123 !@#"),
        "plain text 123 !@#"
    );
}

// ================= register / record / forget bookkeeping =================

#[test]
fn record_is_a_no_op_for_an_unregistered_pane() {
    let c = Conductor::new();
    c.record("never-registered", "data");
    assert_eq!(c.scrollback_of("never-registered"), None);
}

#[test]
fn record_trims_to_the_scroll_cap_from_the_front() {
    let c = Conductor::new();
    c.register("p1", "terminal", "/tmp", false);
    c.record("p1", &"a".repeat(200_100));
    let buf = c.scrollback_of("p1").unwrap();
    assert_eq!(buf.len(), 200_000);
}

#[test]
fn mark_exited_is_a_no_op_for_an_unregistered_pane() {
    let c = Conductor::new();
    c.mark_exited("never-registered"); // must not panic
}

// ================= exact text fidelity vs. src/main/conductor.js =================
// No direct vitest counterpart (JS has no transcription-drift risk against
// itself), but directly in the "carry every meaningful assertion" spirit:
// SYSTEM and the two dynamically-*assembled* (not literal) tool
// descriptions are the assistant-facing text this port must reproduce
// verbatim, and a `.contains(...)` check (as the set_agents tests above
// use) cannot catch a dropped leading sentence the way an earlier draft of
// `tool_schemas` once dropped `open_pane`'s "Open a new pane in the grid. "
// prefix — caught by mechanically diffing a live `conductor.js` import
// against this port's own output, not by eyeballing. These three literals
// were generated from that same diff (a live `import('conductor.js')` +
// JSON dump, byte-copied — never hand-retyped) so the pin itself carries no
// transcription risk either.

#[test]
fn system_prompt_text_matches_conductor_js_exactly_for_the_default_agents() {
    let agent_ids: Vec<String> = crate::agent_spawn::AGENTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(tools::system_prompt_text(&agent_ids), "You are the assistant pane inside Tome, a desktop coding harness whose grid holds terminal panes, agent CLI panes (claude, opencode, pi), editors, documents, and note vaults. You have tools to inspect and drive the workspace: list panes, read a terminal’s recent output, type into a terminal, open new panes or files. Use them whenever the user refers to other panes (\"what is claude doing\", \"run the tests over there\", \"open a terminal\"). When the workspace's code graph has been built (the Code graph pane's Build button), prefer graph_query, graph_path, and graph_explain for structure questions — where something lives, what connects to what — instead of reading many files; if graphify is missing or the graph is not built, those tools say so. type_in_terminal only submits when the user has enabled auto-run; otherwise the text is left for them to press Enter on — say so when it happens. Your replies may be read aloud, so keep them focused, brief, and speakable. Plain text only — no markdown tables. When the user wants to design a workflow, act as a flow architect. Flows are graphs of agent nodes saved as .tome/flows/<name>.flow.json; you shape them with read_flow and draft_flow. Restate the goal in one sentence, then ask one question at a time — never a questionnaire. Draft early and refine as you go: once the user agrees to start, call draft_flow as soon as a shape exists, then say what you added and what you assumed. Every node needs instructions, expects, and produces; a blank contract is a question to ask, not a field to invent, so challenge vagueness and voice every warning draft_flow returns. Never overwrite a flow you did not draft in this conversation without asking. You cannot run flows: when the user approves the final shape, say it is saved and that they press Run on the flow pane.");
}

#[test]
fn open_pane_description_matches_conductor_js_exactly() {
    let agent_ids: Vec<String> = crate::agent_spawn::AGENTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let open_pane = tools::tool_schemas(&agent_ids)
        .into_iter()
        .find(|t| t["name"] == json!("open_pane"))
        .unwrap();
    assert_eq!(open_pane["description"], json!("Open a new pane in the grid. kind is one of: 'terminal', 'claude', 'opencode', 'pi', 'chat', 'brain', 'graphify', 'flow', 'runs'."));
}

#[test]
fn draft_flow_description_matches_conductor_js_exactly() {
    let agent_ids: Vec<String> = crate::agent_spawn::AGENTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let draft_flow = tools::tool_schemas(&agent_ids)
        .into_iter()
        .find(|t| t["name"] == json!("draft_flow"))
        .unwrap();
    assert_eq!(draft_flow["description"], json!("Create or overwrite a flow at .tome/flows/<name>.flow.json; a flow pane opens and live-updates as you refine it. `flow` is the whole document: {version: 1, name, nodes: [], edges: []}. Node: {id, kind, name, instructions, expects, produces, inputs: [{name}], outputs: [{name}], x, y, model?} — kind is \"terminal\" or an agent CLI (claude, opencode, pi); give every node a unique short id like \"n1\"; omit x/y for auto-layout. Edge: {id, from, to, fromOutput, toInput} joining an output port name to an input port name. Structural errors are refused outright; contract warnings come back for you to raise with the user. Only call this after the user agrees to start (or change) a draft."));
}

// ================= new conductor tools (write/read/run/skills) =================

#[test]
fn write_file_refuses_a_path_outside_the_workspace() {
    let c = Conductor::new();
    let (mut env, _sent, logged) = fake_env();
    env.resolve_write =
        Arc::new(|_p: &Path| Err("path is outside the open workspace folders".to_string()));
    let out = run_tool(
        &c,
        &env,
        "write_file",
        &json!({ "path": "/etc/passwd", "content": "x" }),
        "chat-1",
    );
    assert_eq!(out, "Refused: path is outside the open workspace folders");
    assert!(logged.lock().unwrap().is_empty());
}

#[test]
fn write_file_writes_content_and_audits() {
    let c = Conductor::new();
    let (mut env, _sent, logged) = fake_env();
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("out.txt");
    let target_for_closure = target.clone();
    env.resolve_write = Arc::new(move |_p: &Path| Ok(target_for_closure.clone()));
    let out = run_tool(
        &c,
        &env,
        "write_file",
        &json!({ "path": "/work/out.txt", "content": "hello" }),
        "chat-1",
    );
    assert_eq!(out, "Wrote 5 bytes to /work/out.txt.");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
    let logs = logged.lock().unwrap();
    assert_eq!(
        *logs,
        vec![(
            "conductor:writeFile".to_string(),
            vec![("path".to_string(), json!("/work/out.txt"))]
        )]
    );
}

#[test]
fn read_file_returns_content() {
    let c = Conductor::new();
    let (mut env, _sent, _logged) = fake_env();
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("in.txt");
    std::fs::write(&target, "file body").unwrap();
    let target_for_closure = target.clone();
    env.resolve_path = Arc::new(move |_p: &Path| Ok(target_for_closure.clone()));
    let out = run_tool(
        &c,
        &env,
        "read_file",
        &json!({ "path": "/work/in.txt" }),
        "chat-1",
    );
    assert_eq!(out, "file body");
}

#[test]
fn run_command_refuses_when_allow_run_is_false() {
    let c = Conductor::new();
    let (mut env, _sent, _logged) = fake_env();
    c.set_allow_run(false);
    let called = Arc::new(AtomicUsize::new(0));
    let called_for_closure = called.clone();
    env.run_command = Arc::new(move |_cwd: &str, _cmd: &str| {
        called_for_closure.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok("PASS".to_string()) }) as BoxFuture<Result<String, String>>
    });
    let out = run_tool(
        &c,
        &env,
        "run_command",
        &json!({ "cwd": "/work", "cmd": "ls" }),
        "chat-1",
    );
    assert!(out.contains("Refused"), "unexpected output: {out}");
    assert_eq!(called.load(Ordering::SeqCst), 0);
}

#[test]
fn run_command_returns_output_when_allowed() {
    let c = Conductor::new();
    let (mut env, _sent, _logged) = fake_env();
    c.set_allow_run(true);
    env.run_command = Arc::new(|_cwd: &str, _cmd: &str| {
        Box::pin(async move { Ok("PASS".to_string()) }) as BoxFuture<Result<String, String>>
    });
    let out = run_tool(
        &c,
        &env,
        "run_command",
        &json!({ "cwd": "/work", "cmd": "echo PASS" }),
        "chat-1",
    );
    assert_eq!(out, "PASS");
}

#[test]
fn list_skills_returns_json() {
    let c = Conductor::new();
    let (mut env, _sent, _logged) = fake_env();
    let tmp = tempfile::tempdir().unwrap();
    let skill_dir = tmp.path().join("myskill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: myskill\ndescription: does a thing\n---\nbody here",
    )
    .unwrap();
    let root_for_closure = tmp.path().to_path_buf();
    env.skills_root = Arc::new(move || Some(root_for_closure.clone()));
    let out = run_tool(&c, &env, "list_skills", &json!({}), "chat-1");
    assert!(out.contains("myskill"), "unexpected output: {out}");
}
