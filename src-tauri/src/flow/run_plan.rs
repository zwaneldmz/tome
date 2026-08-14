//! Scheduling core for background flow runs — port of
//! `src/shared/flow-run-plan.js` (199 lines): which nodes may start right
//! now, and what becomes of everything downstream of a node that failed or
//! was cancelled. Pure — no fs, no process spawning, no Tauri — because this
//! is the half of a run that has to be right on paper; `flow::runner` owns
//! every side effect (spawn, log files, run.json) and asks this module what
//! to do next.
//!
//! `layers`/`run_plan` ALSO stand in for `flow-model.js`'s separate
//! `topoSort`: both are Kahn's-algorithm topological sorts over the same
//! node/edge set, reading `flow.nodes`/`flow.edges` in the same order and
//! skipping dangling edges identically. `topoSort`'s single-FIFO-queue walk
//! and `layers`' walk-by-generation are provably the same sequence
//! flattened — a single queue seeded with all indegree-0 nodes processes
//! them in the exact order a level-by-level walk would, because every
//! newly-ready node an earlier layer's processing reveals is appended to the
//! queue's tail strictly after every node still-to-process from that same
//! (or an earlier) layer. `flow::runner` therefore uses `run_plan(..).order`
//! wherever the JS original would call `topoSort(flow).map(n => n.id)`,
//! rather than porting a second, redundant traversal.
//!
//! `runningCount`/`elapsedMs`/`formatElapsed` are display helpers the
//! (unchanged) renderer calls directly off `src/shared/flow-run-plan.js` —
//! `src/renderer/statusbar.js`/`panels/runs.js` still import them from
//! there, so nothing on the Rust side ever calls the ported copies below.
//! Kept anyway (the task brief: "port its vitest assertions") for parity;
//! `#[allow(dead_code)]` marks the one that would otherwise warn.

use std::collections::{HashMap, HashSet};

/// Two at a time across the WHOLE run, not two per layer — the point of the
/// cap is that the machine stays usable while a flow runs in the
/// background.
pub const CONCURRENCY_CAP: usize = 2;

/// The air gap is keyed by PANE id, and a background node has no pane — the
/// runner mints one per node under this prefix.
pub const RUN_PANE_PREFIX: &str = "run:";

pub fn run_pane_id(run_id: &str, node_id: &str) -> String {
    format!("{RUN_PANE_PREFIX}{run_id}:{node_id}")
}

const TERMINAL: [&str; 4] = ["done", "failed", "canceled", "skipped"];
const BLOCKING: [&str; 3] = ["failed", "canceled", "skipped"];

/// Kahn's algorithm, keeping each frontier instead of flattening it: layer 0
/// is everything with no unmet dependency, layer 1 is everything that
/// becomes runnable once layer 0 is done, and so on. `None` on a cycle
/// (never a panic) — a cyclic graph is a legal document (hand-edited, or a
/// canvas mid-edit) that Run must refuse, not an impossible state. Ties
/// inside a layer resolve by `node_ids`/`edges` order, like every other
/// ordering in the flow model. A dangling edge (endpoint not in `node_ids`)
/// is ignored rather than rejected — `validateFlow` already errors on those
/// and the runner refuses the whole run before this is ever called.
pub fn layers(node_ids: &[String], edges: &[(String, String)]) -> Option<Vec<Vec<String>>> {
    let mut indegree: HashMap<&str, usize> = node_ids.iter().map(|id| (id.as_str(), 0)).collect();
    let mut outgoing: HashMap<&str, Vec<&str>> = node_ids.iter().map(|id| (id.as_str(), Vec::new())).collect();
    for (from, to) in edges {
        if !outgoing.contains_key(from.as_str()) || !indegree.contains_key(to.as_str()) {
            continue;
        }
        outgoing.get_mut(from.as_str()).expect("checked above").push(to.as_str());
        *indegree.get_mut(to.as_str()).expect("checked above") += 1;
    }

    let mut out: Vec<Vec<String>> = Vec::new();
    let mut frontier: Vec<&str> = node_ids.iter().filter(|id| indegree[id.as_str()] == 0).map(|s| s.as_str()).collect();
    let mut placed = 0usize;
    while !frontier.is_empty() {
        placed += frontier.len();
        out.push(frontier.iter().map(|s| s.to_string()).collect());
        let mut next: Vec<&str> = Vec::new();
        for id in &frontier {
            for dest in outgoing[id].clone() {
                let remaining = indegree[dest] - 1;
                indegree.insert(dest, remaining);
                if remaining == 0 {
                    next.push(dest);
                }
            }
        }
        frontier = next;
    }
    if placed == node_ids.len() {
        Some(out)
    } else {
        None
    }
}

