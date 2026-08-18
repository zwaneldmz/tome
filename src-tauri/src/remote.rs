//! Remote run visibility (plan §Flow products pipeline, phase 3): read-only,
//! consent-gated visibility into ANOTHER machine's `.tome/flows/*/
//! runs-index.json` history over `ssh` — never a live push, never a write.
//! `ipc::remote` is this module's only caller — see that file for the
//! `#[tauri::command]` wrappers (`remote_sources`/`remote_consent`/
//! `remote_revoke`/`remote_runs`/`remote_run_detail`); this module holds the
//! persisted, hash-pinned source records plus the pure argv/parsing/
//! validation pieces those commands thread through, mirroring `export.rs`'s
//! own split exactly (see that module's doc comment for the shape this one
//! repeats: consent-gated records + transports, thin `ipc::` wrapper on top).
//!
//! ## Hash-pinning: self-referential, same as `export::Destination`
//!
//! A [`RemoteSource`] record's `hash` is a self-check, not a TOCTOU race
//! against externally-authored content — see `export.rs`'s module doc
//! comment for the fuller rationale (the record IS the content, freshly
//! typed into `preferences.js`'s "Add remote source" form and hashed the
//! moment [`canonicalize`] builds it). Unlike `Destination` (which has no
//! embedded id — its id is the `HashMap` key), [`RemoteSource`] embeds `id`
//! as a field (matching the container shape `schedule::Schedule` uses, a
//! `Vec<T>` of self-identifying records — see the plan's own pinned JSON
//! shape for `remote-sources.json`), so the id is minted BEFORE
//! [`canonicalize`] is called and IS covered by the hash like every other
//! field except `hash` itself — "identical to export destinations" describes
//! the HASH FORMULA (sha1 of the record's own JSON, `hash` removed), not the
//! container shape, which this module takes from `schedule.rs` instead.
//! [`RemoteSource::verify`] is called before every real use (every
//! `remote_runs`/`remote_run_detail`), matching
//! `export::Destination::verify`'s exact call discipline in
//! `ipc::runs::runs_export`.
//!
//! ## Network access: unrestricted by design, fenced by consent instead
//!
//! Every `ssh` call below ([`run_ssh`]) reaches a host the renderer never
//! named directly — only a `sourceId`, resolved against a record
//! `remote_consent` already hashed and the caller just
//! [`verify`](RemoteSource::verify)'d fresh. See `export.rs`'s module doc
//! comment for why this is safe under the egress (main's own outbound
//! calls have never been confined by it — the egress wraps spawned PANES,
//! not main, via `agent_env::compose_agent_env`'s proxy vars) and gated the
//! same way every other main-initiated network call in this crate is: by
//! what the user already consented to, never by what a flow file or an
//! agent's own output says.
//!
//! ## `find -print -exec cat {} \;` instead of the simpler `-exec cat {} +`
//!
//! [`remote_runs_argv`] asks `find` to print each matched `runs-index.json`
//! path immediately before catting it, rather than batching every match
//! into one plain `cat file1 file2 ...` call. Raw `cat` output alone carries
//! no filename at all — and `remote_run_detail`'s own contract (`flow` is a
//! required, directly-interpolated argument) means the renderer MUST
//! already know which flow a remote run row belongs to before it can ever
//! expand that row for detail. The printed path is what
//! [`parse_remote_runs_blob`] resyncs on to recover that attribution; see
//! its own doc comment for the parse side. Every other part of the pinned
//! ssh invocation (`BatchMode=yes`, `ConnectTimeout=5`, the `--`
//! separator, the 15s/10s timeouts, the front-trimmed 256 KiB output cap,
//! the narrow child-env allowlist) is unchanged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::egress::sha1_hex;
use crate::flow::model::safe_segment;

/// `<app_data_dir>/remote-sources.json` — same directory
/// `export-destinations.json`/`flow-schedules.json` resolve against
/// (`lib.rs`'s `boot_auth_and_egress`).
pub const FILE_NAME: &str = "remote-sources.json";

const REMOTE_RUNS_TIMEOUT: Duration = Duration::from_secs(15);
const REMOTE_DETAIL_TIMEOUT: Duration = Duration::from_secs(10);
/// Applied to raw ssh stdout before any parsing, trimmed from the FRONT
/// (keeping the tail) at a UTF-8 char boundary — same idiom and same
/// direction `conductor::env::run_command_impl`'s `RUN_OUTPUT_CAP` and
/// `export`'s SFTP transport (`TRANSPORT_OUTPUT_CAP`) already use for their
/// own subprocess output caps.
const OUTPUT_CAP: usize = 256 * 1024;

