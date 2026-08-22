//! Chat client: the data-driven provider registry ([`registry`] + its
//! impure shell [`overlay`]), the one-blob key vault ([`vault`]), and both
//! wire dialects' SSE streaming and shape translation ([`sse`]). Ports
//! `src/main/lib/chat-client.js` (279 LOC) + `src/shared/chat-providers.js`
//! (43 LOC) — see each submodule's own doc comment for the exact mapping.
//! The old compile-time `providers.rs` table was deleted by the registry
//! rework (docs/PLAN-chat-provider-registry-rework.md); migration from
//! Electron-era provider state lands in `migrate` (slice 3b).
//!
//! `ipc::chat` is this module's only production caller. It intentionally
//! stops at a single [`sse::stream_chat`] call per `chat:send` — the
//! multi-turn tool-execution loop (`conductor.js`'s `runChat`: system
//! prompt, `TOOLS`, `runTool`, the `chat:tool` event, the 8-turn/token
//! budget) is a DIFFERENT phase's work (phase 5b — conductor depends on
//! this module, per the rewrite plan's phase split) and lives outside this
//! directory entirely. See `ipc::chat`'s own doc comment for the exact
//! scope boundary this implies for `chat:send`/`chat:done`/`chat:tool`.

pub mod migrate;
pub mod overlay;
pub mod registry;
pub mod sse;
pub mod vault;
