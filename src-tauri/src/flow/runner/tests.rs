//! Drives `flow::runner` end to end — real child processes, real log
//! files, real `run.json` — with one substitution: the injected
//! [`super::env::RunnerEnv::spawn`] runs a shell script instead of an agent
//! CLI. NEVER a real agent here. Ports `test/flow-runner.test.js`'s
//! meaningful assertions using the injected-spawn seam (mirrors that
//! file's own `install()`/`stubSpawn()`/`fakeChild()`/`settled()`/
//! `appears()` helpers).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::env;
use super::spawn;
use super::{cancel_run, kill_all, snapshot_all, start_run, Runner};

// ---- fixtures ----

fn workspace() -> PathBuf {
    let root = tempfile::Builder::new().prefix("tome-runs-").tempdir().unwrap().keep();
    std::fs::create_dir_all(root.join(".tome").join("flows")).unwrap();
    root
}

fn tmp_outside() -> PathBuf {
    tempfile::Builder::new().prefix("tome-runs-outside-").tempdir().unwrap().keep()
}

fn flow_doc(name: &str, ids: &[&str], pairs: &[(&str, &str)]) -> Value {
    json!({
        "version": 1,
        "name": name,
        "nodes": ids.iter().map(|id| json!({
            "id": id, "name": id, "kind": "claude", "instructions": format!("do {id}"),
            "outputs": [{"name": "out"}], "inputs": [{"name": "in"}],
        })).collect::<Vec<_>>(),
        "edges": pairs.iter().enumerate().map(|(i, (from, to))| json!({
            "id": format!("e{}", i + 1), "from": from, "to": to, "fromOutput": "out", "toInput": "in",
        })).collect::<Vec<_>>(),
    })
}

fn write_flow(root: &Path, doc: &Value) -> String {
    let name = doc["name"].as_str().unwrap();
    let path = root.join(".tome").join("flows").join(format!("{name}.flow.json"));
    std::fs::write(&path, serde_json::to_string_pretty(doc).unwrap()).unwrap();
    path.to_string_lossy().into_owned()
}

fn new_runs() -> Arc<Runner> {
    Arc::new(Runner::new())
}

fn statuses_of(run: &Value) -> std::collections::BTreeMap<String, String> {
    run["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["id"].as_str().unwrap().to_string(), n["status"].as_str().unwrap().to_string()))
        .collect()
}

// ---- injected-env test double (mirrors install()/stubSpawn()) ----

#[derive(Clone)]
struct SeenSpawn {
    cmd: String,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
}

#[derive(Clone, Default)]
struct Recorders {
    events: Arc<std::sync::Mutex<Vec<Value>>>,
    pushes: Arc<std::sync::Mutex<Vec<Value>>>,
    closed: Arc<std::sync::Mutex<Vec<String>>>,
    spawn_seen: Arc<std::sync::Mutex<Vec<SeenSpawn>>>,
    build_env_calls: Arc<std::sync::Mutex<Vec<(String, bool)>>>,
}

/// The stub: ignores the command line the runner built (asserted
/// separately via `recorders.spawn_seen`) and runs the script this node's
/// brief asks for. The brief's first line is `You are "<node name>" in a
/// Tome flow "<flow>".`, which is how a script is matched to a node
/// without the runner having to tell us.
fn scripted_spawn(
    scripts: HashMap<String, String>,
    seen: Arc<std::sync::Mutex<Vec<SeenSpawn>>>,
) -> Arc<dyn Fn(spawn::SpawnRequest) -> spawn::SpawnOutcome + Send + Sync> {
    Arc::new(move |req: spawn::SpawnRequest| {
        seen.lock().unwrap().push(SeenSpawn { cmd: req.cmd.clone(), args: req.args.clone(), cwd: req.cwd.clone(), env: req.env.clone() });
        let brief = req.args.iter().position(|a| a == "-p").and_then(|i| req.args.get(i + 1)).cloned().unwrap_or_default();
        let who = brief.strip_prefix("You are \"").and_then(|s| s.split('"').next()).unwrap_or("?").to_string();
        let script = scripts.get(&who).cloned().unwrap_or_else(|| "exit 0".to_string());
        spawn::spawn_process(spawn::SpawnRequest { cmd: "/bin/sh".to_string(), args: vec!["-c".to_string(), script], cwd: req.cwd, env: req.env })
    })
}

fn build_env(r: &Recorders, scripts: HashMap<String, String>, sandbox: Option<env::SandboxWrap>, gapped_default: bool) -> env::RunnerEnv {
    let events = r.events.clone();
    let pushes = r.pushes.clone();
    let closed = r.closed.clone();
    let build_calls = r.build_env_calls.clone();
    let spawn_fn = scripted_spawn(scripts, r.spawn_seen.clone());

    env::RunnerEnv {
        can_open_file: Arc::new(|_p: &Path| true),
        build_agent_env: Arc::new(move |pane_id: String, gapped: bool, _inner_argv: Vec<String>| {
            build_calls.lock().unwrap().push((pane_id.clone(), gapped));
            // Mirrors the one branch of the real builder that matters
            // here: no gap, no sandbox wrap — without it a test could
            // only ever pin "the runner applies a wrap it was handed",
            // never "the runner asked to be gapped".
            let sandbox = if gapped { sandbox.clone() } else { None };
            Box::pin(async move {
                let mut vars: Vec<(String, String)> = std::env::vars().collect();
                vars.push(("TOME_TEST_PANE".to_string(), pane_id));
                Ok::<env::BuiltEnv, String>(env::BuiltEnv { env: vars, sandbox })
            }) as env::BoxFuture<Result<env::BuiltEnv, String>>
        }),
        close_agent_env: Arc::new(move |pane_id: &str| closed.lock().unwrap().push(pane_id.to_string())),
        airgap_default: Arc::new(move || Box::pin(async move { gapped_default }) as env::BoxFuture<bool>),
        log_event: Arc::new(move |kind: &str, fields: Vec<(String, Value)>| {
            let mut obj = serde_json::Map::new();
            obj.insert("kind".to_string(), json!(kind));
            for (k, v) in fields {
                obj.insert(k, v);
            }
            events.lock().unwrap().push(Value::Object(obj));
        }),
        push: Arc::new(move |snapshot: Value| pushes.lock().unwrap().push(snapshot)),
        spawn: spawn_fn,
        kill_grace: std::time::Duration::from_millis(5000),
    }
}

/// A fake spawned node: the runner only ever touches `pid`/`kill`/`exit`,
/// and this is the only way to SEE the signals it sends — a real process
/// that obeyed SIGTERM would never reach the escalation, and one that
/// ignored it would make the test wait out the five-second grace in wall
/// time. The pid is deliberately far outside any real range: the runner
/// signals the process GROUP first, and that (real) syscall must land on
/// nothing at all, falling through to this fake `kill`.
fn fake_child_spawn() -> (
    Arc<dyn Fn(spawn::SpawnRequest) -> spawn::SpawnOutcome + Send + Sync>,
    Arc<std::sync::Mutex<Vec<i32>>>,
    Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<spawn::ExitOutcome>>>>,
) {
    let kills = Arc::new(std::sync::Mutex::new(Vec::new()));
    let exit_tx: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<spawn::ExitOutcome>>>> = Arc::new(std::sync::Mutex::new(None));
    let kills2 = kills.clone();
    let exit_tx2 = exit_tx.clone();
    let f: Arc<dyn Fn(spawn::SpawnRequest) -> spawn::SpawnOutcome + Send + Sync> = Arc::new(move |_req| {
        let (tx, rx) = tokio::sync::oneshot::channel();
        *exit_tx2.lock().unwrap() = Some(tx);
        let kills3 = kills2.clone();
        spawn::SpawnOutcome::Started(spawn::Spawned {
            pid: 0x4000_0000,
            stdout: None,
            stderr: None,
            kill: Arc::new(move |sig: i32| {
                kills3.lock().unwrap().push(sig);
                Ok(())
            }),
            exit: rx,
        })
    });
    (f, kills, exit_tx)
}

// ---- polling helpers ----

/// Polls the runner's own snapshot rather than hooking an internal
/// callback — it is the same array the renderer sees.
async fn settled(runs: &Runner, id: &str, timeout_ms: u64) -> Value {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let snap = snapshot_all(runs);
        if let Some(run) = snap.as_array().unwrap().iter().find(|r| r["id"] == json!(id)) {
            if run["status"] != json!("running") {
                return run.clone();
            }
        }
        assert!(tokio::time::Instant::now() <= deadline, "run never settled: {id}");
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }
}

