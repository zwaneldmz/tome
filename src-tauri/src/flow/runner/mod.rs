//! Background flow runs: one headless child process per node, sequenced by
//! the graph — port of `src/main/flow-runner.js` (579 lines). Single-writer
//! of each run's `run.json` (a dedicated task per run drains an ordered
//! channel of pre-serialized snapshots — see [`spawn_run_json_writer`]),
//! every transition pushed to `runs:changed` and the persistent event log.
//!
//! Everything with a side effect outside this module is reached through
//! [`env::RunnerEnv`] — the injected-seam translation of `flow-runner.js`'s
//! `init(opts)` (see that module's doc comment) — which is what lets
//! [`start_run`]/[`cancel_run`] be driven end to end by a test with a fake
//! spawn function in place of a real agent CLI, exactly like the JS
//! original's own test suite.
//!
//! `Runner` (this module) is the live registry — `AppState.flow`, one per
//! process, `Arc`-wrapped so background tasks (the exit-await chain,
//! `run.json` writer, kill-escalation timers) can hold their own strong
//! reference independent of any single command invocation's lifetime.

pub mod env;
pub mod spawn;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Value};

use self::env::{RunnerEnv, SandboxWrap};
use super::{confine, model, run_plan};
use crate::agent_spawn;

// ---- live state ----

struct NodeState {
    id: String,
    name: String,
    kind: String,
    model: Option<String>,
    status: String,
    started: Option<String>,
    ended: Option<String>,
    exit: Option<i32>,
    log: PathBuf,
    /// `[cmd, ...args]` — `agent_spawn::build_headless_spawn`'s output,
    /// resolved once at `start_run` time (mirrors `node.spawn` in JS).
    inner_argv: Vec<String>,
    /// Live handle fields — `None` whenever no process is currently up for
    /// this node (never started, already exited, or the run settled).
    pid: Option<i32>,
    kill_fn: Option<Arc<dyn Fn(i32) -> std::io::Result<()> + Send + Sync>>,
    kill_timer: Option<tokio::task::AbortHandle>,
}

struct RunState {
    id: String,
    flow: String,
    flow_path: String,
    root: PathBuf,
    dir: PathBuf,
    gapped: bool,
    status: String,
    started: String,
    ended: Option<String>,
    canceling: bool,
    plan: run_plan::RunPlan,
    statuses: HashMap<String, String>,
    nodes: Vec<NodeState>,
    write_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Serializes [`pump`]'s decide-and-mark critical section for THIS run
    /// only — see that function's doc comment for the race it closes. An
    /// `Arc<tokio::sync::Mutex<..>>` (not the `std::sync::Mutex` guarding
    /// the rest of this struct's fields): it is held across `.await` points
    /// deliberately, which a `std::sync::Mutex` guard must never be.
    scheduling_lock: Arc<tokio::sync::Mutex<()>>,
}

struct RunnerInner {
    /// Insertion order of run ids — `HashMap` iteration order is not
    /// stable the way a JS `Map`'s is, and [`snapshot_all_locked`]'s
    /// descending-by-`started` sort must break ties by insertion order
    /// (two runs started in the same millisecond) exactly like the JS
    /// original's stable `Array.prototype.sort` over `[...runs.values()]`
    /// does. This is what makes that possible.
    order: Vec<String>,
    runs: HashMap<String, RunState>,
}

/// The live run registry — `AppState.flow`. Every run this session has
/// started stays in memory for the process's lifetime (mirrors JS's
/// module-level `const runs = new Map()`, never pruned).
pub struct Runner {
    inner: std::sync::Mutex<RunnerInner>,
}

