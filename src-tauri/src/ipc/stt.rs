//! Speech-to-text commands. Ports `src/main/lib/stt.js`: exec
//! `whisper-cli` (candidates, `TOME_WHISPER_BIN`, 60s timeout for
//! transcribe), plus the warmup run and the no-spawn status check.

use crate::ipc::stub_command;

stub_command!(stt_transcribe, "stt:transcribe");
stub_command!(stt_warmup, "stt:warmup");
stub_command!(stt_status, "stt:status");
