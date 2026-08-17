//! Promotion, manifest, and runs-index for finished background flow runs —
//! plan step 1.4, sitting directly on top of Slice A's run-scoped artifacts
//! (`runs/<id>/artifacts/`, `RunPlan.terminals`, the fail-closed output
//! contract enforced in `runner::launch`'s exit-await task). A run that
//! settles `"done"` has, by that contract, already proven every node's
//! DECLARED output is really on disk and non-empty — this module's only
//! job is turning that into something OUTSIDE the run's own scratch
//! directory can trust: a flat, hashed, git-pinned `out/<runId>/` snapshot
//! of just the TERMINAL nodes' outputs (the run's real deliverables, not
//! every intermediate handoff), a `manifest.json` describing exactly what
//! they are and where they came from, an always-current `out/latest/`
//! mirror, and a capped `runs-index.json` history. `crate::export` (a
//! parallel slice) is this module's only real downstream: it reads
//! `out/<runId>/` and ships `manifest.json` last as the commit marker,
//! trusting this module to have written it correctly and only once
//! everything underneath is complete — see that module's own doc comment.
//!
//! ## Tauri-free by design
//!
//! This module imports only `std`/`tokio`/`serde`/`sha2` and its own
//! sibling `flow::` modules (just [`confine`], here) — no `tauri`, no
//! `crate::git`, no reach into any OTHER top-level module of this crate,
//! even though `crate::git`'s subprocess idiom specifically would
//! otherwise be the obvious thing to reuse for the provenance fields
//! below. The plan is explicit that this module moves into its own
//! extracted crate once the products pipeline stabilizes, at which point
//! `crate::git` simply will not exist on the other side of that boundary —
//! so [`git_exec`] is a small, self-contained duplicate of `git.rs`'s own
//! `git()` idiom (argv array, `-C <dir>`, 10s timeout, `kill_on_drop`), not
//! a reuse of it, the same "duplicate rather than reach into a file this
//! slice doesn't own" discipline `runner::env`'s doc comment explains for
//! its own `lexical_resolve`.
//!
//! ## Why promotion never fails a run
//!
//! By the time [`promote_and_manifest`] runs, the run has already settled
//! `"done"` and every caller-visible status transition for it is over —
//! `runner::settle_if_done` has already persisted and pushed that verdict
//! before promotion is even spawned (see that function's own doc comment
//! on why the call is a detached `tokio::spawn`, never awaited inline).
//! Promotion is bookkeeping ON TOP of a run that already succeeded on its
//! own terms; a disk-full `out/` volume or a `git` binary that is
//! momentarily missing must not retroactively turn a real success into a
//! reported failure. Its only caller (`runner::spawn_promotion`) logs an
//! event and leaves `RunState.products` at its default `None` on `Err` —
//! it never touches `RunState.status`.
//!
//! ## Every constructed path is confined
//!
//! Every path this module builds itself — `out/`, `out/.gitignore`,
//! `out/<id>/`, each product's destination, `manifest.json`,
//! `out/latest/`, `runs-index.json` — passes
//! [`confine::confine_real_abs`] before this module reads OR writes
//! through it, exactly like every other sink `flow::runner` itself
//! confines. `root` is trusted (it is `RunState.root`, the same open
//! workspace folder every other part of a run's own filesystem footprint
//! is already confined against); nothing else handed in here is.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::confine;

const RUNS_INDEX_CAP: usize = 200;
const HASH_CHUNK_BYTES: usize = 64 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

// ---- public request/record shapes — what `runner::settle_if_done` builds ----

/// One node's audit trail in the manifest — every node the run scheduled,
/// not just the terminals. `runner::settle_if_done` builds one of these
/// per `NodeState` directly; this module never sees `NodeState` itself
/// (see the module doc comment on staying runner-agnostic).
#[derive(Debug, Clone, Serialize)]
pub struct ManifestNode {
    pub id: String,
    pub kind: String,
    pub model: Option<String>,
    pub status: String,
    pub exit: Option<i32>,
    pub started: Option<String>,
    pub ended: Option<String>,
}