impl Runner {
    pub fn new() -> Self {
        Self { inner: std::sync::Mutex::new(RunnerInner { order: Vec::new(), runs: HashMap::new() }) }
    }
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

// ---- timestamps (ISO8601 UTC, matching flow::run_plan's parser) ----

fn now_iso8601() -> String {
    format_iso8601(std::time::SystemTime::now())
}

/// Duplicates `eventlog.rs`'s private `format_iso8601`/`civil_from_days` —
/// that module is a different slice's file (see this crate's other
/// duplication notes in `flow::runner::env` for the same constraint).
fn format_iso8601(t: std::time::SystemTime) -> String {
    let dur = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---- run id / log filename ----

/// Timestamp-based and base36, so ids sort chronologically and are safe as
/// a directory name without escaping. The suffix loop covers two runs
/// landing in the same millisecond.
fn new_run_id(inner: &RunnerInner) -> String {
    let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let base = to_base36(millis);
    let mut id = base.clone();
    let mut n = 2;
    while inner.runs.contains_key(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
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

/// Node ids come out of a hand-editable JSON file and would otherwise
/// become a filename verbatim. Stripping everything but `[A-Za-z0-9._-]`
/// means no separator survives; the leading index keeps two ids that
/// sanitize to the same string from sharing a log.
fn log_name(node_id: &str, i: usize) -> String {
    let sanitized: String =
        node_id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect();
    format!("{}-{sanitized}.log", i + 1)
}

// ---- snapshot ----

fn node_snapshot(n: &NodeState, parents: &[String]) -> Value {
    json!({
        "id": n.id,
        "name": n.name,
        "kind": n.kind,
        "model": n.model,
        "status": n.status,
        "started": n.started,
        "ended": n.ended,
        "exit": n.exit,
        "log": n.log.to_string_lossy(),
        "parents": parents,
    })
}

fn run_snapshot(r: &RunState) -> Value {
    let nodes: Vec<Value> = r
        .nodes
        .iter()
        .map(|n| {
            let parents = r.plan.parents.get(&n.id).cloned().unwrap_or_default();
            node_snapshot(n, &parents)
        })
        .collect();
    json!({
        "id": r.id,
        "flow": r.flow,
        "flowPath": r.flow_path,
        "root": r.root.to_string_lossy(),
        "dir": r.dir.to_string_lossy(),
        "status": r.status,
        "canceling": r.canceling,
        "airgap": r.gapped,
        "started": r.started,
        "ended": r.ended,
        "layers": r.plan.layers,
        "nodes": nodes,
    })
}

/// Every run this session knows about, newest first — stable-sorted
/// descending by `started` (ISO stamps sort lexicographically), ties
/// broken by insertion order (see [`RunnerInner::order`]'s doc comment).
fn snapshot_all_locked(inner: &RunnerInner) -> Value {
    let mut list: Vec<Value> = inner.order.iter().filter_map(|id| inner.runs.get(id)).map(run_snapshot).collect();
    list.sort_by(|a, b| {
        let sa = a["started"].as_str().unwrap_or("");
        let sb = b["started"].as_str().unwrap_or("");
        sb.cmp(sa)
    });
    Value::Array(list)
}

pub fn snapshot_all(runs: &Runner) -> Value {
    let inner = runs.inner.lock().expect("Runner lock poisoned");
    snapshot_all_locked(&inner)
}

// ---- persist / push ----

fn persist(inner: &RunnerInner, run_id: &str) {
    if let Some(r) = inner.runs.get(run_id) {
        let text = serde_json::to_string_pretty(&run_snapshot(r)).unwrap_or_default() + "\n";
        let _ = r.write_tx.send(text);
    }
}

fn push(env: &RunnerEnv, inner: &RunnerInner) {
    (env.push)(snapshot_all_locked(inner));
}

fn persist_and_push(runs: &Runner, env: &RunnerEnv, run_id: &str) {
    let inner = runs.inner.lock().expect("Runner lock poisoned");
    persist(&inner, run_id);
    push(env, &inner);
}

/// Push only, no persist — [`cancel_run`]'s own final step mirrors the JS
/// original exactly here: `run.canceling = true` reaches the LIVE push
/// immediately, but `run.json` on disk only catches up once the next
/// `setStatus`/`settleIfDone` call runs (a skip, or a node's own exit) —
/// see `cancel_run`'s doc comment.
fn push_only(runs: &Runner, env: &RunnerEnv) {
    let inner = runs.inner.lock().expect("Runner lock poisoned");
    push(env, &inner);
}

/// The single writer of one run's `run.json` — a dedicated task draining
/// an ordered channel of already-serialized snapshots, so writes land on
/// disk in the order `persist` was CALLED (text captured synchronously
/// under the same lock as the mutation it follows) rather than the order
/// the underlying fs writes happen to finish.
fn spawn_run_json_writer(root: PathBuf, file: PathBuf, mut rx: tokio::sync::mpsc::UnboundedReceiver<String>) {
    tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if confine::confine_real_abs(&root, &file, false).await.is_some() {
                let _ = tokio::fs::write(&file, &text).await;
            }
        }
    });
}

// ---- start_run ----

/// Start a flow in the background. Returns `{ "id": .. }` once every node
/// is planned and the first layer is spawning, or `{ "error": .. }` —
/// refusals happen BEFORE anything is written or spawned.
pub async fn start_run(runs: Arc<Runner>, env: RunnerEnv, flow_path: String) -> Value {
    if !(env.can_open_file)(Path::new(&flow_path)) {
        return json!({"error": "flow is outside the open workspace folders"});
    }
    let root = PathBuf::from(model::flow_root(&flow_path));

    let raw = match tokio::fs::read_to_string(&flow_path).await {
        Ok(s) => s,
        Err(e) => return json!({"error": format!("could not read flow: {e}")}),
    };
    // Confined AFTER the read succeeds — a missing file must fail as
    // "could not read flow", not an escape refusal true of any missing
    // path. Confirms the file readFile actually followed is still really
    // inside root.
    if confine::confine_real_abs(&root, Path::new(&flow_path), true).await.is_none() {
        return json!({"error": "flow is outside the open workspace folders"});
    }

    let raw_value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("could not read flow: {e}")}),
    };
    let obj = raw_value.as_object();
    let name_ok = obj.and_then(|o| o.get("name")).and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    let nodes_ok = obj.and_then(|o| o.get("nodes")).map(Value::is_array).unwrap_or(false);
    let edges_ok = obj.and_then(|o| o.get("edges")).map(Value::is_array).unwrap_or(false);
    if !name_ok || !nodes_ok || !edges_ok {
        return json!({"error": "not a flow file"});
    }
    let flow: model::FlowDoc = match serde_json::from_value(raw_value) {
        Ok(f) => f,
        Err(_) => return json!({"error": "not a flow file"}),
    };

    let validation = model::validate_flow(&flow);
    if let Some(first) = validation.errors.first() {
        return json!({"error": first});
    }
    if flow.nodes.is_empty() {
        return json!({"error": "this flow has no nodes"});
    }
    let node_ids: Vec<String> = flow.nodes.iter().map(|n| n.id.clone()).collect();
    let edge_pairs: Vec<(String, String)> = flow.edges.iter().map(|e| (e.from.clone(), e.to.clone())).collect();
    let Some(plan) = run_plan::run_plan(&node_ids, &edge_pairs) else {
        return json!({"error": "flow has a cycle — cannot run"});
    };

    // Every command line is built BEFORE anything is spawned or written. A
    // flow with one node whose kind has no headless template is refused
    // WHOLE and by name.
    let node_by_id: HashMap<&str, &model::FlowNode> = flow.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut specs: HashMap<String, Vec<String>> = HashMap::new();
    for node_id in &plan.order {
        let node = node_by_id[node_id.as_str()];
        let brief = model::compose_bootstrap_prompt(&flow, node);
        match agent_spawn::build_headless_spawn(&node.kind, node.model.as_deref(), Some(&brief)) {
            Some(spawn_spec) => {
                let mut argv = vec![spawn_spec.cmd];
                argv.extend(spawn_spec.args);
                specs.insert(node_id.clone(), argv);
            }
            None => {
                let kind_label = if node.kind.is_empty() { "no kind".to_string() } else { node.kind.clone() };
                return json!({"error": format!(
                    "node \"{}\" ({kind_label}) can't run in the background — use Run in terminals",
                    node.display_name()
                )});
            }
        }
    }

    let id = {
        let inner = runs.inner.lock().expect("Runner lock poisoned");
        new_run_id(&inner)
    };
    let dir = root.join(".tome").join("flows").join(&flow.name).join("runs").join(&id);
    if confine::confine_real_abs(&root, &dir, false).await.is_none() {
        return json!({"error": "could not create the run folder: run folder escapes the workspace"});
    }
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return json!({"error": format!("could not create the run folder: {e}")});
    }

    let gapped = (env.airgap_default)().await;
    let started = now_iso8601();
    let nodes: Vec<NodeState> = plan
        .order
        .iter()
        .enumerate()
        .map(|(i, node_id)| {
            let node = node_by_id[node_id.as_str()];
            NodeState {
                id: node_id.clone(),
                name: node.display_name().to_string(),
                kind: node.kind.clone(),
                model: node.model.clone(),
                status: "pending".to_string(),
                started: None,
                ended: None,
                exit: None,
                log: dir.join(log_name(node_id, i)),
                inner_argv: specs.remove(node_id).unwrap_or_default(),
                pid: None,
                kill_fn: None,
                kill_timer: None,
            }
        })
        .collect();
    let statuses: HashMap<String, String> = nodes.iter().map(|n| (n.id.clone(), "pending".to_string())).collect();
    let node_count = nodes.len();

    let (write_tx, write_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    spawn_run_json_writer(root.clone(), dir.join("run.json"), write_rx);

    let state = RunState {
        id: id.clone(),
        flow: flow.name.clone(),
        flow_path: flow_path.clone(),
        root,
        dir,
        gapped,
        status: "running".to_string(),
        started,
        ended: None,
        canceling: false,
        plan,
        statuses,
        nodes,
        write_tx,
        scheduling_lock: Arc::new(tokio::sync::Mutex::new(())),
    };

    {
        let mut inner = runs.inner.lock().expect("Runner lock poisoned");
        inner.order.push(id.clone());
        inner.runs.insert(id.clone(), state);
    }

    (env.log_event)(
        "flow-run",
        vec![
            ("event".to_string(), json!("run")),
            ("run".to_string(), json!(id)),
            ("flow".to_string(), json!(flow.name)),
            ("status".to_string(), json!("running")),
            ("nodes".to_string(), json!(node_count)),
        ],
    );
    persist_and_push(&runs, &env, &id);

    pump(runs.clone(), env.clone(), id.clone()).await;

    json!({"id": id})
}