/// The exact allowlist an `ssh` child gets — identical to `export.rs`'s own
/// `SFTP_ENV_ALLOWLIST` (that transport is ALSO ssh-backed under the hood),
/// narrower than `agent_env::AGENT_ENV_ALLOWLIST` (TOME-007 discipline; see
/// that module's doc comment): just enough to resolve the binary, find a
/// home directory for `known_hosts`/ssh config, authenticate via an
/// already-running agent, and respect locale — never provider credentials
/// or terminal-display variables an interactive pty needs.
const REMOTE_ENV_ALLOWLIST: &[&str] = &["HOME", "PATH", "USER", "SSH_AUTH_SOCK", "LANG"];

// ---- persisted record ----

/// One consented remote source. The only constructor is [`canonicalize`] —
/// every live `RemoteSource` is therefore already validated and hashed by
/// construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteSource {
    pub id: String,
    pub label: String,
    /// An ssh alias (from the user's own `~/.ssh/config`) or `user@host` —
    /// never parsed or validated here beyond non-empty: it is handed to
    /// `ssh` verbatim as ITS OWN destination argument (`remote_runs_argv`'s
    /// `host.to_string()`), never interpolated into a shell string, so
    /// there is no injection surface to vet against.
    pub host: String,
    /// Absolute path on the remote host. Always stored WITHOUT a trailing
    /// slash (see [`canonicalize`]) so every downstream consumer
    /// (`remote_runs_argv`, `remote_run_detail_argv`,
    /// `parse_remote_runs_blob`) can concatenate `"{repo_path}/.tome/..."`
    /// without a doubled separator.
    #[serde(rename = "repoPath")]
    pub repo_path: String,
    pub hash: String,
}

impl RemoteSource {
    /// sha1 hex of this record's own JSON shape with `hash` itself omitted
    /// — see the module doc comment. `serde_json::to_value` always visits
    /// the SAME struct fields in the SAME declared order, so this is
    /// self-consistent across calls regardless of `serde_json`'s
    /// `preserve_order` feature; nothing outside this function ever needs
    /// the intermediate JSON to match some independently-authored byte
    /// sequence, only to agree with ITSELF between the write that computed
    /// it and the later read that re-checks it.
    fn compute_hash(&self) -> String {
        let mut value = serde_json::to_value(self).expect("RemoteSource always serializes");
        if let Value::Object(map) = &mut value {
            map.remove("hash");
        }
        sha1_hex(&serde_json::to_string(&value).expect("Value always serializes"))
    }

    /// Re-derives [`RemoteSource::compute_hash`] and compares it against the
    /// stored `hash` — every real use (`ipc::remote::remote_runs`/
    /// `remote_run_detail`) calls this FIRST and refuses on mismatch, never
    /// proceeding with a record that no longer reads as it did when
    /// [`canonicalize`] hashed it. Matches
    /// `export::Destination::verify`'s exact call discipline.
    pub fn verify(&self) -> Result<(), String> {
        if self.compute_hash() != self.hash {
            return Err(
                "remote source record changed unexpectedly — remove and re-add it".to_string(),
            );
        }
        Ok(())
    }
}

/// The persisted file's whole shape: `{"version":1,"sources":[...]}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSources {
    pub version: u32,
    pub sources: Vec<RemoteSource>,
}

impl Default for RemoteSources {
    fn default() -> Self {
        Self {
            version: 1,
            sources: Vec::new(),
        }
    }
}

fn file_path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// Missing file, corrupt JSON, or a shape that doesn't parse as
/// [`RemoteSources`] all collapse to a fresh, empty v1 store — matching
/// every other main-owned file's "unreadable = start fresh" discipline in
/// this crate (`export::load`, `schedule::load`,
/// `egress::EgressState::load_repo_consents`).
pub fn load(dir: &Path) -> RemoteSources {
    let Ok(text) = std::fs::read_to_string(file_path(dir)) else {
        return RemoteSources::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Writes the whole store back, 0600 — same discipline `export::save`/
/// `schedule::save` use.
pub fn save(dir: &Path, data: &RemoteSources) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = file_path(dir);
    let text = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn to_base36(mut n: u128) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).expect("base36 digits are always valid UTF-8")
}

/// `src-<ts36>`, collision-retried against `existing` — the same shape
/// `schedule::new_schedule_id` uses for its own ids (a small, independent
/// duplicate rather than a cross-module reach-in: `new_schedule_id` is
/// private to its module — see `schedule.rs`'s own doc comment on this
/// crate's established duplication-over-reach-in pattern).
pub fn new_source_id(existing: &[RemoteSource]) -> String {
    let base = to_base36(crate::totp::now_ms() as u128);
    let mut id = format!("src-{base}");
    let mut n = 2;
    while existing.iter().any(|s| s.id == id) {
        id = format!("src-{base}-{n}");
        n += 1;
    }
    id
}

