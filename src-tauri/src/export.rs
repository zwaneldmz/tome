//! Export destinations: consent-gated targets a finished background flow
//! run's promoted products can be copied to (plan §Flow products pipeline,
//! steps 1.5-1.6). Two concerns live here — the persisted, hash-pinned
//! destination records (`export-destinations.json`, main-owned 0600,
//! RESERVED in `store_keys::RESERVED_KEYS` so `store:set` can never forge
//! one — see that module's doc comment) and the three transports that copy a
//! run's products TO one (local folder copy, HTTP PUT/POST, `scp`/`rsync`
//! over SSH). `ipc::export` is this module's only caller for the destination
//! CRUD half — see that file for the `#[tauri::command]` wrappers
//! (`export_destinations`/`export_consent`/`export_revoke`) and
//! `ipc::runs::runs_export` (a different domain file, same slice) for the
//! run-resolution half that calls [`run_transport`]/[`copy_to_local`].
//!
//! ## Hash-pinning: a self-check, not a TOCTOU race against external content
//!
//! `airgap::mod.rs`'s repo-allowlist consent (`consent_repo_allowlist`) pins
//! a hash against content SOMEONE ELSE authored (a repo's committed
//! `.tome/airgap.json`), so a post-consent edit re-prompts — see that
//! function's own doc comment. There is no external author here: a
//! [`Destination`] record IS the content, freshly typed into the Add
//! destination form (`preferences.js`'s `buildExportSection`) and hashed the
//! moment [`canonicalize`] builds it. So `hash` here is a narrower,
//! self-referential integrity check — "does this record still read exactly
//! as it did when `export_consent` wrote it" — that catches a
//! `export-destinations.json` mutated by anything OTHER than
//! `export_consent` (a bug elsewhere in this crate, a manual disk edit) at
//! the next real use ([`Destination::verify`]), rather than silently
//! trusting whatever is on disk. `RESERVED_KEYS` already makes an
//! unauthenticated renderer write to this exact file impossible via
//! `store:set`; this is the belt to that suspenders, the same
//! layered-defense shape `store_keys.rs`'s own doc comment describes for
//! `airgap-repo-consents`.
//!
//! ## Network access: unrestricted by design, fenced by consent instead
//!
//! Every transport below calls out, from the MAIN process, to a host/target
//! the renderer never named directly — only a `destinationId` (resolved
//! against a record `export_consent` already vetted and hashed) or a
//! `localPath` (a native-picker-driven folder, never a bare string the
//! renderer typed). This is safe under the air gap because the air gap only
//! ever wraps SPAWNED PANES (`agent_env::compose_agent_env`'s proxy vars) —
//! main's own outbound calls have never been confined by it (see
//! `airgap::proxy`'s module doc comment: the loopback proxy exists FOR
//! panes) — so exporting is exactly as privileged as any other
//! main-initiated network call (`chat_send`, `git_push`) and is gated the
//! same way: by what the user already consented to, never by what a flow
//! file or an agent's own output says.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::airgap::sha1_hex;

/// `<app_data_dir>/export-destinations.json` — same directory
/// `airgap-repo-consents.json` resolves against (`lib.rs`'s
/// `boot_auth_and_airgap`).
pub const FILE_NAME: &str = "export-destinations.json";

/// One consented copy target. `#[serde(tag = "kind")]` matches the wire
/// shape this slice's plan pins exactly: `{"kind":"http", ...}` /
/// `{"kind":"sftp", ...}`. The only constructor is [`canonicalize`] — every
/// live `Destination` is therefore already validated and hashed by
/// construction; nothing here re-validates `method`/`tool` a second time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Destination {
    Http {
        label: String,
        url: String,
        /// Always `"PUT"` or `"POST"` — see the enum's own doc comment.
        method: String,
        #[serde(
            rename = "authBearer",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        auth_bearer: Option<String>,
        hash: String,
    },
    Sftp {
        label: String,
        target: String,
        /// Always `"scp"` or `"rsync"` — see the enum's own doc comment.
        tool: String,
        hash: String,
    },
}

