// Scheduling core for background flow runs (docs/FEATURE-PLAN-background-flow-runs.md
// §1): which nodes may start right now, and what becomes of everything
// downstream of a node that failed or was cancelled. Pure — no fs, no
// child_process, no Electron — because this is the half of a run that has to
// be right on paper. src/main/flow-runner.js owns every side effect (spawn,
// log files, run.json) and asks this file what to do next; a rule you can
// only exercise by launching real agent CLIs is a rule nobody re-checks, and
// "which agent is allowed to start, and when" is not a rule to leave
// untested.
//
// In shared/ rather than main/ for the same reason pane-kinds.js is there:
// the runs pane draws its pipeline as exactly the layers this file schedules,
// so one definition of a layer serves both the scheduler and the picture of
// it instead of two copies drifting apart.

// Two at a time across the WHOLE run, not two per layer. The point of the cap
// is that the machine stays usable while a flow runs in the background, and a
// wide layer is precisely the case where "parallel within a layer" would
// otherwise light up eight agent CLIs at once.
export const CONCURRENCY_CAP = 2

// The egress is keyed by PANE id, and a background node has no pane — so the
// runner mints one per node under this prefix (flow-runner.js's launch). The
// prefix lives here, with the id built by one function, because a second
// reader depends on it: the status bar counts "N gapped panes", and a run
// proxy is not a pane — it has no strip, no unlock UI, and no window. Counting
// one there would report panes that do not exist and flicker for the length of
// a run, so the bar filters on exactly this prefix and the two sides cannot
// drift apart.
export const RUN_PANE_PREFIX = 'run:'
export const runPaneId = (runId, nodeId) => `${RUN_PANE_PREFIX}${runId}:${nodeId}`

// A node in one of these has finished and will never run again.
const TERMINAL = new Set(['done', 'failed', 'canceled', 'skipped'])
// …and one of these means everything downstream must be given up on rather
// than left pending forever: the node's handoff file was never written, so a
// downstream agent would read a missing file — or, worse, a stale one left by
// an earlier run — and confidently produce nonsense from it.
const BLOCKING = new Set(['failed', 'canceled', 'skipped'])

// Kahn's algorithm, keeping each frontier instead of flattening it: layer 0
// is everything with no unmet dependency, layer 1 is everything that becomes
// runnable once layer 0 is done, and so on. Same cycle contract as topoSort
// in flow-model.js — null rather than a throw, because a cyclic graph is a
// legal document (hand-edited, or a canvas mid-edit) that Run must refuse,
// not an impossible state. Ties inside a layer resolve by insertion order,
// like every other ordering in the flow model.
export function layers(flow) {
  const indegree = new Map()
  const outgoing = new Map()
  for (const node of flow.nodes) {
    indegree.set(node.id, 0)
    outgoing.set(node.id, [])
  }
  for (const edge of flow.edges) {
    // Dangling edge — validateFlow already errors on it and the runner
    // refuses the run; ignoring it here keeps this function total.
    if (!outgoing.has(edge.from) || !indegree.has(edge.to)) continue
    outgoing.get(edge.from).push(edge.to)
    indegree.set(edge.to, indegree.get(edge.to) + 1)
  }

  const out = []
  let frontier = flow.nodes.filter((n) => indegree.get(n.id) === 0).map((n) => n.id)
  let placed = 0
  while (frontier.length) {
    out.push(frontier)
    placed += frontier.length
    const next = []
    for (const id of frontier) {
      for (const dest of outgoing.get(id)) {
        const remaining = indegree.get(dest) - 1
        indegree.set(dest, remaining)
        // Exactly one decrement takes a node to zero, so a node is queued
        // once even when two edges land on it from the same layer.
        if (remaining === 0) next.push(dest)
      }
    }
    frontier = next
  }
  return placed === flow.nodes.length ? out : null // some node never reached indegree 0 — a cycle
}

// The immutable half of a run: the layers to walk, and each node's direct
// parents. Built once at startRun and then only ever read, so the scheduler
// can be re-asked on every process exit without re-deriving the graph. null
// on a cycle, like layers().
//
// `terminals` — node ids with no outgoing edge, in `order` order — is the
// run's own sinks. A dangling edge doesn't count as "outgoing" any more than
// it counts toward `parents` below: the node on its `from` end never really
// depends on anything downstream of a reference validateFlow already refused.
export function runPlan(flow) {
  const ls = layers(flow)
  if (!ls) return null
  const parents = new Map(flow.nodes.map((n) => [n.id, []]))
  const hasOutgoing = new Set()
  for (const edge of flow.edges) {
    if (!parents.has(edge.to) || !parents.has(edge.from)) continue // dangling, as above
    hasOutgoing.add(edge.from)
    const list = parents.get(edge.to)
    // Two edges between the same pair (different ports) are one dependency,
    // not two — otherwise "every parent is done" would be counted twice and
    // the skip pass would report the same node twice.
    if (!list.includes(edge.from)) list.push(edge.from)
  }
  const order = ls.flat()
  const terminals = order.filter((id) => !hasOutgoing.has(id))
  return { layers: ls, order, parents, terminals }
}