/// Validates and canonicalizes one `remote_consent` payload into a hashed,
/// ready-to-persist [`RemoteSource`] — this function is the type's only
/// constructor. `id` is decided by the caller BEFORE this is called
/// (`ipc::remote::remote_consent` mints a fresh one via [`new_source_id`],
/// or reuses an existing id it is updating — the same shape
/// `ipc::schedules::schedules_set` uses for `schedule::Schedule`) since it
/// is part of what gets hashed; see the module doc comment. `remote_consent`
/// never accepts a caller-supplied hash — there is no external content to
/// pin here, only fresh input this function itself just validated.
pub fn canonicalize(
    id: &str,
    label: &str,
    host: &str,
    repo_path: &str,
) -> Result<RemoteSource, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("label is required".to_string());
    }
    let host = host.trim();
    if host.is_empty() {
        return Err("host is required".to_string());
    }
    let repo_path = repo_path.trim().trim_end_matches('/');
    if !repo_path.starts_with('/') {
        return Err("repoPath must be an absolute path on the remote host".to_string());
    }
    let mut record = RemoteSource {
        id: id.to_string(),
        label: label.to_string(),
        host: host.to_string(),
        repo_path: repo_path.to_string(),
        hash: String::new(),
    };
    let hash = record.compute_hash();
    record.hash = hash;
    Ok(record)
}

/// `remote_sources`'s wire shape for one record: everything except the
/// internal integrity `hash` — never meaningful to the renderer, same
/// redaction `export::public_view` applies to a destination's bearer token
/// (there it hides a secret; here there is none, but the hash is still an
/// implementation detail of [`RemoteSource::verify`], not user-facing data).
pub fn public_view(source: &RemoteSource) -> Value {
    json!({
        "id": source.id,
        "label": source.label,
        "host": source.host,
        "repoPath": source.repo_path,
    })
}

// ---- shell quoting ----

/// POSIX single-quote escaping: wraps `s` in `'...'`, replacing each
/// embedded `'` with `'\''` (close the quote, an escaped literal quote,
/// reopen the quote) — the standard technique for handing an arbitrary
/// string to a remote shell as one opaque token regardless of what
/// characters it contains. Applied to every config-sourced segment
/// (`repoPath` always; `flow`/`runId` too, once [`safe_segment`] has
/// already rejected path separators/control characters) interpolated into
/// the ssh remote-command strings below: `safe_segment` guards FILESYSTEM
/// safety (no traversal, no separators), this guards SHELL safety (no
/// metacharacter breakout) — the two checks are independent, and
/// `safe_segment` alone does not forbid a bare `'`, `"`, `$`, or space (see
/// its own doc comment), so both are required.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ---- argv construction (pure — exact arrays) ----

/// The exact argv `remote_runs` invokes: `ssh` to `host`, running a `find`
/// that prints each matched `runs-index.json`'s own path immediately before
/// catting it — see the module doc comment for why `-print` is there at
/// all. `repoPath` is single-quoted into the remote command; `host` is
/// ssh's own destination argument, never interpolated into a string.
pub fn remote_runs_argv(host: &str, repo_path: &str) -> Vec<String> {
    let repo_path = repo_path.trim_end_matches('/');
    vec![
        "ssh".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=5".to_string(),
        "--".to_string(),
        host.to_string(),
        format!(
            "find {}/.tome/flows -maxdepth 2 -name runs-index.json -print -exec cat {{}} \\;",
            shell_single_quote(repo_path)
        ),
    ]
}

/// The exact argv `remote_run_detail` invokes: one ssh `cat` of
/// `run.json` and (if promoted) `manifest.json` — `repoPath`, `flow`, and
/// `runId` are each independently single-quoted into the remote command.
/// `flow`/`runId` must already have passed [`validate_flow_and_run_segment`]
/// before this is called; this function does not re-check them (it is
/// pure string assembly, kept separately testable from that validation).
pub fn remote_run_detail_argv(
    host: &str,
    repo_path: &str,
    flow: &str,
    run_id: &str,
) -> Vec<String> {
    let repo_path = repo_path.trim_end_matches('/');
    let repo_q = shell_single_quote(repo_path);
    let flow_q = shell_single_quote(flow);
    let run_q = shell_single_quote(run_id);
    let remote_cmd = format!(
        "cat {repo_q}/.tome/flows/{flow_q}/runs/{run_q}/run.json {repo_q}/.tome/flows/{flow_q}/out/{run_q}/manifest.json"
    );
    vec![
        "ssh".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=5".to_string(),
        "--".to_string(),
        host.to_string(),
        remote_cmd,
    ]
}