// ---- scheduling loop ----

/// One scheduling step: ask the plan what may happen given the statuses we
/// have, then make it happen. Re-entrant on purpose — every process exit
/// calls it again.
///
/// The JS original's doc comment claims re-entrant safety holds because a
/// node is marked `"running"` "synchronously, before anything is awaited,
/// so a second pass can never pick the same node twice" — true within ONE
/// sequential invocation on JS's single-threaded event loop, where nothing
/// else can run between that read and that write at all. It is NOT true
/// across two truly concurrent invocations on Tauri's real (multi-threaded)
/// tokio runtime: `launch()`'s own exit-await task (below) calls `pump`
/// again from a fresh `tokio::spawn`, so two upstream nodes of a fan-in
/// exiting around the same instant can run their own `pump` calls on two
/// different OS threads at once. Without the lock below, BOTH could read
/// `next_actions()` before either has marked the shared downstream node
/// "running", both would decide to start it, and both would call
/// `launch()` for it — a real duplicate spawn (two live processes for one
/// logical node, one `NodeState.pid` silently overwritten by whichever
/// finishes last so `cancel_run`/`kill_all` can only ever signal one of
/// them, and two truncating opens of the same log file). `scheduling_lock`
/// (per run, held only across the read-then-mark span below, never across
/// `launch()`'s own `.await`s) makes the read (`next_actions`) and the
/// write (marking every chosen node `"running"`/`"skipped"`) ONE atomic
/// critical section per run, so a second, concurrent `pump` call for the
/// SAME run always sees the first call's marks and picks nothing already
/// claimed. Boxed because `async fn` cannot recurse directly.
fn pump(runs: Arc<Runner>, env: RunnerEnv, id: String) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        let scheduling_lock = {
            let inner = runs.inner.lock().expect("Runner lock poisoned");
            let Some(r) = inner.runs.get(&id) else { return };
            r.scheduling_lock.clone()
        };

        // ---- atomic decide-and-mark critical section (see doc comment) ----
        let start: Vec<String> = {
            let _pump_guard = scheduling_lock.lock().await;
            let (start, skip) = {
                let inner = runs.inner.lock().expect("Runner lock poisoned");
                let Some(r) = inner.runs.get(&id) else { return };
                if r.ended.is_some() {
                    return;
                }
                let na = run_plan::next_actions(&r.plan, &r.statuses);
                (na.start, na.skip)
            };
            for skip_id in &skip {
                set_status(&runs, &env, &id, skip_id, "skipped", None);
            }
            for start_id in &start {
                set_status(&runs, &env, &id, start_id, "running", None);
            }
            start
            // `_pump_guard` drops here — released BEFORE `launch()` below,
            // so nodes within one layer (up to CONCURRENCY_CAP) still
            // launch concurrently; only the SELECTION is serialized, not
            // the (potentially slow: proxy setup, sandbox wrap) launch
            // itself.
        };

        for start_id in &start {
            launch(&runs, &env, &id, start_id).await;
        }

        // launch() resolves once the child is UP, not once it exits — a
        // node it started normally is still 'running' here, and the next
        // scheduling pass is that child's own exit handler's job. A
        // launch that FAILED settled its node right here instead, and no
        // exit is ever coming to re-enter the scheduler on its behalf:
        // re-enter now or the run stops dead with descendants stuck
        // 'pending' forever.
        let any_not_running = {
            let inner = runs.inner.lock().expect("Runner lock poisoned");
            let Some(r) = inner.runs.get(&id) else { return };
            start.iter().any(|nid| r.nodes.iter().find(|n| &n.id == nid).map(|n| n.status.as_str()) != Some("running"))
        };
        if any_not_running {
            pump(runs, env, id).await;
            return;
        }
        settle_if_done(&runs, &env, &id);
    })
}