/// One declared output of one TERMINAL node — [`promote_and_manifest`]'s
/// own work list, not the manifest shape itself (that's [`ManifestProduct`]
/// below, filled in once the file is actually found, copied, and hashed).
/// `output_name` is the literal string "undefined" for an unnamed output,
/// mirroring `NodeState.outputs`/`compose_bootstrap_prompt`'s identical
/// fallback — the JS twin's raw, un-fallback'd template-literal
/// interpolation of a missing `output.name`, byte for byte.
pub struct TerminalOutput {
    pub node_id: String,
    pub output_name: String,
}

/// Everything [`promote_and_manifest`] needs, gathered by
/// `runner::settle_if_done` from its own `RunState`/`NodeState` while the
/// run's registry lock is still held — see that function's doc comment.
/// Every path field is ABSOLUTE; every path this module goes on to
/// construct FROM them is re-confined against `root` before use.
pub struct PromoteRequest {
    pub root: PathBuf,
    pub flow_name: String,
    /// Absolute path to the `.flow.json` this run was started from —
    /// hashed into `manifest.flow.sha256` and stripped of `root` for
    /// `manifest.flow.path`.
    pub flow_path: PathBuf,
    pub run_id: String,
    pub started: String,
    pub ended: String,
    pub airgap: bool,
    /// This run's own `runs/<id>/artifacts` directory — where every
    /// terminal output named in `terminal_outputs` is read from.
    pub artifacts_dir: PathBuf,
    pub nodes: Vec<ManifestNode>,
    pub terminal_outputs: Vec<TerminalOutput>,
}

// ---- manifest.json shape (private — callers only ever see the returned
// `products` Value, never these types directly) ----

