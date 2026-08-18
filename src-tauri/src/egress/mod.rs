//! Egress subsystem (Phase 3) — the pane-gapping state machine, repo-
//! allowlist consent bookkeeping, host-matching compiler, per-pane loopback
//! proxy, Linux sandbox argv assembly, and macOS seatbelt profile builder.
//! Plan step 2.1 extracted this entire tree (this file plus [`allowlist`],
//! [`proxy`], [`linux`], [`seatbelt`]) into the `tome-flow` workspace crate
//! — it was already tauri-free by design (no `tauri::AppHandle`, no
//! `tauri::State`, nothing async in the pure state machine below; see each
//! submodule's own doc comment for its half of that discipline). Re-
//! exported here at the original path so every existing call site in this
//! crate (`ipc::egress`, `ipc::pty`, `export.rs`, `schedule.rs`'s
//! `sha1_hex` use, …) keeps compiling unchanged.
//!
//! `ipc::egress::create_gapped_pane_proxy`/`close_pane_and_proxy` are NOT
//! part of this move — they are `pub(crate)` functions defined in
//! `ipc/egress.rs` itself (the Tauri-facing integrator this module's own
//! doc comment, ported below unchanged, calls "Task A4"), not in this
//! module, and stay exactly where they were.

pub use tome_flow::egress::*;
