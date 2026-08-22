//! Both chat wire dialects: shape translation (pure) + SSE streaming (real
//! network I/O over `reqwest`). Ports `src/main/lib/chat-client.js`'s
//! `toolsToOpenAI`/`openAIMessagesFrom`/`streamChat`/`streamOpenAI` and,
//! for the anthropic wire, hand-rolls what the JS original delegated to
//! `@anthropic-ai/sdk`'s `beta.messages.stream` — this crate has no SDK
//! underneath it, so `stream_anthropic` speaks `/v1/messages` SSE directly.
//! This is the plan's one net-new wire code (see the phase brief and
//! `docs/PLAN...`'s "Anthropic wire hand-port" top risk).
//!
//! ## Design: pure incremental parsers, impure network shells
//!
//! Both wires split into an incremental, network-free parser
//! ([`OpenAiSseState`] / [`AnthropicSseState`]) fed raw byte chunks via
//! `.feed()`, and a thin `stream_openai`/`stream_anthropic` function that
//! does the actual `reqwest` POST + `.bytes_stream()` read loop and drives
//! the parser. This is NOT how the JS original is shaped (`streamOpenAI`
//! mixes `fetch` + parsing in one function) — the split exists so this
//! module's tests can feed canned SSE byte chunks straight into the parser
//! (matching the phase brief's explicit test guidance) without a mock HTTP
//! server. Request/header construction is similarly split into pure
//! `build_*_request_body`/`bearer_header`/`*_url` functions the network
//! functions call — so the "request shape" assertions the JS suite makes
//! against a stubbed `fetch` (URL, headers, body) port to plain unit tests
//! here instead of a mocked network call.
//!
//! ## Anthropic SSE event shape
//!
//! No vitest fixture exists for this (the JS test only ever exercises a
//! FAKE `anthropic.beta.messages.stream` mock — it never sees real SSE
//! bytes). The event sequence below is the real, documented, long-stable
//! Anthropic Messages streaming API shape: `message_start` (carries the
//! initial `usage.input_tokens`) → repeated `content_block_start`/
//! `content_block_delta`/`content_block_stop` triples, one per content
//! block index (`text_delta`/`input_json_delta` deltas) → `message_delta`
//! (carries the final `stop_reason` and `usage.output_tokens`) →
//! `message_stop`, with `ping` keepalives interleaved at any point. Each
//! event's own JSON payload carries a `"type"` field naming itself, so this
//! parser dispatches on that field directly rather than tracking the SSE
//! `event:` line — both are equivalent (the payload's `type` mirrors the
//! `event:` line 1:1 on the real API) but reading only `data:` lines keeps
//! this parser structurally identical to the OpenAI one, which never reads
//! anything but `data:` lines either.
//!
//! ## Betas/fallbacks wire mechanics — verified against the pinned SDK
//!
//! `betas` (an array of beta-flag strings) is well-established, long-stable
//! Anthropic API surface: the SDK's own `betas` request param becomes the
//! `anthropic-beta` HTTP header (comma-joined), never a JSON body field —
//! high confidence. `fallbacks` (`provider.fallbacks = 'default'` in
//! `index.js`) was initially flagged as a lower-confidence guess; it is now
//! VERIFIED against the pinned `@anthropic-ai/sdk` 0.115 type definitions
//! (node_modules/@anthropic-ai/sdk/resources/beta/messages/messages.d.ts):
//! `BetaFallbacksParam = Array<BetaFallbackParam> | 'default'`, a top-level
//! body param of the beta `/v1/messages` endpoint ("Opt-in server-side
//! retry on one or more substitute models… The string \"default\" requests
//! the requested model's server-defined default fallback configuration"),
//! and `server-side-fallback-2026-07-01` (the beta flag `ipc::chat`
//! attaches alongside it, porting `index.js`) is a named member of the
//! SDK's `AnthropicBeta` union. So: top-level JSON body field
//! (`"fallbacks": "default"`), exactly as `build_anthropic_request_body`
//! sends it — the string form is all this app uses, matching the JS
//! original's own `'default'` literal. The array form (per-attempt model
//! overrides with optional max_tokens/thinking/speed) is not needed here
//! and would be a new feature, not a port fix.

use serde_json::{json, Map, Value};

use super::registry::{Auth, ResolvedProvider, Wire};

// ============================= shared shapes =============================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
}

/// The wire-agnostic normalized shape both `stream_openai`/`stream_anthropic`
/// resolve to — `content: [{ type: 'text', text } | { type: 'tool_use', id,
/// name, input }]` in the JS original's own doc comment. Content blocks stay
/// `serde_json::Value` rather than a typed enum: they cross the IPC boundary
/// as JSON regardless, this module's own tests compare them the same way the
/// ported vitest suite compares plain objects (`toEqual`), and the shape
/// translators (`tools_to_openai`/`openai_messages_from`) already operate on
/// `Value` for the identical reason — one representation for "loosely-typed
/// JSON shape this module transcribes", not two.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedResponse {
    pub stop_reason: String,
    pub content: Vec<Value>,
    pub usage: Usage,
}

/// Every failure mode `stream_openai`/`stream_anthropic` can produce.
/// Structured (rather than a plain `String`, as `chat-client.js` throws)
/// so `ipc::chat::chat_send` can classify an auth failure the same way
/// `index.js`'s catch block does (`err?.status === 401`) without having to
/// regex the status code back out of a formatted message string.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatError {
    /// A non-2xx HTTP response — `status` backs `.status()`,
    /// `message()` formats it exactly like the JS original's thrown
    /// `Error('chat: HTTP ${status} — ${body.slice(0, 200)}')`.
    Http { status: u16, body: String },
    /// The request itself failed (DNS, TLS, connection reset, …) — no HTTP
    /// status to report.
    Network(String),
    /// The response started streaming but a later chunk read failed.
    Stream(String),
    /// An Anthropic `error` SSE event arrived mid-stream — has no OpenAI-wire
    /// equivalent (that wire has no in-band error-event type; its failures
    /// are always the HTTP-level `Http` variant instead).
    Api(String),
}

impl ChatError {
    pub fn message(&self) -> String {
        match self {
            ChatError::Http { status, body } => format_http_error(*status, body),
            ChatError::Network(m) | ChatError::Stream(m) | ChatError::Api(m) => m.clone(),
        }
    }

