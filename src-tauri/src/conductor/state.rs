//! The conductor's live session state — direct translation of
//! `src/main/conductor.js`'s module-level mutable `let`s (`meta`, `scrolls`,
//! `readConsent`, `readRequested`, `panes`, `allowRun`, `agentIds`,
//! `inflight`) into one instance struct, each field behind its own lock.
//!
//! An INSTANCE rather than a set of process-wide `static`s (unlike, say,
//! `ipc::chat`'s old `INFLIGHT` static this module absorbs) for one
//! concrete reason: `cargo test` runs `#[test]`s in parallel by default,
//! and `test/conductor-security.test.js`'s own shape — call
//! `register`/`record`/`runTool` directly against shared module state
//! across many `it()` blocks — would make two tests racing the SAME pane
//! id a real flake, not a hypothetical one, if that state lived in one
//! process-wide singleton the way JS's module system gives it for free
//! (vitest isolates per file, not per `it()`; Rust's test runner isolates
//! neither). Each test instead builds its own [`Conductor::new`], exactly
//! how `pty::Registry`/`airgap::AirgapState`/`flow::Runner` are already
//! tested elsewhere in this crate. Production wires exactly one instance
//! via `AppState.conductor` (see that field's own doc comment).
//!
//! Scrollback rings (`scrolls`) are deliberately a SEPARATE map from any
//! pty-liveness registry, not folded into `pty::Registry`: `forget()`
//! (pty:kill) clears a pane's scrollback, but a NATURAL process exit
//! (`mark_exited`) does not — the user can still ask the assistant to read
//! a dead pane's last output, same as `conductor.js`'s own `scrolls` Map
//! outliving `ptys.delete(id)` on exit. `pty::Registry`'s own map is
//! exactly "is this pane's process still alive", a different lifetime.
//!
//! Production wiring note: `ipc::pty::pty_create`/`pty_kill` call
//! [`Conductor::register`]/[`Conductor::mark_exited`]/[`Conductor::forget`]
//! from the real pty lifecycle (spawn success, the `on_exit` closure, and
//! `pty:kill`, respectively — mirroring `index.js`'s `conductor.register`/
//! `markExited`/`forget` call sites exactly). [`Conductor::record`] is wired
//! too: `pty::Registry::spawn_raw` takes an optional `pty::DataTap`, and
//! `pty_create` installs one — capturing an `Arc<Conductor>` (see
//! `AppState.conductor`) + the pane id — that the output batcher calls on
//! every flushed chunk, alongside the `pty:data` Channel send. That is the
//! Rust equivalent of `index.js`'s `p.onData((data) => {
//! conductor.record(id, data); queuePtyData(id, data) })`. Net effect:
//! `read_terminal`'s consent gate (airgap refusal, the
//! `conductor:readRequest` prompt, `allowRead`) is reachable against a real
//! live pane AND reads back that pane's actual scrollback.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agent_spawn::AGENTS;

/// `conductor.js`'s `SCROLL_CAP` — verbatim.
// Read by `Conductor::record` (the pty data-tap's trim bound), which
// `ipc::pty::pty_create` now wires to the real pty output stream — see the
// module doc comment's "Production wiring note".
const SCROLL_CAP: usize = 200_000;

/// Mirrors one entry of `conductor.js`'s `meta` Map: `{ kind, cwd, airgap,
/// exited }`, set once by [`Conductor::register`] and only ever touched
/// again by [`Conductor::mark_exited`] (flips `exited`) or
/// [`Conductor::forget`] (removes the entry entirely).
#[derive(Debug, Clone)]
pub(crate) struct PaneMeta {
    pub kind: String,
    pub cwd: String,
    pub airgap: bool,
    pub exited: bool,
}

/// The conductor's whole live-session state — see the module doc comment.
/// Every field owns its own lock (matching `pty::Registry`/
/// `airgap::AirgapState`'s own "owns its own interior locking" shape, per
/// `state.rs`'s doc comment on `AppState` fields), so this type is `Sync`
/// and safe to reach from many concurrent Tauri commands without an extra
/// wrapping `Mutex` in `AppState`.
pub struct Conductor {
    meta: Mutex<HashMap<String, PaneMeta>>,
    scrolls: Mutex<HashMap<String, String>>,
    read_consent: Mutex<HashSet<String>>,
    /// Panes already asked about (TOME-009) — one prompt per pane, not one
    /// per `read_terminal` call. Cleared by [`Conductor::forget`], same as
    /// `readRequested.delete(id)` in `conductor.js`.
    read_requested: Mutex<HashSet<String>>,
    /// The renderer's pane snapshot (`panes:sync`) — `conductor.js`'s
    /// `panes` array, folded in here per `ipc::panes::panes_sync`'s own
    /// left-behind note ("whichever slice ports conductor.js for real
    /// should fold this into that module's own state").
    panes: Mutex<Vec<Value>>,
    allow_run: AtomicBool,
    /// `conductor.js`'s `agentIds` — the built-in list until `set_agents`
    /// widens it (`agents:changed`, not yet wired to any caller — see the
    /// module doc comment).
    agent_ids: Mutex<Vec<String>>,
    /// `conductor.js`'s `inflight` Map (`chatId -> AbortController`) —
    /// absorbed from `ipc::chat`'s old module-local `INFLIGHT` static now
    /// that a real owner exists; see `ipc::chat::chat_send`'s doc comment
    /// for the before/after.
    inflight: Mutex<HashMap<String, CancellationToken>>,
}

