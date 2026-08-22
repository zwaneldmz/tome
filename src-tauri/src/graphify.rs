//! Graphify sidecar wrapper: availability probe, one streaming build
//! (extract → cluster/report/viz), cancellation, and read-only graph
//! queries (`query`, `path`, `explain`, `affected`). Owns only this file
//! plus `ipc/graphify.rs` and one `mod graphify;` line in `lib.rs` — the
//! same slice discipline `brain.rs`'s module doc comment describes.
//!
//! ## Security posture
//!
//! graphify runs entirely locally; Tome's wrapper deliberately pins the
//! offline path. The build runs `--code-only` (tree-sitter AST extraction
//! — no LLM, no network) and `cluster-only --no-label` (Leiden clustering
//! with "Community N" placeholder names — community *naming* is the one
//! LLM-dependent stage, and skipping it keeps a build key- and
//! network-free). The `add <url>` ingest subcommand is never exposed, so a
//! build can never fetch anything. Queries read `graphify-out/graph.json`
//! under the workspace — paths never leave the workspace the renderer
//! passed.
//!
//! ## Process hygiene
//!
//! Every spawn is args-array only (no shell), `stdin` null, `kill_on_drop`
//! true. The build stages hold a module-level build lock (one build at a
//! time) and park the live child's pid in a module-level slot so
//! [`cancel`] can kill it mid-stream — the same module-static discipline
//! `brain.rs`'s watcher maps use, so `state.rs` never changes. PATH is the
//! login-shell-resolved one from `login_env` (GUI-launched apps inherit a
//! minimal PATH; `~/.local/bin/graphify` would otherwise be invisible).
//! Timeouts bound every process: 3s probe, 10min per build stage (a cold
//! extraction of a large repo can take minutes), 120s per query. Output
//! past [`QUERY_CAP`] bytes is trimmed from the front — the tail is what
//! matters, the same boundary-snap idiom `conductor::tools` uses.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Output directory name graphify writes inside the workspace.
pub const OUT_DIR: &str = "graphify-out";

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const STAGE_TIMEOUT: Duration = Duration::from_secs(600);
const QUERY_TIMEOUT: Duration = Duration::from_secs(120);
const QUERY_CAP: usize = 50_000;

/// One build at a time, held across BOTH pipeline stages so a second
/// `graphify:build` can't slip in between extract and cluster.
static BUILD_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
/// Pid of the currently-running graphify process, for [`cancel`]. `Child`
/// itself can't be shared with the kill path and the stream loop at once,
/// so the pid is the rendezvous point.
static RUNNING_PID: OnceLock<Mutex<Option<u32>>> = OnceLock::new();

fn build_lock() -> &'static tokio::sync::Mutex<()> {
    BUILD_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
fn running_pid() -> &'static Mutex<Option<u32>> {
    RUNNING_PID.get_or_init(|| Mutex::new(None))
}

// ---- output paths ----

pub fn out_dir(ws: &Path) -> PathBuf {
    ws.join(OUT_DIR)
}
pub fn graph_json(ws: &Path) -> PathBuf {
    out_dir(ws).join("graph.json")
}
pub fn graph_html(ws: &Path) -> PathBuf {
    out_dir(ws).join("graph.html")
}
pub fn report(ws: &Path) -> PathBuf {
    out_dir(ws).join("GRAPH_REPORT.md")
}

/// Everything the renderer needs to render the pane's availability state.
/// Serialized directly (serde) for the `graphify:status` wire reply, so
/// the field order/shape here IS the JSON shape.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// True when `graphify` is on PATH (login-shell PATH) and answers
    /// `--version` within the probe timeout.
    pub available: bool,
    /// `graphify --version`'s first line, trimmed — e.g. `graphify 0.9.48`.
    pub version: Option<String>,
    /// Why `available` is false (spawn failure, bad exit, timeout) —
    /// surfaced verbatim in the pane's hint.
    pub reason: Option<String>,
    /// Whether a previous build left a graph behind (graph.json exists).
    pub built: bool,
    pub out_dir: PathBuf,
    pub graph_json: PathBuf,
    pub graph_html: PathBuf,
    pub report: PathBuf,
}