/// The immutable half of a run: the layers to walk, and each node's direct
/// parents. Built once at `start_run` and then only ever read.
#[derive(Debug, Clone)]
pub struct RunPlan {
    pub layers: Vec<Vec<String>>,
    pub order: Vec<String>,
    pub parents: HashMap<String, Vec<String>>,
}

pub fn run_plan(node_ids: &[String], edges: &[(String, String)]) -> Option<RunPlan> {
    let ls = layers(node_ids, edges)?;
    let mut parents: HashMap<String, Vec<String>> = node_ids.iter().map(|id| (id.clone(), Vec::new())).collect();
    for (from, to) in edges {
        if !parents.contains_key(to) || !parents.contains_key(from) {
            continue; // dangling, as above
        }
        let list = parents.get_mut(to).expect("checked above");
        // Two edges between the same pair (different ports) are one
        // dependency, not two.
        if !list.contains(from) {
            list.push(from.clone());
        }
    }
    let order = ls.iter().flatten().cloned().collect();
    Some(RunPlan { layers: ls, order, parents })
}

/// What the runner should do next, given where every node currently stands.
/// Statuses missing from `state` count as `"pending"`. Both lists are
/// computed from `state` alone — no memory between calls — which is what
/// lets the runner call this after every single transition and trust the
/// answer.
pub struct NextActions {
    pub start: Vec<String>,
    pub skip: Vec<String>,
}

pub fn next_actions(plan: &RunPlan, state: &HashMap<String, String>) -> NextActions {
    let status = |id: &str| -> &str { state.get(id).map(String::as_str).unwrap_or("pending") };

    // Pass one: propagate write-offs FORWARD through the layers. One pass
    // suffices because layers are topological — every parent of a node in
    // layer n sits in some layer < n and has already been decided here.
    let mut skip: Vec<String> = Vec::new();
    let mut doomed: HashSet<String> = HashSet::new();
    for layer in &plan.layers {
        for id in layer {
            if status(id) != "pending" {
                continue;
            }
            let parents = plan.parents.get(id).map(Vec::as_slice).unwrap_or(&[]);
            if parents.iter().any(|p| doomed.contains(p) || BLOCKING.contains(&status(p))) {
                doomed.insert(id.clone());
                skip.push(id.clone());
            }
        }
    }

    // Pass two: everything whose parents are all done, in layer order, up
    // to the cap. `running` counts the whole run, not this layer.
    let mut start: Vec<String> = Vec::new();
    let running = plan.order.iter().filter(|id| status(id) == "running").count();
    'layers: for layer in &plan.layers {
        for id in layer {
            if running + start.len() >= CONCURRENCY_CAP {
                break 'layers;
            }
            if status(id) != "pending" || doomed.contains(id) {
                continue;
            }
            let parents = plan.parents.get(id).map(Vec::as_slice).unwrap_or(&[]);
            if parents.iter().all(|p| status(p) == "done") {
                start.push(id.clone());
            }
        }
    }
    NextActions { start, skip }
}

/// The run's own status, derived from its nodes rather than tracked
/// separately. Cancellation outranks failure: a run the user stopped is
/// reported as stopped even though the node it killed exited non-zero.
pub fn run_status(state: &HashMap<String, String>) -> &'static str {
    let values: Vec<&str> = state.values().map(String::as_str).collect();
    if values.iter().any(|s| !TERMINAL.contains(s)) {
        return "running";
    }
    if values.contains(&"canceled") {
        return "canceled";
    }
    if values.contains(&"failed") {
        return "failed";
    }
    "done"
}

// ---- display helpers (renderer-only — see this module's doc comment) ----

#[allow(dead_code)]
pub struct RunLike {
    pub status: String,
}

/// Live runs. Keys off the run's own status, which the runner derives from
/// [`run_status`], so a run whose children are still being killed still
/// counts.
#[allow(dead_code)]
pub fn running_count(runs: &[Option<RunLike>]) -> usize {
    runs.iter().filter(|r| matches!(r, Some(v) if v.status == "running")).count()
}