    /// `err?.status === 401` in the JS original's catch block — `None` for
    /// every non-HTTP failure, same as a plain thrown `Error` there has no
    /// `.status` property at all.
    pub fn status(&self) -> Option<u16> {
        match self {
            ChatError::Http { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for ChatError {}

/// `body.slice(0, 200)` in the JS original — `.slice` there operates on
/// UTF-16 code units; this operates on `char`s, which only differs from the
/// JS behavior for astral-plane characters in the first 200 units of a
/// non-ASCII error body, a divergence not worth hand-rolling UTF-16-unit
/// counting to avoid.
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn format_http_error(status: u16, body: &str) -> String {
    format!("chat: HTTP {status} — {}", truncate_chars(body, 200))
}

/// Byte-oriented newline splitter shared by both wires — ports the JS
/// original's `buf += decoder.decode(value, { stream: true }); const lines =
/// buf.split('\n'); buf = lines.pop()` carry-buffer, operating on raw bytes
/// instead of a `TextDecoder`. Splitting on the single-byte `'\n'` (0x0A) is
/// UTF-8-safe regardless of where a chunk boundary falls: 0x0A can never
/// appear as a continuation byte of a multi-byte sequence, so a line is
/// never decoded until every byte of it has arrived, sidestepping the
/// "incomplete trailing multi-byte sequence" problem `pty.rs` had to solve
/// by hand for arbitrary (non-line-oriented) binary output.
#[derive(Default)]
struct LineBuffer {
    buf: Vec<u8>,
}

impl LineBuffer {
    fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // drop the trailing '\n'
            out.push(String::from_utf8_lossy(&line).into_owned());
        }
        out
    }

    /// Mirrors the JS original's post-loop `const tail = buf.trim(); if
    /// (tail.startsWith('data:')) handleEvent(...)` — an SSE body with no
    /// trailing newline still has its last event processed.
    fn take_tail(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned())
        }
    }
}

// =============================== OpenAI wire ===============================

/// `FINISH_REASON` map, verbatim — any unrecognized reason falls back to
/// `'end'`, matching `FINISH_REASON[choice.finish_reason] || 'end'`.
fn finish_reason_from(reason: &str) -> String {
    match reason {
        "stop" => "end",
        "tool_calls" => "tool_use",
        "content_filter" => "refusal",
        _ => "end",
    }
    .to_string()
}

/// `const last = content[content.length - 1]; if (last?.type === 'text')
/// last.text += delta.content; else content.push({ type: 'text', text:
/// delta.content })`. During streaming `content` only ever holds text
/// blocks (tool-call fragments accumulate separately in `tool_calls` below
/// and are appended only at `into_response()` time) — so every text delta,
/// however interleaved with tool-call fragments, merges into exactly one
/// text block, which then sorts before every tool_use block in the final
/// output. That is the REAL behavior the ported fragmented-tool-calls test
/// pins (a single merged text block first, tool_use blocks after) — not an
/// approximation of it.
fn push_text_delta(content: &mut Vec<Value>, text: &str) {
    if let Some(Value::Object(map)) = content.last_mut() {
        if map.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(Value::String(existing)) = map.get_mut("text") {
                existing.push_str(text);
                return;
            }
        }
    }
    content.push(json!({ "type": "text", "text": text }));
}

#[derive(Default)]
struct ToolAccum {
    id: String,
    name: String,
    args: String,
}

/// Incremental OpenAI-wire SSE parser — network-free. Feed raw byte chunks
/// via [`Self::feed`] (as `reqwest::Response::bytes_stream()` would yield
/// them), then [`Self::finish`] once the stream ends.
pub(crate) struct OpenAiSseState {
    lines: LineBuffer,
    content: Vec<Value>,
    // Insertion-ordered by first-seen index (a `Vec`, not a `HashMap`) to
    // match the JS original's `Map` iteration order exactly — tool-call
    // indices happen to arrive in ascending numeric order on the real API,
    // but this doesn't rely on that; it preserves whatever order they were
    // FIRST seen in, same as a JS `Map` would.
    tool_calls: Vec<(i64, ToolAccum)>,
    usage: Option<Usage>,
    stop_reason: String,
}

impl OpenAiSseState {
    pub(crate) fn new() -> Self {
        Self {
            lines: LineBuffer::default(),
            content: Vec::new(),
            tool_calls: Vec::new(),
            usage: None,
            stop_reason: "end".to_string(),
        }
    }

    pub(crate) fn feed(&mut self, chunk: &[u8], on_text: &mut impl FnMut(&str)) {
        for line in self.lines.feed(chunk) {
            self.handle_line(&line, on_text);
        }
    }

    pub(crate) fn finish(mut self, on_text: &mut impl FnMut(&str)) -> NormalizedResponse {
        if let Some(tail) = self.lines.take_tail() {
            self.handle_line(&tail, on_text);
        }
        self.into_response()
    }

    fn handle_line(&mut self, raw: &str, on_text: &mut impl FnMut(&str)) {
        let Some(rest) = raw.trim().strip_prefix("data:") else {
            return;
        };
        self.handle_event(rest.trim(), on_text);
    }

    fn handle_event(&mut self, data: &str, on_text: &mut impl FnMut(&str)) {
        if data == "[DONE]" {
            return;
        }
        // Unparseable JSON is a keepalive/comment line the SSE spec allows
        // — `catch { return }` in the JS original.
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return;
        };

        if let Some(u) = chunk.get("usage").filter(|v| !v.is_null()) {
            self.usage = Some(Usage {
                input: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                output: u
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            });
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        else {
            return;
        };
        let empty = Value::Object(Map::new());
        let delta = choice.get("delta").unwrap_or(&empty);

        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                on_text(text);
                push_text_delta(&mut self.content, text);
            }
        }

        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call.get("index").and_then(Value::as_i64).unwrap_or(0);
                let acc = self.tool_acc_mut(index);
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    acc.id.push_str(id);
                }
                if let Some(func) = call.get("function") {
                    if let Some(name) = func.get("name").and_then(Value::as_str) {
                        acc.name.push_str(name);
                    }
                    if let Some(args) = func.get("arguments").and_then(Value::as_str) {
                        acc.args.push_str(args);
                    }
                }
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = finish_reason_from(reason);
        }
    }

    fn tool_acc_mut(&mut self, index: i64) -> &mut ToolAccum {
        if let Some(pos) = self.tool_calls.iter().position(|(i, _)| *i == index) {
            &mut self.tool_calls[pos].1
        } else {
            self.tool_calls.push((index, ToolAccum::default()));
            &mut self.tool_calls.last_mut().expect("just pushed").1
        }
    }

    fn into_response(self) -> NormalizedResponse {
        let mut content = self.content;
        for (_, acc) in self.tool_calls {
            let input = if acc.args.is_empty() {
                json!({})
            } else {
                serde_json::from_str(&acc.args).unwrap_or_else(|_| json!({}))
            };
            content.push(
                json!({ "type": "tool_use", "id": acc.id, "name": acc.name, "input": input }),
            );
        }
        NormalizedResponse {
            stop_reason: self.stop_reason,
            content,
            usage: self.usage.unwrap_or(Usage {
                input: 0,
                output: 0,
            }),
        }
    }
}

/// Anthropic tool defs → OpenAI function defs — direct port of
/// `toolsToOpenAI`. `input_schema` maps 1:1 onto `parameters` (both plain
/// JSON Schema).
pub fn tools_to_openai(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.get("name").cloned().unwrap_or(Value::Null),
                    "description": t.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": t.get("input_schema").cloned().unwrap_or(Value::Null),
                }
            })
        })
        .collect()
}

