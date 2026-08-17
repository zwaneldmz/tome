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

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Value};

use self::env::{RunnerEnv, SandboxWrap};
use super::{confine, model, products, run_plan};
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
    /// Declared output names (`node.outputs[].name`, the literal string
    /// "undefined" for an unnamed one — mirrors `compose_bootstrap_prompt`'s
    /// own fallback, which in turn mirrors the JS twin's raw, un-fallback'd
    /// template-literal interpolation byte for byte), filled in at plan
    /// time. What the exit-await closure's fail-closed output contract
    /// checks against once this node exits 0 — see `launch`'s doc comment
    /// on that check. Load-bearing that this matches `compose_bootstrap_
    /// prompt` exactly: this is the same string the composed brief actually
    /// told the agent to write its output to, so a divergence here would
    /// have the contract check a different filename than the one the agent
    /// was ever instructed to create.
    outputs: Vec<String>,
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
    /// `dir.join("artifacts")` — where this run's nodes hand off outputs
    /// (`.tome/flows/<flow>/runs/<id>/artifacts`), created alongside `dir`
    /// at `start_run` time. Absolute, like `dir`/`root`; the ROOT-RELATIVE
    /// string every composed brief actually embeds is built once, before
    /// this even exists on disk, from the same `id` (see `start_run`).
    artifacts_dir: PathBuf,
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
    /// The promoted-products list — `None` until (and unless) this run
    /// settles `"done"` AND its own background promotion
    /// ([`products::promote_and_manifest`], spawned from
    /// [`settle_if_done`]) finishes; stays `None` forever for any run that
    /// settles any other way, and also on a promotion FAILURE (see
    /// [`spawn_promotion`]'s doc comment — promotion failing never touches
    /// `status`, so this is the only observable trace of that failure
    /// besides the logged event). One entry per terminal-node output
    /// actually copied into `out/<id>/` once set — see `flow::products`'s
    /// module doc comment for the full shape.
    products: Option<Value>,
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
    /// Ids [`new_run_id`] has minted but whose `RunState` isn't in `runs`
    /// yet — see [`ReservedId`]'s doc comment for the race this closes.
    reserved_ids: HashSet<String>,
}

/// The live run registry — `AppState.flow`. Every run this session has
/// started stays in memory for the process's lifetime (mirrors JS's
/// module-level `const runs = new Map()`, never pruned).
pub struct Runner {
    inner: std::sync::Mutex<RunnerInner>,
}

impl Runner {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(RunnerInner {
                order: Vec::new(),
                runs: HashMap::new(),
                reserved_ids: HashSet::new(),
            }),
        }
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
/// landing in the same millisecond — checked against both `inner.runs`
/// (ids already registered) AND `inner.reserved_ids` (ids a concurrent
/// `start_run` call has minted but not registered yet — see
/// [`ReservedId`]'s doc comment): a caller MUST hold `runs.inner`'s lock
/// across both this call and the matching `reserved_ids.insert`, or the
/// whole point of checking the reservation set here is lost to the same
/// check-then-act gap this closes in the registered-ids case.
fn new_run_id(inner: &RunnerInner) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let base = to_base36(millis);
    let mut id = base.clone();
    let mut n = 2;
    while inner.runs.contains_key(&id) || inner.reserved_ids.contains(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// Holds `id` in `runs.inner.reserved_ids` for as long as this guard is
/// alive, releasing it on drop — whichever way `start_run` leaves: the
/// common path (the real `RunState` lands in `inner.runs`, making the
/// reservation redundant — dropping it right after is a no-op, never a
/// gap, since the insert always happens first) or any of its own several
/// early refusals between minting `id` and that insert (a node kind with
/// no headless template, a confine failure, a mkdir failure).
///
/// Exists because `new_run_id` only checks collision against ids already
/// in `inner.runs`/`inner.reserved_ids` at the INSTANT it is called —
/// `start_run` mints `id` under a lock it releases immediately, then
/// crosses several real `.await` points (confining and creating the run
/// dir and artifacts dir, reading the airgap-default preference) before
/// the matching `inner.runs.insert` far below. No caller serializes
/// concurrent `start_run` invocations (an ordinary click on Run in the UI
/// and the 30s schedule ticker are two independent async tasks on the same
/// tokio runtime, gated only by `lock_gate` — a locked-app check, not a
/// mutex) — without a reservation held across that whole span, two calls
/// minting in the same millisecond would both compute the identical id,
/// and the second `inner.runs.insert` would silently clobber the first
/// run's live registry entry (its exit-await task would keep updating a
/// `RunState` nothing can look up by that id anymore).
struct ReservedId {
    runs: Arc<Runner>,
    id: String,
}

impl Drop for ReservedId {
    fn drop(&mut self) {
        let mut inner = self.runs.inner.lock().expect("Runner lock poisoned");
        inner.reserved_ids.remove(&self.id);
    }
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
    let sanitized: String = node_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
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
        "terminals": r.plan.terminals,
        "nodes": nodes,
        "products": r.products,
    })
}

