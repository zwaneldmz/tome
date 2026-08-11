// Background flow runs: one headless child process per node, sequenced by the
// graph, with no terminal opened and nothing typed into an interactive
// session (docs/FEATURE-PLAN-background-flow-runs.md §1).
//
// THE CONTRACT THIS FILE NARROWS. Until now nothing in this app made a flow
// submit anything: Run typed each node's brief into a gated pane and the user
// pressed Enter per node. Background runs keep every clause of that promise
// except the one they must break, and break it in exactly one shape — a flow
// submits ONLY the composed brief (composeBootstrapPrompt, byte for byte the
// same text terminal mode types), ONLY on an explicit Run click, ONLY
// headless (`-p`: a one-shot process that answers and exits, with no
// interactive session left over for anything to be typed into), and inside
// the SAME air gap a freshly spawned agent pane would get — the same
// `airgap-default` preference, the same seatbelt wrap, the same per-node
// proxy. Gapped exactly when a pane would be gapped, which is not the same
// claim as "always gapped": a user who turned the default off gets background
// nodes on the open internet, so `airgap` rides in the snapshot and the runs
// pane wears it on the row. A background node has no strip and no status-bar
// seat to say it for itself. Every transition also goes to the persistent
// event log, because an agent nobody is watching has to be MORE auditable
// than one in a visible pane, not less.
//
// SINGLE WRITER, by construction: this module is the only thing that writes
// runs/<runId>/run.json. The renderer never writes it and never has to — it
// reads the same snapshot pushed to it on every transition.
//
// flow-model.js is shared between the canvas and this runner on purpose: the
// brief a background node runs has to be the same bytes the canvas would have
// typed, and a second copy of composeBootstrapPrompt is exactly how that
// stops being true.
import { spawn as childSpawn } from 'node:child_process'
import { createWriteStream } from 'node:fs'
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { composeBootstrapPrompt, flowRoot, topoSort, validateFlow } from '../shared/flow-model.js'
import { nextActions, runPaneId, runPlan, runStatus } from '../shared/flow-run-plan.js'
import { buildHeadlessSpawn } from './lib/agent-spawn.js'
import { confineRealAbs } from './lib/flow-confine.js'

// SIGTERM is a request an agent CLI mid-tool-call is free to ignore. After
// this long the process is taken out of the user's machine's hands rather
// than left running invisibly behind a run whose UI already says 'canceled'.
const KILL_GRACE_MS = 5000

// Everything with a side effect outside this file is injected (index.js's
// app.whenReady closure owns all of it), which is what lets the runner be
// driven end to end by a test with a stub in place of a real agent CLI.
// The defaults THROW rather than quietly doing something reasonable: a
// buildAgentEnv that silently returned a bare env would spawn an agent with
// no sandbox and no proxy, and that is not a failure mode to leave one
// forgotten init() call away.
const needInit = () => {
  throw new Error('flow-runner: init() was never called')
}
let canOpenFile = () => false // main's workspace confinement (isConfinedPath)
let buildAgentEnv = needInit // env + seatbelt wrap, shared with createPty
let closeAgentEnv = () => {} // airgap.closePane — tears the per-node proxy down
let airgapDefault = async () => true // the same pref panes default to
let logEvent = () => {} // main's persistent event log (events.js)
let spawn = childSpawn // swapped in tests; never a real agent CLI there

export function init(opts = {}) {
  if (typeof opts.canOpenFile === 'function') canOpenFile = opts.canOpenFile
  if (typeof opts.buildAgentEnv === 'function') buildAgentEnv = opts.buildAgentEnv
  if (typeof opts.closeAgentEnv === 'function') closeAgentEnv = opts.closeAgentEnv
  if (typeof opts.airgapDefault === 'function') airgapDefault = opts.airgapDefault
  if (typeof opts.logEvent === 'function') logEvent = opts.logEvent
  if (typeof opts.spawn === 'function') spawn = opts.spawn
}

const runs = new Map() // runId -> live record (children, timers, statuses)

// Timestamp-based and base36, so ids sort chronologically, stay short enough
// to show as "#m1h2k3" in the runs pane, and are safe as a directory name
// without any escaping. The suffix loop covers two Runs landing in the same
// millisecond.
function newRunId() {
  const base = Date.now().toString(36)
  let id = base
  for (let n = 2; runs.has(id); n++) id = `${base}-${n}`
  return id
}

