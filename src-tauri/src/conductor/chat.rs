//! The tool loop — direct port of `conductor.js`'s `runChat`. Streamed
//! chat with a bounded tool loop: text deltas stream to the renderer as
//! they arrive; tool calls surface as `chat:tool` events between segments.
//!
//! Budget: each tool turn re-sends the whole transcript, so [`MAX_TURNS`]
//! turns at (the provider's own) max output tokens is a worst-case-large
//! output for one user message. [`TOKEN_BUDGET`] caps cumulative usage
//! across the loop; when exceeded this stops gracefully with a visible
//! note instead of burning on silently.
//!
//! Abort (TOME-015): the renderer's stop button lands on `chat:abort` ->
//! [`abort_chat`]; the SAME `CancellationToken` this loop reads is
//! cancelled, so an in-flight stream is raced out immediately
//! ([`run_loop`]'s `tokio::select!`) and no further tool call in the
//! current batch runs after the cancellation lands — checked immediately
//! before EVERY `runTool`-equivalent dispatch, not just between turns, so a
//! `send` callback that cancels mid-batch (as the ported abort test does,
//! synchronously, from inside the FIRST `chat:tool` event) stops the
//! SECOND tool in that same batch from ever running.
//!
//! ## Error-handling split with `ipc::chat::chat_send`
//!
//! Mirrors the JS original's own control flow exactly: `runChat`'s own
//! `try/catch` only ever catches an ABORT (`if (aborted) { break }` else
//! `throw err`) — every OTHER stream failure propagates out of `runChat`
//! to `chat:send`'s own outer `catch`, which does the 401/authy
//! classification and its OWN `chat:done` emit. [`run_chat`] mirrors that
//! split precisely: it emits `chat:done` itself for every INTERNAL exit
//! (refusal, a clean end, the token budget, the loop limit, an abort) and
//! returns `Ok(())` for all of them, but returns `Err(ChatError)` — WITHOUT
//! emitting `chat:done` — for a genuine stream failure, leaving that
//! classification to its caller.

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::env::{ConductorEnv, OnText};
use super::state::Conductor;
use super::tools;
use crate::chat::sse::ChatError;

/// `for (let turn = 0; turn < 8; turn++)` — verbatim in the JS original;
/// deliberately bumped to 12 for orchestration: a `run_agent` delegation
/// consumes one tool turn, so an assistant coordinating several agents and
/// reporting back needs more headroom than the port's 8. The token budget
/// (below) still bounds total spend, so the bump costs nothing when turns
/// are cheap.
const MAX_TURNS: usize = 12;
/// `TOKEN_BUDGET` — verbatim.
const TOKEN_BUDGET: u64 = 400_000;

/// `chat:abort` -> `conductor.abortChat(id)`.
pub fn abort_chat(c: &Conductor, id: &str) {
    c.abort_chat(id);
}

/// `runChat({ id, system, messages, client })`. `system` falls back to
/// `c.system_prompt()` when absent, mirroring `system || SYSTEM`.
///
/// See the module doc comment for the `Ok`/`Err` split with the caller.
pub async fn run_chat(
    c: &Conductor,
    env: &ConductorEnv,
    id: String,
    system: Option<String>,
    messages: Vec<Value>,
) -> Result<(), ChatError> {
    let token = c.begin_chat(&id);
    // `finally { inflight.delete(id) }` — runs on every exit path below,
    // `Ok` or `Err`, since this isn't behind any early `?`/`return` of its
    // own.
    let outcome = run_loop(c, env, &id, system, messages, &token).await;
    c.end_chat(&id);
    outcome
}