async fn launch(runs: &Arc<Runner>, env: &RunnerEnv, run_id: &str, node_id: &str) {
    let pane_id = run_plan::run_pane_id(run_id, node_id);
    let (root, gapped, inner_argv, log_path, node_name, node_kind, node_model, node_started) = {
        let inner = runs.inner.lock().expect("Runner lock poisoned");
        let Some(r) = inner.runs.get(run_id) else { return };
        let Some(n) = r.nodes.iter().find(|n| n.id == node_id) else { return };
        (r.root.clone(), r.gapped, n.inner_argv.clone(), n.log.clone(), n.name.clone(), n.kind.clone(), n.model.clone(), n.started.clone())
    };

    let built = match (env.build_agent_env)(pane_id.clone(), gapped, inner_argv.clone()).await {
        Ok(b) => b,
        Err(msg) => {
            // Best-effort — a log this run cannot safely write to must
            // still fail the node, never the whole run.
            if let Some(confined) = confine::confine_real_abs(&root, &log_path, false).await {
                let _ = tokio::fs::write(&confined, format!("# could not prepare the agent environment: {msg}\n")).await;
            }
            set_status(runs, env, run_id, node_id, "failed", None);
            return;
        }
    };

    // Cancel can land while the proxy is coming up — never spawn into a
    // run the user has already stopped.
    let canceling = {
        let inner = runs.inner.lock().expect("Runner lock poisoned");
        inner.runs.get(run_id).map(|r| r.canceling).unwrap_or(true)
    };
    if canceling {
        (env.close_agent_env)(&pane_id);
        set_status(runs, env, run_id, node_id, "canceled", None);
        return;
    }

    let (spawn_cmd, spawn_args): (String, Vec<String>) = match &built.sandbox {
        None => (inner_argv[0].clone(), inner_argv[1..].to_vec()),
        Some(SandboxWrap::Prefix { cmd, args }) => {
            let mut a = args.clone();
            a.extend(inner_argv.iter().cloned());
            (cmd.clone(), a)
        }
        Some(SandboxWrap::Full { argv }) => (argv[0].clone(), argv[1..].to_vec()),
    };

    // Re-confined for the same reason as the write above — this is the
    // node's own about-to-be-live cwd, not a static vault.
    if confine::confine_real_abs(&root, &log_path, false).await.is_none() {
        (env.close_agent_env)(&pane_id);
        set_status(runs, env, run_id, node_id, "failed", None);
        return;
    }

    let model_suffix = node_model.as_deref().map(|m| format!(" · {m}")).unwrap_or_default();
    let header = format!("# {node_name} · {node_kind}{model_suffix} · {}\n", node_started.unwrap_or_default());
    let file = tokio::fs::File::create(&log_path).await.ok();
    let log: Arc<tokio::sync::Mutex<Option<tokio::fs::File>>> = Arc::new(tokio::sync::Mutex::new(file));
    write_to_log(&log, header.as_bytes()).await;

    // A SECOND cancel re-check, immediately before the real OS spawn.
    // JS's own `launch()` only checks once (above the sandbox-argv build)
    // because nothing between that check and its `spawn(...)` call ever
    // yields to the event loop — cancelRun literally cannot interleave in
    // that span there. This port's own confine re-check + log-file
    // open/write ARE real `.await` points a concurrent `cancel_run` call
    // (a different tokio task) genuinely can land inside, which would
    // otherwise spawn an agent into a run the UI already calls
    // 'canceled' — see this file's `never_spawns_into_a_run_cancelled_...`
    // test, which exercises the FIRST window; this closes the second one
    // the JS original never had to.
    let canceling = {
        let inner = runs.inner.lock().expect("Runner lock poisoned");
        inner.runs.get(run_id).map(|r| r.canceling).unwrap_or(true)
    };
    if canceling {
        write_to_log(&log, b"# canceled before starting\n").await;
        (env.close_agent_env)(&pane_id);
        set_status(runs, env, run_id, node_id, "canceled", None);
        return;
    }

    let req = spawn::SpawnRequest { cmd: spawn_cmd, args: spawn_args, cwd: root.clone(), env: built.env };
    match (env.spawn)(req) {
        spawn::SpawnOutcome::Failed(e) => {
            // A missing CLI arrives here (Rust's spawn fails synchronously
            // for ENOENT, unlike Node's async 'error' event — see
            // spawn.rs's own doc comment) — it goes in the log, where the
            // pane is already looking.
            let detail = if e.kind() == std::io::ErrorKind::NotFound { format!("ENOENT: {e}") } else { e.to_string() };
            write_to_log(&log, format!("# failed to start: {detail}\n").as_bytes()).await;
            (env.close_agent_env)(&pane_id);
            set_status(runs, env, run_id, node_id, "failed", None);
        }
        spawn::SpawnOutcome::Started(spawned) => {
            {
                let mut inner = runs.inner.lock().expect("Runner lock poisoned");
                if let Some(r) = inner.runs.get_mut(run_id) {
                    if let Some(n) = r.nodes.iter_mut().find(|n| n.id == node_id) {
                        n.pid = Some(spawned.pid);
                        n.kill_fn = Some(spawned.kill.clone());
                    }
                }
            }

            let stdout_task = spawned.stdout.map(|s| tokio::spawn(pipe_to_log(s, log.clone())));
            let stderr_task = spawned.stderr.map(|s| tokio::spawn(pipe_to_log(s, log.clone())));

            let runs2 = runs.clone();
            let env2 = env.clone();
            let run_id2 = run_id.to_string();
            let node_id2 = node_id.to_string();
            let pane_id2 = pane_id.clone();
            let log2 = log.clone();
            let exit_rx = spawned.exit;
            tokio::spawn(async move {
                let outcome = exit_rx.await.unwrap_or(spawn::ExitOutcome { code: None, signal: None });
                // "close", not "exit": wait for the stdio drains too, so
                // the log is complete before anything reads it.
                if let Some(t) = stdout_task {
                    let _ = t.await;
                }
                if let Some(t) = stderr_task {
                    let _ = t.await;
                }
                {
                    let mut inner = runs2.inner.lock().expect("Runner lock poisoned");
                    if let Some(r) = inner.runs.get_mut(&run_id2) {
                        if let Some(n) = r.nodes.iter_mut().find(|n| n.id == node_id2) {
                            if let Some(t) = n.kill_timer.take() {
                                t.abort();
                            }
                            n.pid = None;
                            n.kill_fn = None;
                        }
                    }
                }
                let trailer = match outcome.code {
                    Some(c) => format!("# exit {c}\n"),
                    None => format!("# exit signal {}\n", outcome.signal.map(|s| s.to_string()).unwrap_or_else(|| "unknown".to_string())),
                };
                {
                    let mut guard = log2.lock().await;
                    if let Some(mut f) = guard.take() {
                        use tokio::io::AsyncWriteExt;
                        let _ = f.write_all(trailer.as_bytes()).await;
                    }
                }
                (env2.close_agent_env)(&pane_id2);
                let canceling_now = {
                    let inner = runs2.inner.lock().expect("Runner lock poisoned");
                    inner.runs.get(&run_id2).map(|r| r.canceling).unwrap_or(false)
                };
                // Cancelled beats failed: a node we killed exits non-zero
                // by definition, and reporting that as a failure would
                // blame the flow for the user's own Cancel click.
                let status = if canceling_now { "canceled" } else if outcome.code == Some(0) { "done" } else { "failed" };
                set_status(&runs2, &env2, &run_id2, &node_id2, status, outcome.code);
                pump(runs2, env2, run_id2).await;
            });
        }
    }
}