// Node ids come out of a hand-editable JSON file and would otherwise become a
// filename verbatim. Stripping everything but [A-Za-z0-9._-] means no
// separator survives, so no id can aim a log write out of the run folder no
// matter how many dots it carries; the leading index keeps two ids that
// sanitize to the same string from sharing a log.
const logName = (nodeId, i) => `${i + 1}-${String(nodeId).replace(/[^A-Za-z0-9._-]/g, '_')}.log`

const nodeOf = (run, id) => run.nodes.find((n) => n.id === id)

// Signal the node's whole PROCESS GROUP, not just the process we spawned.
// Launching subprocesses is what an agent CLI does all day — `claude -p` runs
// a Bash tool call and that call is an `npm test` of its own — and a signal to
// the CLI alone leaves those grandchildren running long after the run says
// 'canceled', reparented to init with nothing left to reap them. launch()
// spawns detached precisely so each node IS a group leader, which makes the
// negative pid address the node and everything it started, in one call.
//
// The fallback is not decoration: a child that failed to spawn has no pid at
// all, a group that has already died gives ESRCH, and neither is a reason to
// leave the direct child alive.
function signalNode(node, signal) {
  const child = node.child
  if (!child) return
  try {
    process.kill(-child.pid, signal)
  } catch {
    try {
      child.kill(signal)
    } catch {}
  }
}

