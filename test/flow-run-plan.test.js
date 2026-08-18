// Pins the scheduling rules a background flow run is built on
// (src/shared/flow-run-plan.js): the layering, the refusal to schedule a
// cyclic graph, the two-at-a-time cap across the whole run, and — the ones
// that matter most — what happens downstream of a node that failed or was
// cancelled. A background agent nobody is watching must never be started on
// inputs that were never written, and "never started" is a property of these
// two functions and nothing else.
import { describe, it, expect } from 'vitest'
import {
  CONCURRENCY_CAP,
  RUN_PANE_PREFIX,
  elapsedMs,
  formatElapsed,
  layers,
  nextActions,
  runPaneId,
  runPlan,
  runStatus,
  runningCount,
} from '../src/shared/flow-run-plan.js'

// Minimal graph literal — the scheduler reads ids and edges only, never the
// node bodies a real flow.json carries.
const graph = (ids, pairs = []) => ({
  nodes: ids.map((id) => ({ id })),
  edges: pairs.map(([from, to], i) => ({ id: `e${i + 1}`, from, to })),
})

describe('layers', () => {
  it('puts every node with no unmet dependency in layer 0', () => {
    expect(layers(graph(['n1', 'n2', 'n3']))).toEqual([['n1', 'n2', 'n3']])
  })

  it('is one node per layer for a chain', () => {
    expect(layers(graph(['n1', 'n2', 'n3'], [['n1', 'n2'], ['n2', 'n3']]))).toEqual([
      ['n1'],
      ['n2'],
      ['n3'],
    ])
  })

  it('groups a fan-out and re-joins on the fan-in', () => {
    // n1 → n2, n1 → n3, both → n4
    const flow = graph(
      ['n1', 'n2', 'n3', 'n4'],
      [
        ['n1', 'n2'],
        ['n1', 'n3'],
        ['n2', 'n4'],
        ['n3', 'n4'],
      ]
    )
    expect(layers(flow)).toEqual([['n1'], ['n2', 'n3'], ['n4']])
  })

  it('holds a node back until its LAST dependency lands, not its first', () => {
    // n3 depends on n1 (layer 0) and n2 (layer 1) — it belongs in layer 2.
    const flow = graph(['n1', 'n2', 'n3'], [['n1', 'n2'], ['n1', 'n3'], ['n2', 'n3']])
    expect(layers(flow)).toEqual([['n1'], ['n2'], ['n3']])
  })

  it('starts disconnected nodes immediately, alongside the roots', () => {
    const flow = graph(['n1', 'n2', 'n3'], [['n1', 'n3']])
    expect(layers(flow)).toEqual([['n1', 'n2'], ['n3']])
  })

  it('counts two edges between the same pair as one dependency', () => {
    // Two ports of n1 wired into two ports of n2 — one indegree per edge, one
    // decrement per edge, so n2 must still land in layer 1 rather than never.
    const flow = graph(['n1', 'n2'], [['n1', 'n2'], ['n1', 'n2']])
    expect(layers(flow)).toEqual([['n1'], ['n2']])
  })

  it('ignores a dangling edge instead of losing the node it names', () => {
    // validateFlow errors on these and the runner refuses the run — but this
    // function stays total rather than throwing on the way there.
    const flow = graph(['n1', 'n2'], [['ghost', 'n2'], ['n1', 'nowhere']])
    expect(layers(flow)).toEqual([['n1', 'n2']])
  })

  it('returns null for a cycle — the same contract topoSort has', () => {
    expect(layers(graph(['n1', 'n2'], [['n1', 'n2'], ['n2', 'n1']]))).toBe(null)
    expect(layers(graph(['n1'], [['n1', 'n1']]))).toBe(null)
    // A cycle downstream of a clean root still refuses the whole graph.
    expect(layers(graph(['n1', 'n2', 'n3'], [['n1', 'n2'], ['n2', 'n3'], ['n3', 'n2']]))).toBe(null)
  })

  it('has no layers at all for an empty flow', () => {
    expect(layers(graph([]))).toEqual([])
  })
})

