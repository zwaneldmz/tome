//! Conductor consent toggles: allow-run and per-pane allow-read. Ports
//! `src/main/conductor.js`'s consent state machine (see the plan's flag to
//! read that file in full before implementing) — `conductor:allowRun`/
//! `conductor:allowRead` handlers in `src/main/index.js`.

use crate::ipc::stub_command;

stub_command!(conductor_allow_run, "conductor:allowRun");
stub_command!(conductor_allow_read, "conductor:allowRead");