// Start a flow in the background. Returns { id } once every node is planned
// and the first layer is spawning, or { error } — refusals happen BEFORE
// anything is written or spawned, so a refused run leaves no trace.
export async function startRun(flowPath, win) {
  // The renderer supplies this path, so it is vetted exactly like every other
  // renderer-supplied path in main: a run reads this file, creates
  // directories beside it and spawns agents with its folder as cwd, none of
  // which belongs outside the open workspace folders.
  if (!canOpenFile(flowPath)) return { error: 'flow is outside the open workspace folders' }

  // canOpenFile (isConfinedPath, main's index.js) is a LEXICAL check — it
  // never resolves a symlink. flowRoot is pure string-slicing on flowPath
  // itself, so deriving it here, before the file is even read, costs nothing
  // and gives confineRealAbs below a root to hold flowPath to. Every path
  // this run creates (dir, logs, run.json) is anchored to this SAME root —
  // established once, here — which is also what lets it stand in for
  // canOpenFile's workspace-folder check at every later sink without this
  // module ever seeing the open-folders list itself.
  const root = flowRoot(flowPath)

  let raw
  try {
    raw = await readFile(flowPath, 'utf8')
  } catch (err) {
    return { error: `could not read flow: ${err.message}` }
  }
  // Confined AFTER the read succeeds, not before: a flowPath that simply
  // does not exist must still fail as "could not read flow" below, not as an
  // escape refusal that would be true of literally any missing file.
  // Confirms the file readFile actually followed is still really inside
  // root — a symlinked flow.json, or a symlinked ancestor directory (an
  // earlier run's leftover, a hand-edited workspace), would otherwise let
  // content from outside the workspace read as though it were opened from
  // inside it.
  if (!(await confineRealAbs(root, flowPath))) {
    return { error: 'flow is outside the open workspace folders' }
  }

  let flow
  try {
    flow = JSON.parse(raw)
  } catch (err) {
    return { error: `could not read flow: ${err.message}` }
  }
  // validateFlow walks flow.nodes/flow.edges directly, so the SHAPE is checked
  // before it rather than by it — any JSON at all can be sitting at a confined
  // path, including `null` and `[]`. The name is checked here too because it
  // becomes a path segment below: validateFlow refuses one shaped like a
  // traversal, but a name that is a number or missing entirely would reach
  // join() as a non-string and throw out of a handler instead of refusing.
  if (
    !flow ||
    typeof flow !== 'object' ||
    typeof flow.name !== 'string' ||
    !flow.name ||
    !Array.isArray(flow.nodes) ||
    !Array.isArray(flow.edges)
  )
    return { error: 'not a flow file' }
  const { errors } = validateFlow(flow)
  // Errors mean the GRAPH is broken — and include the unsafe-name refusal
  // that is the only thing standing between flow.name and the mkdir below.
  // Warnings are contract drift (an unknown kind, a stale port name) and
  // never block a run, exactly as they never block the canvas.
  if (errors.length) return { error: errors[0] }
  if (!flow.nodes.length) return { error: 'this flow has no nodes' }
  // Same refusal, same wording, as the canvas's Run: a cyclic graph is a
  // legal document that cannot be executed.
  const order = topoSort(flow)
  if (!order) return { error: 'flow has a cycle — cannot run' }

  // Every command line is built BEFORE anything is spawned or written. A flow
  // with one node whose kind has no headless template is refused WHOLE and by
  // name, so the user learns it up front and can take the "Run in terminals"
  // route — rather than discovering the gap three nodes in, with half a
  // pipeline's handoff files already on disk.
  const specs = new Map()
  for (const node of order) {
    const spec = buildHeadlessSpawn(node.kind, {
      model: node.model,
      brief: composeBootstrapPrompt(flow, node),
    })
    if (!spec)
      return {
        error: `node "${node.name || node.id}" (${node.kind || 'no kind'}) can't run in the background — use Run in terminals`,
      }
    specs.set(node.id, spec)
  }

  // composeBootstrapPrompt's handoff paths are relative to the folder holding
  // this flow's own .tome, not to the flow.json's folder two levels deeper —
  // flowRoot derives it (computed above, before flowPath was even read), and
  // it becomes both the run folder's home and every node's cwd, so what a
  // brief tells an agent to read resolves.
  const id = newRunId()
  const dir = join(root, '.tome', 'flows', flow.name, 'runs', id)
  // dir is safe LEXICALLY — flow.name already cleared validateFlow's
  // unsafeFolderName gate and id is our own timestamp — but .tome/flows/
  // <name> or its runs/ folder may already exist as a symlink (an earlier
  // run, a hand-edited workspace) and mkdir's recursive option walks through
  // one exactly like any other directory. Confine the nearest existing
  // ancestor before creating anything; dir itself is stored and used
  // UNCHANGED below either way (confineRealAbs returns the lexical value on
  // success), so a symlinked tmp dir in a test never rewrites it.
  if (!(await confineRealAbs(root, dir, { mustExist: false }))) {
    return { error: 'could not create the run folder: run folder escapes the workspace' }
  }
  try {
    // recursive also creates .tome/flows/<name>/ — the handoff folder every
    // brief tells its node to write into, which must exist before the first
    // agent starts rather than being made by whichever node finishes first.
    await mkdir(dir, { recursive: true })
  } catch (err) {
    return { error: `could not create the run folder: ${err.message}` }
  }

  const run = {
    id,
    win,
    flow: flow.name,
    flowPath,
    root,
    dir,
    // The same default a pane would get: background runs are not a way to
    // opt out of the air gap, they are the air gap with no window attached.
    gapped: await airgapDefault(),
    status: 'running',
    started: new Date().toISOString(),
    ended: null,
    canceling: false,
    plan: runPlan(flow), // non-null: topoSort just proved the graph acyclic
    statuses: {},
    writes: Promise.resolve(),
    nodes: order.map((node, i) => ({
      id: node.id,
      name: node.name || node.id,
      kind: node.kind,
      model: node.model || null,
      status: 'pending',
      started: null,
      ended: null,
      exit: null,
      log: join(dir, logName(node.id, i)),
      spawn: specs.get(node.id),
      child: null,
      killTimer: null,
    })),
  }
  for (const node of run.nodes) run.statuses[node.id] = 'pending'
  runs.set(id, run)
  logEvent('flow-run', { event: 'run', run: id, flow: run.flow, status: 'running', nodes: run.nodes.length })
  persist(run)
  push(run)
  await pump(run)
  return { id }
}