impl Conductor {
    pub fn new() -> Self {
        Self {
            meta: Mutex::new(HashMap::new()),
            scrolls: Mutex::new(HashMap::new()),
            read_consent: Mutex::new(HashSet::new()),
            read_requested: Mutex::new(HashSet::new()),
            panes: Mutex::new(Vec::new()),
            allow_run: AtomicBool::new(false),
            agent_ids: Mutex::new(AGENTS.iter().map(|s| s.to_string()).collect()),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    // ---- pty lifecycle: register / record / mark_exited / forget ----
    // Mirrors conductor.js's exported functions of the same names
    // (index.js calls these from createPty/p.onData/p.onExit/pty:kill —
    // see the module doc comment on why that wiring isn't in this crate
    // yet).

    /// `register(id, { kind, cwd, airgap })` — `exited` always starts
    /// `false`. Also opens this pane's scrollback ring at `""`, matching
    /// `scrolls.set(id, '')`.
    pub fn register(&self, id: &str, kind: &str, cwd: &str, airgap: bool) {
        self.meta.lock().expect("Conductor.meta lock poisoned").insert(
            id.to_string(),
            PaneMeta { kind: kind.to_string(), cwd: cwd.to_string(), airgap, exited: false },
        );
        self.scrolls.lock().expect("Conductor.scrolls lock poisoned").insert(id.to_string(), String::new());
    }

    /// `record(id, data)` — appends to a REGISTERED pane's scrollback ring
    /// only (a no-op for an unknown id, same as `if (!scrolls.has(id))
    /// return`), trimming to [`SCROLL_CAP`] from the front once exceeded.
    /// Byte-length trimming rather than JS's UTF-16-code-unit `.slice`,
    /// rounded forward to the next UTF-8 char boundary so this can never
    /// panic or split a multi-byte sequence — the same class of accepted
    /// divergence `chat::sse`'s `truncate_chars` doc comment already notes
    /// for this crate's other UTF-16-vs-UTF-8 boundary case.
    // Wired to the real pty output stream via `pty::DataTap`:
    // `ipc::pty::pty_create` installs a tap that calls this on every flushed
    // chunk (the batcher's `flush_buf`), the Rust equivalent of
    // `conductor.js`'s `p.onData = data => conductor.record(id, data)`.
    pub fn record(&self, id: &str, data: &str) {
        let mut scrolls = self.scrolls.lock().expect("Conductor.scrolls lock poisoned");
        let Some(buf) = scrolls.get_mut(id) else { return };
        buf.push_str(data);
        if buf.len() > SCROLL_CAP {
            let mut cut = buf.len() - SCROLL_CAP;
            while cut < buf.len() && !buf.is_char_boundary(cut) {
                cut += 1;
            }
            buf.drain(..cut);
        }
    }

    /// `markExited(id)` — a no-op for an unregistered pane, same as the
    /// JS's `const m = meta.get(id); if (m) m.exited = true`.
    pub fn mark_exited(&self, id: &str) {
        if let Some(m) = self.meta.lock().expect("Conductor.meta lock poisoned").get_mut(id) {
            m.exited = true;
        }
    }

    /// `forget(id)` — drops meta, scrollback, read consent, AND the
    /// one-shot read-request gate (so a reopened pane can re-ask), exactly
    /// the four maps `conductor.js`'s `forget` clears.
    pub fn forget(&self, id: &str) {
        self.meta.lock().expect("Conductor.meta lock poisoned").remove(id);
        self.scrolls.lock().expect("Conductor.scrolls lock poisoned").remove(id);
        self.read_consent.lock().expect("Conductor.read_consent lock poisoned").remove(id);
        self.read_requested.lock().expect("Conductor.read_requested lock poisoned").remove(id);
    }

    // ---- renderer-synced / consent-gate setters (ipc::panes / ipc::conductor) ----

    /// `setPanes(list)` — `Array.isArray(list) ? list : []`.
    pub fn set_panes(&self, list: Value) {
        let items = match list {
            Value::Array(items) => items,
            _ => Vec::new(),
        };
        *self.panes.lock().expect("Conductor.panes lock poisoned") = items;
    }

    /// `setAllowRun(v)` — `conductor:allowRun`.
    pub fn set_allow_run(&self, v: bool) {
        self.allow_run.store(v, Ordering::SeqCst);
    }

    pub fn allow_run(&self) -> bool {
        self.allow_run.load(Ordering::SeqCst)
    }

    /// `setReadConsent(paneId, allowed)` — `conductor:allowRead` (TOME-009).
    /// Grants or revokes; never itself asks — see [`Self::mark_read_requested`].
    pub fn set_read_consent(&self, id: &str, allowed: bool) {
        let mut set = self.read_consent.lock().expect("Conductor.read_consent lock poisoned");
        if allowed {
            set.insert(id.to_string());
        } else {
            set.remove(id);
        }
    }

    /// `setAgents(list)` — widens the built-in kind list `TOOLS`/`SYSTEM`
    /// are built from (`agents:changed`, not yet wired to a caller — see
    /// the module doc comment). Falls back to the built-ins on an empty
    /// list, same as `Array.isArray(list) && list.length ? [...list] :
    /// [...AGENTS]`.
    // Only `super::tests` calls this today — `agents:changed`'s handler
    // (`ipc::agents`, a different slice's file) has no call to this yet;
    // see the module doc comment's "Production wiring note".
    #[allow(dead_code)]
    pub fn set_agents(&self, list: &[String]) {
        let mut ids = self.agent_ids.lock().expect("Conductor.agent_ids lock poisoned");
        *ids = if list.is_empty() { AGENTS.iter().map(|s| s.to_string()).collect() } else { list.to_vec() };
    }

    pub(crate) fn agent_ids(&self) -> Vec<String> {
        self.agent_ids.lock().expect("Conductor.agent_ids lock poisoned").clone()
    }

    // ---- read-only accessors for tools.rs's dispatch ----

    pub(crate) fn panes_snapshot(&self) -> Vec<Value> {
        self.panes.lock().expect("Conductor.panes lock poisoned").clone()
    }

    pub(crate) fn meta_of(&self, id: &str) -> Option<PaneMeta> {
        self.meta.lock().expect("Conductor.meta lock poisoned").get(id).cloned()
    }

    pub(crate) fn scrollback_of(&self, id: &str) -> Option<String> {
        self.scrolls.lock().expect("Conductor.scrolls lock poisoned").get(id).cloned()
    }

    pub(crate) fn has_read_consent(&self, id: &str) -> bool {
        self.read_consent.lock().expect("Conductor.read_consent lock poisoned").contains(id)
    }

    /// Returns `true` the FIRST time this pane is asked about (the caller
    /// should then emit `conductor:readRequest`); `false` on every
    /// subsequent call until [`Self::forget`] clears the gate — mirrors
    /// `if (!readRequested.has(id)) { readRequested.add(id); send(...) }`
    /// collapsed into one atomic check-and-set (`HashSet::insert`'s own
    /// return value) rather than a separate has/add pair.
    pub(crate) fn mark_read_requested(&self, id: &str) -> bool {
        self.read_requested.lock().expect("Conductor.read_requested lock poisoned").insert(id.to_string())
    }

    // ---- chat abort registry (TOME-015) ----

    /// Registers a fresh `CancellationToken` for `id`, returning it —
    /// `inflight.set(id, controller)` plus handing the caller the same
    /// `controller.signal` `runChat` reads throughout its loop.
    pub(crate) fn begin_chat(&self, id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.inflight.lock().expect("Conductor.inflight lock poisoned").insert(id.to_string(), token.clone());
        token
    }

    /// `inflight.delete(id)` — the `finally` half of `runChat`.
    pub(crate) fn end_chat(&self, id: &str) {
        self.inflight.lock().expect("Conductor.inflight lock poisoned").remove(id);
    }

    /// `abortChat(id)` (`chat:abort`) — `inflight.get(id)?.abort()`, a safe
    /// no-op for an unknown/already-finished chat id.
    pub fn abort_chat(&self, id: &str) {
        if let Some(token) = self.inflight.lock().expect("Conductor.inflight lock poisoned").get(id) {
            token.cancel();
        }
    }

    // ---- dynamic tool schema / system prompt ----

    /// `TOOLS`, rebuilt fresh from the current `agent_ids` on every call
    /// rather than cached-then-mutated (`rebuildPrompts`'s job in JS) —
    /// there is nothing to keep in sync since nothing is cached.
    pub fn tools(&self) -> Vec<Value> {
        super::tools::tool_schemas(&self.agent_ids())
    }

    /// `SYSTEM`, same freshness rationale as [`Self::tools`].
    pub fn system_prompt(&self) -> String {
        super::tools::system_prompt_text(&self.agent_ids())
    }

    /// `MENTOR_SYSTEM`, the teaching persona chosen when a `chat:send`
    /// arrives with `verbose: true` — same freshness rationale as
    /// [`Self::system_prompt`]. `gate` forwards the renderer's
    /// mentor-gate toggle: `true` keeps the "write a failing test +
    /// gate_question" instruction, `false` drops it.
    pub fn mentor_system_prompt(&self, gate: bool) -> String {
        super::tools::mentor_prompt_text(&self.agent_ids(), gate)
    }
}

impl Default for Conductor {
    fn default() -> Self {
        Self::new()
    }
}