/// `b.input ?? {}` — substitutes only for a missing key OR an explicit JSON
/// `null` (nullish coalescing), never for another falsy-but-present value —
/// matches serde's `Value::Null` variant check, not `.is_none()`-only.
fn tool_use_input(block: &Value) -> Value {
    match block.get("input") {
        Some(v) if !v.is_null() => v.clone(),
        _ => json!({}),
    }
}

/// Conductor history (Anthropic-shaped) → OpenAI messages — direct port of
/// `openAIMessagesFrom`. See that JS function's own doc comment for the
/// three cases (assistant content-array → one assistant message with
/// `tool_calls`; a user message whose content is tool_result blocks → one
/// `role: 'tool'` message per result; everything else passes through).
pub fn openai_messages_from(messages: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(Value::as_str);
        let blocks = m.get("content").and_then(Value::as_array);

        if role == Some("assistant") {
            if let Some(blocks) = blocks {
                let text: String = blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect();
                let calls: Vec<Value> = blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                    .map(|b| {
                        json!({
                            "id": b.get("id").cloned().unwrap_or(Value::Null),
                            "type": "function",
                            "function": {
                                "name": b.get("name").cloned().unwrap_or(Value::Null),
                                "arguments": serde_json::to_string(&tool_use_input(b)).unwrap_or_else(|_| "{}".to_string()),
                            }
                        })
                    })
                    .collect();
                let mut obj = Map::new();
                obj.insert("role".to_string(), json!("assistant"));
                obj.insert(
                    "content".to_string(),
                    if text.is_empty() {
                        Value::Null
                    } else {
                        json!(text)
                    },
                );
                if !calls.is_empty() {
                    obj.insert("tool_calls".to_string(), json!(calls));
                }
                out.push(Value::Object(obj));
                continue;
            }
        }

        if role == Some("user") {
            if let Some(blocks) = blocks {
                let has_tool_result = blocks
                    .iter()
                    .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));
                if has_tool_result {
                    for b in blocks {
                        if b.get("type").and_then(Value::as_str) != Some("tool_result") {
                            continue;
                        }
                        let content_str = match b.get("content") {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => serde_json::to_string(other).unwrap_or_default(),
                            None => serde_json::to_string(&Value::Null).unwrap_or_default(),
                        };
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": b.get("tool_use_id").cloned().unwrap_or(Value::Null),
                            "content": content_str,
                        }));
                    }
                    continue;
                }
            }
        }

        out.push(m.clone());
    }
    out
}

fn bearer_header(api_key: &str) -> String {
    format!("Bearer {api_key}")
}

/// Applies the row's auth choice to a request: `Bearer` →
/// `Authorization: Bearer <key>`, `XApiKey` → `x-api-key: <key>`. Auth is
/// per ROW, not per wire — Anthropic proper wants `x-api-key`, while most
/// Anthropic-*compatible* gateways want `Authorization: Bearer`, and until
/// the registry gained an `auth` field there was no way to configure one.
fn apply_auth(req: reqwest::RequestBuilder, auth: Auth, api_key: &str) -> reqwest::RequestBuilder {
    match auth {
        Auth::Bearer => req.header("authorization", bearer_header(api_key)),
        Auth::XApiKey => req.header("x-api-key", api_key),
    }
}

fn openai_chat_completions_url(base_url: &str) -> String {
    format!("{base_url}/chat/completions")
}

/// `{ model, stream: true, messages: system ? [system, ...openAIMessagesFrom(messages)]
/// : openAIMessagesFrom(messages), tools: toolsToOpenAI(tools) }` — the JSON
/// body `stream_openai` POSTs, factored out so the "request shape" vitest
/// assertions (system first, tools translated) port to a plain unit test
/// instead of a mocked-fetch integration test.
fn build_openai_request_body(
    model: &str,
    system: Option<&str>,
    messages: &[Value],
    tools: &[Value],
) -> Value {
    let mut msgs = openai_messages_from(messages);
    if let Some(sys) = system {
        msgs.insert(0, json!({ "role": "system", "content": sys }));
    }
    json!({
        "model": model,
        "stream": true,
        "messages": msgs,
        "tools": tools_to_openai(tools),
    })
}

/// Real network call — direct port of `streamOpenAI`'s fetch + SSE-read
/// loop, using [`OpenAiSseState`] for the parsing half. Not itself unit
/// tested (it performs a real `reqwest` request); every piece it's built
/// from ([`build_openai_request_body`], [`bearer_header`],
/// [`openai_chat_completions_url`], [`OpenAiSseState`]) is.
async fn stream_openai(
    client: &reqwest::Client,
    provider: &ResolvedProvider,
    system: Option<&str>,
    messages: &[Value],
    tools: &[Value],
    mut on_text: impl FnMut(&str) + Send,
) -> Result<NormalizedResponse, ChatError> {
    let url = openai_chat_completions_url(&provider.base_url);
    let body = build_openai_request_body(&provider.model, system, messages, tools);

    let req = client.post(&url).header("content-type", "application/json");
    let res = apply_auth(req, provider.auth, &provider.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| ChatError::Network(e.to_string()))?;

    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(ChatError::Http {
            status: status.as_u16(),
            body: text,
        });
    }

    let mut state = OpenAiSseState::new();
    let mut stream = res.bytes_stream();
    use futures_util::StreamExt;
    while let Some(next) = stream.next().await {
        let bytes = next.map_err(|e| ChatError::Stream(e.to_string()))?;
        state.feed(&bytes, &mut on_text);
    }
    Ok(state.finish(&mut on_text))
}

// ============================= Anthropic wire =============================

/// The Messages API's stable required version header — long-unchanged
/// public surface, distinct from the one-off `anthropic-beta` flags a
/// specific request may additionally opt into.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// `max_tokens` fallback when a row doesn't set one — `registry::resolve`
/// already applies this default, so the wire functions just receive the
/// resolved value.
enum OpenBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        json: String,
    },
}

/// Incremental Anthropic-wire SSE parser — network-free, the hand-rolled
/// replacement for the SDK's own stream-assembly logic. See the module doc
/// comment for the event sequence this dispatches on.
#[derive(Default)]
pub(crate) struct AnthropicSseState {
    lines: LineBuffer,
    // Insertion-ordered by content-block index, same rationale as
    // OpenAiSseState::tool_calls — the real API opens blocks in strictly
    // ascending index order, but this doesn't depend on that.
    blocks: Vec<(u64, OpenBlock)>,
    input_tokens: u64,
    output_tokens: u64,
    raw_stop_reason: Option<String>,
    error: Option<String>,
}

impl AnthropicSseState {
    pub(crate) fn feed(&mut self, chunk: &[u8], on_text: &mut impl FnMut(&str)) {
        for line in self.lines.feed(chunk) {
            self.handle_line(&line, on_text);
        }
    }