impl Destination {
    pub fn label(&self) -> &str {
        match self {
            Destination::Http { label, .. } | Destination::Sftp { label, .. } => label,
        }
    }

    fn stored_hash(&self) -> &str {
        match self {
            Destination::Http { hash, .. } | Destination::Sftp { hash, .. } => hash,
        }
    }

    fn set_hash(&mut self, new_hash: String) {
        match self {
            Destination::Http { hash, .. } | Destination::Sftp { hash, .. } => *hash = new_hash,
        }
    }

    /// sha1 hex of this record's own JSON shape with `hash` itself omitted
    /// — see the module doc comment. `serde_json::to_value` always visits
    /// the SAME struct fields in the SAME declared order, so this is
    /// self-consistent across calls regardless of `serde_json`'s
    /// `preserve_order` feature; nothing outside this function ever needs
    /// the intermediate JSON to match some independently-authored byte
    /// sequence, only to agree with ITSELF between the write that computed
    /// it and the later read that re-checks it.
    fn compute_hash(&self) -> String {
        let mut value = serde_json::to_value(self).expect("Destination always serializes");
        if let Value::Object(map) = &mut value {
            map.remove("hash");
        }
        sha1_hex(&serde_json::to_string(&value).expect("Value always serializes"))
    }

    /// Re-derives [`Destination::compute_hash`] and compares it against the
    /// stored `hash` — every real use (an actual transport call; see
    /// `ipc::runs::runs_export`) must call this FIRST and refuse on
    /// mismatch, never proceed with a record that no longer reads as it did
    /// when [`canonicalize`] hashed it.
    pub fn verify(&self) -> Result<(), String> {
        if self.compute_hash() != self.stored_hash() {
            return Err(
                "export destination record changed unexpectedly — remove and re-add it".to_string(),
            );
        }
        Ok(())
    }
}

/// The persisted file's whole shape: `{"version":1,"destinations":{"<id>":
/// ...}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Destinations {
    pub version: u32,
    pub destinations: HashMap<String, Destination>,
}

impl Default for Destinations {
    fn default() -> Self {
        Self {
            version: 1,
            destinations: HashMap::new(),
        }
    }
}

fn file_path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// Missing file, corrupt JSON, or a shape that doesn't parse as
/// [`Destinations`] all collapse to a fresh, empty v1 store — matching every
/// other main-owned file's "unreadable = start fresh" discipline in this
/// crate (`airgap::AirgapState::load_repo_consents`, `store::get`).
pub fn load(dir: &Path) -> Destinations {
    let Ok(text) = std::fs::read_to_string(file_path(dir)) else {
        return Destinations::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Writes the whole store back, 0600 — same discipline
/// `airgap::AirgapState::save_repo_consents` uses for its own persisted
/// consent map (see that function's doc comment).
pub fn save(dir: &Path, data: &Destinations) -> Result<(), String> {
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

/// Mints a fresh id for a new destination — collision-retried, the same
/// shape `flow::runner`'s own `new_run_id` uses (that function is private
/// to its module, so this is a small, independent copy rather than a
/// cross-module reach-in). Not itself security-sensitive: an id is only
/// ever a map key here, never trusted content.
pub fn new_destination_id(existing: &HashMap<String, Destination>) -> String {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = sha1_hex(&now_nanos.to_string())[..10].to_string();
    let mut id = base.clone();
    let mut n = 2;
    while existing.contains_key(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// Validates and canonicalizes one `export_consent` payload into a hashed,
/// ready-to-persist [`Destination`] — this function is the type's only
/// constructor. `export_consent` never accepts a caller-supplied hash
/// (unlike `airgap::consent_repo_allowlist`'s `presented_hash` — see the
/// module doc comment for why: there is no external content to pin here,
/// only fresh input this function itself just validated).
pub fn canonicalize(
    kind: &str,
    label: &str,
    url: Option<&str>,
    method: Option<&str>,
    auth_bearer: Option<&str>,
    target: Option<&str>,
    tool: Option<&str>,
) -> Result<Destination, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("label is required".to_string());
    }
    let mut record = match kind {
        "http" => {
            let url = url
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "url is required".to_string())?;
            let method = method
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_ascii_uppercase)
                .unwrap_or_else(|| "PUT".to_string());
            if method != "PUT" && method != "POST" {
                return Err("method must be PUT or POST".to_string());
            }
            let auth_bearer = auth_bearer
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            Destination::Http {
                label: label.to_string(),
                url: url.trim_end_matches('/').to_string(),
                method,
                auth_bearer,
                hash: String::new(),
            }
        }
        "sftp" => {
            let target = target
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "target is required".to_string())?;
            let tool = tool
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_ascii_lowercase)
                .unwrap_or_else(|| "scp".to_string());
            if tool != "scp" && tool != "rsync" {
                return Err("tool must be scp or rsync".to_string());
            }
            Destination::Sftp {
                label: label.to_string(),
                target: target.to_string(),
                tool,
                hash: String::new(),
            }
        }
        other => return Err(format!("unknown destination kind \"{other}\"")),
    };
    let hash = record.compute_hash();
    record.set_hash(hash);
    Ok(record)
}