async fn write_to_log(log: &Arc<tokio::sync::Mutex<Option<tokio::fs::File>>>, bytes: &[u8]) {
    use tokio::io::AsyncWriteExt;
    let mut guard = log.lock().await;
    if let Some(f) = guard.as_mut() {
        let _ = f.write_all(bytes).await;
    }
}

/// Both streams into one file, interleaved as the agent produced them —
/// raw byte piping (not line-buffered), mirroring Node's `.pipe()`.
async fn pipe_to_log(mut reader: Box<dyn tokio::io::AsyncRead + Unpin + Send>, log: Arc<tokio::sync::Mutex<Option<tokio::fs::File>>>) {
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 8192];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        write_to_log(&log, &buf[..n]).await;
    }
}

fn set_status(runs: &Runner, env: &RunnerEnv, run_id: &str, node_id: &str, status: &str, exit: Option<i32>) {
    let (flow_name, kind) = {
        let mut inner = runs.inner.lock().expect("Runner lock poisoned");
        let Some(r) = inner.runs.get_mut(run_id) else { return };
        let now = now_iso8601();
        let kind = {
            let Some(n) = r.nodes.iter_mut().find(|n| n.id == node_id) else { return };
            n.status = status.to_string();
            if status == "running" {
                n.started = Some(now.clone());
            } else if n.started.is_some() {
                // a skipped node never started, so it never ended
                n.ended = Some(now.clone());
            }
            if let Some(e) = exit {
                n.exit = Some(e);
            }
            n.kind.clone()
        };
        r.statuses.insert(node_id.to_string(), status.to_string());
        let flow_name = r.flow.clone();
        // `r`'s mutable borrow of `inner.runs` ends here (last use above)
        // — `persist`/`push` below need only a SHARED borrow of `inner`,
        // which NLL allows once `r` is no longer read.
        persist(&inner, run_id);
        push(env, &inner);
        (flow_name, kind)
    };

    // Identifiers only, never the brief or the agent's output.
    let mut fields = vec![
        ("event".to_string(), json!("node")),
        ("run".to_string(), json!(run_id)),
        ("flow".to_string(), json!(flow_name)),
        ("node".to_string(), json!(node_id)),
        ("agent".to_string(), json!(kind)),
        ("status".to_string(), json!(status)),
    ];
    if let Some(e) = exit {
        fields.push(("exit".to_string(), json!(e)));
    }
    (env.log_event)("flow-run", fields);
}