    pub(crate) fn finish(
        mut self,
        on_text: &mut impl FnMut(&str),
    ) -> Result<NormalizedResponse, ChatError> {
        if let Some(tail) = self.lines.take_tail() {
            self.handle_line(&tail, on_text);
        }
        self.into_response()
    }

    fn handle_line(&mut self, raw: &str, on_text: &mut impl FnMut(&str)) {
        let Some(rest) = raw.trim().strip_prefix("data:") else {
            return;
        };
        let Ok(payload) = serde_json::from_str::<Value>(rest.trim()) else {
            return;
        };
        self.handle_event(&payload, on_text);
    }

    fn handle_event(&mut self, payload: &Value, on_text: &mut impl FnMut(&str)) {
        match payload.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(t) = payload
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64)
                {
                    self.input_tokens = t;
                }
                if let Some(t) = payload
                    .pointer("/message/usage/output_tokens")
                    .and_then(Value::as_u64)
                {
                    self.output_tokens = t;
                }
            }
            Some("content_block_start") => {
                let Some(index) = payload.get("index").and_then(Value::as_u64) else {
                    return;
                };
                let block = if payload
                    .pointer("/content_block/type")
                    .and_then(Value::as_str)
                    == Some("tool_use")
                {
                    OpenBlock::ToolUse {
                        id: payload
                            .pointer("/content_block/id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        name: payload
                            .pointer("/content_block/name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        json: String::new(),
                    }
                } else {
                    OpenBlock::Text(String::new())
                };
                self.blocks.push((index, block));
            }
            Some("content_block_delta") => {
                let Some(index) = payload.get("index").and_then(Value::as_u64) else {
                    return;
                };
                let Some((_, block)) = self.blocks.iter_mut().find(|(i, _)| *i == index) else {
                    return;
                };
                match block {
                    OpenBlock::Text(text) => {
                        if let Some(t) = payload.pointer("/delta/text").and_then(Value::as_str) {
                            on_text(t);
                            text.push_str(t);
                        }
                    }
                    OpenBlock::ToolUse { json, .. } => {
                        if let Some(fragment) = payload
                            .pointer("/delta/partial_json")
                            .and_then(Value::as_str)
                        {
                            json.push_str(fragment);
                        }
                    }
                }
            }
            Some("message_delta") => {
                if let Some(reason) = payload
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                {
                    self.raw_stop_reason = Some(reason.to_string());
                }
                if let Some(t) = payload
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64)
                {
                    self.output_tokens = t;
                }
            }
            Some("error") => {
                let msg = payload
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("anthropic stream error")
                    .to_string();
                self.error = Some(msg);
            }
            // content_block_stop / message_stop / ping / anything unrecognized:
            // no-op, same as the OpenAI parser silently ignoring a keepalive.
            _ => {}
        }
    }

    fn into_response(self) -> Result<NormalizedResponse, ChatError> {
        if let Some(msg) = self.error {
            return Err(ChatError::Api(msg));
        }
        let content = self
            .blocks
            .into_iter()
            .map(|(_, block)| match block {
                OpenBlock::Text(text) => json!({ "type": "text", "text": text }),
                OpenBlock::ToolUse { id, name, json } => {
                    let input = if json.is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&json).unwrap_or_else(|_| serde_json::json!({}))
                    };
                    json!({ "type": "tool_use", "id": id, "name": name, "input": input })
                }
            })
            .collect();
        // `final.stop_reason === 'refusal' ? 'refusal' : final.stop_reason
        // === 'tool_use' ? 'tool_use' : 'end'` — verbatim three-way mapping;
        // end_turn/max_tokens/stop_sequence/pause_turn/absent all fall to 'end'.
        let stop_reason = match self.raw_stop_reason.as_deref() {
            Some("refusal") => "refusal",
            Some("tool_use") => "tool_use",
            _ => "end",
        }
        .to_string();
        Ok(NormalizedResponse {
            stop_reason,
            content,
            usage: Usage {
                input: self.input_tokens,
                output: self.output_tokens,
            },
        })
    }
}

/// `{base}/v1/messages`, with `?beta=true` appended when `beta` is set.
/// The pinned SDK posts beta requests to `/v1/messages?beta=true` while
/// this client always posts GA — and attaching beta-only body params
/// (`fallbacks`) to the GA endpoint 400s. Shipping rows with empty
/// `betas` keeps the GA path; rows that opt in get the beta endpoint.
fn anthropic_messages_url(base_url: &str, beta: bool) -> String {
    format!(
        "{}/v1/messages{}",
        base_url.trim_end_matches('/'),
        if beta { "?beta=true" } else { "" }
    )
}

/// `betas.join(',')` for the `anthropic-beta` header — `None` when empty,
/// so callers never send an empty header.
fn anthropic_beta_header(betas: &[String]) -> Option<String> {
    if betas.is_empty() {
        None
    } else {
        Some(betas.join(","))
    }
}

/// `{ model, max_tokens, stream: true, system?, messages, tools,
/// fallbacks? }` — see the module doc comment's note on `fallbacks`' wire
/// placement. `max_tokens` is per-row now (the old model-blind
/// `ANTHROPIC_MAX_TOKENS` const is gone). `system`/`fallbacks` are omitted
/// entirely when absent (`if (provider.betas) args.betas = ...` — the JS
/// original only ever conditionally ADDS a key, never sets it to an
/// explicit null).
fn build_anthropic_request_body(
    model: &str,
    system: Option<&str>,
    messages: &[Value],
    tools: &[Value],
    fallbacks: Option<&str>,
    max_tokens: u64,
) -> Value {
    let mut body = Map::new();
    body.insert("model".to_string(), json!(model));
    body.insert("max_tokens".to_string(), json!(max_tokens));
    body.insert("stream".to_string(), json!(true));
    if let Some(sys) = system {
        body.insert("system".to_string(), json!(sys));
    }
    body.insert("messages".to_string(), json!(messages));
    body.insert("tools".to_string(), json!(tools));
    if let Some(fb) = fallbacks {
        body.insert("fallbacks".to_string(), json!(fb));
    }
    Value::Object(body)
}