/// `flow`/`runId` become literal filesystem path segments
/// (`.tome/flows/<flow>/runs/<runId>/...`) via direct interpolation
/// (`remote_run_detail_argv`) — the same "no path separator, no bare `..`,
/// no leading `-`, no control character" gate `flow::model::safe_segment`
/// already enforces for a flow's own node ids/handoff names. Called first
/// thing in [`fetch_remote_run_detail`], before any argv is built.
pub fn validate_flow_and_run_segment(flow: &str, run_id: &str) -> Result<(), String> {
    if !safe_segment(flow) {
        return Err(format!("unsafe flow name \"{flow}\""));
    }
    if !safe_segment(run_id) {
        return Err(format!("unsafe run id \"{run_id}\""));
    }
    Ok(())
}

// ---- output cap ----

/// Front-trims `text` to at most [`OUTPUT_CAP`] bytes in place, snapping
/// forward to the next UTF-8 char boundary so the kept tail is always valid
/// — same idiom `conductor::env::run_command_impl`/`export`'s SFTP
/// transport use for their own subprocess output caps.
fn cap_from_front(text: &mut String, cap: usize) {
    if text.len() <= cap {
        return;
    }
    let mut cut = text.len() - cap;
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    text.drain(..cut);
}

// ---- defensive concatenated-JSON parsing ----

/// Splits the raw stdout of [`remote_runs_argv`]'s `find ... -print -exec
/// cat {} \;` into one flattened list of run-summary objects, each tagged
/// with the flow directory name it came from. Raw `cat` output alone never
/// carries a filename — this walks the blob looking for a line that is
/// EXACTLY one of the absolute paths `find -print` would have emitted
/// (`<repo_path>/.tome/flows/<flow>/runs-index.json`), treats the JSON
/// value starting right after it as that flow's `runs-index.json` content,
/// and advances past exactly as many bytes as that ONE value consumed
/// (never the rest of the blob) before looking for the next path line.
///
/// Defensive in two independent directions: a line that doesn't match the
/// expected path shape (most likely a garbled fragment left by the 256 KiB
/// front-trim cap slicing through the middle of one) is skipped rather than
/// trusted as a sync point, and a document that fails to parse (the SAME
/// cap slicing through the middle of the LAST file's JSON) simply stops the
/// walk there — every flow synced before it is kept, nothing after it is
/// guessed at. Never panics and never returns an `Err`: an empty result is
/// indistinguishable from "nothing matched", which is the caller's
/// (`fetch_remote_runs`) problem to report via stderr, not this function's.
pub fn parse_remote_runs_blob(blob: &str, repo_path: &str) -> Vec<Value> {
    let prefix = format!("{}/.tome/flows/", repo_path.trim_end_matches('/'));
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < blob.len() {
        let Some(nl) = blob[pos..].find('\n') else {
            break;
        };
        let line = &blob[pos..pos + nl];
        pos += nl + 1;
        let Some(flow) = line
            .strip_prefix(prefix.as_str())
            .and_then(|rest| rest.strip_suffix("/runs-index.json"))
        else {
            continue; // JSON content, or a garbled leading fragment — not a sync line
        };
        if flow.is_empty() || flow.contains('/') {
            continue; // -maxdepth 2 never legitimately produces this — never trust it
        }
        let mut stream = serde_json::Deserializer::from_str(&blob[pos..]).into_iter::<Value>();
        let Some(Ok(doc)) = stream.next() else {
            break; // truncated or malformed — nothing reliable follows either
        };
        pos += stream.byte_offset();
        let Some(runs) = doc.get("runs").and_then(Value::as_array) else {
            continue;
        };
        for entry in runs {
            let mut tagged = entry.clone();
            if let Value::Object(map) = &mut tagged {
                map.insert("flow".to_string(), Value::String(flow.to_string()));
            }
            out.push(tagged);
        }
    }
    out
}

/// Splits the raw stdout of [`remote_run_detail_argv`]'s `cat run.json
/// manifest.json` into `(run, manifest)`, classified by SHAPE rather than
/// position: a run's own JSON always has a top-level string `status` field;
/// a manifest always has a top-level `products` array (see
/// `flow::products::Manifest`'s fields — this module cannot import that
/// private struct, so it duck-types against the same two keys instead, the
/// same "small enough to duplicate" call `flow::products`'s own
/// `git_exec`/`copy_dir_recursive` make about `git.rs`/`export.rs`). Staying
/// shape-based (rather than assuming "first document is the run, second is
/// the manifest") keeps this correct even when only ONE of the two files
/// exists — a run still in progress has no `manifest.json` at all, since
/// promotion only happens once a run settles `"done"` — in EITHER position
/// `cat` happened to emit it. A trailing document that fails to parse (the
/// front-trim cap cutting through it) is dropped, never surfaced as an
/// error.
fn parse_remote_run_detail_blob(blob: &str) -> (Option<Value>, Option<Value>) {
    let mut run = None;
    let mut manifest = None;
    let mut stream = serde_json::Deserializer::from_str(blob).into_iter::<Value>();
    while let Some(Ok(doc)) = stream.next() {
        if doc.get("products").and_then(Value::as_array).is_some() {
            manifest = Some(doc);
        } else if doc.get("status").and_then(Value::as_str).is_some() {
            run = Some(doc);
        }
        // Any other shape is ignored — never errors the whole call over one
        // unrecognized document.
    }
    (run, manifest)
}