describe('runPlan', () => {
  it('carries the layers, a flat order, and each node’s parents', () => {
    const plan = runPlan(graph(['n1', 'n2', 'n3'], [['n1', 'n3'], ['n2', 'n3']]))
    expect(plan.layers).toEqual([['n1', 'n2'], ['n3']])
    expect(plan.order).toEqual(['n1', 'n2', 'n3'])
    expect(plan.parents.get('n3')).toEqual(['n1', 'n2'])
    expect(plan.parents.get('n1')).toEqual([])
  })

  it('de-duplicates parents so a double-wired pair is one dependency', () => {
    const plan = runPlan(graph(['n1', 'n2'], [['n1', 'n2'], ['n1', 'n2']]))
    expect(plan.parents.get('n2')).toEqual(['n1'])
  })

  it('is null for a cycle', () => {
    expect(runPlan(graph(['n1', 'n2'], [['n1', 'n2'], ['n2', 'n1']]))).toBe(null)
  })

  // terminals — the same node ids the Rust twin's run_plan.rs pins, so a
  // flow's sinks read the same on both sides of the port.
  it('terminals is every node when there are no edges at all', () => {
    const plan = runPlan(graph(['n1', 'n2', 'n3']))
    expect(plan.terminals).toEqual(['n1', 'n2', 'n3'])
  })

  it('terminals is only the fan-in sink, not the fan-out nodes feeding it', () => {
    const flow = graph(
      ['n1', 'n2', 'n3', 'n4'],
      [
        ['n1', 'n2'],
        ['n1', 'n3'],
        ['n2', 'n4'],
        ['n3', 'n4'],
      ]
    )
    expect(runPlan(flow).terminals).toEqual(['n4'])
  })

  it('terminals lists every leaf of a branching graph, in order order', () => {
    const plan = runPlan(graph(['n1', 'n2', 'n3'], [['n1', 'n2'], ['n1', 'n3']]))
    expect(plan.terminals).toEqual(['n2', 'n3'])
  })

  it('terminals ignores a dangling edge so the real node still counts', () => {
    const plan = runPlan(graph(['n1', 'n2'], [['n1', 'ghost']]))
    expect(plan.terminals).toEqual(['n1', 'n2'])
  })
})

describe('nextActions — what may start', () => {
  it('starts the roots, capped at two across the whole run', () => {
    expect(CONCURRENCY_CAP).toBe(2)
    const plan = runPlan(graph(['n1', 'n2', 'n3', 'n4']))
    expect(nextActions(plan, {})).toEqual({ start: ['n1', 'n2'], skip: [] })
  })

  it('counts nodes already running against the cap', () => {
    const plan = runPlan(graph(['n1', 'n2', 'n3', 'n4']))
    expect(nextActions(plan, { n1: 'running' }).start).toEqual(['n2'])
    expect(nextActions(plan, { n1: 'running', n2: 'running' }).start).toEqual([])
  })

  it('counts them across layers, not per layer', () => {
    // n2 is a root in the same layer as n1; n3 is downstream of n1. With n1
    // done and n2 running there is room for exactly one more.
    const plan = runPlan(graph(['n1', 'n2', 'n3', 'n4'], [['n1', 'n3'], ['n1', 'n4']]))
    expect(nextActions(plan, { n1: 'done', n2: 'running' }).start).toEqual(['n3'])
  })

  it('holds a node until EVERY parent is done', () => {
    const plan = runPlan(graph(['n1', 'n2', 'n3'], [['n1', 'n3'], ['n2', 'n3']]))
    expect(nextActions(plan, { n1: 'done', n2: 'running' }).start).toEqual([])
    expect(nextActions(plan, { n1: 'done', n2: 'done' }).start).toEqual(['n3'])
  })

  it('never re-starts a node that has already run', () => {
    const plan = runPlan(graph(['n1', 'n2'], [['n1', 'n2']]))
    expect(nextActions(plan, { n1: 'done', n2: 'done' })).toEqual({ start: [], skip: [] })
    expect(nextActions(plan, { n1: 'done', n2: 'running' })).toEqual({ start: [], skip: [] })
  })

  it('treats a missing status as pending, so a fresh run needs no seeding', () => {
    const plan = runPlan(graph(['n1', 'n2'], [['n1', 'n2']]))
    expect(nextActions(plan).start).toEqual(['n1'])
  })
})

describe('nextActions — a failure writes off everything downstream', () => {
  it('skips the whole descendant cone in ONE call, not one generation at a time', () => {
    // n1 → n2 → n3 → n4. The runner only calls this on a transition, so a
    // per-generation propagation would leave n3/n4 pending forever once n2's
    // process is gone and no further exits are coming.
    const plan = runPlan(graph(['n1', 'n2', 'n3', 'n4'], [['n1', 'n2'], ['n2', 'n3'], ['n3', 'n4']]))
    const { start, skip } = nextActions(plan, { n1: 'done', n2: 'failed' })
    expect(skip).toEqual(['n3', 'n4'])
    expect(start).toEqual([])
  })

  it('leaves a sibling branch alone — only descendants are written off', () => {
    // n1 → n3, n2 → n4: n1 failing must not touch n2's branch.
    const plan = runPlan(graph(['n1', 'n2', 'n3', 'n4'], [['n1', 'n3'], ['n2', 'n4']]))
    const { start, skip } = nextActions(plan, { n1: 'failed', n2: 'done' })
    expect(skip).toEqual(['n3'])
    expect(start).toEqual(['n4'])
  })

  it('writes off a fan-in when only one of its parents failed', () => {
    const plan = runPlan(graph(['n1', 'n2', 'n3'], [['n1', 'n3'], ['n2', 'n3']]))
    expect(nextActions(plan, { n1: 'done', n2: 'failed' }).skip).toEqual(['n3'])
  })

  it('treats cancelled and already-skipped upstreams exactly like a failure', () => {
    const plan = runPlan(graph(['n1', 'n2', 'n3'], [['n1', 'n2'], ['n2', 'n3']]))
    expect(nextActions(plan, { n1: 'canceled' }).skip).toEqual(['n2', 'n3'])
    expect(nextActions(plan, { n1: 'done', n2: 'skipped' }).skip).toEqual(['n3'])
  })

  it('does not write anything off while the failing branch is still running', () => {
    const plan = runPlan(graph(['n1', 'n2'], [['n1', 'n2']]))
    expect(nextActions(plan, { n1: 'running' }).skip).toEqual([])
  })

  it('reports skips even when the cap has no room to start anything', () => {
    // Cancellation semantics: two nodes still running, one branch already
    // dead. The dead branch must be reported now — the cap only limits what
    // may START, never what may be given up on.
    const plan = runPlan(graph(['n1', 'n2', 'n3', 'n4'], [['n1', 'n4']]))
    const { start, skip } = nextActions(plan, { n1: 'failed', n2: 'running', n3: 'running' })
    expect(skip).toEqual(['n4'])
    expect(start).toEqual([])
  })
})