/// Days-since-1970-01-01 <-> proleptic-Gregorian civil date — Howard
/// Hinnant's well-known algorithm (public domain), the exact inverse pair
/// `eventlog.rs`'s own `civil_from_days`/`format_iso8601` implement there
/// (that module is not this slice's file to widen the visibility of, so
/// this is a from-scratch, independently-checked copy — see the tests
/// below for the same cross-check eventlog.rs's own suite runs).
mod civil {
    pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400; // [0, 399]
        let mp = (m + 9) % 12; // [0, 11]
        let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
        era * 146_097 + doe - 719_468
    }
}

/// Parses the exact `"YYYY-MM-DDTHH:MM:SS.sssZ"` shape this crate's own
/// timestamps are always written in (`flow::runner`'s `now_iso8601`,
/// mirroring `eventlog.rs`'s) into milliseconds since the Unix epoch. Not a
/// general ISO8601 parser — `None` for anything else, matching
/// `Number.isFinite(Date.parse(s))` being false for a stamp this shape.
fn parse_iso8601_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 24
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || b[23] != b'Z'
    {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    let second: i64 = s.get(17..19)?.parse().ok()?;
    let millis: i64 = s.get(20..23)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = civil::days_from_civil(year, month, day);
    Some(days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1000 + millis)
}

/// Wall time a run has taken: still ticking while it is live, frozen at
/// `ended` once it settles. `now_ms` is a parameter rather than read
/// internally, so a whole render agrees on one instant. Unparseable stamps
/// give 0 rather than a negative/garbage value.
#[allow(dead_code)]
pub fn elapsed_ms(started: Option<&str>, ended: Option<&str>, now_ms: i64) -> i64 {
    let Some(from) = started.and_then(parse_iso8601_ms) else { return 0 };
    let to = ended.and_then(parse_iso8601_ms).unwrap_or(now_ms);
    (to - from).max(0)
}