/// Real network call — hand-rolled replacement for the SDK's
/// `anthropic.beta.messages.stream(args, { signal })`. Same
/// split-for-testability rationale as `stream_openai`.
#[allow(clippy::too_many_arguments)]
async fn stream_anthropic(
    client: &reqwest::Client,
    provider: &ResolvedProvider,
    system: Option<&str>,
    messages: &[Value],
    tools: &[Value],
    betas: Option<&[String]>,
    fallbacks: Option<&str>,
    mut on_text: impl FnMut(&str) + Send,
) -> Result<NormalizedResponse, ChatError> {
    let url = anthropic_messages_url(&provider.base_url, betas.is_some_and(|b| !b.is_empty()));
    let body = build_anthropic_request_body(
        &provider.model,
        system,
        messages,
        tools,
        fallbacks,
        provider.max_output_tokens,
    );

    let req = client
        .post(&url)
        .header("content-type", "application/json")
        .header("anthropic-version", ANTHROPIC_VERSION);
    let mut req = apply_auth(req, provider.auth, &provider.api_key);
    if let Some(header_val) = betas.and_then(anthropic_beta_header) {
        req = req.header("anthropic-beta", header_val);
    }

    let res = req
        .json(&body)
        .send()
        .await
        .map_err(|e| ChatError::Network(e.to_string()))?;

    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(ChatError::Http {
            status: status.as_u16(),
            body: text,
        });
    }

    let mut state = AnthropicSseState::default();
    let mut stream = res.bytes_stream();
    use futures_util::StreamExt;
    while let Some(next) = stream.next().await {
        let bytes = next.map_err(|e| ChatError::Stream(e.to_string()))?;
        state.feed(&bytes, &mut on_text);
    }
    state.finish(&mut on_text)
}

// =============================== dispatcher ===============================

/// Bundles `stream_chat`'s wire-agnostic call arguments — avoids an
/// unwieldy positional-parameter list across two very different wires'
/// worth of optional fields (`betas`/`fallbacks` are anthropic-only).
pub struct StreamChatArgs<'a> {
    pub system: Option<&'a str>,
    pub messages: &'a [Value],
    pub tools: &'a [Value],
    pub betas: Option<&'a [String]>,
    pub fallbacks: Option<&'a str>,
}

