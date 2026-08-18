//! Usage review report: aggregates LOCAL usage signals into a counts-only
//! JSON summary ([`build_summary`]) and sends that summary to the user's
//! configured chat provider for a one-shot, non-streaming markdown report
//! ([`generate_report`]). Nothing here phones home on its own: the only
//! network call is the single LLM request to whatever provider chat already
//! resolves (same ceremony as `ipc::chat::chat_send`), and the payload is
//! derived purely from on-disk data (`events.jsonl`, `chat-log-*.json` file
//! count, the skills catalog) plus live session state (`AppState.open_folders`
//! git status, the `AppState.flow` run registry).
//!
//! ## Counts only, never contents
//!
//! The summary deliberately never includes the BODY of any event, chat
//! transcript, skill, or file — just tallies and per-repo git counts. Two
//! reasons: (1) the event log and chat transcripts may carry secrets
//! (`events.rs`'s own doc comment makes the same distinction — kinds +
//! identifiers only), so a review report that echoed their contents would be
//! a new exfiltration surface the moment the provider endpoint is anything
//! but the user's own; (2) counts are enough to ground the report's
//! suggestions (how often each tool was run, how many flows, which skills
//! exist), and anything richer belongs in the chat pane where the user has
//! already decided to send it.
//!
//! ## Provider resolution — shared with chat
//!
//! [`generate_report`] delegates provider resolution to
//! `ipc::chat::resolve_chat` (the same path `chat_send` and `mentor_judge`
//! use), so built-ins, the custom provider, DeepSeek, Requesty, and the env
//! override all resolve identically for a report. The only structural
//! difference from chat is the one-shot shape: `tools: &[]` and an `on_text`
//! closure that accumulates deltas into a `String`, then returns that string
//! rather than streaming it.
//!
//! ## Testing boundary
//!
//! [`build_summary`] is `AppHandle`-driven (filesystem + state), so its pure
//! parts are factored out as [`tally_events`] (parse + count kinds) and
//! [`count_chat_logs`] (read_dir filter) and tested directly with tempdirs —
//! the same "testable core + thin impure wrapper" split `events.rs`/
//! `skills.rs` use. [`generate_report`] performs a real network call and is
//! left untested, same as `sse::stream_openai`/`stream_anthropic`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// Parses `events.jsonl` text and tallies each record's `kind` — the
/// counts-only core of [`build_summary`]'s events section. Records with no
/// `kind` (or that fail to parse) are skipped by `eventlog::parse_events`
/// and contribute nothing, same as every other read of the log.
fn tally_events(text: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for rec in crate::eventlog::parse_events(text) {
        if let Some(kind) = rec.get("kind").and_then(Value::as_str) {
            *counts.entry(kind.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

/// Counts entries in `dir` whose file name starts with `chat-log-` and ends
/// with `.json` — the persistent transcript files `chat` writes. A missing
/// directory (or any `read_dir` failure) is `0`, matching the "missing is a
/// normal first-run state, not an error" convention everywhere else in this
/// crate.
fn count_chat_logs(dir: &Path) -> usize {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.starts_with("chat-log-") && name.ends_with(".json")
            })
            .count(),
        Err(_) => 0,
    }
}

/// Builds the counts-only usage summary sent to the provider. Everything
/// here is local and cheap: a read of `events.jsonl`, a directory listing
/// for the chat-transcript count, one `git status`-shaped call per open
/// folder (only for those that are actually repos), the in-memory flow-run
/// count, and the skills catalog length.
pub async fn build_summary(app: &AppHandle) -> Value {
    let state = app.state::<AppState>();
    let app_data = app.path().app_data_dir().ok();

    // events: tally kinds from the persistent events.jsonl.
    let (events, events_total) = match app_data.as_deref() {
        Some(dir) => {
            let text = std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
            let counts = tally_events(&text);
            let total: usize = counts.values().sum();
            (counts, total)
        }
        None => (HashMap::new(), 0),
    };
    let events_json = serde_json::to_value(&events).unwrap_or_else(|_| json!({}));

    // chat transcripts: count the persistent chat-log-*.json files.
    let chat_transcripts = app_data.as_deref().map(count_chat_logs).unwrap_or(0);

    // repos: git status per open folder, only for folders that are repos.
    let folders: Vec<PathBuf> = state
        .open_folders
        .read()
        .expect("AppState.open_folders lock poisoned")
        .clone();
    let mut repos = Vec::new();
    for folder in folders {
        let info = crate::git::info(folder.to_string_lossy().as_ref()).await;
        if info.get("repo").and_then(Value::as_bool) == Some(true) {
            repos.push(json!({
                "dir": folder.to_string_lossy().into_owned(),
                "branch": info.get("branch").cloned().unwrap_or(Value::Null),
                "ahead": info.get("ahead").cloned().unwrap_or(Value::Null),
                "behind": info.get("behind").cloned().unwrap_or(Value::Null),
                "added": info.get("added").cloned().unwrap_or(Value::Null),
                "modified": info.get("modified").cloned().unwrap_or(Value::Null),
            }));
        }
    }

    // runs: the current in-memory flow-run count.
    let flow_runs = crate::flow::runner::snapshot_all(&state.flow)
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);

    // skills: number of skills in the catalog (0 when no root resolves).
    let skills = crate::skills::default_root(app)
        .map(|root| crate::skills::list(&root).len())
        .unwrap_or(0);

    json!({
        "events": events_json,
        "events_total": events_total,
        "chat_transcripts": chat_transcripts,
        "repos": repos,
        "flow_runs": flow_runs,
        "skills": skills,
    })
}