describe('runStatus', () => {
  it('is running while anything is pending or running', () => {
    expect(runStatus({ n1: 'running', n2: 'pending' })).toBe('running')
    expect(runStatus({ n1: 'done', n2: 'pending' })).toBe('running')
  })

  it('is done only when every node is done', () => {
    expect(runStatus({ n1: 'done', n2: 'done' })).toBe('done')
  })

  it('is failed when a node failed, however many others succeeded', () => {
    expect(runStatus({ n1: 'done', n2: 'failed', n3: 'skipped' })).toBe('failed')
  })

  it('reports cancellation ahead of the failure cancelling caused', () => {
    // The killed node exits non-zero by definition; blaming the flow for the
    // user's own Cancel click would be a lie the runs pane repeats forever.
    expect(runStatus({ n1: 'canceled', n2: 'skipped' })).toBe('canceled')
    expect(runStatus({ n1: 'failed', n2: 'canceled' })).toBe('canceled')
  })
})

// The runs pane and the status bar render from the same snapshot array, so
// these are here rather than in either of them — a second copy in the status
// bar is exactly how "2 running" and an empty pipeline end up on screen at
// the same time.
describe('runningCount', () => {
  it('counts the live runs and nothing else', () => {
    const runs = [{ status: 'running' }, { status: 'done' }, { status: 'failed' }, { status: 'running' }]
    expect(runningCount(runs)).toBe(2)
    expect(runningCount([{ status: 'done' }])).toBe(0)
  })

  it('still counts a run that is being cancelled — its children are up until they are not', () => {
    expect(runningCount([{ status: 'running', canceling: true }])).toBe(1)
  })

  it('survives an empty list and a hole in one', () => {
    expect(runningCount()).toBe(0)
    expect(runningCount([null, undefined, { status: 'running' }])).toBe(1)
  })
})

describe('runPaneId', () => {
  it('carries the prefix the status bar filters on', () => {
    // Two readers, one definition. The runner mints these so each background
    // node gets an egress proxy of its own; the status bar counts "N
    // gapped panes" and must exclude them, because a run has no pane — no
    // strip, no unlock UI, no window to go and look at. If the prefix here and
    // the prefix there ever drift, pressing Run lights the egress chip up for
    // panes that do not exist.
    expect(runPaneId('m1h2k3', 'n1')).toBe('run:m1h2k3:n1')
    expect(runPaneId('m1h2k3', 'n1').startsWith(RUN_PANE_PREFIX)).toBe(true)
    // …and a real pane id — the renderer's `pty-4`, `chat-2` — never does.
    for (const paneId of ['pty-4', 'chat-2', 'editor-1']) expect(paneId.startsWith(RUN_PANE_PREFIX)).toBe(false)
  })
})

describe('elapsedMs / formatElapsed', () => {
  const at = (iso) => Date.parse(iso)

  it('ticks against now while a run is live', () => {
    const run = { started: '2026-08-09T10:00:00.000Z', ended: null }
    expect(elapsedMs(run, at('2026-08-09T10:00:09.000Z'))).toBe(9000)
  })

  it('freezes at ended once the run settles, whatever now says', () => {
    const run = { started: '2026-08-09T10:00:00.000Z', ended: '2026-08-09T10:01:30.000Z' }
    expect(elapsedMs(run, at('2026-08-09T12:00:00.000Z'))).toBe(90000)
  })

  it('is 0 rather than NaN for a stamp it cannot read, and never negative', () => {
    expect(elapsedMs({ started: 'not a date' }, at('2026-08-09T10:00:00.000Z'))).toBe(0)
    expect(elapsedMs({}, 1000)).toBe(0)
    // A clock that stepped backwards between the two stamps.
    expect(elapsedMs({ started: '2026-08-09T10:00:05.000Z' }, at('2026-08-09T10:00:00.000Z'))).toBe(0)
  })

  it('shows two units at most, zero-padding the smaller one', () => {
    expect(formatElapsed(0)).toBe('0s')
    expect(formatElapsed(8400)).toBe('8s')
    expect(formatElapsed(64000)).toBe('1m 04s')
    expect(formatElapsed(59 * 60000 + 59000)).toBe('59m 59s')
    expect(formatElapsed(2 * 3600000 + 3 * 60000)).toBe('2h 03m')
  })
})
