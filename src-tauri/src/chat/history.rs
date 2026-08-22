//! Chat history search — the searchable archive behind the assistant's
//! per-conversation logs. Transcripts persist to the JSON store as
//! `chat-log-<chatId>` arrays of `{role, content}` (see the renderer's
//! `chat-history.js`); a workspace startup deliberately starts the pane
//! FRESH (a new chatId), so this module is how the old conversations stay
//! reachable.
//!
//! Pure functions over (text, mtime) pairs so the command shell
//! (`ipc::chat::chat_history_list`) stays thin and this file stays
//! unit-testable with no filesystem. Everything here is read-only and
//! keys are vetted twice: the file scan only reads names matching
//! `chat-log-` + a valid store key shape, and the payload is parsed with
//! the same shape validation `loadHistory` applies on the renderer side.

use serde_json::{json, Value};

/// A summarized, search-matched conversation — the wire shape for
/// `chat:history-list`. `id` is the chat id (the `chat-log-` prefix
/// stripped); `snippet` is the first user message (the question that
/// started it), truncated; `count` the validated message count; `mtimeMs`
/// the log file's last-write time (ordering, newest first).
fn summary_value(id: &str, count: usize, snippet: &str, mtime_ms: u64) -> Value {
    json!({
        "id": id,
        "count": count,
        "snippet": snippet,
        "mtimeMs": mtime_ms,
    })
}

/// Truncates to `max` chars on a char boundary, appending an ellipsis
/// character when it actually cut something.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Validates + summarizes one log's contents. `None` when the payload
/// isn't a non-empty array of user/assistant string messages — an
/// empty or corrupt log is not a conversation worth listing.
pub fn summarize_log(id: &str, payload: &str, mtime_ms: u64) -> Option<Value> {
    let msgs: Vec<Value> = serde_json::from_str(payload).ok()?;
    let mut count = 0usize;
    let mut first_user: Option<String> = None;
    for m in &msgs {
        let role = m.get("role").and_then(Value::as_str)?;
        let content = m.get("content").and_then(Value::as_str)?;
        if content.is_empty() {
            continue;
        }
        if role != "user" && role != "assistant" {
            return None;
        }
        if role == "user" && first_user.is_none() {
            first_user = Some(content.to_string());
        }
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let snippet = first_user.map(|s| truncate(s.trim(), 90)).unwrap_or_else(|| "(no user message)".to_string());
    Some(summary_value(id, count, &snippet, mtime_ms))
}

/// Case-insensitive substring match over one log's user+assistant text.
/// `query` empty/whitespace matches everything (the unfiltered listing).
pub fn log_matches(payload: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    let q = q.to_lowercase();
    payload.to_lowercase().contains(&q)
}

/// Sorts summaries newest-first (mtimeMs desc, id as the tiebreak).
pub fn sort_newest_first(entries: &mut [Value]) {
    entries.sort_by(|a, b| {
        let am = a.get("mtimeMs").and_then(Value::as_u64).unwrap_or(0);
        let bm = b.get("mtimeMs").and_then(Value::as_u64).unwrap_or(0);
        bm.cmp(&am).then_with(|| {
            let ai = a.get("id").and_then(Value::as_str).unwrap_or("");
            let bi = b.get("id").and_then(Value::as_str).unwrap_or("");
            ai.cmp(bi)
        })
    });
}

/// `chat-log-<id>` → `Some(id)` when the WHOLE file name is a valid store
/// key shape (`store_keys::is_key_shape_valid` semantics, ported locally
/// so this module stays free of crate:: paths in tests): lowercase
/// alphanumerics and dashes, starting alphanumeric. Anything else in the
/// directory is not ours and is never read.
pub fn chat_id_of_file_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("chat-log-")?;
    if rest.is_empty() {
        return None;
    }
    let mut chars = rest.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return None,
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return None;
    }
    Some(rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(text: &str) -> String {
        format!(r#"[{{"role":"user","content":"{text}"}},{{"role":"assistant","content":"ok"}}]"#)
    }

    #[test]
    fn summarize_extracts_first_user_message_and_count() {
        let v = summarize_log("chat-7", &log("where does auth live"), 1_700_000_000_000).unwrap();
        assert_eq!(v["id"], json!("chat-7"));
        assert_eq!(v["count"], json!(2));
        assert_eq!(v["snippet"], json!("where does auth live"));
        assert_eq!(v["mtimeMs"], json!(1_700_000_000_000u64));
    }

    #[test]
    fn summarize_truncates_long_snippets_on_a_char_boundary() {
        let long = "x".repeat(120);
        let v = summarize_log("c", &format!(r#"[{{"role":"user","content":"{long}"}}]"#), 1).unwrap();
        let s = v["snippet"].as_str().unwrap();
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 91);
    }

    #[test]
    fn summarize_rejects_empty_corrupt_and_foreign_shapes() {
        assert!(summarize_log("c", "[]", 1).is_none());
        assert!(summarize_log("c", "not json", 1).is_none());
        // a role the renderer would never persist
        assert!(summarize_log("c", r#"[{"role":"system","content":"x"}]"#, 1).is_none());
        // empty content doesn't count; all-empty is not a conversation
        assert!(summarize_log("c", r#"[{"role":"user","content":""}]"#, 1).is_none());
    }

    #[test]
    fn matching_is_case_insensitive_and_blank_matches_all() {
        let payload = r#"[{"role":"user","content":"Fix the Cargo.toml"}]"#;
        assert!(log_matches(payload, "cargo"));
        assert!(log_matches(payload, "  "));
        assert!(!log_matches(payload, "gemfile"));
    }

    #[test]
    fn sorting_is_newest_first_with_a_stable_tiebreak() {
        let mut entries = vec![
            summary_value("a", 1, "old", 100),
            summary_value("c", 1, "newest", 300),
            summary_value("b", 1, "mid", 200),
        ];
        sort_newest_first(&mut entries);
        let ids: Vec<&str> = entries
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["c", "b", "a"]);

        let mut tie = vec![
            summary_value("b", 1, "x", 100),
            summary_value("a", 1, "x", 100),
        ];
        sort_newest_first(&mut tie);
        assert_eq!(tie[0]["id"], json!("a"));
    }

    #[test]
    fn file_names_are_vetted_before_anything_is_read() {
        assert_eq!(chat_id_of_file_name("chat-log-chat-1"), Some("chat-1".to_string()));
        assert_eq!(chat_id_of_file_name("chat-log-voice"), Some("voice".to_string()));
        assert_eq!(chat_id_of_file_name("chat-log-"), None);
        assert_eq!(chat_id_of_file_name("chat-log-A-B"), None);
        assert_eq!(chat_id_of_file_name("chat-log-a..b"), None);
        assert_eq!(chat_id_of_file_name("chat-log-a/b"), None);
        assert_eq!(chat_id_of_file_name("store.json"), None);
        assert_eq!(chat_id_of_file_name("chat-secrets"), None);
    }
}