/// `export_destinations`'s wire shape for one record: everything EXCEPT the
/// bearer token itself, which collapses to a presence boolean — the same
/// "prove it's set, never show it" discipline `preferences.js`'s Assistant
/// section already applies to provider API keys (`p.keySet`, never the key
/// itself).
pub fn public_view(id: &str, dest: &Destination) -> Value {
    match dest {
        Destination::Http {
            label,
            url,
            method,
            auth_bearer,
            ..
        } => json!({
            "id": id,
            "kind": "http",
            "label": label,
            "url": url,
            "method": method,
            "authBearer": auth_bearer.is_some(),
        }),
        Destination::Sftp {
            label,
            target,
            tool,
            ..
        } => json!({
            "id": id,
            "kind": "sftp",
            "label": label,
            "target": target,
            "tool": tool,
        }),
    }
}

// ---- transports ----

/// `runs_export`'s only entry point into this module's transport half —
/// dispatches on the destination's own kind. `source_dir` is the caller's
/// already-confirmed `<root>/.tome/flows/<flow>/out/<runId>/` (see
/// `ipc::runs::runs_export`'s doc comment); `run_id` is threaded through
/// separately only because the HTTP transport's URL shape needs it
/// ([`join_export_url`]) — the SFTP transport below ignores it, it just
/// copies `source_dir`'s own contents.
pub async fn run_transport(
    dest: &Destination,
    source_dir: &Path,
    run_id: &str,
) -> Result<(), String> {
    match dest {
        Destination::Http {
            url,
            method,
            auth_bearer,
            ..
        } => upload_http(source_dir, run_id, url, method, auth_bearer.as_deref()).await,
        Destination::Sftp { target, tool, .. } => upload_sftp(tool, source_dir, target).await,
    }
}

/// Copies `source_dir`'s whole tree into `dest_dir` (created if missing) —
/// `runs_export`'s `localPath` branch. A user-driven write to a folder the
/// user picked themselves (`tome.pickFolder`, a native OS dialog) —
/// unconfined by design, mirroring `ipc::fs`'s own rationale for
/// `fs_write_file` (`fs.rs`'s module doc comment: "unvetted by design",
/// direct traffic the user drove, not a model- or flow-driven path that
/// needs `crate::confine`).
pub fn copy_to_local(source_dir: &Path, dest_dir: &Path) -> std::io::Result<()> {
    copy_dir_recursive(source_dir, dest_dir)
}

fn copy_dir_recursive(source_dir: &Path, dest_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let dest_path = dest_dir.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

// ---- HTTP transport ----

/// Hardened once per process — the same three settings `airgap::proxy`'s
/// `ProxyState::new` builds its own client with (`.no_proxy()`/`.redirect(
/// Policy::none())`/`.connect_timeout`), plus an overall request timeout.
/// Unlike that client — which exists to fence what a GAPPED PANE can reach
/// — this one's hardening is just good hygiene for a background upload; see
/// the module doc comment for why main's own outbound calls are
/// unrestricted by design regardless. Deliberately NOT `ipc::chat::
/// http_client()` (that one sets neither a connect nor an overall timeout —
/// fine for a user-watched, abortable chat stream, wrong for an unattended
/// background export that must not hang forever on a dead destination).
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client builds from static, always-valid config")
    })
}

