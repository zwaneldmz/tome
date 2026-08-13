//! Chat send/abort/providers. OpenAI wire ports `streamOpenAI` SSE
//! directly; Anthropic goes through a hand-rolled `/v1/messages` SSE client
//! on `reqwest` rather than the SDK (the plan's one net-new wire code,
//! ~350 LOC + recorded fixtures). Deltas stream via a Channel; abort via
//! `CancellationToken`.

use crate::ipc::stub_command;

stub_command!(chat_send, "chat:send");
stub_command!(chat_abort, "chat:abort");
stub_command!(chat_providers, "chat:providers");
