//! Background flow-run commands: start/cancel/list. Ports
//! `src/main/flow-runner.js` (run.json schema, cancel edges — see the
//! plan's "flow-runner.js 120-579" flag) — `tokio::process` +
//! `.process_group(0)`, `killpg` SIGTERM then SIGKILL after 5s, single
//! writer of run.json.

use crate::ipc::stub_command;

stub_command!(runs_start, "runs:start");
stub_command!(runs_cancel, "runs:cancel");
stub_command!(runs_list, "runs:list");