// Stop a run: the nodes that are up get SIGTERM (then SIGKILL), everything
// still waiting is written off, and nothing new starts.
export function cancelRun(id) {
  const run = runs.get(id)
  if (!run) return { error: 'no such run' }
  if (run.ended) return { ok: true } // already finished — cancelling is a no-op, not an error
  // Idempotent: a second click while the children are still dying must not
  // re-send SIGTERM or start a second kill timer over the first one.
  if (run.canceling) return { ok: true }
  run.canceling = true
  logEvent('flow-run', { event: 'cancel', run: run.id, flow: run.flow })
  // Downstream first: a node that never started is 'skipped', which is both
  // what the pipeline picture shows and what stops the next pump — fired by
  // the exit of the child we are about to kill — from starting anything else.
  for (const node of run.nodes) if (node.status === 'pending') setStatus(run, node.id, 'skipped')
  for (const node of run.nodes) {
    if (!node.child) continue
    signalNode(node, 'SIGTERM')
    // Re-read node.child at fire time (signalNode does), so a node that
    // exited inside the grace period is a no-op rather than a signal aimed at
    // a pid the OS may since have handed to somebody else.
    node.killTimer = setTimeout(() => signalNode(node, 'SIGKILL'), KILL_GRACE_MS)
  }
  // Nothing was running, so no exit handler is coming to settle this run.
  settleIfDone(run)
  // `canceling` is in this snapshot, and the row renders it as 'canceling…'
  // with the button disabled in place — not as a button that vanishes off a
  // row still reading 'running', which is what the grace period above would
  // otherwise look like for five seconds.
  push(run)
  return { ok: true }
}

// On the way out (index.js hooks both will-quit and window-all-closed, and
// this is idempotent so being called twice costs a dead signal). A background
// agent must not outlive the app that launched it: an orphaned `claude -p`
// keeps working, and billing, with no window left to show for it — and so does
// the build it had running when the window went away, which is why this takes
// the whole process group rather than the CLI on top of it. SIGKILL rather
// than the polite path because the process is seconds from exiting and there
// is nobody left to wait for a graceful shutdown.
export function killAll() {
  for (const run of runs.values()) {
    run.canceling = true
    for (const node of run.nodes) signalNode(node, 'SIGKILL')
  }
}

// Every run this session knows about, newest first — the shape both the runs
// pane and the status bar render, and the payload of every runs:changed push.
// Plain data only: a live ChildProcess cannot cross IPC.
export function snapshotAll() {
  // ISO stamps sort lexicographically, so this is chronological; a true
  // three-way compare rather than a two-way one because two runs started in
  // the same millisecond must compare equal and keep their insertion order.
  return [...runs.values()]
    .map(snapshot)
    .sort((a, b) => (a.started < b.started ? 1 : a.started > b.started ? -1 : 0))
}

function snapshot(run) {
  return {
    id: run.id,
    flow: run.flow,
    flowPath: run.flowPath,
    root: run.root,
    dir: run.dir,
    status: run.status,
    canceling: run.canceling,
    airgap: run.gapped,
    started: run.started,
    ended: run.ended,
    // The layered shape the runs pane draws its columns from, computed by the
    // scheduler itself so the picture cannot disagree with the schedule.
    layers: run.plan.layers,
    nodes: run.nodes.map((n) => ({
      id: n.id,
      name: n.name,
      kind: n.kind,
      model: n.model,
      status: n.status,
      started: n.started,
      ended: n.ended,
      exit: n.exit,
      log: n.log,
      // The dependencies the SCHEDULER used, not the flow file's edges: the
      // runs pane draws a connector per entry here, so the picture shows the
      // graph that actually decided when this node started (de-duplicated,
      // dangling edges dropped) rather than a second reading of the same file
      // that could disagree with it.
      parents: run.plan.parents.get(n.id) || [],
    })),
  }
}

// One scheduling step: ask the plan what may happen given the statuses we
// have, then make it happen. Re-entrant on purpose — every process exit calls
// it again — and safe to re-enter because a node is marked 'running'
// synchronously, before anything is awaited, so a second pass can never pick
// the same node twice.
async function pump(run) {
  if (run.ended) return
  const { start, skip } = nextActions(run.plan, run.statuses)
  for (const id of skip) setStatus(run, id, 'skipped')
  for (const id of start) setStatus(run, id, 'running')
  await Promise.all(start.map((id) => launch(run, id)))
  // launch() resolves once the child is UP, not once it exits — so a node it
  // started normally is still 'running' here, and the next scheduling pass is
  // that child's exit handler's job. A launch that FAILED (no environment, no
  // process) settled its node right here instead, and no exit is ever coming
  // to re-enter the scheduler on its behalf: without this the run stops dead
  // with its descendants stuck 'pending' and its status stuck 'running'
  // forever. Re-entering is safe for the same reason an exit handler's call
  // is — nextActions reads only the statuses, and the nodes already up are
  // marked 'running' — and it terminates because a pass that starts nothing
  // falls through to settleIfDone.
  if (start.some((id) => nodeOf(run, id).status !== 'running')) return pump(run)
  // Reached when a run ends on skips alone (an upstream failed and there was
  // nothing left to start); otherwise the last child's exit settles it.
  settleIfDone(run)
}

