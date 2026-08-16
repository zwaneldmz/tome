//! Mentor-mode comprehension gates — the backend half of the test-first
//! teaching loop (`gate_question` -> `mentor:check` -> `mentor_answer`).
//!
//! When the assistant is running in mentor mode (`chat_send` with
//! `verbose: true`), its system prompt tells it to write a failing test and
//! then call the `gate_question` tool BEFORE implementing. That tool
//! registers a gate here, emits `mentor:check` to the renderer, and awaits
//! the user's answers. The user's reply arrives via the `mentor_answer`
//! command, which completes the gate and lets the paused tool loop resume.
//!
//! The gate itself is a [`Mentor`] instance living at `AppState.mentor` — a
//! plain value field that owns its own interior locking, the same shape as
//! `conductor::Conductor` / `airgap::AirgapState` (see `state.rs`'s doc
//! comment on subsystem-owned state).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::Value;
use tokio::sync::oneshot;

/// Pending comprehension gates (gate id -> answer sender). Owns its own
/// interior locking, same shape as `conductor::Conductor`/`airgap::AirgapState`.
pub struct Mentor {
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    seq: AtomicU64,
}

impl Mentor {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
        }
    }

    /// Mint a fresh gate id and register a waiter, returning (id, receiver).
    pub fn register(&self) -> (String, oneshot::Receiver<Value>) {
        let id = format!("gate-{}", self.seq.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("Mentor.pending poisoned")
            .insert(id.clone(), tx);
        (id, rx)
    }

    /// Complete a gate with the answer value. Returns true if a waiter existed.
    pub fn answer(&self, id: &str, value: Value) -> bool {
        match self
            .pending
            .lock()
            .expect("Mentor.pending poisoned")
            .remove(id)
        {
            Some(tx) => tx.send(value).is_ok(),
            None => false,
        }
    }
}

impl Default for Mentor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn register_mints_unique_ids() {
        let m = Mentor::new();
        let (a, _) = m.register();
        let (b, _) = m.register();
        assert_ne!(a, b);
        assert!(a.starts_with("gate-"));
    }

    #[tokio::test]
    async fn answer_completes_the_receiver() {
        let m = Mentor::new();
        let (id, rx) = m.register();
        assert!(m.answer(&id, json!("ok")));
        assert_eq!(rx.await.unwrap(), json!("ok"));
        // A completed gate cannot be answered twice.
        assert!(!m.answer(&id, json!("again")));
    }

    #[test]
    fn answer_returns_false_for_unknown_id() {
        let m = Mentor::new();
        assert!(!m.answer("gate-nope", json!("x")));
    }

    #[test]
    fn answer_returns_false_when_already_answered() {
        let m = Mentor::new();
        // A dropped receiver means the gate was abandoned: the send fails,
        // and a second answer finds no pending entry either.
        let (id, _) = m.register();
        assert!(!m.answer(&id, json!("x")));
        assert!(!m.answer(&id, json!("x")));
    }
}