/// Every run this session knows about, newest first — stable-sorted
/// descending by `started` (ISO stamps sort lexicographically), ties
/// broken by insertion order (see [`RunnerInner::order`]'s doc comment).
fn snapshot_all_locked(inner: &RunnerInner) -> Value {
    let mut list: Vec<Value> = inner
        .order
        .iter()
        .filter_map(|id| inner.runs.get(id))
        .map(run_snapshot)
        .collect();
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
fn spawn_run_json_writer(
    root: PathBuf,
    file: PathBuf,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if confine::confine_real_abs(&root, &file, false)
                .await
                .is_some()
            {
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
    if confine::confine_real_abs(&root, Path::new(&flow_path), true)
        .await
        .is_none()
    {
        return json!({"error": "flow is outside the open workspace folders"});
    }

    let raw_value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("could not read flow: {e}")}),
    };
    let obj = raw_value.as_object();
    let name_ok = obj
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    let nodes_ok = obj
        .and_then(|o| o.get("nodes"))
        .map(Value::is_array)
        .unwrap_or(false);
    let edges_ok = obj
        .and_then(|o| o.get("edges"))
        .map(Value::is_array)
        .unwrap_or(false);
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
    let edge_pairs: Vec<(String, String)> = flow
        .edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();
    let Some(plan) = run_plan::run_plan(&node_ids, &edge_pairs) else {
        return json!({"error": "flow has a cycle — cannot run"});
    };

    // Minted AND reserved under the SAME lock acquisition — see
    // `ReservedId`'s doc comment for the concurrent-`start_run` race this
    // closes. Every node's composed brief embeds this run's own artifacts
    // directory (below), which needs `id` before any brief can be built.
    // Harmless to mint this early even on a refusal further down: nothing
    // is inserted into the VISIBLE registry until the run is actually
    // accepted, and `_reserved`'s `Drop` releases the reservation the
    // moment this function returns, whichever way it does.
    let id = {
        let mut inner = runs.inner.lock().expect("Runner lock poisoned");
        let id = new_run_id(&inner);
        inner.reserved_ids.insert(id.clone());
        id
    };
    let _reserved = ReservedId {
        runs: runs.clone(),
        id: id.clone(),
    };
    // ROOT-RELATIVE — a headless node's spawn cwd is `root` itself (below),
    // so this is exactly the string `compose_bootstrap_prompt` embeds in
    // every handoff path. Run-scoped so two runs of the same flow, or a
    // background run racing a terminal-mode one, never contend for the same
    // handoff file.
    let artifacts_dir_rel = format!(".tome/flows/{}/runs/{id}/artifacts", flow.name);

    // Every command line is built BEFORE anything is spawned or written. A
    // flow with one node whose kind has no headless template is refused
    // WHOLE and by name.
    let node_by_id: HashMap<&str, &model::FlowNode> =
        flow.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut specs: HashMap<String, Vec<String>> = HashMap::new();
    for node_id in &plan.order {
        let node = node_by_id[node_id.as_str()];
        let brief = model::compose_bootstrap_prompt(&flow, node, &artifacts_dir_rel);
        match agent_spawn::build_headless_spawn(&node.kind, node.model.as_deref(), Some(&brief)) {
            Some(spawn_spec) => {
                let mut argv = vec![spawn_spec.cmd];
                argv.extend(spawn_spec.args);
                specs.insert(node_id.clone(), argv);
            }
            None => {
                let kind_label = if node.kind.is_empty() {
                    "no kind".to_string()
                } else {
                    node.kind.clone()
                };
                return json!({"error": format!(
                    "node \"{}\" ({kind_label}) can't run in the background — use Run in terminals",
                    node.display_name()
                )});
            }
        }
    }

    let dir = root
        .join(".tome")
        .join("flows")
        .join(&flow.name)
        .join("runs")
        .join(&id);
    if confine::confine_real_abs(&root, &dir, false)
        .await
        .is_none()
    {
        return json!({"error": "could not create the run folder: run folder escapes the workspace"});
    }
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return json!({"error": format!("could not create the run folder: {e}")});
    }
    let artifacts_dir = dir.join("artifacts");
    if confine::confine_real_abs(&root, &artifacts_dir, false)
        .await
        .is_none()
    {
        return json!({"error": "could not create the run folder: artifacts folder escapes the workspace"});
    }
    if let Err(e) = tokio::fs::create_dir_all(&artifacts_dir).await {
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
                outputs: node
                    .outputs
                    .iter()
                    .map(|o| o.name.clone().unwrap_or_else(|| "undefined".to_string()))
                    .collect(),
                inner_argv: specs.remove(node_id).unwrap_or_default(),
                pid: None,
                kill_fn: None,
                kill_timer: None,
            }
        })
        .collect();
    let statuses: HashMap<String, String> = nodes
        .iter()
        .map(|n| (n.id.clone(), "pending".to_string()))
        .collect();
    let node_count = nodes.len();

    let (write_tx, write_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    spawn_run_json_writer(root.clone(), dir.join("run.json"), write_rx);

    let state = RunState {
        id: id.clone(),
        flow: flow.name.clone(),
        flow_path: flow_path.clone(),
        root,
        dir,
        artifacts_dir,
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
        products: None,
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
            start.iter().any(|nid| {
                r.nodes
                    .iter()
                    .find(|n| &n.id == nid)
                    .map(|n| n.status.as_str())
                    != Some("running")
            })
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
    let (
        root,
        gapped,
        inner_argv,
        log_path,
        node_name,
        node_kind,
        node_model,
        node_started,
        artifacts_dir,
        node_outputs,
    ) = {
        let inner = runs.inner.lock().expect("Runner lock poisoned");
        let Some(r) = inner.runs.get(run_id) else {
            return;
        };
        let Some(n) = r.nodes.iter().find(|n| n.id == node_id) else {
            return;
        };
        (
            r.root.clone(),
            r.gapped,
            n.inner_argv.clone(),
            n.log.clone(),
            n.name.clone(),
            n.kind.clone(),
            n.model.clone(),
            n.started.clone(),
            r.artifacts_dir.clone(),
            n.outputs.clone(),
        )
    };

    let built = match (env.build_agent_env)(pane_id.clone(), gapped, inner_argv.clone()).await {
        Ok(b) => b,
        Err(msg) => {
            // Best-effort — a log this run cannot safely write to must
            // still fail the node, never the whole run.
            if let Some(confined) = confine::confine_real_abs(&root, &log_path, false).await {
                let _ = tokio::fs::write(
                    &confined,
                    format!("# could not prepare the agent environment: {msg}\n"),
                )
                .await;
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
    if confine::confine_real_abs(&root, &log_path, false)
        .await
        .is_none()
    {
        (env.close_agent_env)(&pane_id);
        set_status(runs, env, run_id, node_id, "failed", None);
        return;
    }

    let model_suffix = node_model
        .as_deref()
        .map(|m| format!(" · {m}"))
        .unwrap_or_default();
    let header = format!(
        "# {node_name} · {node_kind}{model_suffix} · {}\n",
        node_started.unwrap_or_default()
    );
    let file = tokio::fs::File::create(&log_path).await.ok();
    let log: Arc<tokio::sync::Mutex<Option<tokio::fs::File>>> =
        Arc::new(tokio::sync::Mutex::new(file));
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

    let req = spawn::SpawnRequest {
        cmd: spawn_cmd,
        args: spawn_args,
        cwd: root.clone(),
        env: built.env,
    };
    match (env.spawn)(req) {
        spawn::SpawnOutcome::Failed(e) => {
            // A missing CLI arrives here (Rust's spawn fails synchronously
            // for ENOENT, unlike Node's async 'error' event — see
            // spawn.rs's own doc comment) — it goes in the log, where the
            // pane is already looking.
            let detail = if e.kind() == std::io::ErrorKind::NotFound {
                format!("ENOENT: {e}")
            } else {
                e.to_string()
            };
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

            let stdout_task = spawned
                .stdout
                .map(|s| tokio::spawn(pipe_to_log(s, log.clone())));
            let stderr_task = spawned
                .stderr
                .map(|s| tokio::spawn(pipe_to_log(s, log.clone())));

            let runs2 = runs.clone();
            let env2 = env.clone();
            let run_id2 = run_id.to_string();
            let node_id2 = node_id.to_string();
            let pane_id2 = pane_id.clone();
            let log2 = log.clone();
            let root2 = root.clone();
            let artifacts_dir2 = artifacts_dir.clone();
            let node_outputs2 = node_outputs.clone();
            let exit_rx = spawned.exit;
            tokio::spawn(async move {
                let outcome = exit_rx.await.unwrap_or(spawn::ExitOutcome {
                    code: None,
                    signal: None,
                });
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
                // The fail-closed output contract: an exit-0 process still
                // hasn't kept its side of the flow unless every output it
                // DECLARED actually landed on disk with real content — the
                // runner is the only reader positioned to check before a
                // downstream node's own composed brief promises a file that
                // was never really written (or was left a stale empty
                // truncation from a crashed earlier attempt). Skipped
                // entirely on a non-zero/signal exit: the process already
                // failed on its own terms, and a second reason on top would
                // only bury the one that matters in the log. `contract_lines`
                // becoming non-empty is the ONLY new way a node's exit code
                // can read `Some(0)` and still settle "failed" — see the
                // status computation below.
                let mut contract_lines: Vec<String> = Vec::new();
                if outcome.code == Some(0) {
                    for name in &node_outputs2 {
                        let abs = artifacts_dir2.join(format!("{node_id2}-{name}.md"));
                        // `must_exist: false` — the whole point is that the
                        // file may not exist at all; this still validates
                        // every ANCESTOR (the artifacts dir itself) is real
                        // and inside `root2`, so a symlink swapped in after
                        // `start_run` created it can't be used to claim a
                        // write that never really landed inside the run.
                        let len = match confine::confine_real_abs(&root2, &abs, false).await {
                            Some(confined) => {
                                tokio::fs::metadata(&confined).await.ok().map(|m| m.len())
                            }
                            None => None,
                        };
                        if len.unwrap_or(0) == 0 {
                            let rel = artifacts_dir2
                                .strip_prefix(&root2)
                                .unwrap_or(&artifacts_dir2)
                                .to_string_lossy()
                                .into_owned();
                            let path = model::handoff_path(&rel, &node_id2, name);
                            contract_lines.push(format!(
                                r#"# contract: missing or empty output "{name}" ({path})"#
                            ));
                        }
                    }
                }

                let trailer = match outcome.code {
                    Some(c) => format!("# exit {c}\n"),
                    None => format!(
                        "# exit signal {}\n",
                        outcome
                            .signal
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "unknown".to_string())
                    ),
                };
                {
                    let mut guard = log2.lock().await;
                    if let Some(mut f) = guard.take() {
                        use tokio::io::AsyncWriteExt;
                        // Contract lines first, exit trailer last — the log
                        // reads as "here's what's wrong, here's how it
                        // ended", never the other way around.
                        for line in &contract_lines {
                            let _ = f.write_all(line.as_bytes()).await;
                            let _ = f.write_all(b"\n").await;
                        }
                        let _ = f.write_all(trailer.as_bytes()).await;
                        // Flush before `f` drops (and before `set_status`
                        // below flips this node's status and notifies
                        // listeners) — same reasoning as `write_to_log`'s
                        // own flush: a reader that opens the log the moment
                        // it observes the status flip (the runs pane's log
                        // tail, a remote `ssh cat`, a tight polling test)
                        // must not find a log with neither the contract
                        // line nor the exit trailer just because the write
                        // was still sitting in tokio's buffer when the flip
                        // woke it — a lost race on a loaded CI runner, and
                        // exactly the "why did this fail? the log says
                        // nothing" outcome the fail-closed contract exists
                        // to prevent.
                        let _ = f.flush().await;
                    }
                }
                (env2.close_agent_env)(&pane_id2);
                let canceling_now = {
                    let inner = runs2.inner.lock().expect("Runner lock poisoned");
                    inner
                        .runs
                        .get(&run_id2)
                        .map(|r| r.canceling)
                        .unwrap_or(false)
                };
                // Cancelled beats failed: a node we killed exits non-zero
                // by definition, and reporting that as a failure would
                // blame the flow for the user's own Cancel click.
                let status = if canceling_now {
                    "canceled"
                } else if outcome.code == Some(0) && contract_lines.is_empty() {
                    "done"
                } else {
                    "failed"
                };
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
        // Flush so a reader that opens the log the moment the run settles
        // (the runner's own tests do exactly this) sees the failure line —
        // without it the write can still be in tokio's buffer when the
        // status flip wakes the reader, which is a lost race on a loaded
        // CI runner.
        let _ = f.flush().await;
    }
}

/// Both streams into one file, interleaved as the agent produced them —
/// raw byte piping (not line-buffered), mirroring Node's `.pipe()`.
async fn pipe_to_log(
    mut reader: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    log: Arc<tokio::sync::Mutex<Option<tokio::fs::File>>>,
) {
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

fn set_status(
    runs: &Runner,
    env: &RunnerEnv,
    run_id: &str,
    node_id: &str,
    status: &str,
    exit: Option<i32>,
) {
    let (flow_name, kind) = {
        let mut inner = runs.inner.lock().expect("Runner lock poisoned");
        let Some(r) = inner.runs.get_mut(run_id) else {
            return;
        };
        let now = now_iso8601();
        let kind = {
            let Some(n) = r.nodes.iter_mut().find(|n| n.id == node_id) else {
                return;
            };
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

fn settle_if_done(runs: &Arc<Runner>, env: &RunnerEnv, run_id: &str) {
    let settled = {
        let mut inner = runs.inner.lock().expect("Runner lock poisoned");
        let Some(r) = inner.runs.get_mut(run_id) else {
            return;
        };
        if r.ended.is_some() {
            return;
        }
        if r.nodes
            .iter()
            .any(|n| n.status == "pending" || n.status == "running")
        {
            return;
        }
        // Derived from the nodes, never tracked alongside them, so a run
        // can never claim 'done' with a failed node in it.
        let derived = run_plan::run_status(&r.statuses);
        let final_status = if r.canceling && derived == "done" {
            "canceled"
        } else {
            derived
        };
        r.status = final_status.to_string();
        r.ended = Some(now_iso8601());
        let flow_name = r.flow.clone();
        // Gathered now, while `r` is still in hand, rather than a second
        // registry lookup once this block ends — plain owned data is all
        // `flow::products::promote_and_manifest` needs to do its whole job
        // (see that module's doc comment on staying runner-agnostic).
        // `None` for anything but a `"done"` settlement: a canceled or
        // failed run has nothing worth promoting, and `products.rs`'s own
        // binding decision is explicit that this only ever runs on
        // success.
        let promote_req = (final_status == "done").then(|| {
            let mut terminal_outputs = Vec::new();
            for tid in &r.plan.terminals {
                if let Some(n) = r.nodes.iter().find(|n| &n.id == tid) {
                    for name in &n.outputs {
                        terminal_outputs.push(products::TerminalOutput {
                            node_id: n.id.clone(),
                            output_name: name.clone(),
                        });
                    }
                }
            }
            products::PromoteRequest {
                root: r.root.clone(),
                flow_name: r.flow.clone(),
                flow_path: PathBuf::from(r.flow_path.clone()),
                run_id: run_id.to_string(),
                started: r.started.clone(),
                ended: r.ended.clone().expect("just set above"),
                airgap: r.gapped,
                artifacts_dir: r.artifacts_dir.clone(),
                nodes: r
                    .nodes
                    .iter()
                    .map(|n| products::ManifestNode {
                        id: n.id.clone(),
                        kind: n.kind.clone(),
                        model: n.model.clone(),
                        status: n.status.clone(),
                        exit: n.exit,
                        started: n.started.clone(),
                        ended: n.ended.clone(),
                    })
                    .collect(),
                terminal_outputs,
            }
        });
        // `r`'s mutable borrow ends here — see set_status's identical note.
        persist(&inner, run_id);
        push(env, &inner);
        Some((flow_name, final_status.to_string(), promote_req))
    };
    let Some((flow_name, final_status, promote_req)) = settled else {
        return;
    };
    (env.log_event)(
        "flow-run",
        vec![
            ("event".to_string(), json!("run")),
            ("run".to_string(), json!(run_id)),
            ("flow".to_string(), json!(flow_name)),
            ("status".to_string(), json!(final_status)),
        ],
    );
    if let Some(req) = promote_req {
        spawn_promotion(runs.clone(), env.clone(), run_id.to_string(), req);
    }
}

/// Runs after a run settles `"done"` — [`products::promote_and_manifest`]
/// copies each terminal node's declared outputs into `out/<id>/`, writes
/// `manifest.json`, refreshes `out/latest/`, and appends `runs-index.json`
/// (see that module's own doc comment for the full contract). Detached
/// from `settle_if_done` itself (a `tokio::spawn`, never awaited inline):
/// the run has already settled and been pushed by the time this is called,
/// and promotion's own file IO — usually small, but not bounded — is not
/// something any caller of `settle_if_done` (`pump`'s own exit-await
/// chain among them) should have to block behind.
///
/// Its own failure never reaches back to `RunState.status`: on `Err` this
/// only logs an event and leaves `RunState.products` at its default
/// `None` ("null" over the wire, exactly as if promotion had never run at
/// all) — a run that finished on its own terms must not be retroactively
/// reported as failed by bookkeeping layered on top of it. On `Ok`, this
/// is the second `persist`/`push` the binding decision calls for: the
/// first (inside `settle_if_done`, above) already put `"done"` in front of
/// the user; this one is what makes `run.json` and the renderer catch up
/// with the final product list.
fn spawn_promotion(
    runs: Arc<Runner>,
    env: RunnerEnv,
    run_id: String,
    req: products::PromoteRequest,
) {
    tokio::spawn(async move {
        match products::promote_and_manifest(req).await {
            Ok(products_value) => {
                {
                    let mut inner = runs.inner.lock().expect("Runner lock poisoned");
                    if let Some(r) = inner.runs.get_mut(&run_id) {
                        r.products = Some(products_value);
                    }
                }
                persist_and_push(&runs, &env, &run_id);
            }
            Err(msg) => {
                (env.log_event)(
                    "flow-run",
                    vec![
                        ("event".to_string(), json!("products")),
                        ("run".to_string(), json!(run_id)),
                        ("status".to_string(), json!("failed")),
                        ("error".to_string(), json!(msg)),
                    ],
                );
            }
        }
    });
}

// ---- cancel / kill ----

/// Signal the node's whole PROCESS GROUP first, not just the process we
/// spawned — an agent CLI's own tool calls are grandchildren, and a
/// signal to the CLI alone leaves those running long after the run says
/// 'canceled'. Falls back to the single-pid `kill_fn` only when the
/// process-group signal itself errors (no such process/group — including
/// a child that failed to spawn at all, or one whose group has already
/// died).
fn signal_pid_group_or_fallback(
    pid: i32,
    kill_fn: Option<Arc<dyn Fn(i32) -> std::io::Result<()> + Send + Sync>>,
    sig: i32,
) {
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
        let pending: Vec<String> = r
            .nodes
            .iter()
            .filter(|n| n.status == "pending")
            .map(|n| n.id.clone())
            .collect();
        let live: Vec<(
            String,
            i32,
            Option<Arc<dyn Fn(i32) -> std::io::Result<()> + Send + Sync>>,
        )> = r
            .nodes
            .iter()
            .filter_map(|n| n.pid.map(|pid| (n.id.clone(), pid, n.kill_fn.clone())))
            .collect();
        (r.flow.clone(), pending, live)
    };

    (env.log_event)(
        "flow-run",
        vec![
            ("event".to_string(), json!("cancel")),
            ("run".to_string(), json!(id)),
            ("flow".to_string(), json!(flow_name)),
        ],
    );

    // Downstream first: a node that never started is 'skipped', which
    // stops the next pump (fired by the exit of the child about to be
    // killed) from starting anything else.
    for node_id in &pending {
        set_status(runs, env, id, node_id, "skipped", None);
    }
    for (node_id, pid, kill_fn) in live {
        signal_pid_group_or_fallback(pid, kill_fn, spawn::SIGTERM);
        arm_kill_timer(
            runs.clone(),
            id.to_string(),
            node_id.clone(),
            env.kill_grace,
        );
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
            let Some(r) = inner.runs.get(&run_id_for_timer) else {
                return;
            };
            let Some(n) = r.nodes.iter().find(|n| n.id == node_id_for_timer) else {
                return;
            };
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