async function launch(run, nodeId) {
  const node = nodeOf(run, nodeId)
  // Per-node pane id: the air gap is keyed by pane, so each node gets its own
  // proxy — and its own blocked-host tally — exactly as two agent panes
  // would, rather than sharing one and blurring which node reached where.
  const paneId = runPaneId(run.id, nodeId)
  let env
  let sandbox
  try {
    ;({ env, sandbox } = await buildAgentEnv({ paneId, agent: true, gapped: run.gapped, ws: undefined }))
  } catch (err) {
    // node.log was confined once at run creation (dir, which it lives
    // under) — re-check here rather than trust that: this node's own cwd IS
    // run.root, so by the time ANY of its code has run, the run folder is no
    // longer only something this process controls. Best-effort either way,
    // same as the append itself: a log this run cannot safely write to must
    // still fail the node, never the whole run.
    if (await confineRealAbs(run.root, node.log, { mustExist: false })) {
      await appendFile(node.log, `# could not prepare the agent environment: ${err.message}\n`).catch(() => {})
    }
    setStatus(run, nodeId, 'failed')
    return
  }
  // Cancel can land while the proxy is coming up — never spawn into a run the
  // user has already stopped.
  if (run.canceling) {
    closeAgentEnv(paneId)
    setStatus(run, nodeId, 'canceled')
    return
  }

  let cmd = node.spawn.cmd
  let args = node.spawn.args
  if (sandbox) {
    // Identical wrap to a gapped pane's: sandbox-exec with the seatbelt
    // profile, the real command line as its tail. Same order, same profile,
    // same everything — a background agent outside the sandbox would be the
    // one process in this app with unfiltered egress.
    args = [...sandbox.args, cmd, ...args]
    cmd = sandbox.cmd
  }

  // Re-confined for the same reason as the appendFile above — this is the
  // node's own about-to-be-live cwd, not a static vault, so "confined when
  // dir was created" is a fact about the past, not the present.
  if (!(await confineRealAbs(run.root, node.log, { mustExist: false }))) {
    closeAgentEnv(paneId)
    setStatus(run, nodeId, 'failed')
    return
  }
  const log = createWriteStream(node.log)
  log.on('error', () => {}) // a log we cannot write must never take the run down
  log.write(`# ${node.name} · ${node.kind}${node.model ? ' · ' + node.model : ''} · ${node.started}\n`)
  let child
  try {
    // ARGV ARRAY, no shell: cmd and args reach execvp untouched, which is
    // what makes the composed brief safe as a single element (see
    // buildHeadlessSpawn). cwd is the flow root so the relative handoff paths
    // in that brief resolve to the same files terminal mode would write.
    //
    // stdin is /dev/null on purpose. Node's default gives fd 0 a pipe that
    // main holds open and never writes to or ends, and a headless CLI reading
    // a non-TTY stdin treats it as piped context — `claude -p <prompt>` would
    // sit there waiting for an EOF nothing in this file ever sends. A one-shot
    // has nothing to say on stdin, so it gets nothing, and any future
    // per-kind headless template is safe on that point by default.
    //
    // detached makes the child its own process-group leader, which is what
    // lets cancelRun and killAll reach the subprocesses the agent itself
    // spawns (see signalNode). Deliberately NOT unref'd, and stdout/stderr
    // stay piped: the run only advances when 'close' fires, and a child this
    // process had stopped listening to would strand the whole pipeline.
    child = spawn(cmd, args, { cwd: run.root, env, stdio: ['ignore', 'pipe', 'pipe'], detached: true })
  } catch (err) {
    log.end(`# failed to start: ${err.message}\n`)
    closeAgentEnv(paneId)
    setStatus(run, nodeId, 'failed')
    return
  }
  node.child = child

  // Both streams into one file, interleaved as the agent produced them: the
  // pane tails one log per node, and an error printed to stderr is exactly
  // the line you want next to the output that preceded it. end:false because
  // whichever stream finished first would otherwise close the file under the
  // other one.
  child.stdout?.pipe(log, { end: false })
  child.stderr?.pipe(log, { end: false })
  // A missing CLI arrives here rather than as an exit code, and it is the
  // single most likely failure of a background run — it goes in the log, where
  // the pane is already looking.
  child.on('error', (err) => log.write(`# ${err.message}\n`))
  child.on('close', (code, signal) => {
    clearTimeout(node.killTimer)
    node.killTimer = null
    node.child = null
    log.end(`# exit ${code === null ? `signal ${signal}` : code}\n`)
    closeAgentEnv(paneId)
    // Cancelled beats failed: a node we killed exits non-zero by definition,
    // and reporting that as a failure would blame the flow for the user's
    // own Cancel click.
    setStatus(run, nodeId, run.canceling ? 'canceled' : code === 0 ? 'done' : 'failed', code)
    // Fire-and-forget by nature — nobody is awaiting an exit handler. pump
    // handles its own failures per node, so this catch is for the impossible
    // case only.
    pump(run).catch(() => {})
  })
}