/// `{url-trailing-slash-trimmed}/{runId}/{filename}` — pure so the exact
/// join is unit-testable without a real request.
pub fn join_export_url(base_url: &str, run_id: &str, filename: &str) -> String {
    format!("{}/{run_id}/{filename}", base_url.trim_end_matches('/'))
}

/// Sorts `names` alphabetically except `manifest.json`, which always sorts
/// LAST — the HTTP transport's commit-marker ordering (module doc comment),
/// factored out as a pure function so the rule is unit-testable with no
/// filesystem involved.
fn manifest_last_order(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    if let Some(pos) = names.iter().position(|n| n == "manifest.json") {
        let manifest = names.remove(pos);
        names.push(manifest);
    }
    names
}

/// Every file under `source_dir`, recursively, as `(relative POSIX path,
/// absolute path)` pairs, ordered by [`manifest_last_order`].
fn collect_export_files(source_dir: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    fn walk(base: &Path, dir: &Path, out: &mut HashMap<String, PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(base, &path, out)?;
            } else if entry.file_type()?.is_file() {
                let rel = path.strip_prefix(base).unwrap_or(&path);
                let rel_str = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.insert(rel_str, path);
            }
        }
        Ok(())
    }
    let mut by_name = HashMap::new();
    walk(source_dir, source_dir, &mut by_name)?;
    let ordered = manifest_last_order(by_name.keys().cloned().collect());
    Ok(ordered
        .into_iter()
        .map(|name| {
            let path = by_name
                .remove(&name)
                .expect("name came from this same map's own keys");
            (name, path)
        })
        .collect())
}

/// One `{method}` request per file, raw bytes body, an optional
/// `Authorization: Bearer` header — `manifest.json` (if present at
/// `source_dir`'s own root) sent LAST as the commit marker. See the module
/// doc comment for why main's own outbound network access is unrestricted
/// by design; `url` reaching this function only ever came from a consented
/// [`Destination`] (already [`Destination::verify`]'d by the caller), never
/// a flow file or agent output.
async fn upload_http(
    source_dir: &Path,
    run_id: &str,
    url: &str,
    method: &str,
    auth_bearer: Option<&str>,
) -> Result<(), String> {
    let walk_dir = source_dir.to_path_buf();
    let files = tokio::task::spawn_blocking(move || collect_export_files(&walk_dir))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if files.is_empty() {
        return Err(format!("no files to export in {}", source_dir.display()));
    }
    let client = http_client();
    for (rel_path, abs_path) in files {
        let bytes = tokio::fs::read(&abs_path)
            .await
            .map_err(|e| e.to_string())?;
        let target = join_export_url(url, run_id, &rel_path);
        let mut req = if method == "PUT" {
            client.put(&target)
        } else {
            client.post(&target)
        };
        if let Some(token) = auth_bearer {
            req = req.bearer_auth(token);
        }
        let resp = req.body(bytes).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("{target} responded {}", resp.status()));
        }
    }
    Ok(())
}

// ---- SFTP transport ----

const TRANSPORT_TIMEOUT: Duration = Duration::from_secs(120);
const TRANSPORT_OUTPUT_CAP: usize = 50_000;

/// The exact allowlist an `scp`/`rsync` child gets — narrower than
/// `agent_env::AGENT_ENV_ALLOWLIST` (TOME-007 discipline; see that module's
/// doc comment): just enough to resolve the binary, find a home directory
/// for `known_hosts`/ssh config, authenticate via an already-running agent,
/// and respect locale — never provider credentials or terminal-display
/// variables an interactive pty needs, none of which this child could use
/// anyway.
const SFTP_ENV_ALLOWLIST: &[&str] = &["HOME", "PATH", "USER", "SSH_AUTH_SOCK", "LANG"];

