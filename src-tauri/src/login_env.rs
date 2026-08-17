//! Login-shell environment harvest — moved into the `tome-flow` workspace
//! crate by plan step 2.1 (already tauri-free by design: a cached shell
//! subprocess harvest with no `tauri`/`AppState` reach). Re-exported here
//! at the original path so every existing call site in this crate
//! (`flow_env.rs`, …) keeps compiling unchanged. See
//! `tome_flow::login_env`'s own doc comment for the real module
//! documentation.

pub use tome_flow::login_env::*;