/// Generates the review report: builds the summary, resolves the chat
/// provider (mirroring `ipc::chat::chat_send`), and sends a one-shot
/// request whose text deltas are accumulated into a single `String` returned
/// to the caller. `Err` carries a user-facing message for a missing key or
/// a provider/network failure (`ChatError::message()`).
pub async fn generate_report(app: &AppHandle) -> Result<String, String> {
    let summary = build_summary(app).await;

    // Same provider resolution + betas/fallbacks as chat_send, shared via
    // ipc::chat::resolve_chat (also picks up the custom provider and DeepSeek).
    let state = app.state::<AppState>();
    let (provider, betas, fallbacks) = crate::ipc::chat::resolve_chat(app, &state).await?;

    let system = "You are Tome's usage reviewer. Write a concise Markdown report with three sections: (1) Skill improvements — which skills to adopt or sharpen; (2) Usage tips — underused Tome features to try; (3) Novel ideas — new ways to combine the user's observed workflow. Ground every suggestion in the provided usage summary; be specific and practical, not generic.";

    let user_content = format!(
        "Usage summary:\n```json\n{}\n```",
        serde_json::to_string(&summary).map_err(|e| e.to_string())?
    );
    let messages = vec![json!({ "role": "user", "content": user_content })];

    let mut text = String::new();
    let result = crate::chat::sse::stream_chat(
        crate::ipc::chat::http_client(),
        &provider,
        crate::chat::sse::StreamChatArgs {
            system: Some(system),
            messages: &messages,
            tools: &[],
            betas: betas.as_deref(),
            fallbacks: fallbacks.as_deref(),
        },
        |t| text.push_str(t),
    )
    .await;

    result.map(|_| text).map_err(|e| e.message())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- tally_events ----

    #[test]
    fn tally_events_counts_kinds_and_skips_records_without_one() {
        let text = concat!(
            "{\"ts\":\"t1\",\"kind\":\"conductor:tool\"}\n",
            "{\"ts\":\"t2\",\"kind\":\"egress:unlock\"}\n",
            "{\"ts\":\"t3\",\"kind\":\"conductor:tool\"}\n",
            "{\"ts\":\"t4\",\"note\":\"no kind\"}\n"
        );
        let counts = tally_events(text);
        assert_eq!(counts.get("conductor:tool"), Some(&2));
        assert_eq!(counts.get("egress:unlock"), Some(&1));
        assert_eq!(counts.len(), 2);
    }

    #[test]
    fn tally_events_of_empty_text_is_empty() {
        assert!(tally_events("").is_empty());
    }

    #[test]
    fn tally_events_skips_malformed_lines() {
        let text = "{\"ts\":\"t1\",\"kind\":\"k\"}\n{\"truncated";
        let counts = tally_events(text);
        assert_eq!(counts.get("k"), Some(&1));
        assert_eq!(counts.len(), 1);
    }

    // ---- count_chat_logs ----

    #[test]
    fn count_chat_logs_counts_only_chat_log_json_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("chat-log-abc.json"), "{}").unwrap();
        std::fs::write(dir.path().join("chat-log-def.json"), "{}").unwrap();
        std::fs::write(dir.path().join("events.jsonl"), "").unwrap();
        std::fs::write(dir.path().join("chat-log-nope.txt"), "").unwrap();
        std::fs::write(dir.path().join("store.json"), "").unwrap();
        assert_eq!(count_chat_logs(dir.path()), 2);
    }

    #[test]
    fn count_chat_logs_of_missing_dir_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(count_chat_logs(&dir.path().join("absent")), 0);
    }
}