fn settle_if_done(runs: &Runner, env: &RunnerEnv, run_id: &str) {
    let settled = {
        let mut inner = runs.inner.lock().expect("Runner lock poisoned");
        let Some(r) = inner.runs.get_mut(run_id) else { return };
        if r.ended.is_some() {
            return;
        }
        if r.nodes.iter().any(|n| n.status == "pending" || n.status == "running") {
            return;
        }
        // Derived from the nodes, never tracked alongside them, so a run
        // can never claim 'done' with a failed node in it.
        let derived = run_plan::run_status(&r.statuses);
        let final_status = if r.canceling && derived == "done" { "canceled" } else { derived };
        r.status = final_status.to_string();
        r.ended = Some(now_iso8601());
        let flow_name = r.flow.clone();
        // `r`'s mutable borrow ends here — see set_status's identical note.
        persist(&inner, run_id);
        push(env, &inner);
        Some((flow_name, final_status.to_string()))
    };
    let Some((flow_name, final_status)) = settled else { return };
    (env.log_event)(
        "flow-run",
        vec![
            ("event".to_string(), json!("run")),
            ("run".to_string(), json!(run_id)),
            ("flow".to_string(), json!(flow_name)),
            ("status".to_string(), json!(final_status)),
        ],
    );
}

// ---- cancel / kill ----