fn sftp_child_env(process_env: &HashMap<String, String>) -> HashMap<String, String> {
    process_env
        .iter()
        .filter(|(k, _)| SFTP_ENV_ALLOWLIST.contains(&k.as_str()) || k.starts_with("LC_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Pure: the exact argv `scp`/`rsync` is invoked with. `--` terminates
/// option parsing before `source_dir`/`target`, so neither is ever misread
/// as a flag no matter what it starts with.
pub fn sftp_argv(tool: &str, source_dir: &str, target: &str) -> Vec<String> {
    let mut argv: Vec<String> = if tool == "rsync" {
        vec![
            "rsync".to_string(),
            "-a".to_string(),
            "-e".to_string(),
            "ssh -o BatchMode=yes".to_string(),
            "--".to_string(),
        ]
    } else {
        vec![
            "scp".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-r".to_string(),
            "--".to_string(),
        ]
    };
    argv.push(source_dir.to_string());
    argv.push(target.to_string());
    argv
}

/// Copies `source_dir` to `target` via `scp -r` or `rsync -a` over SSH —
/// `kill_on_drop(true)` plus a hard timeout matches `git.rs`'s `git()`
/// helper exactly (see that function's doc comment for why this is
/// required for parity, not just tidiness); stderr, trimmed and
/// front-capped, is preferred over a generic message on failure, the same
/// idiom `conductor::env`'s `run_command_impl` uses for its own output cap.
/// `env_clear()` + `envs(&env)` matches `lsp::spawn_child`'s own idiom for
/// the same TOME-007 reason: `Command`'s default is to inherit the full
/// parent environment, so this crate must clear first or a future
/// narrowing of [`sftp_child_env`] would silently leak the unnarrowed vars
/// back in via inheritance. See the module doc comment for why main's own
/// outbound network/subprocess access is unrestricted by design; `target`/
/// `tool` reaching this function only ever came from a consented
/// [`Destination`], never a flow file or agent output.
async fn upload_sftp(tool: &str, source_dir: &Path, target: &str) -> Result<(), String> {
    let argv = sftp_argv(tool, &source_dir.to_string_lossy(), target);
    let process_env: HashMap<String, String> = std::env::vars().collect();
    let env = sftp_child_env(&process_env);

    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env_clear()
        .envs(&env)
        .kill_on_drop(true);

    let output = tokio::time::timeout(TRANSPORT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("{} timed out", argv[0]))?
        .map_err(|e| e.to_string())?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut trimmed = stderr.trim().to_string();
    if trimmed.len() > TRANSPORT_OUTPUT_CAP {
        let mut cut = trimmed.len() - TRANSPORT_OUTPUT_CAP;
        while cut < trimmed.len() && !trimmed.is_char_boundary(cut) {
            cut += 1;
        }
        trimmed.drain(..cut);
    }
    if trimmed.is_empty() {
        Err(format!("{} exited with {}", argv[0], output.status))
    } else {
        Err(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- hash / verify round-trip ----

    #[test]
    fn canonicalize_hashes_an_http_record_and_verify_accepts_it() {
        let dest = canonicalize(
            "http",
            "  Staging  ",
            Some(" https://example.com/uploads/ "),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(dest.label(), "Staging");
        assert!(dest.verify().is_ok());
        match &dest {
            Destination::Http {
                url,
                method,
                auth_bearer,
                ..
            } => {
                assert_eq!(url, "https://example.com/uploads");
                assert_eq!(method, "PUT"); // default
                assert_eq!(*auth_bearer, None);
            }
            _ => panic!("expected Http"),
        }
    }

    #[test]
    fn canonicalize_hashes_an_sftp_record_and_verify_accepts_it() {
        let dest = canonicalize(
            "sftp",
            "Backup host",
            None,
            None,
            None,
            Some("user@host:/data"),
            Some("RSYNC"),
        )
        .unwrap();
        assert!(dest.verify().is_ok());
        match &dest {
            Destination::Sftp { tool, target, .. } => {
                assert_eq!(tool, "rsync"); // lowercased
                assert_eq!(target, "user@host:/data");
            }
            _ => panic!("expected Sftp"),
        }
    }

    #[test]
    fn compute_hash_round_trips_through_json_serialization() {
        let dest = canonicalize(
            "http",
            "Prod",
            Some("https://example.com"),
            Some("post"),
            None,
            None,
            None,
        )
        .unwrap();
        let text = serde_json::to_string(&dest).unwrap();
        let back: Destination = serde_json::from_str(&text).unwrap();
        assert_eq!(dest, back);
        assert!(back.verify().is_ok());
    }

    #[test]
    fn canonicalize_rejects_an_empty_label() {
        assert!(canonicalize(
            "http",
            "  ",
            Some("https://example.com"),
            None,
            None,
            None,
            None
        )
        .is_err());
    }

    #[test]
    fn canonicalize_rejects_an_unknown_kind() {
        let err = canonicalize("ftp", "x", None, None, None, None, None).unwrap_err();
        assert!(err.contains("ftp"));
    }

    #[test]
    fn canonicalize_rejects_a_bad_http_method() {
        let err = canonicalize(
            "http",
            "x",
            Some("https://example.com"),
            Some("DELETE"),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("PUT or POST"));
    }

    #[test]
    fn canonicalize_rejects_a_bad_sftp_tool() {
        let err =
            canonicalize("sftp", "x", None, None, None, Some("h:/p"), Some("ftp")).unwrap_err();
        assert!(err.contains("scp or rsync"));
    }

    // ---- TOCTOU-style mismatch rejection ----

    #[test]
    fn verify_rejects_a_record_whose_content_changed_after_hashing() {
        let mut dest = canonicalize(
            "http",
            "Prod",
            Some("https://example.com/api"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(dest.verify().is_ok());
        // Simulate `export-destinations.json` edited by something other than
        // export_consent: the field changes, the hash does not — verify()
        // must catch it, the same way `consent_repo_allowlist`'s TOCTOU
        // re-hash catches a repo file edited after it was last read.
        if let Destination::Http { url, .. } = &mut dest {
            *url = "https://evil.example.com/api".to_string();
        }
        let err = dest.verify().unwrap_err();
        assert!(err.contains("changed"));
    }

    #[test]
    fn verify_rejects_a_hand_edited_hash_too() {
        let mut dest = canonicalize(
            "sftp",
            "Backup",
            None,
            None,
            None,
            Some("user@host:/data"),
            None,
        )
        .unwrap();
        dest.set_hash("0000000000000000000000000000000000000000".to_string());
        assert!(dest.verify().is_err());
    }

    // ---- load / save round trip ----

    #[test]
    fn save_then_load_round_trips_and_writes_0600() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Destinations::default();
        let dest = canonicalize(
            "http",
            "Prod",
            Some("https://example.com"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        store.destinations.insert("d1".to_string(), dest.clone());
        save(dir.path(), &store).unwrap();

        let loaded = load(dir.path());
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.destinations.get("d1"), Some(&dest));

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
        assert!(store.destinations.is_empty());
    }

    #[test]
    fn load_of_corrupt_json_is_an_empty_v1_store_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(file_path(dir.path()), b"{not json").unwrap();
        let store = load(dir.path());
        assert_eq!(store.version, 1);
        assert!(store.destinations.is_empty());
    }

    #[test]
    fn new_destination_id_avoids_a_forced_collision() {
        let mut existing = HashMap::new();
        let placeholder =
            canonicalize("http", "x", Some("https://e.com"), None, None, None, None).unwrap();
        // Force a collision deterministically by pre-seeding the exact id a
        // fresh call would mint next, rather than trusting two real-clock
        // calls a nanosecond apart to actually collide.
        let forced = new_destination_id(&existing);
        existing.insert(forced.clone(), placeholder);
        assert_ne!(new_destination_id(&existing), forced);
    }

    // ---- authBearer redaction in list ----

    #[test]
    fn public_view_reports_auth_bearer_presence_never_the_value() {
        let dest = canonicalize(
            "http",
            "Prod",
            Some("https://example.com"),
            Some("POST"),
            Some("super-secret-token"),
            None,
            None,
        )
        .unwrap();
        let view = public_view("d1", &dest);
        assert_eq!(view["authBearer"], json!(true));
        assert!(!view.to_string().contains("super-secret-token"));
    }

    #[test]
    fn public_view_reports_false_when_no_auth_bearer_was_set() {
        let dest = canonicalize(
            "http",
            "Prod",
            Some("https://example.com"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let view = public_view("d1", &dest);
        assert_eq!(view["authBearer"], json!(false));
    }

    #[test]
    fn public_view_carries_sftp_fields_and_no_bearer_key_at_all() {
        let dest = canonicalize(
            "sftp",
            "Backup",
            None,
            None,
            None,
            Some("user@host:/data"),
            Some("scp"),
        )
        .unwrap();
        let view = public_view("d1", &dest);
        assert_eq!(view["kind"], json!("sftp"));
        assert_eq!(view["target"], json!("user@host:/data"));
        assert!(view.get("authBearer").is_none());
    }

    // ---- url join + trailing-slash trim ----

    #[test]
    fn join_export_url_trims_any_number_of_trailing_slashes() {
        assert_eq!(
            join_export_url("https://example.com/api", "run1", "manifest.json"),
            "https://example.com/api/run1/manifest.json"
        );
        assert_eq!(
            join_export_url("https://example.com/api/", "run1", "manifest.json"),
            "https://example.com/api/run1/manifest.json"
        );
        assert_eq!(
            join_export_url("https://example.com/api///", "run1", "out.md"),
            "https://example.com/api/run1/out.md"
        );
    }

    // ---- manifest-last ordering (pure) ----

    #[test]
    fn manifest_last_order_sorts_alphabetically_but_defers_manifest_json() {
        let names = vec!["manifest.json", "b.txt", "a.txt"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            manifest_last_order(names),
            vec!["a.txt", "b.txt", "manifest.json"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn manifest_last_order_is_a_no_op_without_a_manifest_file() {
        let names = vec!["b.txt", "a.txt"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            manifest_last_order(names),
            vec!["a.txt", "b.txt"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    // ---- argv construction (pure — exact arrays) ----

    #[test]
    fn sftp_argv_scp_matches_the_pinned_shape() {
        assert_eq!(
            sftp_argv("scp", "/tmp/src", "user@host:/data"),
            vec![
                "scp",
                "-o",
                "BatchMode=yes",
                "-r",
                "--",
                "/tmp/src",
                "user@host:/data"
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn sftp_argv_rsync_matches_the_pinned_shape() {
        assert_eq!(
            sftp_argv("rsync", "/tmp/src", "user@host:/data"),
            vec![
                "rsync",
                "-a",
                "-e",
                "ssh -o BatchMode=yes",
                "--",
                "/tmp/src",
                "user@host:/data"
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn sftp_argv_defaults_to_scp_for_an_unrecognized_tool_string() {
        // canonicalize() is the real gate against a bad tool string ever
        // reaching this function — this just documents argv's own fallback.
        assert_eq!(sftp_argv("bogus", "/s", "t")[0], "scp");
    }

    // ---- sftp_child_env allowlist (pure) ----

    #[test]
    fn sftp_child_env_keeps_only_the_narrow_allowlist_plus_lc_prefix() {
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
        let result = sftp_child_env(&env);
        for key in ["HOME", "PATH", "USER", "SSH_AUTH_SOCK", "LANG", "LC_ALL"] {
            assert!(result.contains_key(key), "{key} should survive");
        }
        for key in ["SHELL", "ANTHROPIC_API_KEY"] {
            assert!(
                !result.contains_key(key),
                "{key} must not leak into the sftp child"
            );
        }
    }

    // ---- collect_export_files / manifest ordering against a real dir ----

    #[test]
    fn collect_export_files_walks_recursively_and_defers_manifest_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("manifest.json"), b"{}").unwrap();
        std::fs::write(dir.path().join("a.md"), b"a").unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested").join("b.md"), b"b").unwrap();

        let files = collect_export_files(dir.path()).unwrap();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a.md", "nested/b.md", "manifest.json"]);
    }
}