function setStatus(run, nodeId, status, exit) {
  const node = nodeOf(run, nodeId)
  node.status = status
  run.statuses[nodeId] = status
  const now = new Date().toISOString()
  if (status === 'running') node.started = now
  else if (node.started) node.ended = now // a skipped node never started, so it never ended
  if (exit !== undefined) node.exit = exit
  // Identifiers only, never the brief or the agent's output: the event log
  // records ACTIONS (see lib/eventlog.js), and a composed brief carries
  // whatever the flow author put in it.
  //
  // `agent`, not `kind`: makeEvent spreads these fields over { ts, kind }, so
  // a field named `kind` would overwrite the record's own event kind and this
  // whole family would land in the log filed as 'claude'.
  logEvent('flow-run', {
    event: 'node',
    run: run.id,
    flow: run.flow,
    node: nodeId,
    agent: node.kind,
    status,
    ...(exit != null ? { exit } : {}),
  })
  persist(run)
  push(run)
}

function settleIfDone(run) {
  if (run.ended) return
  if (run.nodes.some((n) => n.status === 'pending' || n.status === 'running')) return
  // Derived from the nodes rather than tracked alongside them, so a run can
  // never claim 'done' with a failed node in it — and never claims 'done'
  // after a Cancel either, however little was left to kill by the time the
  // signals went out.
  const derived = runStatus(run.statuses)
  run.status = run.canceling && derived === 'done' ? 'canceled' : derived
  run.ended = new Date().toISOString()
  logEvent('flow-run', { event: 'run', run: run.id, flow: run.flow, status: run.status })
  persist(run)
  push(run)
}

// The single writer of run.json. The text is serialized SYNCHRONOUSLY and the
// write is then queued behind the previous one, so a burst of transitions
// lands on disk in the order it happened rather than in the order the writes
// happened to finish — a run.json that goes backwards is worse than one that
// lags. Failures are swallowed: a run must not die because its own
// bookkeeping file could not be written (the same reasoning as the event
// log's fire-and-forget append).
function persist(run) {
  const text = JSON.stringify(snapshot(run), null, 2) + '\n'
  const file = join(run.dir, 'run.json')
  // Re-confined on every write, not just once at dir's creation — persist()
  // fires for the life of the run, and each node's cwd is run.root, so the
  // folder run.json lives in is not something only this process can change
  // between one transition and the next. A confinement failure joins every
  // other failure here: swallowed, because a run must not die over its own
  // bookkeeping file.
  run.writes = run.writes
    .then(async () => {
      if (!(await confineRealAbs(run.root, file, { mustExist: false }))) return
      await writeFile(file, text)
    })
    .catch(() => {})
}

// Every transition, not just the interesting ones: the runs pane and the
// status-bar count both render straight off this snapshot, and a missed push
// is a pane that quietly stops matching disk. Guarded because a window that
// has gone away — or is mid-teardown — must not take a background run with it.
function push(run) {
  try {
    run.win?.webContents?.send('runs:changed', snapshotAll())
  } catch {}
}