/// A run is registered — and therefore cancellable — before its first
/// node has an environment; this is that window.
async fn appears(runs: &Runner, flow: &str, timeout_ms: u64) -> String {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let snap = snapshot_all(runs);
        if let Some(run) = snap.as_array().unwrap().iter().find(|r| r["flow"] == json!(flow)) {
            return run["id"].as_str().unwrap().to_string();
        }
        assert!(tokio::time::Instant::now() <= deadline, "run never appeared: {flow}");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

// ======================================================================
// startRun — refusals happen before anything is spawned or written
// ======================================================================

#[tokio::test]
async fn refuses_a_path_outside_the_open_workspace_folders() {
    let root = workspace();
    let path = write_flow(&root, &flow_doc("outside", &["n1"], &[]));
    let runs = new_runs();
    let mut e = build_env(&Recorders::default(), HashMap::new(), None, true);
    e.can_open_file = Arc::new(|_p| false);
    assert_eq!(start_run(runs, e, path).await, json!({"error": "flow is outside the open workspace folders"}));
}

#[tokio::test]
async fn refuses_a_file_that_is_not_a_flow() {
    let root = workspace();
    let bad = root.join(".tome").join("flows").join("notes.flow.json");
    std::fs::write(&bad, r#"{"version":1,"name":"x"}"#).unwrap();
    let runs = new_runs();
    let e = build_env(&Recorders::default(), HashMap::new(), None, true);
    let result = start_run(runs.clone(), e.clone(), bad.to_string_lossy().into_owned()).await;
    assert_eq!(result["error"], json!("not a flow file"));

    std::fs::write(&bad, "not json at all").unwrap();
    let result = start_run(runs.clone(), e.clone(), bad.to_string_lossy().into_owned()).await;
    assert!(result["error"].as_str().unwrap().contains("could not read flow"));

    let missing = root.join("nope.flow.json").to_string_lossy().into_owned();
    let result = start_run(runs, e, missing).await;
    assert!(result["error"].as_str().unwrap().contains("could not read flow"));
}

#[tokio::test]
async fn refuses_a_name_that_could_not_be_a_folder_including_a_non_string() {
    let root = workspace();
    let mut doc = flow_doc("unsafe", &["n1"], &[]);
    doc["name"] = json!("../escape");
    let path = root.join(".tome").join("flows").join("unsafe.flow.json");
    std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
    let runs = new_runs();
    let e = build_env(&Recorders::default(), HashMap::new(), None, true);
    let result = start_run(runs.clone(), e.clone(), path.to_string_lossy().into_owned()).await;
    assert!(result["error"].as_str().unwrap().contains("can't be used as a folder name"));

    // A non-string name would otherwise reach the folder join and panic
    // rather than coming back as a refusal.
    doc["name"] = json!(42);
    std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
    let result = start_run(runs, e, path.to_string_lossy().into_owned()).await;
    assert_eq!(result, json!({"error": "not a flow file"}));
}

#[tokio::test]
async fn refuses_an_empty_flow_and_a_cyclic_one() {
    let root = workspace();
    let runs = new_runs();
    let e = build_env(&Recorders::default(), HashMap::new(), None, true);

    let empty = write_flow(&root, &flow_doc("empty", &[], &[]));
    assert_eq!(start_run(runs.clone(), e.clone(), empty).await, json!({"error": "this flow has no nodes"}));

    let cyclic = write_flow(&root, &flow_doc("cyclic", &["n1", "n2"], &[("n1", "n2"), ("n2", "n1")]));
    assert_eq!(start_run(runs, e, cyclic).await, json!({"error": "flow has a cycle — cannot run"}));
}

#[tokio::test]
async fn refuses_a_structurally_broken_graph_rather_than_running_the_good_half() {
    let root = workspace();
    let mut doc = flow_doc("dangling", &["n1"], &[]);
    doc["edges"] = json!([{"id":"e1","from":"n1","to":"ghost","fromOutput":"out","toInput":"in"}]);
    let path = write_flow(&root, &doc);
    let runs = new_runs();
    let e = build_env(&Recorders::default(), HashMap::new(), None, true);
    let result = start_run(runs, e, path).await;
    assert!(result["error"].as_str().unwrap().contains("missing node"));
}

#[tokio::test]
async fn refuses_the_whole_run_naming_the_node_with_no_headless_template() {
    let root = workspace();
    let mut doc = flow_doc("mixed", &["n1", "n2"], &[("n1", "n2")]);
    doc["nodes"][1]["kind"] = json!("opencode");
    doc["nodes"][1]["name"] = json!("Summarizer");
    let path = write_flow(&root, &doc);
    let runs = new_runs();
    let e = build_env(&Recorders::default(), HashMap::new(), None, true);
    let result = start_run(runs.clone(), e, path).await;
    let err = result["error"].as_str().unwrap();
    assert!(err.contains("Summarizer"));
    assert!(err.contains("Run in terminals"));
    assert!(snapshot_all(&runs).as_array().unwrap().iter().all(|r| r["flow"] != json!("mixed")));
}

// ---- symlinked paths are confined by real location, not just lexical
// spelling (TOME-008) ----

#[tokio::test]
async fn refuses_a_flow_reached_through_a_symlinked_tome_flows_directory() {
    let root = workspace();
    let outside = tmp_outside();
    std::fs::remove_dir_all(root.join(".tome").join("flows")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(".tome").join("flows")).unwrap();
    let path = root.join(".tome").join("flows").join("escape.flow.json");
    std::fs::write(&path, serde_json::to_string(&flow_doc("escape", &["n1"], &[])).unwrap()).unwrap();
    let runs = new_runs();
    let e = build_env(&Recorders::default(), HashMap::new(), None, true);
    let result = start_run(runs.clone(), e, path.to_string_lossy().into_owned()).await;
    assert_eq!(result, json!({"error": "flow is outside the open workspace folders"}));
    assert!(snapshot_all(&runs).as_array().unwrap().iter().all(|r| r["flow"] != json!("escape")));
}

#[tokio::test]
async fn refuses_to_create_a_run_directory_through_a_pre_existing_symlinked_ancestor() {
    let root = workspace();
    let outside = tmp_outside();
    let path = write_flow(&root, &flow_doc("planted", &["n1"], &[]));
    std::os::unix::fs::symlink(&outside, root.join(".tome").join("flows").join("planted")).unwrap();
    let runs = new_runs();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exit 0".to_string());
    let e = build_env(&Recorders::default(), scripts, None, true);
    let result = start_run(runs.clone(), e, path).await;
    assert!(result["error"].as_str().unwrap().contains("could not create the run folder"));
    assert!(snapshot_all(&runs).as_array().unwrap().iter().all(|r| r["flow"] != json!("planted")));
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
}

// ======================================================================
// startRun — the command line each node gets
// ======================================================================

#[tokio::test]
async fn spawns_an_argv_array_with_the_composed_brief_as_one_element_cwd_at_flow_root() {
    let root = workspace();
    let recorders = Recorders::default();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exit 0".to_string());
    let e = build_env(&recorders, scripts, None, true);
    let path = write_flow(&root, &flow_doc("shape", &["n1"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    settled(&runs, &id, 8000).await;

    let seen = recorders.spawn_seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].cmd, "claude");
    assert_eq!(seen[0].args[0], "-p");
    assert!(seen[0].args[1].contains("You are \"n1\" in a Tome flow \"shape\"."));
    assert!(seen[0].args[1].contains(".tome/flows/shape/n1-out.md"));
    assert_eq!(seen[0].args.len(), 2);
    assert_eq!(seen[0].cwd, root);
    assert!(seen[0].env.iter().any(|(k, v)| k == "TOME_TEST_PANE" && v == &format!("run:{id}:n1")));
}

#[tokio::test]
async fn pins_an_allowlisted_model_and_drops_one_that_is_not() {
    let root = workspace();
    let recorders = Recorders::default();
    let e = build_env(&recorders, HashMap::new(), None, true);
    let mut doc = flow_doc("pins", &["n1", "n2"], &[]);
    doc["nodes"][0]["model"] = json!("haiku");
    doc["nodes"][1]["model"] = json!("gpt-5");
    let path = write_flow(&root, &doc);
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    settled(&runs, &id, 8000).await;

    let seen = recorders.spawn_seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for s in seen.iter() {
        if s.args[1].contains("\"n1\"") {
            assert_eq!(&s.args[2..], ["--model".to_string(), "haiku".to_string()]);
        } else {
            assert_eq!(s.args.len(), 2, "n2's off-allowlist model must be dropped");
        }
    }
}

#[tokio::test]
async fn wraps_the_whole_command_line_in_the_sandbox_exactly_as_a_gapped_pane_does() {
    let root = workspace();
    let recorders = Recorders::default();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exit 0".to_string());
    let sandbox = env::SandboxWrap::Prefix { cmd: "/usr/bin/sandbox-exec".to_string(), args: vec!["-p".to_string(), "(profile…)".to_string()] };
    let e = build_env(&recorders, scripts, Some(sandbox), true);
    let path = write_flow(&root, &flow_doc("gapped", &["n1"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    settled(&runs, &id, 8000).await;

    let seen = recorders.spawn_seen.lock().unwrap();
    assert_eq!(seen[0].cmd, "/usr/bin/sandbox-exec");
    assert_eq!(seen[0].args[0..4], ["-p".to_string(), "(profile…)".to_string(), "claude".to_string(), "-p".to_string()]);
    // The wrap is the consequence; the ASK is the thing worth pinning.
    assert_eq!(*recorders.build_env_calls.lock().unwrap(), vec![(format!("run:{id}:n1"), true)]);
}

#[tokio::test]
async fn runs_ungapped_with_no_sandbox_wrap_at_all_when_the_air_gap_default_is_off() {
    let root = workspace();
    let recorders = Recorders::default();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exit 0".to_string());
    let sandbox = env::SandboxWrap::Prefix { cmd: "/usr/bin/sandbox-exec".to_string(), args: vec!["-p".to_string(), "(profile…)".to_string()] };
    let e = build_env(&recorders, scripts, Some(sandbox), false);
    let path = write_flow(&root, &flow_doc("ungapped", &["n1"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;

    assert_eq!(*recorders.build_env_calls.lock().unwrap(), vec![(format!("run:{id}:n1"), false)]);
    let seen = recorders.spawn_seen.lock().unwrap();
    assert_eq!(seen[0].cmd, "claude");
    assert_eq!(seen[0].args[0], "-p");
    assert_eq!(run["airgap"], json!(false));
}

#[tokio::test]
async fn gives_every_node_its_own_air_gap_pane_id_and_closes_it_when_the_node_exits() {
    let root = workspace();
    let recorders = Recorders::default();
    let e = build_env(&recorders, HashMap::new(), None, true);
    let path = write_flow(&root, &flow_doc("panes", &["n1", "n2"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;

    let mut closed = recorders.closed.lock().unwrap().clone();
    closed.sort();
    assert_eq!(closed, vec![format!("run:{id}:n1"), format!("run:{id}:n2")]);
    let mut calls = recorders.build_env_calls.lock().unwrap().clone();
    calls.sort();
    let mut expected = vec![(format!("run:{id}:n1"), true), (format!("run:{id}:n2"), true)];
    expected.sort();
    assert_eq!(calls, expected);
    assert_eq!(run["airgap"], json!(true));
}

// ======================================================================
// startRun — sequencing
// ======================================================================

#[tokio::test]
async fn starts_a_node_only_after_every_upstream_exited_0() {
    let root = workspace();
    let order_file = root.join("order.txt");
    let step = |n: &str| format!("echo {n}-start >> order.txt; sleep 0.1; echo {n}-end >> order.txt");
    let mut scripts = HashMap::new();
    for n in ["n1", "n2", "n3"] {
        scripts.insert(n.to_string(), step(n));
    }
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("chain", &["n1", "n2", "n3"], &[("n1", "n2"), ("n2", "n3")]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;
    assert_eq!(run["status"], json!("done"));
    let text = std::fs::read_to_string(&order_file).unwrap();
    let lines: Vec<&str> = text.trim().split('\n').collect();
    assert_eq!(lines, vec!["n1-start", "n1-end", "n2-start", "n2-end", "n3-start", "n3-end"]);
}

#[tokio::test]
async fn runs_a_layer_in_parallel_but_never_more_than_two_nodes_at_once() {
    let root = workspace();
    let mut scripts = HashMap::new();
    for n in ["n1", "n2", "n3", "n4"] {
        scripts.insert(n.to_string(), "sleep 0.3".to_string());
    }
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("wide", &["n1", "n2", "n3", "n4"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();

    let live = snapshot_all(&runs).as_array().unwrap().iter().find(|r| r["id"] == json!(id)).unwrap().clone();
    let live_statuses = statuses_of(&live);
    assert_eq!(live_statuses.values().filter(|s| *s == "running").count(), 2);
    assert_eq!(live_statuses.values().filter(|s| *s == "pending").count(), 2);

    let run = settled(&runs, &id, 8000).await;
    assert!(statuses_of(&run).values().all(|s| s == "done"));
}

// ======================================================================
// startRun — a failure stops the branch below it
// ======================================================================

#[tokio::test]
async fn marks_the_failure_skips_its_descendants_and_leaves_a_sibling_branch_alone() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "echo ok".to_string());
    scripts.insert("n2".to_string(), "echo broke >&2; exit 3".to_string());
    scripts.insert("n3".to_string(), "echo n3-ran >> ran.txt".to_string());
    scripts.insert("n4".to_string(), "echo fine".to_string());
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("failing", &["n1", "n2", "n3", "n4"], &[("n1", "n2"), ("n2", "n3")]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;

    let st = statuses_of(&run);
    assert_eq!(st["n1"], "done");
    assert_eq!(st["n2"], "failed");
    assert_eq!(st["n3"], "skipped");
    assert_eq!(st["n4"], "done");
    assert_eq!(run["status"], json!("failed"));
    let n2 = run["nodes"].as_array().unwrap().iter().find(|n| n["id"] == json!("n2")).unwrap();
    assert_eq!(n2["exit"], json!(3));
    // The skipped node never ran — the file its script would have written
    // is the only proof that matters.
    assert!(!root.join("ran.txt").exists());
    let n3 = run["nodes"].as_array().unwrap().iter().find(|n| n["id"] == json!("n3")).unwrap();
    assert_eq!(n3["started"], Value::Null);
    assert_eq!(n3["ended"], Value::Null);
    assert_eq!(n3["exit"], Value::Null);
    assert!(!Path::new(n3["log"].as_str().unwrap()).exists());
}

#[tokio::test]
async fn keeps_scheduling_when_a_node_fails_before_its_process_ever_exists() {
    let root = workspace();
    let mut e = build_env(&Recorders::default(), HashMap::new(), None, true);
    e.build_agent_env = Arc::new(|_pane_id: String, _gapped: bool, _argv: Vec<String>| {
        Box::pin(async move { Err::<env::BuiltEnv, String>("proxy port exhausted".to_string()) }) as env::BoxFuture<Result<env::BuiltEnv, String>>
    });
    e.spawn = Arc::new(|_req| panic!("nothing may be spawned without an environment"));
    let path = write_flow(&root, &flow_doc("no-env", &["n1", "n2"], &[("n1", "n2")]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;
    let st = statuses_of(&run);
    assert_eq!(st["n1"], "failed");
    assert_eq!(st["n2"], "skipped");
    assert_eq!(run["status"], json!("failed"));
    let log = run["nodes"][0]["log"].as_str().unwrap();
    assert!(std::fs::read_to_string(log).unwrap().contains("proxy port exhausted"));
}

#[tokio::test]
async fn records_a_missing_cli_as_a_failed_node_instead_of_taking_the_run_down() {
    let root = workspace();
    let mut e = build_env(&Recorders::default(), HashMap::new(), None, false);
    e.spawn = Arc::new(|req: spawn::SpawnRequest| {
        spawn::spawn_process(spawn::SpawnRequest { cmd: "/definitely/not/installed-xyz".to_string(), args: vec![], cwd: req.cwd, env: req.env })
    });
    let path = write_flow(&root, &flow_doc("missing", &["n1"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;
    assert_eq!(run["status"], json!("failed"));
    let log = run["nodes"][0]["log"].as_str().unwrap().to_string();
    // The failure line is written by the runner task just before the
    // status flip; poll briefly rather than assuming the write is
    // visible the instant the run settles (a lost race here flaked
    // linux-sandbox CI).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let body = std::fs::read_to_string(&log).unwrap_or_default();
        if body.contains("ENOENT") {
            break;
        }
        assert!(std::time::Instant::now() <= deadline, "log never recorded ENOENT: {body:?}");
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }
}

// ======================================================================
// cancelRun
// ======================================================================

#[tokio::test]
async fn cancel_kills_what_is_running_skips_what_has_not_started_settles_canceled() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exec sleep 30".to_string());
    scripts.insert("n2".to_string(), "echo n2-ran >> ran.txt".to_string());
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("long", &["n1", "n2"], &[("n1", "n2")]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e.clone(), path).await;
    let id = result["id"].as_str().unwrap().to_string();
    assert_eq!(cancel_run(&runs, &e, &id), json!({"ok": true}));
    assert_eq!(cancel_run(&runs, &e, &id), json!({"ok": true})); // idempotent
    let run = settled(&runs, &id, 8000).await;
    let st = statuses_of(&run);
    assert_eq!(st["n1"], "canceled");
    assert_eq!(st["n2"], "skipped");
    assert_eq!(run["status"], json!("canceled"));
    assert!(!root.join("ran.txt").exists());
}

// These two use a SHORTENED `kill_grace` (an injected `RunnerEnv` field —
// see that field's own doc comment) rather than a real 5-second wait or a
// virtual/paused clock (which would need tokio's `test-util` feature, a
// new Cargo dependency this slice does not add), so the real escalation
// timer/logic is still exercised, just against a much smaller grace
// window and real (short) sleeps.

#[tokio::test]
async fn cancel_escalates_to_sigkill_when_a_node_ignores_sigterm() {
    let root = workspace();
    let (spawn_fn, kills, exit_tx) = fake_child_spawn();
    let mut e = build_env(&Recorders::default(), HashMap::new(), None, true);
    e.spawn = spawn_fn;
    e.kill_grace = std::time::Duration::from_millis(80);
    let path = write_flow(&root, &flow_doc("stubborn", &["n1"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e.clone(), path).await;
    let id = result["id"].as_str().unwrap().to_string();

    assert_eq!(cancel_run(&runs, &e, &id), json!({"ok": true}));
    assert_eq!(*kills.lock().unwrap(), vec![spawn::SIGTERM]);
    // Well inside the (shortened) grace period: still just the SIGTERM —
    // SIGTERM is a request, and the grace is real.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(*kills.lock().unwrap(), vec![spawn::SIGTERM]);
    // …and past it: the process is taken out of the machine's hands.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(*kills.lock().unwrap(), vec![spawn::SIGTERM, spawn::SIGKILL]);

    let tx = exit_tx.lock().unwrap().take().unwrap();
    let _ = tx.send(spawn::ExitOutcome { code: None, signal: Some(spawn::SIGKILL) });
    let run = settled(&runs, &id, 4000).await;
    assert_eq!(statuses_of(&run)["n1"], "canceled");
    assert_eq!(run["status"], json!("canceled"));
}

#[tokio::test]
async fn cancel_never_sigkills_a_node_that_exited_inside_the_grace_period() {
    let root = workspace();
    let (spawn_fn, kills, exit_tx) = fake_child_spawn();
    let mut e = build_env(&Recorders::default(), HashMap::new(), None, true);
    e.spawn = spawn_fn;
    e.kill_grace = std::time::Duration::from_millis(150);
    let path = write_flow(&root, &flow_doc("polite", &["n1"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e.clone(), path).await;
    let id = result["id"].as_str().unwrap().to_string();

    cancel_run(&runs, &e, &id);
    assert_eq!(*kills.lock().unwrap(), vec![spawn::SIGTERM]);
    // Obeyed, well inside the grace.
    let tx = exit_tx.lock().unwrap().take().unwrap();
    let _ = tx.send(spawn::ExitOutcome { code: None, signal: Some(spawn::SIGTERM) });
    let run = settled(&runs, &id, 4000).await;
    assert_eq!(run["status"], json!("canceled"));

    // The kill timer must have been cleared, not merely aimed at a child
    // that has gone — waiting past the (shortened) grace must add nothing.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert_eq!(*kills.lock().unwrap(), vec![spawn::SIGTERM]);
}

#[tokio::test]
async fn never_spawns_into_a_run_cancelled_while_its_air_gap_was_still_coming_up() {
    let root = workspace();
    let closed: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let gate = Arc::new(tokio::sync::Notify::new());
    let closed2 = closed.clone();
    let gate2 = gate.clone();
    let e = env::RunnerEnv {
        can_open_file: Arc::new(|_p| true),
        build_agent_env: Arc::new(move |_pane_id, _gapped, _argv| {
            let gate = gate2.clone();
            Box::pin(async move {
                gate.notified().await;
                Ok::<env::BuiltEnv, String>(env::BuiltEnv { env: vec![], sandbox: None })
            }) as env::BoxFuture<Result<env::BuiltEnv, String>>
        }),
        close_agent_env: Arc::new(move |pane_id: &str| closed2.lock().unwrap().push(pane_id.to_string())),
        airgap_default: Arc::new(|| Box::pin(async { true }) as env::BoxFuture<bool>),
        log_event: Arc::new(|_k, _f| {}),
        push: Arc::new(|_v| {}),
        spawn: Arc::new(|_req| panic!("a cancelled run must not spawn anything")),
        kill_grace: std::time::Duration::from_millis(5000),
    };
    let path = write_flow(&root, &flow_doc("raced", &["n1", "n2"], &[("n1", "n2")]));
    let runs = new_runs();
    let starting = tokio::spawn(start_run(runs.clone(), e.clone(), path));
    let id = appears(&runs, "raced", 4000).await;

    // The node is up as far as every reader is concerned — its proxy is
    // not.
    let live = snapshot_all(&runs).as_array().unwrap().iter().find(|r| r["id"] == json!(id)).unwrap().clone();
    let st = statuses_of(&live);
    assert_eq!(st["n1"], "running");
    assert_eq!(st["n2"], "pending");

    assert_eq!(cancel_run(&runs, &e, &id), json!({"ok": true}));
    gate.notify_one();
    starting.await.unwrap();

    let run = settled(&runs, &id, 8000).await;
    let st = statuses_of(&run);
    assert_eq!(st["n1"], "canceled");
    assert_eq!(st["n2"], "skipped");
    assert_eq!(run["status"], json!("canceled"));
    // The proxy that finished binding behind the cancel is torn down.
    assert_eq!(*closed.lock().unwrap(), vec![format!("run:{id}:n1")]);
}

#[tokio::test]
async fn cancel_is_a_no_op_on_a_finished_run_and_an_error_on_an_unknown_one() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exit 0".to_string());
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("short", &["n1"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e.clone(), path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;
    assert_eq!(run["status"], json!("done"));
    assert_eq!(cancel_run(&runs, &e, &id), json!({"ok": true})); // already over — not an error
    let still = snapshot_all(&runs).as_array().unwrap().iter().find(|r| r["id"] == json!(id)).unwrap().clone();
    assert_eq!(still["status"], json!("done"));
    assert_eq!(cancel_run(&runs, &e, "no-such-run"), json!({"error": "no such run"}));
}

// ======================================================================
// killAll — the app going away
// ======================================================================

#[tokio::test]
async fn kill_all_kills_every_live_node_and_the_run_settles_as_canceled() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exec sleep 30".to_string());
    scripts.insert("n2".to_string(), "echo n2-ran >> ran.txt".to_string());
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("quitting", &["n1", "n2"], &[("n1", "n2")]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    kill_all(&runs);
    kill_all(&runs); // idempotent
    let run = settled(&runs, &id, 8000).await;
    let st = statuses_of(&run);
    assert_eq!(st["n1"], "canceled");
    assert_eq!(st["n2"], "skipped");
    assert_eq!(run["status"], json!("canceled"));
    assert!(!root.join("ran.txt").exists());
}

// ======================================================================
// run.json and the logs
// ======================================================================

#[tokio::test]
async fn run_json_is_rewritten_on_every_transition_and_matches_the_final_snapshot() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "echo hello-from-n1".to_string());
    scripts.insert("n2".to_string(), "echo hello-from-n2 >&2".to_string());
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("bookkeeping", &["n1", "n2"], &[("n1", "n2")]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;

    let file = PathBuf::from(run["dir"].as_str().unwrap()).join("run.json");
    let mut on_disk = Value::Null;
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(&file) {
            if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                on_disk = parsed;
                if on_disk["status"] != json!("running") {
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }
    assert_eq!(on_disk, run);
    assert_eq!(on_disk["id"], json!(id));
    assert_eq!(on_disk["flow"], json!("bookkeeping"));
    assert_eq!(on_disk["layers"], json!([["n1"], ["n2"]]));
    let parents: Vec<Value> = on_disk["nodes"].as_array().unwrap().iter().map(|n| n["parents"].clone()).collect();
    assert_eq!(parents, vec![json!([]), json!(["n1"])]);
    let exits: Vec<Value> = on_disk["nodes"].as_array().unwrap().iter().map(|n| n["exit"].clone()).collect();
    assert_eq!(exits, vec![json!(0), json!(0)]);
    let expected_dir = root.join(".tome").join("flows").join("bookkeeping").join("runs").join(&id);
    assert_eq!(run["dir"], json!(expected_dir.to_string_lossy()));
}

#[tokio::test]
async fn captures_stdout_and_stderr_in_one_log_per_node() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "echo to-stdout; echo to-stderr >&2; exit 0".to_string());
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("logs", &["n1"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;
    let log_path = run["nodes"][0]["log"].as_str().unwrap();
    let log = std::fs::read_to_string(log_path).unwrap();
    assert!(log.contains("to-stdout"));
    assert!(log.contains("to-stderr"));
    assert!(log.contains("# exit 0"));
    let expected = PathBuf::from(run["dir"].as_str().unwrap()).join("1-n1.log");
    assert_eq!(log_path, expected.to_string_lossy());
}

#[tokio::test]
async fn refuses_a_hand_edited_node_id_shaped_like_a_traversal_before_any_run_is_created() {
    let root = workspace();
    let e = build_env(&Recorders::default(), HashMap::new(), None, true);
    let path = write_flow(&root, &flow_doc("traversal", &["../../../escaped", "n2"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    assert!(result["error"].as_str().unwrap().contains("can't be used in a handoff path"));
    assert!(snapshot_all(&runs).as_array().unwrap().iter().all(|r| r["flow"] != json!("traversal")));
}

#[tokio::test]
async fn still_sanitizes_a_safe_segment_legal_id_for_the_log_filename() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("weird id!".to_string(), "exit 0".to_string());
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("sanitized", &["weird id!"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;
    let log = run["nodes"][0]["log"].as_str().unwrap();
    let dir = run["dir"].as_str().unwrap();
    assert!(log.starts_with(&format!("{dir}/")));
    let expected = PathBuf::from(dir).join("1-weird_id_.log");
    assert_eq!(log, expected.to_string_lossy());
}

// ======================================================================
// the event log
// ======================================================================

#[tokio::test]
async fn event_log_records_the_run_and_every_node_transition_identifiers_only() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exit 0".to_string());
    scripts.insert("n2".to_string(), "exit 7".to_string());
    let recorders = Recorders::default();
    let e = build_env(&recorders, scripts, None, true);
    let path = write_flow(&root, &flow_doc("audited", &["n1", "n2"], &[("n1", "n2")]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    settled(&runs, &id, 8000).await;

    let events = recorders.events.lock().unwrap();
    let logged: Vec<&Value> = events.iter().filter(|e| e["kind"] == json!("flow-run")).collect();
    assert!(logged.iter().all(|e| e["run"] == json!(id) && e["flow"] == json!("audited")));
    let agents: Vec<Value> = logged.iter().filter(|e| e["event"] == json!("node")).map(|e| e["agent"].clone()).collect();
    assert_eq!(agents, vec![json!("claude"); 4]);
    let sequence: Vec<(Value, Value, Value)> =
        logged.iter().map(|e| (e["event"].clone(), e.get("node").cloned().unwrap_or(Value::Null), e["status"].clone())).collect();
    assert_eq!(
        sequence,
        vec![
            (json!("run"), Value::Null, json!("running")),
            (json!("node"), json!("n1"), json!("running")),
            (json!("node"), json!("n1"), json!("done")),
            (json!("node"), json!("n2"), json!("running")),
            (json!("node"), json!("n2"), json!("failed")),
            (json!("run"), Value::Null, json!("failed")),
        ]
    );
    // The brief is never in the log — it records actions, not payloads.
    assert!(!serde_json::to_string(&*events).unwrap().contains("You are"));
}

#[tokio::test]
async fn logs_the_cancellation_itself_not_just_the_fallout() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exec sleep 30".to_string());
    let recorders = Recorders::default();
    let e = build_env(&recorders, scripts, None, true);
    let path = write_flow(&root, &flow_doc("stopped", &["n1"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e.clone(), path).await;
    let id = result["id"].as_str().unwrap().to_string();
    cancel_run(&runs, &e, &id);
    settled(&runs, &id, 8000).await;
    let events = recorders.events.lock().unwrap();
    assert!(events.iter().any(|e| e["kind"] == json!("flow-run") && e["event"] == json!("cancel")));
}

// ======================================================================
// runs:changed — the push the whole pane is built on
// ======================================================================

#[tokio::test]
async fn runs_changed_sends_the_full_snapshot_array_on_every_transition() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exit 0".to_string());
    scripts.insert("n2".to_string(), "exit 0".to_string());
    let recorders = Recorders::default();
    let e = build_env(&recorders, scripts, None, true);
    let path = write_flow(&root, &flow_doc("pushed", &["n1", "n2"], &[("n1", "n2")]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e.clone(), path).await;
    let id = result["id"].as_str().unwrap().to_string();
    settled(&runs, &id, 8000).await;

    {
        let pushes = recorders.pushes.lock().unwrap();
        // One per transition and no more: the run starting, each node
        // going running then done, and the run settling.
        assert_eq!(pushes.len(), 6);
        let first_run = pushes[0].as_array().unwrap().iter().find(|r| r["id"] == json!(id)).unwrap();
        let mut first_view = vec![first_run["status"].as_str().unwrap().to_string()];
        first_view.extend(first_run["nodes"].as_array().unwrap().iter().map(|n| n["status"].as_str().unwrap().to_string()));
        assert_eq!(first_view, vec!["running".to_string(), "pending".to_string(), "pending".to_string()]);
        assert_eq!(*pushes.last().unwrap(), snapshot_all(&runs));
    }

    // …and every payload is the WHOLE array, not the run that moved.
    let two = start_run(runs.clone(), e, write_flow(&root, &flow_doc("pushed-again", &["n1"], &[]))).await;
    let two_id = two["id"].as_str().unwrap().to_string();
    settled(&runs, &two_id, 8000).await;
    let pushes = recorders.pushes.lock().unwrap();
    let ids_in_last: Vec<&str> = pushes.last().unwrap().as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert!(ids_in_last.contains(&id.as_str()));
    assert!(ids_in_last.contains(&two_id.as_str()));
    assert_eq!(*pushes.last().unwrap(), snapshot_all(&runs));
}

// ======================================================================
// snapshotAll
// ======================================================================

// ======================================================================
// pump() concurrency — a fan-in's shared downstream node must never be
// double-launched (mod.rs's `pump`/`RunState::scheduling_lock` doc comment)
// ======================================================================

/// Direct, synthetic reproduction of the exact race the reviewer's own
/// repro described: a run state frozen at the instant BOTH upstream
/// parents of a fan-in have just landed on `"done"` and the downstream
/// node is still `"pending"` — the precise moment two exit handlers
/// racing on separate OS threads would each call `pump()`. Bypasses
/// `start_run` entirely (constructs `RunState`/`NodeState` directly — a
/// private-item access only `super::tests` has, per `state.rs`'s own
/// module doc comment on the same pattern) so every iteration is a fast,
/// pure in-memory async race with no real subprocess, letting this run
/// many iterations cheaply. `#[tokio::test(flavor = "multi_thread")]`
/// (unlike every other test in this file) is load-bearing: the two
/// `pump()` calls below only have a chance to race on separate OS
/// threads if this test's own runtime actually has more than one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pump_calls_never_double_launch_a_shared_fan_in_node() {
    use super::{NodeState, RunState};

    for _ in 0..200 {
        let root = workspace();
        let spawn_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sc = spawn_count.clone();
        let mut e = build_env(&Recorders::default(), HashMap::new(), None, true);
        e.spawn = Arc::new(move |_req: spawn::SpawnRequest| {
            sc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (_tx, rx) = tokio::sync::oneshot::channel::<spawn::ExitOutcome>();
            spawn::SpawnOutcome::Started(spawn::Spawned {
                pid: 0x4000_0000,
                stdout: None,
                stderr: None,
                kill: Arc::new(|_sig: i32| Ok(())),
                exit: rx,
            })
        });

        let plan = crate::flow::run_plan::run_plan(
            &["n1".to_string(), "n2".to_string(), "n3".to_string()],
            &[("n1".to_string(), "n3".to_string()), ("n2".to_string(), "n3".to_string())],
        )
        .unwrap();
        let mut statuses = HashMap::new();
        statuses.insert("n1".to_string(), "done".to_string());
        statuses.insert("n2".to_string(), "done".to_string());
        statuses.insert("n3".to_string(), "pending".to_string());
        let node = |id: &str, status: &str| NodeState {
            id: id.to_string(),
            name: id.to_string(),
            kind: "claude".to_string(),
            model: None,
            status: status.to_string(),
            started: if status == "pending" { None } else { Some("2026-01-01T00:00:00.000Z".to_string()) },
            ended: None,
            exit: None,
            log: root.join(format!("{id}.log")),
            inner_argv: vec!["claude".to_string(), "-p".to_string(), "hi".to_string()],
            pid: None,
            kill_fn: None,
            kill_timer: None,
        };
        let (write_tx, _write_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let state = RunState {
            id: "r1".to_string(),
            flow: "diamond".to_string(),
            flow_path: "irrelevant-for-this-test".to_string(),
            root: root.clone(),
            dir: root.clone(),
            gapped: false,
            status: "running".to_string(),
            started: "2026-01-01T00:00:00.000Z".to_string(),
            ended: None,
            canceling: false,
            plan,
            statuses,
            nodes: vec![node("n1", "done"), node("n2", "done"), node("n3", "pending")],
            write_tx,
            scheduling_lock: Arc::new(tokio::sync::Mutex::new(())),
        };
        let runs = new_runs();
        {
            let mut inner = runs.inner.lock().unwrap();
            inner.order.push("r1".to_string());
            inner.runs.insert("r1".to_string(), state);
        }

        // Fire two pump() calls at once on SEPARATE tokio tasks — exactly
        // how two upstream exit-handlers racing on separate OS threads
        // call it in production (mod.rs's own `pump` doc comment). This
        // must be `tokio::spawn`, not `tokio::join!`: `join!` only
        // interleaves futures cooperatively within the CURRENT task (no
        // second OS thread ever touches this run's state), so it could
        // never reproduce a race whose whole premise is two OS threads
        // actually running at once. Both spawned tasks are awaited to
        // completion below, and `launch()` (called from inside `pump()`)
        // is itself awaited before `pump()` returns, so every `spawn()`
        // call either invocation was ever going to make has already
        // happened by the time both handles resolve — nothing outstanding
        // left to race against the assertion below.
        let h1 = tokio::spawn(super::pump(runs.clone(), e.clone(), "r1".to_string()));
        let h2 = tokio::spawn(super::pump(runs.clone(), e.clone(), "r1".to_string()));
        h1.await.expect("pump task 1 panicked");
        h2.await.expect("pump task 2 panicked");

        assert_eq!(
            spawn_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "n3 must be launched exactly once — never zero, never twice — regardless of which concurrent pump() call wins the race"
        );
    }
}

/// The same invariant, end to end through the real `start_run`/`launch`
/// path with real (fast) child processes: a genuine diamond DAG where both
/// parents finish close together must still run the fan-in node exactly
/// once. `flow-runner.test.js` never exercised a true multi-parent
/// fan-in end to end (only single-parent chains and independent siblings —
/// see `starts_a_node_only_after_every_upstream_exited_0` /
/// `runs_a_layer_in_parallel_but_never_more_than_two_nodes_at_once` above),
/// so this is new coverage this fix earns, not a carried-over assertion.
#[tokio::test]
async fn a_real_fan_in_node_runs_exactly_once_even_when_both_parents_finish_together() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exit 0".to_string());
    scripts.insert("n2".to_string(), "exit 0".to_string());
    scripts.insert("n3".to_string(), "echo n3-ran >> fanin.txt".to_string());
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("fanin", &["n1", "n2", "n3"], &[("n1", "n3"), ("n2", "n3")]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    let run = settled(&runs, &id, 8000).await;

    assert_eq!(run["status"], json!("done"));
    let st = statuses_of(&run);
    assert_eq!(st["n3"], "done");
    // Ran exactly once: a second concurrent launch would append a second
    // line (or, since both would separately truncate-open the SAME log
    // path, corrupt/interleave the file — either way the count below
    // would not be 1).
    let text = std::fs::read_to_string(root.join("fanin.txt")).unwrap();
    assert_eq!(text.lines().count(), 1, "fan-in node must not have run twice: {text:?}");
}

#[tokio::test]
async fn snapshot_all_is_plain_data_with_no_spawn_field_newest_first() {
    let root = workspace();
    let mut scripts = HashMap::new();
    scripts.insert("n1".to_string(), "exit 0".to_string());
    let e = build_env(&Recorders::default(), scripts, None, true);
    let path = write_flow(&root, &flow_doc("cloneable", &["n1"], &[]));
    let runs = new_runs();
    let result = start_run(runs.clone(), e, path).await;
    let id = result["id"].as_str().unwrap().to_string();
    settled(&runs, &id, 8000).await;
    let all = snapshot_all(&runs);
    assert!(!serde_json::to_string(&all).unwrap().contains("\"spawn\""));
    assert_eq!(all[0]["id"], json!(id));
}