/// Signal the node's whole PROCESS GROUP first, not just the process we
/// spawned — an agent CLI's own tool calls are grandchildren, and a
/// signal to the CLI alone leaves those running long after the run says
/// 'canceled'. Falls back to the single-pid `kill_fn` only when the
/// process-group signal itself errors (no such process/group — including
/// a child that failed to spawn at all, or one whose group has already
/// died).
fn signal_pid_group_or_fallback(pid: i32, kill_fn: Option<Arc<dyn Fn(i32) -> std::io::Result<()> + Send + Sync>>, sig: i32) {
    if spawn::signal_pid(-pid, sig).is_err() {
        if let Some(f) = kill_fn {
            let _ = f(sig);
        }
    }
}

/// Stop a run: nodes that are up get SIGTERM (then SIGKILL after 5s),
/// everything still waiting is written off, nothing new starts.
///
/// Mirrors the JS original's own asymmetry: `run.canceling = true` reaches
/// the LIVE `push` unconditionally (below), but `run.json` on disk is only
/// re-persisted here indirectly, through whichever `set_status` calls this
/// function happens to trigger (skipping a pending node) — a run with
/// nothing pending to skip leaves `run.json` lagging `canceling: true`
/// until the next real transition (a node's own exit). Not "fixed" to
/// always persist: that would be a deliberate behavior change from the
/// spec this ports, not a bug fix.
pub fn cancel_run(runs: &Arc<Runner>, env: &RunnerEnv, id: &str) -> Value {
    let (flow_name, pending, live) = {
        let mut inner = runs.inner.lock().expect("Runner lock poisoned");
        let Some(r) = inner.runs.get_mut(id) else {
            return json!({"error": "no such run"});
        };
        // already finished — cancelling is a no-op, not an error.
        // Idempotent: a second click while children are still dying must
        // not re-send SIGTERM or start a second kill timer.
        if r.ended.is_some() || r.canceling {
            return json!({"ok": true});
        }
        r.canceling = true;
        let pending: Vec<String> = r.nodes.iter().filter(|n| n.status == "pending").map(|n| n.id.clone()).collect();
        let live: Vec<(String, i32, Option<Arc<dyn Fn(i32) -> std::io::Result<()> + Send + Sync>>)> =
            r.nodes.iter().filter_map(|n| n.pid.map(|pid| (n.id.clone(), pid, n.kill_fn.clone()))).collect();
        (r.flow.clone(), pending, live)
    };

    (env.log_event)(
        "flow-run",
        vec![("event".to_string(), json!("cancel")), ("run".to_string(), json!(id)), ("flow".to_string(), json!(flow_name))],
    );

    // Downstream first: a node that never started is 'skipped', which
    // stops the next pump (fired by the exit of the child about to be
    // killed) from starting anything else.
    for node_id in &pending {
        set_status(runs, env, id, node_id, "skipped", None);
    }
    for (node_id, pid, kill_fn) in live {
        signal_pid_group_or_fallback(pid, kill_fn, spawn::SIGTERM);
        arm_kill_timer(runs.clone(), id.to_string(), node_id.clone(), env.kill_grace);
    }

    // Nothing was running, so no exit handler is coming to settle this run.
    settle_if_done(runs, env, id);
    // `canceling` is in this snapshot, and the row renders it as
    // 'canceling…' with the button disabled — not as a button that
    // vanishes off a row still reading 'running' for the five-second
    // grace period.
    push_only(runs, env);
    json!({"ok": true})
}

