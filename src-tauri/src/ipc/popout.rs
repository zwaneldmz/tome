//! Popout window close acknowledgement. Electron holds a popped-out window
//! open until the renderer calls this; the plan's "one genuine regression"
//! note applies here — v1 ships with `window.tome.popout.supported = false`
//! (a renderer-side flag, not this command), real `WebviewWindow`-backed
//! popouts are Phase 6. This command still needs to exist now so the wire
//! surface is complete and the renderer shim can no-op against it.

use crate::ipc::stub_command;

stub_command!(popout_close, "popout:close");