/// Probes `graphify --version` (3s cap) and checks for a built graph.
/// Never fails the command — unavailability is data in the [`Status`], not
/// an `Err`, so the pane can render an install hint instead of a toast.
pub async fn status(ws: &Path) -> Status {
    let (available, version, reason) = match spawn_plain(&["--version"]).await {
        Ok(child) => {
            let out = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()).await;
            match out {
                Ok(Ok(out)) if out.status.success() => {
                    let v = String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    (true, Some(v), None)
                }
                Ok(Ok(out)) => (
                    false,
                    None,
                    Some(format!(
                        "graphify --version exited {}",
                        out.status.code().unwrap_or(-1)
                    )),
                ),
                Ok(Err(e)) => (false, None, Some(e.to_string())),
                Err(_) => (
                    false,
                    None,
                    Some("graphify --version timed out".to_string()),
                ),
            }
        }
        Err(e) => (false, None, Some(format!("graphify not found ({e})"))),
    };
    Status {
        available,
        version,
        reason,
        built: graph_json(ws).is_file(),
        out_dir: out_dir(ws),
        graph_json: graph_json(ws),
        graph_html: graph_html(ws),
        report: report(ws),
    }
}

/// The one-click build: `graphify <ws> --code-only` then
/// `graphify cluster-only <ws> --no-label`. Both stages stream their
/// stdout+stderr lines through `on_line` (the renderer's Tauri Channel).
/// Returns a one-line summary the renderer can toast.
pub async fn build(ws: &Path, mut on_line: impl FnMut(String) + Send) -> Result<String, String> {
    let _guard = build_lock().lock().await;
    let ws_str = ws.to_string_lossy().into_owned();

    on_line(format!("graphify — building the workspace graph ({ws_str})"));
    on_line("[1/2] extracting code with tree-sitter (offline, no LLM)".to_string());
    run_stage(
        ws,
        &[&ws_str, "--code-only"],
        &mut on_line,
    )
    .await
    .map_err(|e| format!("extract failed: {e}"))?;

    on_line("[2/2] clustering communities and writing report + graph.html".to_string());
    run_stage(
        ws,
        &["cluster-only", &ws_str, "--no-label"],
        &mut on_line,
    )
    .await
    .map_err(|e| format!("cluster failed: {e}"))?;

    on_line("done".to_string());
    let json = graph_json(ws);
    Ok(format!("graph built — {}", json.to_string_lossy()))
}

/// Kills the in-flight graphify process, if any. Returns whether there was
/// one to kill (the renderer uses it to decide what to say).
pub async fn cancel() -> bool {
    let pid = running_pid().lock().expect("graphify RUNNING_PID poisoned").take();
    let Some(pid) = pid else { return false };
    // Reap via the OS kill binary — the Child object is owned by the
    // stream loop, so there is no handle to kill it with from here.
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdin(Stdio::null())
        .status()
        .await;
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .stdin(Stdio::null())
        .status()
        .await;
    true
}