async fn run_loop(
    c: &Conductor,
    env: &ConductorEnv,
    id: &str,
    system: Option<String>,
    messages: Vec<Value>,
    token: &CancellationToken,
) -> Result<(), ChatError> {
    let sys = system.unwrap_or_else(|| c.system_prompt());
    let tool_defs = c.tools();
    let mut msgs = messages;
    let mut total_tokens: u64 = 0;
    let mut aborted = false;

    for _turn in 0..MAX_TURNS {
        if token.is_cancelled() {
            aborted = true;
            break;
        }

        let on_text: OnText = {
            let emit_id = id.to_string();
            let send = env.send.clone();
            Box::new(move |text: &str| {
                (send)("chat:delta", json!({ "id": emit_id, "text": text }));
            })
        };

        let stream_fut =
            (env.stream_chat)(Some(sys.clone()), msgs.clone(), tool_defs.clone(), on_text);
        let raced = tokio::select! {
            r = stream_fut => Some(r),
            _ = token.cancelled() => None,
        };
        let final_resp = match raced {
            None => {
                aborted = true;
                break;
            }
            Some(Err(e)) => return Err(e),
            Some(Ok(r)) => r,
        };

        total_tokens += final_resp.usage.input + final_resp.usage.output;

        if final_resp.stop_reason == "refusal" {
            (env.send)(
                "chat:done",
                json!({ "id": id, "aborted": false, "error": "Request declined by safety classifiers." }),
            );
            return Ok(());
        }
        if final_resp.stop_reason != "tool_use" {
            (env.send)(
                "chat:done",
                json!({ "id": id, "aborted": false, "error": Value::Null }),
            );
            return Ok(());
        }

        msgs.push(json!({ "role": "assistant", "content": final_resp.content }));
        let mut results = Vec::new();
        for block in final_resp
            .content
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        {
            // A stop mid-turn must not let the rest of THIS batch of tool
            // calls run headless after the renderer stopped listening —
            // bail before the next dispatch (TOME-015).
            if token.is_cancelled() {
                break;
            }
            let tool_name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
            let hint = tools::tool_hint(&input);
            (env.send)(
                "chat:tool",
                json!({ "id": id, "tool": tool_name, "hint": hint }),
            );
            let started = std::time::Instant::now();
            let out = tools::run_tool(c, env, &tool_name, &input, id).await;
            let ok = !tools::is_tool_failure(&out);
            // The plan tracker's step-completion event: same shape as
            // `chat:tool`, plus the honest outcome and wall-clock ms.
            (env.send)(
                "chat:tool-done",
                json!({
                    "id": id,
                    "tool": tool_name,
                    "hint": hint,
                    "ok": ok,
                    "ms": started.elapsed().as_millis() as u64,
                }),
            );
            // Audit the ACTION only: tool name, chat, outcome, and the same
            // hint the chat:tool event carries. Tool input/output never
            // goes in the log — typed text may contain secrets.
            (env.log_event)(
                "conductor:tool",
                vec![
                    ("tool".to_string(), json!(tool_name)),
                    ("chatId".to_string(), json!(id)),
                    ("ok".to_string(), json!(ok)),
                    ("hint".to_string(), json!(hint)),
                ],
            );
            let tool_use_id = block.get("id").cloned().unwrap_or(Value::Null);
            results
                .push(json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": out }));
        }
        msgs.push(json!({ "role": "user", "content": results }));

        if total_tokens > TOKEN_BUDGET {
            let thousands = ((total_tokens as f64) / 1000.0).round() as u64;
            let error = format!(
                "Token budget reached (~{thousands}k tokens across tool turns) — stopped early. Ask again to continue."
            );
            (env.send)(
                "chat:done",
                json!({ "id": id, "aborted": false, "error": error }),
            );
            return Ok(());
        }
    }

    if aborted || token.is_cancelled() {
        (env.send)(
            "chat:done",
            json!({ "id": id, "aborted": true, "error": "Stopped." }),
        );
        return Ok(());
    }
    (env.send)(
        "chat:done",
        json!({ "id": id, "aborted": false, "error": "Tool loop limit reached — ask again to continue." }),
    );
    Ok(())
}