// What the runner should do next, given where every node currently stands:
// `skip` are nodes to write off (an upstream failed, was cancelled, or was
// itself skipped), `start` are nodes to spawn now. Statuses are a plain
// object keyed by node id; anything missing counts as 'pending', so a caller
// may hand in {} for a run that has not begun.
//
// Both lists are computed from statuses alone — this function has no memory
// between calls, which is what lets the runner call it after every single
// transition and trust the answer.
export function nextActions(plan, state = {}) {
  const status = (id) => state[id] || 'pending'

  // Pass one: propagate write-offs FORWARD through the layers. One pass is
  // enough precisely because layers are topological — every parent of a node
  // in layer n sits in some layer < n and has already been decided here — so
  // a single failure reaches its whole descendant cone in one call rather
  // than one generation per process exit.
  const skip = []
  const doomed = new Set()
  for (const layer of plan.layers) {
    for (const id of layer) {
      if (status(id) !== 'pending') continue
      const parents = plan.parents.get(id) || []
      if (parents.some((p) => doomed.has(p) || BLOCKING.has(status(p)))) {
        doomed.add(id)
        skip.push(id)
      }
    }
  }

  // Pass two: everything whose parents are all done, in layer order, up to
  // the cap. `running` counts the whole run, not this layer — see
  // CONCURRENCY_CAP. A sibling branch keeps going after a failure elsewhere;
  // only descendants of the failure are written off.
  const start = []
  let running = plan.order.filter((id) => status(id) === 'running').length
  for (const layer of plan.layers) {
    for (const id of layer) {
      if (running + start.length >= CONCURRENCY_CAP) return { start, skip }
      if (status(id) !== 'pending' || doomed.has(id)) continue
      const parents = plan.parents.get(id) || []
      if (parents.every((p) => status(p) === 'done')) start.push(id)
    }
  }
  return { start, skip }
}

// The run's own status, derived from its nodes rather than tracked
// separately — one source of truth, so a run can never claim 'done' with a
// failed node in it. Cancellation outranks failure: a run the user stopped is
// reported as stopped even though the node it killed exited non-zero.
export function runStatus(state = {}) {
  const values = Object.values(state)
  if (values.some((s) => !TERMINAL.has(s))) return 'running'
  if (values.includes('canceled')) return 'canceled'
  if (values.includes('failed')) return 'failed'
  return 'done'
}

// ---- what the two views of a run agree on ----
// The runs pane and the status bar both read the same snapshot array and both
// have to answer "how many runs are live" and "how long has this one been
// going". Each is three lines, which is exactly how two copies of them end up
// in two files and then disagree — one counting a mid-cancel run as live and
// the other not, one freezing the clock at 'ended' and the other ticking
// forever. One definition here, with a test, instead.

// Live runs. Keys off the run's own status, which the runner derives from
// runStatus above, so a run whose children are still being killed still
// counts — it is still using the machine.
export function runningCount(runs = []) {
  return runs.filter((r) => r && r.status === 'running').length
}

// Wall time a run has taken: still ticking while it is live, frozen at
// `ended` once it settles. `now` is a parameter rather than a Date.now()
// inside, so a whole render agrees on one instant and a test can name it.
// Unparseable stamps give 0 rather than NaN — a row reading "NaNs" would be
// the only trace of a snapshot bug, and a wrong-looking zero is easier to
// chase than a formatter crash.
export function elapsedMs(run, now = Date.now()) {
  const from = Date.parse(run?.started)
  if (!Number.isFinite(from)) return 0
  const to = run?.ended ? Date.parse(run.ended) : now
  return Math.max(0, (Number.isFinite(to) ? to : now) - from)
}

// "8s" · "1m 04s" · "2h 03m" — two units at most, the smaller one zero-padded
// so a row's width stops jumping while the seconds tick. Sub-minute runs skip
// the padding: "05s" reads like a countdown, "5s" like a stopwatch.
export function formatElapsed(ms) {
  const secs = Math.max(0, Math.floor(ms / 1000))
  if (secs < 60) return `${secs}s`
  const mins = Math.floor(secs / 60)
  if (mins < 60) return `${mins}m ${String(secs % 60).padStart(2, '0')}s`
  return `${Math.floor(mins / 60)}h ${String(mins % 60).padStart(2, '0')}m`
}
