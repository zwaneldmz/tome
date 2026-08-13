//! PTY lifecycle. Ports `src/main/index.js`'s `pty:*` handlers (spawn via
//! `portable-pty`, the 4ms/64KB output batcher, explicit kill/reap) plus
//! `src/main/lib/{agent-spawn,agent-env,pty-authority,custom-agents}.js`
//! for spawn vetting. Streamed to the renderer via a Tauri Channel per pane
//! rather than the `pty:data`/`pty:exit` event bus. Phase 2 work.

use crate::ipc::stub_command;

stub_command!(pty_create, "pty:create");
stub_command!(pty_write, "pty:write");
stub_command!(pty_resize, "pty:resize");
stub_command!(pty_kill, "pty:kill");