// ---- ssh transport ----

fn remote_child_env(process_env: &HashMap<String, String>) -> HashMap<String, String> {
    process_env
        .iter()
        .filter(|(k, _)| REMOTE_ENV_ALLOWLIST.contains(&k.as_str()) || k.starts_with("LC_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Runs `argv` (always `ssh ...`, built by [`remote_runs_argv`]/
/// [`remote_run_detail_argv`]) with a hard timeout and the narrow child-env
/// allowlist above — `kill_on_drop(true)` is REQUIRED for the timeout path
/// to actually kill the child rather than orphan it, matching `git.rs`'s
/// own `git()` helper and `export.rs`'s `upload_sftp` exactly.
///
/// MAIN-PROCESS NETWORK ACCESS IS UNRESTRICTED BY DESIGN — the egress only
/// ever wraps SPAWNED PANES (`agent_env::compose_agent_env`'s proxy vars),
/// never main's own outbound calls (see the module doc comment). This is
/// acceptable here ONLY because `host`/`repo_path` (baked into `argv` by
/// the caller) come exclusively from a `remote_consent`-vetted, freshly
/// [`RemoteSource::verify`]'d record — never a flow file or agent output.
///
/// Unlike `git()` (which gates its `Result` on `output.status.success()`),
/// this hands back raw `(stdout, stderr)` regardless of exit status: `cat`
/// exits nonzero the moment ANY one of its file arguments is unreadable but
/// still writes whatever it COULD read to stdout first (a run still in
/// progress legitimately has no `manifest.json` yet — see
/// `parse_remote_run_detail_blob`'s doc comment) — deciding whether a
/// partial result is usable is [`fetch_remote_runs`]/
/// [`fetch_remote_run_detail`]'s job, not this function's.
async fn run_ssh(argv: &[String], timeout: Duration) -> Result<(String, String), String> {
    let process_env: HashMap<String, String> = std::env::vars().collect();
    let env = remote_child_env(&process_env);

    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env_clear()
        .envs(&env)
        .kill_on_drop(true);

    let output = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("{} timed out", argv[0]))?
        .map_err(|e| e.to_string())?;

    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// `ipc::remote::remote_runs`'s backend: fetch, cap, and defensively parse
/// every `runs-index.json` under `repo_path/.tome/flows/*`. An empty result
/// with non-empty stderr (host unreachable, auth refused, `find`/`cat`
/// missing on the remote) is reported as `Err` so the renderer can show
/// something more useful than a silent empty list; an empty result with
/// empty stderr (a real repo with no runs yet, or no flows at all) is a
/// legitimate `Ok(vec![])` — never an error.
pub async fn fetch_remote_runs(host: &str, repo_path: &str) -> Result<Vec<Value>, String> {
    let argv = remote_runs_argv(host, repo_path);
    let (mut stdout, stderr) = run_ssh(&argv, REMOTE_RUNS_TIMEOUT).await?;
    cap_from_front(&mut stdout, OUTPUT_CAP);
    let runs = parse_remote_runs_blob(&stdout, repo_path);
    if runs.is_empty() && !stderr.trim().is_empty() {
        return Err(stderr.trim().to_string());
    }
    Ok(runs)
}

/// `ipc::remote::remote_run_detail`'s backend: validate `flow`/`run_id`,
/// fetch, cap, and defensively parse one run's `run.json` (+ `manifest.json`
/// if promoted). `Err` only when NEITHER file could be read at all — a
/// present run with no manifest yet is a normal `Ok` with `manifest: null`.
pub async fn fetch_remote_run_detail(
    host: &str,
    repo_path: &str,
    flow: &str,
    run_id: &str,
) -> Result<Value, String> {
    validate_flow_and_run_segment(flow, run_id)?;
    let argv = remote_run_detail_argv(host, repo_path, flow, run_id);
    let (mut stdout, stderr) = run_ssh(&argv, REMOTE_DETAIL_TIMEOUT).await?;
    cap_from_front(&mut stdout, OUTPUT_CAP);
    let (run, manifest) = parse_remote_run_detail_blob(&stdout);
    let Some(run) = run else {
        let msg = stderr.trim();
        return Err(if msg.is_empty() {
            "run not found on the remote host".to_string()
        } else {
            msg.to_string()
        });
    };
    Ok(json!({ "run": run, "manifest": manifest }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- hash / verify round-trip ----

    #[test]
    fn canonicalize_hashes_a_record_and_verify_accepts_it() {
        let src = canonicalize("src-1", "  Staging box  ", " build@ci ", "/srv/repo/").unwrap();
        assert_eq!(src.label, "Staging box");
        assert_eq!(src.host, "build@ci");
        assert_eq!(src.repo_path, "/srv/repo"); // trailing slash trimmed
        assert!(src.verify().is_ok());
    }

    #[test]
    fn canonicalize_rejects_an_empty_label_or_host() {
        assert!(canonicalize("src-1", "  ", "host", "/srv/repo").is_err());
        assert!(canonicalize("src-1", "label", "  ", "/srv/repo").is_err());
    }

    #[test]
    fn canonicalize_rejects_a_relative_repo_path() {
        let err = canonicalize("src-1", "label", "host", "srv/repo").unwrap_err();
        assert!(err.contains("absolute"));
    }

    #[test]
    fn verify_rejects_a_record_whose_content_changed_after_hashing() {
        let mut src = canonicalize("src-1", "Prod", "build@ci", "/srv/repo").unwrap();
        assert!(src.verify().is_ok());
        // Simulate remote-sources.json edited by something other than
        // remote_consent: the field changes, the hash does not — verify()
        // must catch it, the same way export::Destination::verify does.
        src.host = "evil@host".to_string();
        let err = src.verify().unwrap_err();
        assert!(err.contains("changed"));
    }

    #[test]
    fn verify_rejects_a_hand_edited_hash() {
        let mut src = canonicalize("src-1", "Prod", "build@ci", "/srv/repo").unwrap();
        src.hash = "0000000000000000000000000000000000000000".to_string();
        assert!(src.verify().is_err());
    }

    #[test]
    fn canonicalize_covers_the_id_in_the_hash() {
        // Two records differing ONLY by id must hash differently — the
        // module doc comment's "id is covered by the hash like every other
        // field" claim, checked directly.
        let a = canonicalize("src-1", "Prod", "build@ci", "/srv/repo").unwrap();
        let b = canonicalize("src-2", "Prod", "build@ci", "/srv/repo").unwrap();
        assert_ne!(a.hash, b.hash);
    }

    // ---- public_view redaction ----

    #[test]
    fn public_view_omits_the_hash() {
        let src = canonicalize("src-1", "Prod", "build@ci", "/srv/repo").unwrap();
        let view = public_view(&src);
        assert!(view.get("hash").is_none());
        assert_eq!(view["id"], json!("src-1"));
        assert_eq!(view["repoPath"], json!("/srv/repo"));
    }

    // ---- new_source_id ----

    #[test]
    fn new_source_id_has_the_src_prefix_and_avoids_a_forced_collision() {
        let id = new_source_id(&[]);
        assert!(id.starts_with("src-"));
        let placeholder = canonicalize(&id, "x", "h", "/r").unwrap();
        let existing = vec![placeholder];
        assert_ne!(new_source_id(&existing), id);
    }

    // ---- load / save round trip ----

    #[test]
    fn save_then_load_round_trips_and_writes_0600() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RemoteSources::default();
        let src = canonicalize("src-1", "Prod", "build@ci", "/srv/repo").unwrap();
        store.sources.push(src.clone());
        save(dir.path(), &store).unwrap();

        let loaded = load(dir.path());
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.sources, vec![src]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(file_path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn load_of_a_missing_file_is_an_empty_v1_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = load(dir.path());
        assert_eq!(store.version, 1);
        assert!(store.sources.is_empty());
    }

    #[test]
    fn load_of_corrupt_json_is_an_empty_v1_store_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(file_path(dir.path()), b"{not json").unwrap();
        let store = load(dir.path());
        assert_eq!(store.version, 1);
        assert!(store.sources.is_empty());
    }

    // ---- shell_single_quote (pure) ----

    #[test]
    fn shell_single_quote_wraps_an_ordinary_string() {
        assert_eq!(shell_single_quote("/srv/repo"), "'/srv/repo'");
    }

    #[test]
    fn shell_single_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_single_quote("o'brien"), r"'o'\''brien'");
    }

    // ---- argv construction (pure — exact arrays) ----

    #[test]
    fn remote_runs_argv_matches_the_pinned_shape() {
        assert_eq!(
            remote_runs_argv("build@ci", "/srv/repo"),
            vec![
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "--",
                "build@ci",
                "find '/srv/repo'/.tome/flows -maxdepth 2 -name runs-index.json -print -exec cat {} \\;",
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn remote_runs_argv_trims_a_trailing_slash_off_repo_path() {
        let argv = remote_runs_argv("build@ci", "/srv/repo/");
        assert!(argv
            .last()
            .unwrap()
            .starts_with("find '/srv/repo'/.tome/flows"));
    }

    #[test]
    fn remote_runs_argv_single_quotes_a_repo_path_containing_a_single_quote() {
        let argv = remote_runs_argv("build@ci", "/srv/o'brien");
        assert!(argv.last().unwrap().contains(r"/srv/o'\''brien"));
    }

    #[test]
    fn remote_run_detail_argv_matches_the_pinned_shape() {
        assert_eq!(
            remote_run_detail_argv("build@ci", "/srv/repo", "release-notes", "abc123"),
            vec![
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "--",
                "build@ci",
                "cat '/srv/repo'/.tome/flows/'release-notes'/runs/'abc123'/run.json \
                 '/srv/repo'/.tome/flows/'release-notes'/out/'abc123'/manifest.json",
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn remote_run_detail_argv_single_quotes_flow_and_run_id_independently() {
        // safe_segment allows a bare space or `$` (it only forbids `/`, `\`,
        // `:`, control characters, and a leading `-` — see its own doc
        // comment) — this is the module doc comment's "both checks are
        // independent" claim, exercised directly: a value safe_segment
        // would accept must still come out shell-quoted.
        let argv = remote_run_detail_argv("h", "/r", "a b", "$run");
        let cmd = argv.last().unwrap();
        assert!(cmd.contains("'a b'"));
        assert!(cmd.contains("'$run'"));
    }

    // ---- validate_flow_and_run_segment (safe-segment rejection) ----

    #[test]
    fn validate_flow_and_run_segment_accepts_ordinary_names() {
        assert!(validate_flow_and_run_segment("release-notes", "abc123").is_ok());
    }

    #[test]
    fn validate_flow_and_run_segment_rejects_traversal_and_separators() {
        for (flow, run) in [
            ("..", "abc"),
            ("a/b", "abc"),
            ("release-notes", "../etc"),
            ("release-notes", "a/b"),
            ("a:b", "abc"),
            ("", "abc"),
            ("-rf", "abc"),
        ] {
            assert!(
                validate_flow_and_run_segment(flow, run).is_err(),
                "({flow:?}, {run:?}) should have been rejected"
            );
        }
    }

    // ---- remote_child_env allowlist (pure) ----

    #[test]
    fn remote_child_env_keeps_only_the_narrow_allowlist_plus_lc_prefix() {
        let mut env = HashMap::new();
        for (k, v) in [
            ("HOME", "/h"),
            ("PATH", "/bin"),
            ("USER", "u"),
            ("SSH_AUTH_SOCK", "/tmp/sock"),
            ("LANG", "en_US.UTF-8"),
            ("LC_ALL", "en_US.UTF-8"),
            ("SHELL", "/bin/zsh"),
            ("ANTHROPIC_API_KEY", "sk-ant-placeholder-must-not-leak"),
        ] {
            env.insert(k.to_string(), v.to_string());
        }
        let result = remote_child_env(&env);
        for key in ["HOME", "PATH", "USER", "SSH_AUTH_SOCK", "LANG", "LC_ALL"] {
            assert!(result.contains_key(key), "{key} should survive");
        }
        for key in ["SHELL", "ANTHROPIC_API_KEY"] {
            assert!(
                !result.contains_key(key),
                "{key} must not leak into the ssh child"
            );
        }
    }

    // ---- cap_from_front (pure) ----

    #[test]
    fn cap_from_front_is_a_no_op_under_the_cap() {
        let mut s = "hello".to_string();
        cap_from_front(&mut s, 10);
        assert_eq!(s, "hello");
    }

    #[test]
    fn cap_from_front_keeps_the_tail_and_snaps_to_a_char_boundary() {
        // "é" is 2 bytes (U+00E9) — landing the cut mid-character must snap
        // forward rather than produce an invalid split.
        let mut s = "a".repeat(9) + "é" + "bb"; // 9 + 2 + 2 = 13 bytes
        cap_from_front(&mut s, 4); // a raw byte-cut here would land inside 'é'
        assert!(s.is_char_boundary(0));
        assert!(s.ends_with("bb"));
    }

    // ---- parse_remote_runs_blob (defensive concatenated-JSON parsing) ----

    fn runs_index_doc(ids: &[&str]) -> String {
        let runs: Vec<Value> = ids
            .iter()
            .map(|id| json!({"id": id, "status": "done", "started": "2026-01-01T00:00:00.000Z", "ended": null, "products": [], "manifest": format!("out/{id}/manifest.json")}))
            .collect();
        serde_json::to_string(&json!({"version": 1, "runs": runs})).unwrap()
    }

    #[test]
    fn parse_remote_runs_blob_flattens_multiple_flows_and_tags_each_entry() {
        let repo = "/srv/repo";
        let blob = format!(
            "{repo}/.tome/flows/alpha/runs-index.json\n{}{repo}/.tome/flows/beta/runs-index.json\n{}",
            runs_index_doc(&["r1"]),
            runs_index_doc(&["r2", "r3"]),
        );
        let entries = parse_remote_runs_blob(&blob, repo);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["flow"], json!("alpha"));
        assert_eq!(entries[0]["id"], json!("r1"));
        assert_eq!(entries[1]["flow"], json!("beta"));
        assert_eq!(entries[2]["flow"], json!("beta"));
    }

    #[test]
    fn parse_remote_runs_blob_drops_a_truncated_trailing_document() {
        let repo = "/srv/repo";
        let good = runs_index_doc(&["r1"]);
        let truncated = &runs_index_doc(&["r2"])[..10]; // cut mid-object
        let blob = format!(
            "{repo}/.tome/flows/alpha/runs-index.json\n{good}{repo}/.tome/flows/beta/runs-index.json\n{truncated}"
        );
        let entries = parse_remote_runs_blob(&blob, repo);
        // The first, complete flow survives; the truncated second one
        // contributes nothing — and this must not panic.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["flow"], json!("alpha"));
    }

    #[test]
    fn parse_remote_runs_blob_skips_a_garbled_leading_fragment() {
        let repo = "/srv/repo";
        let real_line = format!("{repo}/.tome/flows/alpha/runs-index.json");
        // Simulate the 256 KiB front-trim cap having sliced through the
        // middle of an earlier line — the leading fragment doesn't match
        // the expected path shape at all.
        let blob = format!(
            "some-garbled-fragment\n{real_line}\n{}",
            runs_index_doc(&["r1"])
        );
        let entries = parse_remote_runs_blob(&blob, repo);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], json!("r1"));
    }

    #[test]
    fn parse_remote_runs_blob_is_empty_for_a_blob_with_no_matches() {
        assert!(parse_remote_runs_blob("", "/srv/repo").is_empty());
        assert!(parse_remote_runs_blob("no matches here\n", "/srv/repo").is_empty());
    }

    // ---- parse_remote_run_detail_blob (shape-based, order-independent) ----

    fn run_json(id: &str) -> String {
        serde_json::to_string(&json!({
            "id": id, "flow": "demo", "status": "done", "started": "2026-01-01T00:00:00.000Z",
            "ended": "2026-01-01T00:00:02.000Z", "nodes": [], "layers": [], "egress": true,
            "canceling": false, "dir": "/x", "flowPath": "/x.flow.json", "root": "/x",
        }))
        .unwrap()
    }

    fn manifest_json() -> String {
        serde_json::to_string(&json!({
            "version": 1, "flow": {"name": "demo", "path": "x", "sha256": "abc"},
            "run": {"id": "r1", "started": "s", "ended": "e", "egress": true},
            "git": {"head": null, "dirty": false}, "nodes": [], "products": [],
        }))
        .unwrap()
    }

    #[test]
    fn parse_remote_run_detail_blob_splits_run_and_manifest_by_shape() {
        let blob = format!("{}{}", run_json("r1"), manifest_json());
        let (run, manifest) = parse_remote_run_detail_blob(&blob);
        assert_eq!(run.unwrap()["id"], json!("r1"));
        assert_eq!(manifest.unwrap()["version"], json!(1));
    }

    #[test]
    fn parse_remote_run_detail_blob_handles_a_missing_manifest() {
        // A run still in progress: promotion has not happened yet, so `cat`
        // only ever produced run.json's content — never mistaken for a
        // manifest, since it has no top-level `products` array.
        let (run, manifest) = parse_remote_run_detail_blob(&run_json("r1"));
        assert!(run.is_some());
        assert!(manifest.is_none());
    }

    #[test]
    fn parse_remote_run_detail_blob_handles_a_missing_run_json() {
        // The inverse: run.json unreadable (bad runId) but a stray
        // manifest.json somehow readable — still never misattributed.
        let (run, manifest) = parse_remote_run_detail_blob(&manifest_json());
        assert!(run.is_none());
        assert!(manifest.is_some());
    }

    #[test]
    fn parse_remote_run_detail_blob_drops_a_truncated_trailing_document() {
        let run = run_json("r1");
        let truncated = &manifest_json()[..8];
        let blob = format!("{run}{truncated}");
        let (parsed_run, manifest) = parse_remote_run_detail_blob(&blob);
        assert!(parsed_run.is_some());
        assert!(manifest.is_none());
    }

    #[test]
    fn parse_remote_run_detail_blob_of_empty_input_is_both_none() {
        let (run, manifest) = parse_remote_run_detail_blob("");
        assert!(run.is_none());
        assert!(manifest.is_none());
    }
}
