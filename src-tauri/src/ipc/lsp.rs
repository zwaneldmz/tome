//! Language server commands: document sync (open/change/close, fire-and-
//! forget) plus hover/definition requests. Ports `src/main/lsp.js`'s
//! `Server` class as-is: `tokio::process`, hand-rolled Content-Length
//! framing, untyped `serde_json::Value` (skip `lsp-types`), same 7 servers,
//! one `lsp:missing` push per absent binary.

use crate::ipc::stub_command;

stub_command!(lsp_did_open, "lsp:didOpen");
stub_command!(lsp_did_change, "lsp:didChange");
stub_command!(lsp_did_close, "lsp:didClose");
stub_command!(lsp_hover, "lsp:hover");
stub_command!(lsp_definition, "lsp:definition");