#[derive(Debug, Clone, Serialize)]
struct ManifestProduct {
    node: String,
    output: String,
    file: String,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestFlow {
    name: String,
    path: String,
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestRun {
    id: String,
    started: String,
    ended: String,
    airgap: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ManifestGit {
    head: Option<String>,
    dirty: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Manifest {
    version: u32,
    flow: ManifestFlow,
    run: ManifestRun,
    git: ManifestGit,
    nodes: Vec<ManifestNode>,
    products: Vec<ManifestProduct>,
}

// ---- runs-index.json shape ----

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunsIndex {
    version: u32,
    runs: Vec<RunsIndexEntry>,
}

impl Default for RunsIndex {
    fn default() -> Self {
        Self {
            version: 1,
            runs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunsIndexEntry {
    id: String,
    status: String,
    started: String,
    ended: Option<String>,
    products: Vec<String>,
    manifest: String,
}

// ---- entry point ----

/// On a run that just settled `"done"`: ensure `out/` (+ its `.gitignore`)
/// exists, copy+hash every terminal output into `out/<runId>/`, write
/// `manifest.json` there, rebuild `out/latest/` from it, and append
/// `runs-index.json`. Returns the manifest's own `products` array (a plain
/// JSON array, possibly empty for a flow whose terminal nodes declare no
/// outputs at all) — never the whole manifest; see `RunState.products`'s
/// doc comment for why only this much is kept in memory.
pub async fn promote_and_manifest(req: PromoteRequest) -> Result<Value, String> {
    let flow_dir = req.root.join(".tome").join("flows").join(&req.flow_name);
    let out_dir = flow_dir.join("out");
    ensure_dir(&req.root, &out_dir).await?;
    ensure_gitignore(&req.root, &out_dir).await?;

    let run_out_dir = out_dir.join(&req.run_id);
    ensure_dir(&req.root, &run_out_dir).await?;

    let mut products = Vec::with_capacity(req.terminal_outputs.len());
    for t in &req.terminal_outputs {
        let file_name = format!("{}-{}.md", t.node_id, t.output_name);
        let src = confine::confine_real_abs(&req.root, &req.artifacts_dir.join(&file_name), true)
            .await
            .ok_or_else(|| format!("product source escapes the workspace: {file_name}"))?;
        let dst = confine::confine_real_abs(&req.root, &run_out_dir.join(&file_name), false)
            .await
            .ok_or_else(|| format!("product destination escapes the workspace: {file_name}"))?;
        let (bytes, sha256) = copy_and_hash_streamed(&src, &dst)
            .await
            .map_err(|e| format!("copy {file_name}: {e}"))?;
        products.push(ManifestProduct {
            node: t.node_id.clone(),
            output: t.output_name.clone(),
            file: file_name,
            bytes,
            sha256,
        });
    }

    let flow_path = confine::confine_real_abs(&req.root, &req.flow_path, true)
        .await
        .ok_or_else(|| "flow file escapes the workspace".to_string())?;
    let flow_sha256 = hash_file_streamed(&flow_path)
        .await
        .map_err(|e| format!("hash flow file: {e}"))?;
    let flow_rel = flow_path
        .strip_prefix(&req.root)
        .unwrap_or(&flow_path)
        .to_string_lossy()
        .into_owned();

    let (git_head, git_dirty) = git_provenance(&req.root).await;

    // Snapshotted before `products` moves into `manifest` below — this is
    // the ONLY part of the manifest `RunState.products` ever holds.
    let products_value = serde_json::to_value(&products).map_err(|e| e.to_string())?;

    let manifest = Manifest {
        version: 1,
        flow: ManifestFlow {
            name: req.flow_name.clone(),
            path: flow_rel,
            sha256: flow_sha256,
        },
        run: ManifestRun {
            id: req.run_id.clone(),
            started: req.started.clone(),
            ended: req.ended.clone(),
            airgap: req.airgap,
        },
        git: ManifestGit {
            head: git_head,
            dirty: git_dirty,
        },
        nodes: req.nodes,
        products,
    };

    let manifest_path = run_out_dir.join("manifest.json");
    write_json_confined(&req.root, &manifest_path, &manifest).await?;

    refresh_latest(&req.root, &out_dir, &run_out_dir).await?;

    let entry = RunsIndexEntry {
        id: req.run_id.clone(),
        status: "done".to_string(),
        started: req.started.clone(),
        ended: Some(req.ended.clone()),
        products: manifest.products.iter().map(|p| p.file.clone()).collect(),
        manifest: format!("out/{}/manifest.json", req.run_id),
    };
    update_runs_index(&req.root, &flow_dir, entry).await?;

    Ok(products_value)
}

// ---- out/ + out/.gitignore ----

async fn ensure_dir(root: &Path, dir: &Path) -> Result<PathBuf, String> {
    let confined = confine::confine_real_abs(root, dir, false)
        .await
        .ok_or_else(|| format!("{} escapes the workspace", dir.display()))?;
    tokio::fs::create_dir_all(&confined)
        .await
        .map_err(|e| e.to_string())?;
    Ok(confined)
}

/// Writes `out/.gitignore` containing `"*\n"` only if no file is there yet
/// — the binding decision this exists to satisfy is explicit that this
/// must never touch the user's own root `.gitignore`, and just as
/// importantly must never clobber a `out/.gitignore` the user themselves
/// edited (to un-ignore a specific run, say).
async fn ensure_gitignore(root: &Path, out_dir: &Path) -> Result<(), String> {
    let path = out_dir.join(".gitignore");
    let confined = confine::confine_real_abs(root, &path, false)
        .await
        .ok_or_else(|| "out/.gitignore escapes the workspace".to_string())?;
    if tokio::fs::metadata(&confined).await.is_ok() {
        return Ok(());
    }
    tokio::fs::write(&confined, "*\n")
        .await
        .map_err(|e| e.to_string())
}

// ---- out/latest/ ----

/// `remove_dir_all` then a full recursive copy — no symlinks, per the
/// binding decision. Independent copy of `export.rs`'s identically-shaped
/// `copy_dir_recursive` (a different slice's file this module cannot
/// import — see the module doc comment on the tauri-free,
/// sibling-modules-only constraint).
async fn refresh_latest(root: &Path, out_dir: &Path, run_out_dir: &Path) -> Result<(), String> {
    let latest = out_dir.join("latest");
    let confined = confine::confine_real_abs(root, &latest, false)
        .await
        .ok_or_else(|| "out/latest escapes the workspace".to_string())?;
    if tokio::fs::metadata(&confined).await.is_ok() {
        tokio::fs::remove_dir_all(&confined)
            .await
            .map_err(|e| format!("clear out/latest: {e}"))?;
    }
    let src = run_out_dir.to_path_buf();
    let dst = confined;
    tokio::task::spawn_blocking(move || copy_dir_recursive(&src, &dst))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("rebuild out/latest: {e}"))
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

// ---- runs-index.json ----

/// Every promotion for every flow in this process funnels through one
/// lock — coarse, but correct: two runs of the SAME flow settling `"done"`
/// moments apart can never read-modify-write `runs-index.json`
/// interleaved and silently drop one of their own entries. A run of a
/// DIFFERENT flow pays for a lock it never actually contends with, which
/// is cheap here — promotion is infrequent, and everything inside the
/// critical section below is a handful of small file operations, never a
/// spawned child process.
fn runs_index_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Prepends `entry` (replacing any existing entry with the same `id`, so a
/// re-promotion is never duplicated), then caps at [`RUNS_INDEX_CAP`] —
/// newest-first is therefore just "insert at the front", never a sort: the
/// run this call is promoting is by definition the most recent one there
/// is.
async fn update_runs_index(
    root: &Path,
    flow_dir: &Path,
    entry: RunsIndexEntry,
) -> Result<(), String> {
    let _guard = runs_index_lock().lock().await;

    let path = flow_dir.join("runs-index.json");
    let confined = confine::confine_real_abs(root, &path, false)
        .await
        .ok_or_else(|| "runs-index.json escapes the workspace".to_string())?;
    let mut index = match tokio::fs::read_to_string(&confined).await {
        Ok(text) => serde_json::from_str::<RunsIndex>(&text).unwrap_or_default(),
        Err(_) => RunsIndex::default(),
    };
    index.version = 1;
    index.runs.retain(|r| r.id != entry.id);
    index.runs.insert(0, entry);
    index.runs.truncate(RUNS_INDEX_CAP);

    let text = serde_json::to_string_pretty(&index).map_err(|e| e.to_string())? + "\n";
    tokio::fs::write(&confined, text)
        .await
        .map_err(|e| e.to_string())
}

// ---- hashing ----

/// Matches `airgap::sha1_hex`'s own hand-rolled hex encoder — no `hex`
/// crate dependency needed for either.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn hash_file_streamed(path: &Path) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

/// Copies `src` to `dst` and hashes it in the SAME pass — one 64 KiB
/// buffer read, written, and fed to the hasher per iteration, so a large
/// product is never held in memory whole just to learn its own digest.
async fn copy_and_hash_streamed(src: &Path, dst: &Path) -> std::io::Result<(u64, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut reader = tokio::fs::File::open(src).await?;
    let mut writer = tokio::fs::File::create(dst).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK_BYTES];
    let mut total: u64 = 0;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n]).await?;
        total += n as u64;
    }
    writer.flush().await?;
    Ok((total, hex_encode(&hasher.finalize())))
}

// ---- git provenance ----

/// Local copy of `git.rs`'s own `git()` subprocess idiom (argv array,
/// `-C <dir>`, 10s timeout, `kill_on_drop`) — see the module doc comment
/// on why this duplicates rather than imports it. Collapses every failure
/// (not a repo, `git` missing, the call timing out) to `None` — this
/// module has no stderr-reporting caller the way `git.rs`'s own IPC
/// commands do; a manifest's provenance fields are best-effort by nature.
async fn git_exec(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(root).args(args).kill_on_drop(true);
    let output = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `(head, dirty)` — `head` is `None` outside a repo (or if `git` itself
/// is missing), matching the manifest's own "null if not a repo" contract;
/// `dirty` defaults to `false` whenever it can't be determined, the least
/// alarming reading of "we genuinely don't know".
async fn git_provenance(root: &Path) -> (Option<String>, bool) {
    let head = git_exec(root, &["rev-parse", "HEAD"])
        .await
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let dirty = git_exec(root, &["status", "--porcelain"])
        .await
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    (head, dirty)
}

// ---- json write ----

async fn write_json_confined<T: Serialize>(
    root: &Path,
    path: &Path,
    value: &T,
) -> Result<(), String> {
    let confined = confine::confine_real_abs(root, path, false)
        .await
        .ok_or_else(|| format!("{} escapes the workspace", path.display()))?;
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    tokio::fs::write(&confined, text)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> PathBuf {
        tempfile::Builder::new()
            .prefix("tome-products-")
            .tempdir()
            .unwrap()
            .keep()
    }

    fn node(id: &str, status: &str) -> ManifestNode {
        ManifestNode {
            id: id.to_string(),
            kind: "claude".to_string(),
            model: None,
            status: status.to_string(),
            exit: Some(0),
            started: Some("2026-08-09T10:00:00.000Z".to_string()),
            ended: Some("2026-08-09T10:00:01.000Z".to_string()),
        }
    }

    fn base_request(root: &Path, flow_name: &str, run_id: &str) -> PromoteRequest {
        PromoteRequest {
            root: root.to_path_buf(),
            flow_name: flow_name.to_string(),
            flow_path: root
                .join(".tome")
                .join("flows")
                .join(format!("{flow_name}.flow.json")),
            run_id: run_id.to_string(),
            started: "2026-08-09T10:00:00.000Z".to_string(),
            ended: "2026-08-09T10:00:02.000Z".to_string(),
            airgap: true,
            artifacts_dir: root
                .join(".tome")
                .join("flows")
                .join(flow_name)
                .join("runs")
                .join(run_id)
                .join("artifacts"),
            nodes: vec![node("n1", "done")],
            terminal_outputs: vec![TerminalOutput {
                node_id: "n1".to_string(),
                output_name: "out".to_string(),
            }],
        }
    }

    /// Lays out exactly what `runner::start_run`/`launch` would have left
    /// behind for a one-node flow that already ran and wrote its declared
    /// output — the flow file, the run's artifacts dir, and the one
    /// artifact `base_request`'s `terminal_outputs` names.
    fn seed_run(root: &Path, flow_name: &str, run_id: &str, content: &str) {
        std::fs::create_dir_all(root.join(".tome").join("flows")).unwrap();
        std::fs::write(
            root.join(".tome")
                .join("flows")
                .join(format!("{flow_name}.flow.json")),
            format!(r#"{{"version":1,"name":"{flow_name}","nodes":[],"edges":[]}}"#),
        )
        .unwrap();
        let artifacts = root
            .join(".tome")
            .join("flows")
            .join(flow_name)
            .join("runs")
            .join(run_id)
            .join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("n1-out.md"), content).unwrap();
    }

    // ---- promote_and_manifest — full pipeline against a real tempdir ----

    #[tokio::test]
    async fn promotes_a_product_writes_gitignore_and_a_manifest_matching_an_independent_hash() {
        let root = workspace();
        seed_run(&root, "demo", "run1", "hello world");
        let req = base_request(&root, "demo", "run1");

        let products_value = promote_and_manifest(req).await.unwrap();

        let out_dir = root.join(".tome/flows/demo/out");
        assert_eq!(
            std::fs::read_to_string(out_dir.join(".gitignore")).unwrap(),
            "*\n"
        );
        let product_path = out_dir.join("run1").join("n1-out.md");
        assert_eq!(
            std::fs::read_to_string(&product_path).unwrap(),
            "hello world"
        );

        let mut hasher = Sha256::new();
        hasher.update(b"hello world");
        let expected_sha = hex_encode(&hasher.finalize());

        let products = products_value.as_array().unwrap();
        assert_eq!(products.len(), 1);
        assert_eq!(products[0]["node"], "n1");
        assert_eq!(products[0]["output"], "out");
        assert_eq!(products[0]["file"], "n1-out.md");
        assert_eq!(products[0]["bytes"], "hello world".len());
        assert_eq!(products[0]["sha256"], expected_sha);

        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(out_dir.join("run1").join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["version"], 1);
        assert_eq!(manifest["flow"]["name"], "demo");
        assert_eq!(manifest["flow"]["path"], ".tome/flows/demo.flow.json");
        assert_eq!(manifest["run"]["id"], "run1");
        assert_eq!(manifest["run"]["airgap"], true);
        assert_eq!(manifest["git"]["head"], Value::Null);
        assert_eq!(manifest["git"]["dirty"], false);
        assert_eq!(manifest["nodes"][0]["id"], "n1");
        assert_eq!(manifest["products"][0]["sha256"], expected_sha);
    }

    #[tokio::test]
    async fn never_overwrites_an_existing_out_gitignore() {
        let root = workspace();
        seed_run(&root, "demo", "run1", "content");
        std::fs::create_dir_all(root.join(".tome/flows/demo/out")).unwrap();
        std::fs::write(root.join(".tome/flows/demo/out/.gitignore"), "custom\n").unwrap();

        promote_and_manifest(base_request(&root, "demo", "run1"))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join(".tome/flows/demo/out/.gitignore")).unwrap(),
            "custom\n"
        );
    }

    #[tokio::test]
    async fn refreshes_latest_from_scratch_on_a_second_run_no_stale_files_survive() {
        let root = workspace();
        seed_run(&root, "demo", "run1", "first");
        promote_and_manifest(base_request(&root, "demo", "run1"))
            .await
            .unwrap();
        let latest = root.join(".tome/flows/demo/out/latest");
        assert_eq!(
            std::fs::read_to_string(latest.join("n1-out.md")).unwrap(),
            "first"
        );

        // A stale file from a hypothetical older layout that a plain merge
        // copy would have left behind — `remove_dir_all` must clear it.
        std::fs::write(latest.join("stale.md"), "old").unwrap();

        seed_run(&root, "demo", "run2", "second");
        promote_and_manifest(base_request(&root, "demo", "run2"))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(latest.join("n1-out.md")).unwrap(),
            "second"
        );
        assert!(!latest.join("stale.md").exists());
        assert!(latest.join("manifest.json").exists());
    }

    #[tokio::test]
    async fn runs_index_is_newest_first_and_deduped_by_id() {
        let root = workspace();
        seed_run(&root, "demo", "run1", "a");
        promote_and_manifest(base_request(&root, "demo", "run1"))
            .await
            .unwrap();
        seed_run(&root, "demo", "run2", "b");
        promote_and_manifest(base_request(&root, "demo", "run2"))
            .await
            .unwrap();

        let index: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".tome/flows/demo/runs-index.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(index["version"], 1);
        let runs = index["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["id"], "run2");
        assert_eq!(runs[1]["id"], "run1");
        assert_eq!(runs[0]["manifest"], "out/run2/manifest.json");
        assert_eq!(runs[0]["products"], serde_json::json!(["n1-out.md"]));

        // Re-promoting the same run id must replace, not duplicate.
        seed_run(&root, "demo", "run1", "a-again");
        promote_and_manifest(base_request(&root, "demo", "run1"))
            .await
            .unwrap();
        let index: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".tome/flows/demo/runs-index.json")).unwrap(),
        )
        .unwrap();
        let runs = index["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["id"], "run1"); // just re-promoted -> newest
    }

    #[tokio::test]
    async fn runs_index_caps_at_200_keeping_the_newest() {
        let root = workspace();
        let flow_dir = root.join(".tome/flows/demo");
        std::fs::create_dir_all(&flow_dir).unwrap();
        for i in 0..205 {
            update_runs_index(
                &root,
                &flow_dir,
                RunsIndexEntry {
                    id: format!("r{i}"),
                    status: "done".to_string(),
                    started: "2026-01-01T00:00:00.000Z".to_string(),
                    ended: Some("2026-01-01T00:00:01.000Z".to_string()),
                    products: vec![],
                    manifest: format!("out/r{i}/manifest.json"),
                },
            )
            .await
            .unwrap();
        }
        let index: RunsIndex = serde_json::from_str(
            &std::fs::read_to_string(flow_dir.join("runs-index.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(index.runs.len(), RUNS_INDEX_CAP);
        assert_eq!(index.runs[0].id, "r204"); // most recently inserted
        assert_eq!(index.runs[199].id, "r5"); // the oldest 5 (r0..=r4) fell off
    }

    #[tokio::test]
    async fn errs_without_writing_anything_when_a_declared_output_is_missing() {
        let root = workspace();
        // Flow file present, artifacts dir present, but the declared
        // output itself was never written — cannot happen for a REAL
        // "done" run (the fail-closed contract already guarantees it), but
        // this still must fail closed rather than silently promoting an
        // empty/missing product.
        std::fs::create_dir_all(root.join(".tome/flows")).unwrap();
        std::fs::write(
            root.join(".tome/flows/demo.flow.json"),
            r#"{"version":1,"name":"demo","nodes":[],"edges":[]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".tome/flows/demo/runs/run1/artifacts")).unwrap();

        let err = promote_and_manifest(base_request(&root, "demo", "run1"))
            .await
            .unwrap_err();
        assert!(err.contains("n1-out.md"));
        assert!(
            !root.join(".tome/flows/demo/out").exists() || {
                // ensure_dir/gitignore may have landed before the missing-file
                // error — either way, no manifest and no runs-index entry.
                !root
                    .join(".tome/flows/demo/out/run1/manifest.json")
                    .exists()
            }
        );
        assert!(!root.join(".tome/flows/demo/runs-index.json").exists());
    }

    // ---- git provenance, against a real throwaway repo ----

    fn run_git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("failed to spawn git for test setup");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo() -> PathBuf {
        let dir = workspace();
        run_git(&dir, &["init", "-b", "main"]);
        run_git(&dir, &["config", "user.email", "test@example.com"]);
        run_git(&dir, &["config", "user.name", "Test"]);
        run_git(&dir, &["config", "commit.gpgsign", "false"]);
        dir
    }

    #[tokio::test]
    async fn git_provenance_is_null_and_not_dirty_outside_any_repo() {
        let dir = workspace();
        let (head, dirty) = git_provenance(&dir).await;
        assert_eq!(head, None);
        assert!(!dirty);
    }

    #[tokio::test]
    async fn git_provenance_reports_head_and_a_clean_tree_right_after_a_commit() {
        let dir = init_repo();
        std::fs::write(dir.join("a.txt"), "1\n").unwrap();
        run_git(&dir, &["add", "."]);
        run_git(&dir, &["commit", "-m", "initial"]);

        let (head, dirty) = git_provenance(&dir).await;
        let expected = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(head.as_deref(), Some(expected.trim()));
        assert!(!dirty);
    }

    #[tokio::test]
    async fn git_provenance_reports_dirty_with_an_uncommitted_change() {
        let dir = init_repo();
        std::fs::write(dir.join("a.txt"), "1\n").unwrap();
        run_git(&dir, &["add", "."]);
        run_git(&dir, &["commit", "-m", "initial"]);
        std::fs::write(dir.join("a.txt"), "2\n").unwrap();

        let (_, dirty) = git_provenance(&dir).await;
        assert!(dirty);
    }

    // ---- hashing ----

    #[tokio::test]
    async fn hash_file_streamed_matches_the_well_known_empty_string_sha256() {
        let dir = workspace();
        let path = dir.join("empty.bin");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            hash_file_streamed(&path).await.unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[tokio::test]
    async fn copy_and_hash_streamed_handles_content_spanning_multiple_64kib_chunks() {
        let dir = workspace();
        let src = dir.join("big.bin");
        let dst = dir.join("copy.bin");
        // > 64 KiB so the read/hash/write loop actually iterates more than
        // once — the whole point of streaming rather than a single
        // whole-file read.
        let content: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &content).unwrap();

        let (bytes, sha256) = copy_and_hash_streamed(&src, &dst).await.unwrap();
        assert_eq!(bytes, content.len() as u64);
        assert_eq!(std::fs::read(&dst).unwrap(), content);

        let mut hasher = Sha256::new();
        hasher.update(&content);
        assert_eq!(sha256, hex_encode(&hasher.finalize()));
    }

    #[test]
    fn hex_encode_is_lowercase_and_zero_padded() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xff]), "000fff");
    }

    // ---- manifest serialization shape (pure) ----

    #[test]
    fn manifest_serializes_with_the_expected_shape_and_top_level_key_order() {
        let manifest = Manifest {
            version: 1,
            flow: ManifestFlow {
                name: "demo".to_string(),
                path: ".tome/flows/demo.flow.json".to_string(),
                sha256: "abc123".to_string(),
            },
            run: ManifestRun {
                id: "run1".to_string(),
                started: "2026-01-01T00:00:00.000Z".to_string(),
                ended: "2026-01-01T00:00:02.000Z".to_string(),
                airgap: false,
            },
            git: ManifestGit {
                head: None,
                dirty: false,
            },
            nodes: vec![node("n1", "done")],
            products: vec![ManifestProduct {
                node: "n1".to_string(),
                output: "out".to_string(),
                file: "n1-out.md".to_string(),
                bytes: 11,
                sha256: "def456".to_string(),
            }],
        };

        // Top-level key order is a property of the derived `Serialize`
        // impl writing straight to the output in DECLARATION order — that
        // only holds serializing the TYPED STRUCT directly to a
        // string/writer, exactly what `write_json_confined` does for the
        // real `manifest.json` on disk. `serde_json::to_value` would NOT
        // preserve this: its `Value::Object` is `Map`(`BTreeMap`)-backed
        // and alphabetizes without the `preserve_order` cargo feature,
        // which this crate does not enable — so this checks the STRING
        // form, never a round-tripped `Value`.
        let text = serde_json::to_string(&manifest).unwrap();
        let mut last = 0;
        for key in ["version", "flow", "run", "git", "nodes", "products"] {
            let pos = text
                .find(&format!("\"{key}\":"))
                .unwrap_or_else(|| panic!("missing top-level key {key:?} in {text}"));
            assert!(pos >= last, "key {key:?} out of declared order in {text}");
            last = pos;
        }

        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["git"]["head"], Value::Null);
        assert_eq!(value["flow"]["name"], "demo");
        for key in ["id", "kind", "model", "status", "exit", "started", "ended"] {
            assert!(
                value["nodes"][0].get(key).is_some(),
                "manifest node missing {key:?}"
            );
        }
        for key in ["node", "output", "file", "bytes", "sha256"] {
            assert!(
                value["products"][0].get(key).is_some(),
                "manifest product missing {key:?}"
            );
        }
    }

    #[test]
    fn runs_index_defaults_to_an_empty_v1_store() {
        let index = RunsIndex::default();
        assert_eq!(index.version, 1);
        assert!(index.runs.is_empty());
    }

    #[test]
    fn runs_index_entry_round_trips_through_json() {
        let entry = RunsIndexEntry {
            id: "run1".to_string(),
            status: "done".to_string(),
            started: "2026-01-01T00:00:00.000Z".to_string(),
            ended: Some("2026-01-01T00:00:02.000Z".to_string()),
            products: vec!["n1-out.md".to_string()],
            manifest: "out/run1/manifest.json".to_string(),
        };
        let text = serde_json::to_string(&entry).unwrap();
        let back: RunsIndexEntry = serde_json::from_str(&text).unwrap();
        assert_eq!(back.id, "run1");
        assert_eq!(back.products, vec!["n1-out.md".to_string()]);
    }
}