/// Direct port of `streamChat`'s dispatch — picks the wire off
/// `provider.wire` and delegates. Cancellation is deliberately NOT a
/// parameter here (unlike the JS original's `signal`): `ipc::chat::
/// chat_send` races this whole future against a `CancellationToken` via
/// `tokio::select!` instead, the same way dropping a `fetch` in-flight
/// (JS's `AbortController.abort()`) and dropping this future both stop the
/// underlying connection — see that command's doc comment.
pub async fn stream_chat(
    client: &reqwest::Client,
    provider: &ResolvedProvider,
    args: StreamChatArgs<'_>,
    on_text: impl FnMut(&str) + Send,
) -> Result<NormalizedResponse, ChatError> {
    match provider.wire {
        Wire::OpenAi => {
            stream_openai(
                client,
                provider,
                args.system,
                args.messages,
                args.tools,
                on_text,
            )
            .await
        }
        Wire::Anthropic => {
            stream_anthropic(
                client,
                provider,
                args.system,
                args.messages,
                args.tools,
                args.betas,
                args.fallbacks,
                on_text,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop(_: &str) {}

    // ================= toolsToOpenAI — ported from
    // test/chat-providers.test.js's `describe('toolsToOpenAI', ...)` =================

    fn tools_fixture() -> Vec<Value> {
        vec![
            json!({ "name": "list_panes", "description": "List open panes.", "input_schema": { "type": "object", "properties": {}, "required": [] } }),
            json!({
                "name": "read_terminal",
                "description": "Read a pane's recent scrollback.",
                "input_schema": { "type": "object", "properties": { "pane_id": { "type": "string" } }, "required": ["pane_id"] }
            }),
        ]
    }

    #[test]
    fn tools_to_openai_round_trips_name_description_and_input_schema() {
        let tools = tools_fixture();
        let out = tools_to_openai(&tools);
        assert_eq!(out.len(), tools.len());
        for (i, t) in tools.iter().enumerate() {
            assert_eq!(out[i]["type"], json!("function"));
            assert_eq!(out[i]["function"]["name"], t["name"]);
            assert_eq!(out[i]["function"]["description"], t["description"]);
            assert_eq!(out[i]["function"]["parameters"], t["input_schema"]);
        }
        let read = out
            .iter()
            .find(|t| t["function"]["name"] == "read_terminal")
            .expect("read_terminal present");
        assert_eq!(
            read["function"]["parameters"]["required"],
            json!(["pane_id"])
        );
    }

    #[test]
    fn tools_to_openai_of_empty_is_empty() {
        assert_eq!(tools_to_openai(&[]), Vec::<Value>::new());
    }

    // ================= openAIMessagesFrom — ported from
    // test/chat-providers.test.js's `describe('openAIMessagesFrom', ...)` =================

    fn tool_loop_transcript() -> Vec<Value> {
        vec![
            json!({ "role": "user", "content": "what is claude doing?" }),
            json!({
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "Let me look." },
                    { "type": "tool_use", "id": "toolu_1", "name": "list_panes", "input": {} }
                ]
            }),
            json!({ "role": "user", "content": [ { "type": "tool_result", "tool_use_id": "toolu_1", "content": "[{\"id\":\"p1\"}]" } ] }),
            json!({ "role": "assistant", "content": [ { "type": "text", "text": "Claude is editing a file." } ] }),
        ]
    }

    #[test]
    fn openai_messages_from_converts_a_recorded_tool_loop_transcript() {
        let out = openai_messages_from(&tool_loop_transcript());
        assert_eq!(
            out,
            vec![
                json!({ "role": "user", "content": "what is claude doing?" }),
                json!({
                    "role": "assistant",
                    "content": "Let me look.",
                    "tool_calls": [ { "id": "toolu_1", "type": "function", "function": { "name": "list_panes", "arguments": "{}" } } ]
                }),
                json!({ "role": "tool", "tool_call_id": "toolu_1", "content": "[{\"id\":\"p1\"}]" }),
                json!({ "role": "assistant", "content": "Claude is editing a file." }),
            ]
        );
    }

    #[test]
    fn openai_messages_from_emits_one_role_tool_message_per_tool_result_block() {
        let input = vec![json!({
            "role": "user",
            "content": [
                { "type": "tool_result", "tool_use_id": "a", "content": "one" },
                { "type": "tool_result", "tool_use_id": "b", "content": "two" }
            ]
        })];
        let out = openai_messages_from(&input);
        assert_eq!(
            out,
            vec![
                json!({ "role": "tool", "tool_call_id": "a", "content": "one" }),
                json!({ "role": "tool", "tool_call_id": "b", "content": "two" })
            ]
        );
    }

    #[test]
    fn openai_messages_from_serializes_tool_use_input_and_nulls_textless_assistant_content() {
        let input = vec![
            json!({ "role": "assistant", "content": [ { "type": "tool_use", "id": "x", "name": "read_terminal", "input": { "pane_id": "p1" } } ] }),
        ];
        let out = openai_messages_from(&input);
        assert_eq!(out[0]["content"], Value::Null);
        assert_eq!(
            out[0]["tool_calls"][0]["function"]["arguments"],
            json!("{\"pane_id\":\"p1\"}")
        );
    }

    #[test]
    fn openai_messages_from_passes_plain_string_messages_through_untouched() {
        let input = vec![json!({ "role": "user", "content": "hi" })];
        assert_eq!(openai_messages_from(&input), input);
    }

    #[test]
    fn openai_messages_from_treats_an_explicit_null_input_the_same_as_missing() {
        let input = vec![
            json!({ "role": "assistant", "content": [ { "type": "tool_use", "id": "x", "name": "f", "input": null } ] }),
        ];
        let out = openai_messages_from(&input);
        assert_eq!(
            out[0]["tool_calls"][0]["function"]["arguments"],
            json!("{}")
        );
    }

    // ================= build_openai_request_body / header / url helpers —
    // the "request shape" half of test/chat-providers.test.js's streamChat
    // (openai wire) tests, ported as pure-function assertions instead of a
    // mocked-fetch integration test (see this module's doc comment) =================

    #[test]
    fn build_openai_request_body_places_system_first_and_translates_tools() {
        let messages = vec![json!({ "role": "user", "content": "hi" })];
        let tools = tools_fixture();
        let body = build_openai_request_body("kimi-k3", Some("sys"), &messages, &tools);
        assert_eq!(body["stream"], json!(true));
        assert_eq!(
            body["messages"][0],
            json!({ "role": "system", "content": "sys" })
        );
        assert_eq!(
            body["messages"][1],
            json!({ "role": "user", "content": "hi" })
        );
        assert_eq!(body["tools"][0]["type"], json!("function"));
    }

    #[test]
    fn build_openai_request_body_omits_system_when_none() {
        let body = build_openai_request_body("kimi-k3", None, &[], &[]);
        assert!(body.get("system").is_none());
        assert_eq!(body["messages"], json!([]));
    }

    #[test]
    fn bearer_header_formats_the_authorization_value() {
        assert_eq!(bearer_header("sk-test"), "Bearer sk-test");
    }

    #[test]
    fn openai_chat_completions_url_appends_the_fixed_path() {
        assert_eq!(
            openai_chat_completions_url("https://api.moonshot.ai/v1"),
            "https://api.moonshot.ai/v1/chat/completions"
        );
    }

    // ================= OpenAiSseState — ported from
    // test/chat-providers.test.js's `describe('streamChat (openai wire)', ...)` =================

    fn openai_sse_bytes(chunks: &[Value]) -> Vec<u8> {
        let mut body = String::new();
        for c in chunks {
            body.push_str(&format!("data: {c}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");
        body.into_bytes()
    }

    #[test]
    fn openai_sse_streams_text_deltas_and_maps_finish_reason_stop_to_end() {
        let bytes = openai_sse_bytes(&[
            json!({ "choices": [ { "delta": { "role": "assistant", "content": "" } } ] }),
            json!({ "choices": [ { "delta": { "content": "Hel" } } ] }),
            json!({ "choices": [ { "delta": { "content": "lo" } } ] }),
            json!({ "choices": [ { "delta": {}, "finish_reason": "stop" } ] }),
        ]);
        let mut texts = Vec::new();
        let mut on_text = |t: &str| texts.push(t.to_string());
        let mut state = OpenAiSseState::new();
        state.feed(&bytes, &mut on_text);
        let res = state.finish(&mut on_text);
        assert_eq!(texts, vec!["Hel".to_string(), "lo".to_string()]);
        assert_eq!(res.stop_reason, "end");
        assert_eq!(
            res.content,
            vec![json!({ "type": "text", "text": "Hello" })]
        );
    }

    #[test]
    fn openai_sse_accumulates_fragmented_tool_calls_per_index_and_parses_arguments() {
        let bytes = openai_sse_bytes(&[
            json!({ "choices": [ { "delta": { "content": "Checking." } } ] }),
            json!({ "choices": [ { "delta": { "tool_calls": [ { "index": 0, "id": "call_1", "function": { "name": "read_terminal", "arguments": "" } } ] } } ] }),
            json!({ "choices": [ { "delta": { "tool_calls": [ { "index": 0, "function": { "arguments": "{\"pane_" } } ] } } ] }),
            json!({ "choices": [ { "delta": { "tool_calls": [ { "index": 0, "function": { "arguments": "id\":\"p1\"}" } } ] } } ] }),
            json!({ "choices": [ { "delta": { "tool_calls": [ { "index": 1, "id": "call_2", "function": { "name": "list_panes", "arguments": "{}" } } ] } } ] }),
            json!({ "choices": [ { "delta": {}, "finish_reason": "tool_calls" } ] }),
            json!({ "usage": { "prompt_tokens": 11, "completion_tokens": 7 } }),
        ]);
        let mut state = OpenAiSseState::new();
        let mut cb = noop;
        state.feed(&bytes, &mut cb);
        let res = state.finish(&mut cb);
        assert_eq!(res.stop_reason, "tool_use");
        assert_eq!(
            res.content,
            vec![
                json!({ "type": "text", "text": "Checking." }),
                json!({ "type": "tool_use", "id": "call_1", "name": "read_terminal", "input": { "pane_id": "p1" } }),
                json!({ "type": "tool_use", "id": "call_2", "name": "list_panes", "input": {} }),
            ]
        );
        assert_eq!(
            res.usage,
            Usage {
                input: 11,
                output: 7
            }
        );
    }

    #[test]
    fn openai_sse_maps_content_filter_to_refusal() {
        let bytes = openai_sse_bytes(&[
            json!({ "choices": [ { "delta": { "content": "I can" }, "finish_reason": "content_filter" } ] }),
        ]);
        let mut state = OpenAiSseState::new();
        let mut cb = noop;
        state.feed(&bytes, &mut cb);
        let res = state.finish(&mut cb);
        assert_eq!(res.stop_reason, "refusal");
    }

    #[test]
    fn openai_sse_survives_a_chunk_boundary_splitting_an_sse_line() {
        let chunk1 = b"data: {\"choices\":[{\"delta\":{\"content\":\"sp";
        let chunk2 = b"lit\"}}]}\n\ndata: [DONE]\n\n";
        let mut texts = Vec::new();
        let mut on_text = |t: &str| texts.push(t.to_string());
        let mut state = OpenAiSseState::new();
        state.feed(chunk1, &mut on_text);
        state.feed(chunk2, &mut on_text);
        let res = state.finish(&mut on_text);
        assert_eq!(texts, vec!["split".to_string()]);
        assert_eq!(
            res.content,
            vec![json!({ "type": "text", "text": "split" })]
        );
    }

    #[test]
    fn openai_sse_ignores_an_empty_content_delta() {
        // typeof delta.content === 'string' && delta.content — an empty
        // string is present-but-falsy and must contribute nothing.
        let bytes = openai_sse_bytes(&[json!({ "choices": [ { "delta": { "content": "" } } ] })]);
        let mut texts = Vec::new();
        let mut on_text = |t: &str| texts.push(t.to_string());
        let mut state = OpenAiSseState::new();
        state.feed(&bytes, &mut on_text);
        let res = state.finish(&mut on_text);
        assert!(texts.is_empty());
        assert!(res.content.is_empty());
    }

    #[test]
    fn openai_sse_ignores_a_done_line_and_an_unparseable_data_line() {
        let mut bytes = b"data: not json at all\n\n".to_vec();
        bytes.extend_from_slice(b"data: [DONE]\n\n");
        let mut state = OpenAiSseState::new();
        let mut cb = noop;
        state.feed(&bytes, &mut cb);
        let res = state.finish(&mut cb);
        assert_eq!(res.stop_reason, "end");
        assert!(res.content.is_empty());
    }

    // ================= format_http_error — the "throws with status + body
    // snippet on non-2xx" test, ported as a pure-function assertion =================

    #[test]
    fn format_http_error_matches_the_js_message_shape() {
        assert_eq!(
            format_http_error(401, r#"{"error":"bad key"}"#),
            "chat: HTTP 401 — {\"error\":\"bad key\"}"
        );
    }

    #[test]
    fn format_http_error_truncates_a_long_body_to_200_chars() {
        let body = "x".repeat(500);
        assert_eq!(
            format_http_error(500, &body),
            format!("chat: HTTP 500 — {}", "x".repeat(200))
        );
    }

    #[test]
    fn chat_error_status_is_only_present_for_the_http_variant() {
        assert_eq!(
            ChatError::Http {
                status: 401,
                body: String::new()
            }
            .status(),
            Some(401)
        );
        assert_eq!(ChatError::Network("x".to_string()).status(), None);
        assert_eq!(ChatError::Api("x".to_string()).status(), None);
    }

    // ================= AnthropicSseState — this module's own recorded
    // fixtures (no vitest original; the JS suite only ever mocks the SDK).
    // See the module doc comment's "Anthropic SSE event shape" section. =================

    fn anthropic_sse_bytes(events: &[Value]) -> Vec<u8> {
        let mut body = String::new();
        for e in events {
            body.push_str(&format!("data: {e}\n\n"));
        }
        body.into_bytes()
    }

    #[test]
    fn anthropic_sse_streams_text_and_normalizes_end_turn_to_end() {
        let bytes = anthropic_sse_bytes(&[
            json!({ "type": "message_start", "message": { "id": "msg_1", "role": "assistant", "content": [], "usage": { "input_tokens": 10, "output_tokens": 1 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hel" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "lo" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 5 } }),
            json!({ "type": "message_stop" }),
        ]);
        let mut texts = Vec::new();
        let mut on_text = |t: &str| texts.push(t.to_string());
        let mut state = AnthropicSseState::default();
        state.feed(&bytes, &mut on_text);
        let res = state.finish(&mut on_text).expect("no error event");
        assert_eq!(texts, vec!["Hel".to_string(), "lo".to_string()]);
        assert_eq!(res.stop_reason, "end");
        assert_eq!(
            res.content,
            vec![json!({ "type": "text", "text": "Hello" })]
        );
        assert_eq!(
            res.usage,
            Usage {
                input: 10,
                output: 5
            }
        );
    }

    #[test]
    fn anthropic_sse_normalizes_tool_use_stop_reason_and_parses_accumulated_input_json() {
        let bytes = anthropic_sse_bytes(&[
            json!({ "type": "message_start", "message": { "usage": { "input_tokens": 20, "output_tokens": 1 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Checking." } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read_terminal", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"pane_" } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "id\":\"p1\"}" } }),
            json!({ "type": "content_block_stop", "index": 1 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 12 } }),
            json!({ "type": "message_stop" }),
        ]);
        let mut state = AnthropicSseState::default();
        let mut cb = noop;
        state.feed(&bytes, &mut cb);
        let res = state.finish(&mut cb).expect("no error event");
        assert_eq!(res.stop_reason, "tool_use");
        assert_eq!(
            res.content,
            vec![
                json!({ "type": "text", "text": "Checking." }),
                json!({ "type": "tool_use", "id": "toolu_1", "name": "read_terminal", "input": { "pane_id": "p1" } }),
            ]
        );
        assert_eq!(
            res.usage,
            Usage {
                input: 20,
                output: 12
            }
        );
    }

    #[test]
    fn anthropic_sse_maps_refusal_stop_reason_through_unchanged() {
        let bytes = anthropic_sse_bytes(&[
            json!({ "type": "message_start", "message": { "usage": { "input_tokens": 5, "output_tokens": 1 } } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "refusal" }, "usage": { "output_tokens": 3 } }),
        ]);
        let mut state = AnthropicSseState::default();
        let mut cb = noop;
        state.feed(&bytes, &mut cb);
        let res = state.finish(&mut cb).expect("no error event");
        assert_eq!(res.stop_reason, "refusal");
    }

    #[test]
    fn anthropic_sse_maps_every_other_stop_reason_to_end() {
        for reason in ["max_tokens", "stop_sequence", "pause_turn"] {
            let bytes = anthropic_sse_bytes(&[
                json!({ "type": "message_delta", "delta": { "stop_reason": reason }, "usage": { "output_tokens": 1 } }),
            ]);
            let mut state = AnthropicSseState::default();
            let mut cb = noop;
            state.feed(&bytes, &mut cb);
            let res = state.finish(&mut cb).expect("no error event");
            assert_eq!(
                res.stop_reason, "end",
                "stop_reason {reason} should normalize to \"end\""
            );
        }
    }

    #[test]
    fn anthropic_sse_surfaces_an_error_event_as_an_err() {
        let bytes = anthropic_sse_bytes(&[
            json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } }),
        ]);
        let mut state = AnthropicSseState::default();
        let mut cb = noop;
        state.feed(&bytes, &mut cb);
        let err = state
            .finish(&mut cb)
            .expect_err("error event should surface as Err");
        assert_eq!(err.message(), "Overloaded");
        assert_eq!(err.status(), None);
    }

    #[test]
    fn anthropic_sse_ignores_ping_events() {
        let bytes = anthropic_sse_bytes(&[
            json!({ "type": "ping" }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 0 } }),
        ]);
        let mut state = AnthropicSseState::default();
        let mut cb = noop;
        state.feed(&bytes, &mut cb);
        let res = state.finish(&mut cb).expect("no error event");
        assert_eq!(res.stop_reason, "end");
    }

    #[test]
    fn anthropic_sse_survives_a_chunk_boundary_splitting_a_data_line() {
        let full = format!(
            "data: {}\n\ndata: {}\n\n",
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "split" } }),
        );
        let bytes = full.into_bytes();
        let mid = bytes.len() / 2;
        let (part1, part2) = bytes.split_at(mid);
        let mut texts = Vec::new();
        let mut on_text = |t: &str| texts.push(t.to_string());
        let mut state = AnthropicSseState::default();
        state.feed(part1, &mut on_text);
        state.feed(part2, &mut on_text);
        let res = state.finish(&mut on_text).expect("no error event");
        assert_eq!(texts, vec!["split".to_string()]);
        assert_eq!(
            res.content,
            vec![json!({ "type": "text", "text": "split" })]
        );
    }

    // ================= Anthropic request-building helpers =================

    #[test]
    fn build_anthropic_request_body_includes_system_and_fallbacks_when_given() {
        let body = build_anthropic_request_body(
            "claude-opus-5",
            Some("sys"),
            &[],
            &[],
            Some("default"),
            64_000,
        );
        assert_eq!(body["model"], json!("claude-opus-5"));
        assert_eq!(body["max_tokens"], json!(64_000));
        assert_eq!(body["system"], json!("sys"));
        assert_eq!(body["fallbacks"], json!("default"));
    }

    #[test]
    fn build_anthropic_request_body_uses_the_rows_max_tokens() {
        // per-row max_output_tokens replaces the model-blind
        // ANTHROPIC_MAX_TOKENS const (plan §4.1)
        let body = build_anthropic_request_body("m", None, &[], &[], None, 8_192);
        assert_eq!(body["max_tokens"], json!(8_192));
    }

    #[test]
    fn build_anthropic_request_body_omits_system_and_fallbacks_when_absent() {
        let body = build_anthropic_request_body("claude-opus-5", None, &[], &[], None, 64_000);
        assert!(body.get("system").is_none());
        assert!(body.get("fallbacks").is_none());
        assert_eq!(body["tools"], json!([]));
    }

    #[test]
    fn anthropic_beta_header_joins_multiple_betas_and_is_none_when_empty() {
        assert_eq!(
            anthropic_beta_header(&["a".to_string(), "b".to_string()]),
            Some("a,b".to_string())
        );
        assert_eq!(anthropic_beta_header(&[]), None);
    }

    #[test]
    fn anthropic_messages_url_posts_ga_by_default_and_beta_only_when_flagged() {
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com", false),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://router.requesty.ai", false),
            "https://router.requesty.ai/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com", true),
            "https://api.anthropic.com/v1/messages?beta=true"
        );
        // a row's base_url is trailing-slash-trimmed at resolve time, but
        // the URL builder must not double up if one ever sneaks through
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com/", true),
            "https://api.anthropic.com/v1/messages?beta=true"
        );
    }

    #[test]
    fn apply_auth_puts_bearer_in_authorization_and_x_api_key_in_its_own_header() {
        let bearer = apply_auth(
            reqwest::Client::new().post("https://example.com"),
            Auth::Bearer,
            "sk-b",
        );
        let xkey = apply_auth(
            reqwest::Client::new().post("https://example.com"),
            Auth::XApiKey,
            "sk-ant",
        );
        let dump = |r: &reqwest::RequestBuilder| {
            format!("{:?}", r.try_clone().unwrap().build().unwrap().headers())
        };
        let bearer_h = dump(&bearer);
        let xkey_h = dump(&xkey);
        assert!(bearer_h.contains("authorization") && bearer_h.contains("Bearer sk-b"));
        assert!(!bearer_h.to_lowercase().contains("x-api-key"));
        assert!(xkey_h.contains("x-api-key") && xkey_h.contains("sk-ant"));
        assert!(!xkey_h.to_lowercase().contains("authorization"));
    }

    // ================= LIVE probe (ignored — real network) =================
    //
    // Exercises the exact OpenAI-wire tool-call path the conductor drives:
    // `stream_chat` → `stream_openai` → SSE accumulation → normalized
    // `tool_use` blocks. Run manually against a real endpoint:
    //
    //   TOME_PROBE_KEY=sk-… TOME_PROBE_MODEL=glm-5.2 \
    //     cargo test --lib chat::sse::tests::live_openai_wire_tool_call_probe -- --ignored
    //
    // No network in normal test runs — the repo's purity discipline
    // (parallel tests, no ambient I/O) holds; this is the same
    // `#[ignore]`'d-live-proof precedent as the Linux bwrap matrix.
    #[tokio::test]
    #[ignore]
    async fn live_openai_wire_tool_call_probe() {
        use crate::chat::registry::KeyOrigin;
        // Skip (not fail) when the key is absent: CI's `--ignored` sweep
        // reaches every ignored test, and this one is only meaningful when
        // a human opts in with a real key (same discipline as the docker
        // gateway and bwrap-userns skips).
        let Ok(key) = std::env::var("TOME_PROBE_KEY") else {
            eprintln!("SKIP: TOME_PROBE_KEY not set — live probe runs only with a real key");
            return;
        };
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
        let tools = vec![json!({
            "name": "list_panes",
            "description": "List every open pane in the workspace grid.",
            "input_schema": { "type": "object", "properties": {} },
        })];
        let messages = vec![json!({
            "role": "user",
            "content": "Call the list_panes tool now. No text, just the tool call."
        })];
        let client = reqwest::Client::new();
        let resp = stream_chat(
            &client,
            &provider,
            StreamChatArgs {
                system: Some("You are a test probe. You must call list_panes."),
                messages: &messages,
                tools: &tools,
                betas: None,
                fallbacks: None,
            },
            noop,
        )
        .await
        .expect("stream_chat should succeed");
        println!("stop_reason: {}", resp.stop_reason);
        println!("usage: {:?}", resp.usage);
        for b in &resp.content {
            println!("block: {b}");
        }
        let tool_uses: Vec<&Value> = resp
            .content
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .collect();
        assert_eq!(
            resp.stop_reason, "tool_use",
            "probe should end on a tool call"
        );
        assert!(
            !tool_uses.is_empty(),
            "probe should contain a tool_use block"
        );
        let name = tool_uses[0].get("name").and_then(Value::as_str);
        assert_eq!(name, Some("list_panes"), "probe should call list_panes");

        // ---- turn 2: feed the tool result back, exactly as the
        // conductor's run_loop does — assistant tool_use message + one
        // user message of tool_result blocks. The loop is broken iff this
        // second turn errors (a wire-shape refusal would 400 here).
        let tool_id = tool_uses[0].get("id").cloned().unwrap_or(Value::Null);
        let mut msgs2 = messages.clone();
        msgs2.push(json!({ "role": "assistant", "content": resp.content.clone() }));
        msgs2.push(json!({
            "role": "user",
            "content": [ { "type": "tool_result", "tool_use_id": tool_id, "content": "[{\"id\":\"p1\",\"title\":\"opencode\"}]" } ]
        }));
        let resp2 = stream_chat(
            &client,
            &provider,
            StreamChatArgs {
                system: Some(
                    "You are a test probe. After the tool result, answer with one short sentence.",
                ),
                messages: &msgs2,
                tools: &tools,
                betas: None,
                fallbacks: None,
            },
            noop,
        )
        .await
        .expect("second turn should succeed");
        println!("turn2 stop_reason: {}", resp2.stop_reason);
        for b in &resp2.content {
            println!("turn2 block: {b}");
        }
        let text: String = resp2
            .content
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        assert!(
            !text.is_empty(),
            "turn 2 should produce a text answer, got: {resp2:?}"
        );
    }
}
