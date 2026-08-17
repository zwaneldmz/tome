//! The environment every pty (agent or plain terminal) is built from —
//! moved into the `tome-flow` workspace crate by plan step 2.1 (already
//! tauri-free by design: pure env-map layering, no `tauri`/`AppState`
//! reach). Re-exported here at the original path so every existing call
//! site in this crate (`login_env.rs`, `flow_env.rs`, …) keeps compiling
//! unchanged. See `tome_flow::agent_env`'s own doc comment for the real
//! module documentation.

pub use tome_flow::agent_env::*;