/// A read-only graph query: `query`, `path "A" "B"`, `explain`, or
/// `affected`, run with cwd = workspace so the CLI's default
/// `graphify-out/graph.json` resolves. Takes the build lock so a query
/// never reads a half-written graph.json mid-build.
pub async fn ask(ws: &Path, args: &[&str]) -> Result<String, String> {
    let _guard = build_lock().lock().await;
    let out = tokio::time::timeout(QUERY_TIMEOUT, async {
        let child = spawn_in(Some(ws), args).await?;
        child.wait_with_output().await.map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "graphify timed out".to_string())?
    .map_err(|e| format!("spawn graphify: {e}"))?;

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    cap_tail(&mut text, QUERY_CAP);
    if out.status.success() {
        Ok(text)
    } else {
        Err(format!("exit {}:\n{}", out.status.code().unwrap_or(-1), text))
    }
}

// ---- internals ----

/// Spawns `graphify` with an args array (no shell), cwd = `ws`, PATH from
/// the login-shell harvest, stdin null, kill_on_drop. `ask` and `status`
/// both build on this.
async fn spawn_plain(args: &[&str]) -> Result<Child, String> {
    spawn_in(None, args).await
}

async fn spawn_in(ws: Option<&Path>, args: &[&str]) -> Result<Child, String> {
    let login = crate::login_env::login_env().await;
    let mut cmd = Command::new("graphify");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("PATH", &login.path);
    if let Some(ws) = ws {
        cmd.current_dir(ws);
    }
    cmd.spawn().map_err(|e| format!("spawn graphify: {e}"))
}

/// One pipeline stage: spawn, park the pid in the kill slot, stream both
/// pipes through `on_line`, clear the slot, report the exit. The
/// `RUNNING_PID` slot is set/cleared around the stream loop so `cancel`
/// always has a pid to kill exactly while output can still be flowing.
async fn run_stage(
    ws: &Path,
    args: &[&str],
    on_line: &mut (impl FnMut(String) + Send),
) -> Result<(), String> {
    let mut child = spawn_in(Some(ws), args).await?;
    *running_pid().lock().expect("graphify RUNNING_PID poisoned") = child.id();

    let result = tokio::time::timeout(STAGE_TIMEOUT, stream_child(&mut child, on_line)).await;
    let result = match result {
        // The timeout dropped the stream loop (and its pipe readers); kill
        // the child explicitly — kill_on_drop only fires when the Child
        // itself drops, and this scope still owns it.
        Err(_) => {
            let _ = child.kill().await;
            Err("stage timed out".to_string())
        }
        Ok(r) => r,
    };
    *running_pid().lock().expect("graphify RUNNING_PID poisoned") = None;

    let status = result?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "graphify {} exited {}",
            args.join(" "),
            status.code().unwrap_or(-1)
        ))
    }
}

/// Reads stdout and stderr to EOF (concurrently, both through `on_line`),
/// then awaits the exit status. A cancel() kill makes both pipes EOF, so
/// the loop always terminates.
async fn stream_child(
    child: &mut Child,
    on_line: &mut (impl FnMut(String) + Send),
) -> Result<std::process::ExitStatus, String> {
    let stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let stderr = child.stderr.take().ok_or("no stderr pipe")?;
    let mut out = BufReader::new(stdout).lines();
    let mut err = BufReader::new(stderr).lines();
    let (mut out_done, mut err_done) = (false, false);
    while !(out_done && err_done) {
        tokio::select! {
            r = out.next_line(), if !out_done => match r {
                Ok(Some(line)) => on_line(line),
                Ok(None) => out_done = true,
                Err(e) => { on_line(format!("[stdout: {e}]")); out_done = true; }
            },
            r = err.next_line(), if !err_done => match r {
                Ok(Some(line)) => on_line(line),
                Ok(None) => err_done = true,
                Err(e) => { on_line(format!("[stderr: {e}]")); err_done = true; }
            },
        }
    }
    child.wait().await.map_err(|e| e.to_string())
}

/// Trims `text` to `cap` bytes from the FRONT, snapping to a char boundary
/// — the same idiom `conductor::tools::read_file` uses, because the tail
/// of a long answer is the useful part.
fn cap_tail(text: &mut String, cap: usize) {
    if text.len() > cap {
        let mut cut = text.len() - cap;
        while cut < text.len() && !text.is_char_boundary(cut) {
            cut += 1;
        }
        text.drain(..cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_tail_keeps_the_tail_and_respects_char_boundaries() {
        let mut s = "aaaaaaaaaaaaaaaaaaaa".to_string();
        cap_tail(&mut s, 10);
        assert_eq!(s, "aaaaaaaaaa");

        // multibyte: cutting must never land mid-codepoint
        let mut s = "éééééééééé".to_string(); // 10 × 2 bytes
        cap_tail(&mut s, 6);
        assert_eq!(s, "ééé");
    }

    #[test]
    fn out_paths_live_under_the_workspace() {
        let ws = Path::new("/tmp/ws");
        assert_eq!(graph_json(ws), Path::new("/tmp/ws/graphify-out/graph.json"));
        assert_eq!(graph_html(ws), Path::new("/tmp/ws/graphify-out/graph.html"));
        assert_eq!(report(ws), Path::new("/tmp/ws/graphify-out/GRAPH_REPORT.md"));
        assert_eq!(out_dir(ws), Path::new("/tmp/ws/graphify-out"));
    }
}