fn arm_kill_timer(runs: Arc<Runner>, run_id: String, node_id: String, grace: std::time::Duration) {
    let runs_for_timer = runs.clone();
    let run_id_for_timer = run_id.clone();
    let node_id_for_timer = node_id.clone();
    // SIGTERM is a request an agent CLI mid-tool-call is free to ignore.
    // After this long the process is taken out of the user's machine's
    // hands rather than left running invisibly behind a run whose UI
    // already says 'canceled'.
    let handle = tokio::spawn(async move {
        tokio::time::sleep(grace).await;
        // Re-read the node's live pid at fire time, so a node that
        // exited inside the grace period is a no-op rather than a
        // signal aimed at a pid the OS may since have handed to
        // somebody else.
        let (pid, kill_fn) = {
            let inner = runs_for_timer.inner.lock().expect("Runner lock poisoned");
            let Some(r) = inner.runs.get(&run_id_for_timer) else { return };
            let Some(n) = r.nodes.iter().find(|n| n.id == node_id_for_timer) else { return };
            (n.pid, n.kill_fn.clone())
        };
        if let Some(pid) = pid {
            signal_pid_group_or_fallback(pid, kill_fn, spawn::SIGKILL);
        }
    });
    let mut inner = runs.inner.lock().expect("Runner lock poisoned");
    if let Some(r) = inner.runs.get_mut(&run_id) {
        if let Some(n) = r.nodes.iter_mut().find(|n| n.id == node_id) {
            n.kill_timer = Some(handle.abort_handle());
        }
    }
}

/// On the way out — hooked into the quit handshake the same way
/// `will-quit`/`window-all-closed` call `flowRunner.killAll()` in the JS
/// original (`lib.rs`'s `CloseRequested` handler calls this alongside
/// `shutdown_all_proxies`; idempotent, so being called twice costs a dead
/// signal). A background agent must not outlive the app that launched it.
/// SIGKILL rather than the polite path: the process is seconds from
/// exiting and there is nobody left to wait for a graceful shutdown. No
/// persist/push at all — the app is on its way down and there is nowhere
/// left to deliver either.
pub fn kill_all(runs: &Runner) {
    let mut inner = runs.inner.lock().expect("Runner lock poisoned");
    let ids = inner.order.clone();
    for id in ids {
        if let Some(r) = inner.runs.get_mut(&id) {
            r.canceling = true;
            for n in &r.nodes {
                if let Some(pid) = n.pid {
                    signal_pid_group_or_fallback(pid, n.kill_fn.clone(), spawn::SIGKILL);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