/// "8s" · "1m 04s" · "2h 03m" — two units at most, the smaller one
/// zero-padded so a row's width stops jumping while the seconds tick.
#[allow(dead_code)]
pub fn format_elapsed(ms: i64) -> String {
    let secs = (ms / 1000).max(0);
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m {:02}s", secs % 60);
    }
    format!("{}h {:02}m", mins / 60, mins % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(ids: &[&str], pairs: &[(&str, &str)]) -> (Vec<String>, Vec<(String, String)>) {
        (
            ids.iter().map(|s| s.to_string()).collect(),
            pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
        )
    }

    fn statuses(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(id, s)| (id.to_string(), s.to_string())).collect()
    }

    // ---- layers ----

    #[test]
    fn layers_puts_every_node_with_no_unmet_dependency_in_layer_0() {
        let (ids, edges) = graph(&["n1", "n2", "n3"], &[]);
        assert_eq!(layers(&ids, &edges), Some(vec![vec!["n1".into(), "n2".into(), "n3".into()]]));
    }

    #[test]
    fn layers_is_one_node_per_layer_for_a_chain() {
        let (ids, edges) = graph(&["n1", "n2", "n3"], &[("n1", "n2"), ("n2", "n3")]);
        assert_eq!(layers(&ids, &edges), Some(vec![vec!["n1".into()], vec!["n2".into()], vec!["n3".into()]]));
    }

    #[test]
    fn layers_groups_a_fan_out_and_rejoins_on_the_fan_in() {
        let (ids, edges) = graph(&["n1", "n2", "n3", "n4"], &[("n1", "n2"), ("n1", "n3"), ("n2", "n4"), ("n3", "n4")]);
        assert_eq!(layers(&ids, &edges), Some(vec![vec!["n1".into()], vec!["n2".into(), "n3".into()], vec!["n4".into()]]));
    }

    #[test]
    fn layers_holds_a_node_back_until_its_last_dependency_lands() {
        let (ids, edges) = graph(&["n1", "n2", "n3"], &[("n1", "n2"), ("n1", "n3"), ("n2", "n3")]);
        assert_eq!(layers(&ids, &edges), Some(vec![vec!["n1".into()], vec!["n2".into()], vec!["n3".into()]]));
    }

    #[test]
    fn layers_starts_disconnected_nodes_immediately_alongside_the_roots() {
        let (ids, edges) = graph(&["n1", "n2", "n3"], &[("n1", "n3")]);
        assert_eq!(layers(&ids, &edges), Some(vec![vec!["n1".into(), "n2".into()], vec!["n3".into()]]));
    }

    #[test]
    fn layers_counts_two_edges_between_the_same_pair_as_one_dependency() {
        let (ids, edges) = graph(&["n1", "n2"], &[("n1", "n2"), ("n1", "n2")]);
        assert_eq!(layers(&ids, &edges), Some(vec![vec!["n1".into()], vec!["n2".into()]]));
    }

    #[test]
    fn layers_ignores_a_dangling_edge_instead_of_losing_the_node_it_names() {
        let (ids, edges) = graph(&["n1", "n2"], &[("ghost", "n2"), ("n1", "nowhere")]);
        assert_eq!(layers(&ids, &edges), Some(vec![vec!["n1".into(), "n2".into()]]));
    }

    #[test]
    fn layers_returns_none_for_a_cycle() {
        let (ids, edges) = graph(&["n1", "n2"], &[("n1", "n2"), ("n2", "n1")]);
        assert_eq!(layers(&ids, &edges), None);
        let (ids, edges) = graph(&["n1"], &[("n1", "n1")]);
        assert_eq!(layers(&ids, &edges), None);
        let (ids, edges) = graph(&["n1", "n2", "n3"], &[("n1", "n2"), ("n2", "n3"), ("n3", "n2")]);
        assert_eq!(layers(&ids, &edges), None);
    }

    #[test]
    fn layers_has_no_layers_at_all_for_an_empty_flow() {
        let (ids, edges) = graph(&[], &[]);
        assert_eq!(layers(&ids, &edges), Some(vec![]));
    }

    // ---- run_plan ----

    #[test]
    fn run_plan_carries_layers_flat_order_and_each_nodes_parents() {
        let (ids, edges) = graph(&["n1", "n2", "n3"], &[("n1", "n3"), ("n2", "n3")]);
        let plan = run_plan(&ids, &edges).unwrap();
        assert_eq!(plan.layers, vec![vec!["n1".to_string(), "n2".to_string()], vec!["n3".to_string()]]);
        assert_eq!(plan.order, vec!["n1".to_string(), "n2".to_string(), "n3".to_string()]);
        assert_eq!(plan.parents["n3"], vec!["n1".to_string(), "n2".to_string()]);
        assert_eq!(plan.parents["n1"], Vec::<String>::new());
    }

    #[test]
    fn run_plan_dedupes_parents_so_a_double_wired_pair_is_one_dependency() {
        let (ids, edges) = graph(&["n1", "n2"], &[("n1", "n2"), ("n1", "n2")]);
        let plan = run_plan(&ids, &edges).unwrap();
        assert_eq!(plan.parents["n2"], vec!["n1".to_string()]);
    }

    #[test]
    fn run_plan_is_none_for_a_cycle() {
        let (ids, edges) = graph(&["n1", "n2"], &[("n1", "n2"), ("n2", "n1")]);
        assert!(run_plan(&ids, &edges).is_none());
    }

    // ---- next_actions — what may start ----

    #[test]
    fn next_actions_starts_the_roots_capped_at_two_across_the_whole_run() {
        assert_eq!(CONCURRENCY_CAP, 2);
        let (ids, edges) = graph(&["n1", "n2", "n3", "n4"], &[]);
        let plan = run_plan(&ids, &edges).unwrap();
        let na = next_actions(&plan, &HashMap::new());
        assert_eq!(na.start, vec!["n1".to_string(), "n2".to_string()]);
        assert!(na.skip.is_empty());
    }

    #[test]
    fn next_actions_counts_nodes_already_running_against_the_cap() {
        let (ids, edges) = graph(&["n1", "n2", "n3", "n4"], &[]);
        let plan = run_plan(&ids, &edges).unwrap();
        assert_eq!(next_actions(&plan, &statuses(&[("n1", "running")])).start, vec!["n2".to_string()]);
        assert!(next_actions(&plan, &statuses(&[("n1", "running"), ("n2", "running")])).start.is_empty());
    }

    #[test]
    fn next_actions_counts_them_across_layers_not_per_layer() {
        let (ids, edges) = graph(&["n1", "n2", "n3", "n4"], &[("n1", "n3"), ("n1", "n4")]);
        let plan = run_plan(&ids, &edges).unwrap();
        let na = next_actions(&plan, &statuses(&[("n1", "done"), ("n2", "running")]));
        assert_eq!(na.start, vec!["n3".to_string()]);
    }

    #[test]
    fn next_actions_holds_a_node_until_every_parent_is_done() {
        let (ids, edges) = graph(&["n1", "n2", "n3"], &[("n1", "n3"), ("n2", "n3")]);
        let plan = run_plan(&ids, &edges).unwrap();
        assert!(next_actions(&plan, &statuses(&[("n1", "done"), ("n2", "running")])).start.is_empty());
        assert_eq!(
            next_actions(&plan, &statuses(&[("n1", "done"), ("n2", "done")])).start,
            vec!["n3".to_string()]
        );
    }

    #[test]
    fn next_actions_never_restarts_a_node_that_has_already_run() {
        let (ids, edges) = graph(&["n1", "n2"], &[("n1", "n2")]);
        let plan = run_plan(&ids, &edges).unwrap();
        let na = next_actions(&plan, &statuses(&[("n1", "done"), ("n2", "done")]));
        assert!(na.start.is_empty() && na.skip.is_empty());
        let na = next_actions(&plan, &statuses(&[("n1", "done"), ("n2", "running")]));
        assert!(na.start.is_empty() && na.skip.is_empty());
    }

    #[test]
    fn next_actions_treats_a_missing_status_as_pending() {
        let (ids, edges) = graph(&["n1", "n2"], &[("n1", "n2")]);
        let plan = run_plan(&ids, &edges).unwrap();
        assert_eq!(next_actions(&plan, &HashMap::new()).start, vec!["n1".to_string()]);
    }

    // ---- next_actions — a failure writes off everything downstream ----

    #[test]
    fn next_actions_skips_the_whole_descendant_cone_in_one_call() {
        let (ids, edges) = graph(&["n1", "n2", "n3", "n4"], &[("n1", "n2"), ("n2", "n3"), ("n3", "n4")]);
        let plan = run_plan(&ids, &edges).unwrap();
        let na = next_actions(&plan, &statuses(&[("n1", "done"), ("n2", "failed")]));
        assert_eq!(na.skip, vec!["n3".to_string(), "n4".to_string()]);
        assert!(na.start.is_empty());
    }

    #[test]
    fn next_actions_leaves_a_sibling_branch_alone() {
        let (ids, edges) = graph(&["n1", "n2", "n3", "n4"], &[("n1", "n3"), ("n2", "n4")]);
        let plan = run_plan(&ids, &edges).unwrap();
        let na = next_actions(&plan, &statuses(&[("n1", "failed"), ("n2", "done")]));
        assert_eq!(na.skip, vec!["n3".to_string()]);
        assert_eq!(na.start, vec!["n4".to_string()]);
    }

    #[test]
    fn next_actions_writes_off_a_fan_in_when_only_one_parent_failed() {
        let (ids, edges) = graph(&["n1", "n2", "n3"], &[("n1", "n3"), ("n2", "n3")]);
        let plan = run_plan(&ids, &edges).unwrap();
        assert_eq!(next_actions(&plan, &statuses(&[("n1", "done"), ("n2", "failed")])).skip, vec!["n3".to_string()]);
    }

    #[test]
    fn next_actions_treats_cancelled_and_skipped_upstreams_like_a_failure() {
        let (ids, edges) = graph(&["n1", "n2", "n3"], &[("n1", "n2"), ("n2", "n3")]);
        let plan = run_plan(&ids, &edges).unwrap();
        assert_eq!(
            next_actions(&plan, &statuses(&[("n1", "canceled")])).skip,
            vec!["n2".to_string(), "n3".to_string()]
        );
        assert_eq!(
            next_actions(&plan, &statuses(&[("n1", "done"), ("n2", "skipped")])).skip,
            vec!["n3".to_string()]
        );
    }

    #[test]
    fn next_actions_writes_nothing_off_while_the_failing_branch_still_runs() {
        let (ids, edges) = graph(&["n1", "n2"], &[("n1", "n2")]);
        let plan = run_plan(&ids, &edges).unwrap();
        assert!(next_actions(&plan, &statuses(&[("n1", "running")])).skip.is_empty());
    }

    #[test]
    fn next_actions_reports_skips_even_when_the_cap_has_no_room_to_start() {
        let (ids, edges) = graph(&["n1", "n2", "n3", "n4"], &[("n1", "n4")]);
        let plan = run_plan(&ids, &edges).unwrap();
        let na = next_actions(&plan, &statuses(&[("n1", "failed"), ("n2", "running"), ("n3", "running")]));
        assert_eq!(na.skip, vec!["n4".to_string()]);
        assert!(na.start.is_empty());
    }

    // ---- run_status ----

    #[test]
    fn run_status_is_running_while_anything_is_pending_or_running() {
        assert_eq!(run_status(&statuses(&[("n1", "running"), ("n2", "pending")])), "running");
        assert_eq!(run_status(&statuses(&[("n1", "done"), ("n2", "pending")])), "running");
    }

    #[test]
    fn run_status_is_done_only_when_every_node_is_done() {
        assert_eq!(run_status(&statuses(&[("n1", "done"), ("n2", "done")])), "done");
    }

    #[test]
    fn run_status_is_failed_when_a_node_failed() {
        assert_eq!(run_status(&statuses(&[("n1", "done"), ("n2", "failed"), ("n3", "skipped")])), "failed");
    }

    #[test]
    fn run_status_reports_cancellation_ahead_of_the_failure_cancelling_caused() {
        assert_eq!(run_status(&statuses(&[("n1", "canceled"), ("n2", "skipped")])), "canceled");
        assert_eq!(run_status(&statuses(&[("n1", "failed"), ("n2", "canceled")])), "canceled");
    }

    // ---- running_count ----

    #[test]
    fn running_count_counts_the_live_runs_and_nothing_else() {
        let live = |s: &str| Some(RunLike { status: s.to_string() });
        let runs = vec![live("running"), live("done"), live("failed"), live("running")];
        assert_eq!(running_count(&runs), 2);
        assert_eq!(running_count(&[live("done")]), 0);
    }

    #[test]
    fn running_count_survives_an_empty_list_and_a_hole_in_one() {
        assert_eq!(running_count(&[]), 0);
        assert_eq!(running_count(&[None, None, Some(RunLike { status: "running".to_string() })]), 1);
    }

    // ---- run_pane_id ----

    #[test]
    fn run_pane_id_carries_the_prefix_the_status_bar_filters_on() {
        assert_eq!(run_pane_id("m1h2k3", "n1"), "run:m1h2k3:n1");
        assert!(run_pane_id("m1h2k3", "n1").starts_with(RUN_PANE_PREFIX));
        for pane_id in ["pty-4", "chat-2", "editor-1"] {
            assert!(!pane_id.starts_with(RUN_PANE_PREFIX));
        }
    }

    // ---- elapsed_ms / format_elapsed ----

    #[test]
    fn elapsed_ms_ticks_against_now_while_a_run_is_live() {
        let now = parse_iso8601_ms("2026-08-09T10:00:09.000Z").unwrap();
        assert_eq!(elapsed_ms(Some("2026-08-09T10:00:00.000Z"), None, now), 9000);
    }

    #[test]
    fn elapsed_ms_freezes_at_ended_once_settled() {
        let now = parse_iso8601_ms("2026-08-09T12:00:00.000Z").unwrap();
        assert_eq!(elapsed_ms(Some("2026-08-09T10:00:00.000Z"), Some("2026-08-09T10:01:30.000Z"), now), 90_000);
    }

    #[test]
    fn elapsed_ms_is_0_for_an_unparseable_stamp_and_never_negative() {
        assert_eq!(elapsed_ms(Some("not a date"), None, 0), 0);
        assert_eq!(elapsed_ms(None, None, 1000), 0);
        let now = parse_iso8601_ms("2026-08-09T10:00:00.000Z").unwrap();
        assert_eq!(elapsed_ms(Some("2026-08-09T10:00:05.000Z"), None, now), 0);
    }

    #[test]
    fn format_elapsed_shows_two_units_at_most_zero_padding_the_smaller() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(8400), "8s");
        assert_eq!(format_elapsed(64_000), "1m 04s");
        assert_eq!(format_elapsed(59 * 60_000 + 59_000), "59m 59s");
        assert_eq!(format_elapsed(2 * 3_600_000 + 3 * 60_000), "2h 03m");
    }

    #[test]
    fn parse_iso8601_ms_cross_checks_against_known_epoch_instants() {
        // Same reference instants eventlog.rs's own format_iso8601 test
        // cross-checks, run through this module's independent inverse.
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_iso8601_ms("2000-02-29T00:00:00.000Z"), Some(951_782_400_000));
        assert_eq!(parse_iso8601_ms("2024-02-29T23:59:59.000Z"), Some(1_709_251_199_000));
    }
}
