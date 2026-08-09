// Drives src/main/flow-runner.js end to end — real child processes, real log
// files, real run.json — with one substitution: the injected spawn runs a
// shell script instead of an agent CLI. NEVER a real agent here; a test that
// launched `claude -p` would spend money, need credentials, and tell us
// nothing about the sequencing, which is the whole point.
//
// What is pinned: that a node starts only once every upstream exited 0, that
// a failure or a cancellation stops everything downstream instead of running
// it on inputs that were never written, that the command line reaching spawn
// is an argv array with the brief as ONE element (and carries the same
// sandbox wrap a gapped pane gets), and that run.json — written by the runner
// and nobody else — tells the truth at the end.
import { describe, it, expect, beforeEach, afterAll, vi } from 'vitest'
import { spawn as realSpawn } from 'node:child_process'
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import * as runner from '../src/main/flow-runner.js'

const tmpRoots = []
async function workspace() {
  const root = await mkdtemp(join(tmpdir(), 'tome-runs-'))
  tmpRoots.push(root)
  await mkdir(join(root, '.tome', 'flows'), { recursive: true })
  return root
}
afterAll(async () => {
  for (const root of tmpRoots) await rm(root, { recursive: true, force: true }).catch(() => {})
})

// A flow document shaped like the canvas writes them: every node an agent
// kind with a headless template, every edge with an id (validateFlow errors
// on duplicates, and two undefined ids are duplicates).
function flowDoc(name, ids, pairs = []) {
  return {
    version: 1,
    name,
    nodes: ids.map((id) => ({
      id,
      name: id,
      kind: 'claude',
      instructions: `do ${id}`,
      outputs: [{ name: 'out' }],
      inputs: [{ name: 'in' }],
    })),
    edges: pairs.map(([from, to], i) => ({
      id: `e${i + 1}`,
      from,
      to,
      fromOutput: 'out',
      toInput: 'in',
    })),
  }
}

async function writeFlow(root, doc) {
  const path = join(root, '.tome', 'flows', `${doc.name}.flow.json`)
  await writeFile(path, JSON.stringify(doc, null, 2))
  return path
}

// The stub: ignores the command line the runner built (asserted separately
// via `seen`) and runs the script this node's brief asks for. The brief's
// first line is `You are "<node name>" in a Tome flow "<flow>".`, which is
// how a script is matched to a node without the runner having to tell us.
function stubSpawn(scripts, seen) {
  return (cmd, args, opts) => {
    seen?.push({ cmd, args, opts })
    const brief = args[args.indexOf('-p') + 1]
    const who = /^You are "([^"]+)"/.exec(brief || '')?.[1] || '?'
    return realSpawn('/bin/sh', ['-c', scripts[who] ?? 'exit 0'], opts)
  }
}

// A fake ChildProcess: the runner only ever touches .on, .kill, .pid and
// (optionally) .stdout/.stderr, and this is the only way to SEE the signals it
// sends — a real process that obeyed SIGTERM would never reach the escalation,
// and one that ignored it would make the test wait out the five-second grace
// in wall time. The pid is deliberately far outside any real range: the runner
// signals the process GROUP first, and that call must land on nothing at all.
function fakeChild() {
  const on = {}
  return {
    pid: 0x40000000,
    kills: [],
    on: (event, fn) => (on[event] = fn),
    kill(sig) {
      this.kills.push(sig)
    },
    fire: (event, ...args) => on[event]?.(...args),
  }
}

function install({ scripts = {}, seen, sandbox = null, events = [], gapped = true, closed = [], spawn } = {}) {
  runner.init({
    canOpenFile: () => true,
    buildAgentEnv: async ({ paneId, agent, gapped: g }) => {
      events.push({ env: paneId, agent, gapped: g })
      // Mirrors the one branch of the real builder that matters here: no gap,
      // no seatbelt wrap. Without it a test could only ever pin "the runner
      // applies a wrap it was handed", never "the runner asked to be gapped".
      return { env: { ...process.env, TOME_TEST_PANE: paneId }, sandbox: g ? sandbox : null }
    },
    closeAgentEnv: (paneId) => closed.push(paneId),
    airgapDefault: async () => gapped,
    logEvent: (kind, fields) => events.push({ kind, ...fields }),
    spawn: spawn || stubSpawn(scripts, seen),
  })
}

// Polls the runner's own snapshot rather than hooking an internal callback —
// it is the same array the renderer sees, so waiting on it is waiting on what
// the UI would show.
async function settled(id, ms = 8000) {
  const deadline = Date.now() + ms
  for (;;) {
    const run = runner.snapshotAll().find((r) => r.id === id)
    if (run && run.status !== 'running') return run
    if (Date.now() > deadline) throw new Error(`run never settled: ${JSON.stringify(run)}`)
    await new Promise((r) => setTimeout(r, 15))
  }
}

// The other end of settled(): a run is registered — and therefore cancellable
// — before its first node has an environment, which is exactly the window the
// cancel-during-bind test has to open.
async function appears(flow, ms = 4000) {
  const deadline = Date.now() + ms
  for (;;) {
    const run = runner.snapshotAll().find((r) => r.flow === flow)
    if (run) return run.id
    if (Date.now() > deadline) throw new Error(`run never appeared: ${flow}`)
    await new Promise((r) => setTimeout(r, 5))
  }
}

const statuses = (run) => Object.fromEntries(run.nodes.map((n) => [n.id, n.status]))

beforeEach(() => install()) // a clean, harmless default for the refusal tests

describe('startRun — refusals happen before anything is spawned or written', () => {
  it('refuses a path outside the open workspace folders', async () => {
    const root = await workspace()
    const path = await writeFlow(root, flowDoc('outside', ['n1']))
    runner.init({ canOpenFile: () => false })
    expect(await runner.startRun(path)).toEqual({
      error: 'flow is outside the open workspace folders',
    })
    install()
  })

  it('refuses a file that is not a flow', async () => {
    const root = await workspace()
    const bad = join(root, '.tome', 'flows', 'notes.flow.json')
    await writeFile(bad, '{"version":1,"name":"x"}')
    expect((await runner.startRun(bad)).error).toBe('not a flow file')
    await writeFile(bad, 'not json at all')
    expect((await runner.startRun(bad)).error).toMatch(/could not read flow/)
    expect((await runner.startRun(join(root, 'nope.flow.json'))).error).toMatch(/could not read flow/)
  })

  it('refuses a name that could not be a folder — including one that is not a string', async () => {
    const root = await workspace()
    const doc = flowDoc('unsafe', ['n1'])
    doc.name = '../escape'
    const traversal = join(root, '.tome', 'flows', 'unsafe.flow.json')
    await writeFile(traversal, JSON.stringify(doc))
    expect((await runner.startRun(traversal)).error).toMatch(/can't be used as a folder name/)
    // A non-string name would otherwise reach join() and throw out of the IPC
    // handler rather than coming back as a refusal.
    doc.name = 42
    await writeFile(traversal, JSON.stringify(doc))
    expect((await runner.startRun(traversal)).error).toBe('not a flow file')
  })

  it('refuses an empty flow and a cyclic one, with the canvas’s own wording', async () => {
    const root = await workspace()
    const empty = await writeFlow(root, flowDoc('empty', []))
    expect((await runner.startRun(empty)).error).toBe('this flow has no nodes')
    const cyclic = await writeFlow(root, flowDoc('cyclic', ['n1', 'n2'], [['n1', 'n2'], ['n2', 'n1']]))
    expect((await runner.startRun(cyclic)).error).toBe('flow has a cycle — cannot run')
  })

  it('refuses a structurally broken graph rather than running the good half', async () => {
    const root = await workspace()
    const doc = flowDoc('dangling', ['n1'])
    doc.edges = [{ id: 'e1', from: 'n1', to: 'ghost', fromOutput: 'out', toInput: 'in' }]
    const path = await writeFlow(root, doc)
    expect((await runner.startRun(path)).error).toMatch(/missing node/)
  })

  it('refuses the WHOLE run, naming the node, when one kind has no headless template', async () => {
    const root = await workspace()
    const doc = flowDoc('mixed', ['n1', 'n2'], [['n1', 'n2']])
    doc.nodes[1].kind = 'opencode'
    doc.nodes[1].name = 'Summarizer'
    const path = await writeFlow(root, doc)
    const res = await runner.startRun(path)
    expect(res.error).toContain('Summarizer')
    expect(res.error).toContain('Run in terminals')
    // Nothing was created for a refused run.
    expect(runner.snapshotAll().some((r) => r.flow === 'mixed')).toBe(false)
    await expect(readFile(join(root, '.tome', 'flows', 'mixed', 'runs'), 'utf8')).rejects.toThrow()
  })
})

describe('startRun — the command line each node gets', () => {
  it('spawns an argv array with the composed brief as one element, cwd at the flow root', async () => {
    const root = await workspace()
    const seen = []
    install({ seen, scripts: { n1: 'exit 0' } })
    const path = await writeFlow(root, flowDoc('shape', ['n1']))
    const { id } = await runner.startRun(path)
    await settled(id)
    expect(seen).toHaveLength(1)
    expect(seen[0].cmd).toBe('claude')
    expect(seen[0].args[0]).toBe('-p')
    // One element, multi-line, byte for byte what the canvas would have typed.
    expect(seen[0].args[1]).toContain('You are "n1" in a Tome flow "shape".')
    expect(seen[0].args[1]).toContain('.tome/flows/shape/n1-out.md')
    expect(seen[0].args).toHaveLength(2)
    expect(seen[0].opts.cwd).toBe(root)
    expect(seen[0].opts.env.TOME_TEST_PANE).toBe(`run:${id}:n1`)
  })

  it('pins an allowlisted model and drops one that is not', async () => {
    const root = await workspace()
    const seen = []
    install({ seen })
    const doc = flowDoc('pins', ['n1', 'n2'])
    doc.nodes[0].model = 'haiku'
    doc.nodes[1].model = 'gpt-5'
    const { id } = await runner.startRun(await writeFlow(root, doc))
    await settled(id)
    const argv = Object.fromEntries(seen.map((s) => [/"([^"]+)"/.exec(s.args[1])[1], s.args]))
    expect(argv.n1.slice(2)).toEqual(['--model', 'haiku'])
    expect(argv.n2).toHaveLength(2)
  })

  it('closes the node’s stdin instead of leaving a pipe nobody ever ends', async () => {
    const root = await workspace()
    const seen = []
    install({ seen, scripts: { n1: 'exit 0' } })
    const { id } = await runner.startRun(await writeFlow(root, flowDoc('stdin', ['n1'])))
    await settled(id)
    // Node's default would give fd 0 a pipe main holds open and never writes
    // to or ends, and `claude -p <prompt>` reads a non-TTY stdin as extra
    // piped context — blocking for an EOF nothing in the runner ever sends.
    // A one-shot has nothing to say on stdin, so it gets /dev/null.
    expect(seen[0].opts.stdio).toEqual(['ignore', 'pipe', 'pipe'])
    // …and its own process group, which is what lets a cancel reach the tool
    // calls the agent itself spawned rather than only the agent.
    expect(seen[0].opts.detached).toBe(true)
  })

  it('wraps the whole command line in the sandbox exactly as a gapped pane does', async () => {
    const root = await workspace()
    const seen = []
    const events = []
    install({ seen, events, sandbox: { cmd: '/usr/bin/sandbox-exec', args: ['-p', '(profile…)'] } })
    const { id } = await runner.startRun(await writeFlow(root, flowDoc('gapped', ['n1'])))
    await settled(id)
    expect(seen[0].cmd).toBe('/usr/bin/sandbox-exec')
    expect(seen[0].args.slice(0, 4)).toEqual(['-p', '(profile…)', 'claude', '-p'])
    // The wrap is the consequence; the ASK is the thing worth pinning. A
    // launch() mutated to pass `gapped: false` would leave the two assertions
    // above green, because the stub would still have handed back a wrap.
    expect(events.filter((e) => e.env)).toEqual([{ env: `run:${id}:n1`, agent: true, gapped: true }])
  })

  it('runs ungapped, with no sandbox wrap at all, when the air-gap default is off', async () => {
    const root = await workspace()
    const seen = []
    const events = []
    install({
      seen,
      events,
      gapped: false,
      sandbox: { cmd: '/usr/bin/sandbox-exec', args: ['-p', '(profile…)'] },
    })
    const { id } = await runner.startRun(await writeFlow(root, flowDoc('ungapped', ['n1'])))
    const run = await settled(id)
    // The user's own preference reaches the env builder untouched: a
    // background run is not a second, quieter way into OR out of the air gap.
    expect(events.filter((e) => e.env)).toEqual([{ env: `run:${id}:n1`, agent: true, gapped: false }])
    // No gap, no wrap — the command line is the CLI itself.
    expect(seen[0].cmd).toBe('claude')
    expect(seen[0].args[0]).toBe('-p')
    // And the snapshot says so, because this run is the only place in the app
    // that can: an ungapped background node has no pane strip and no seat in
    // the status bar's air-gap item.
    expect(run.airgap).toBe(false)
  })

  it('gives every node its own air-gap pane id, and closes it when the node exits', async () => {
    const root = await workspace()
    const closed = []
    const events = []
    install({ closed, events })
    const { id } = await runner.startRun(await writeFlow(root, flowDoc('panes', ['n1', 'n2'])))
    const run = await settled(id)
    expect(closed.sort()).toEqual([`run:${id}:n1`, `run:${id}:n2`])
    // One environment built per node, each gapped, each under its own pane id.
    // The teardown above would survive a launch() that stopped asking for the
    // gap at all — this is the assertion that would not.
    expect(events.filter((e) => e.env)).toEqual([
      { env: `run:${id}:n1`, agent: true, gapped: true },
      { env: `run:${id}:n2`, agent: true, gapped: true },
    ])
    expect(run.airgap).toBe(true)
  })
})

describe('startRun — sequencing', () => {
  it('starts a node only after every upstream exited 0', async () => {
    const root = await workspace()
    const order = join(root, 'order.txt')
    // Each script announces itself, dawdles, then announces again: a node
    // that started early would interleave its marks with its upstream's.
    const step = (n) => `echo ${n}-start >> order.txt; sleep 0.1; echo ${n}-end >> order.txt`
    install({ scripts: { n1: step('n1'), n2: step('n2'), n3: step('n3') } })
    const path = await writeFlow(root, flowDoc('chain', ['n1', 'n2', 'n3'], [['n1', 'n2'], ['n2', 'n3']]))
    const { id } = await runner.startRun(path)
    const run = await settled(id)
    expect(run.status).toBe('done')
    expect((await readFile(order, 'utf8')).trim().split('\n')).toEqual([
      'n1-start',
      'n1-end',
      'n2-start',
      'n2-end',
      'n3-start',
      'n3-end',
    ])
  })

  it('runs a layer in parallel but never more than two nodes at once', async () => {
    const root = await workspace()
    install({ scripts: Object.fromEntries(['n1', 'n2', 'n3', 'n4'].map((n) => [n, 'sleep 0.3'])) })
    const path = await writeFlow(root, flowDoc('wide', ['n1', 'n2', 'n3', 'n4']))
    const { id } = await runner.startRun(path)
    // startRun awaits its first scheduling pass, so this is a fact and not a
    // race: four independent nodes, two of them up.
    const live = runner.snapshotAll().find((r) => r.id === id)
    expect(statuses(live)).toEqual({ n1: 'running', n2: 'running', n3: 'pending', n4: 'pending' })
    const run = await settled(id)
    expect(statuses(run)).toEqual({ n1: 'done', n2: 'done', n3: 'done', n4: 'done' })
  })
})

describe('startRun — a failure stops the branch below it', () => {
  it('marks the failure, skips its descendants, and leaves a sibling branch alone', async () => {
    const root = await workspace()
    // n1 → n2 → n3 (n2 fails), n4 independent.
    install({
      scripts: {
        n1: 'echo ok',
        n2: 'echo broke >&2; exit 3',
        n3: 'echo n3-ran >> ran.txt',
        n4: 'echo fine',
      },
    })
    const path = await writeFlow(
      root,
      flowDoc('failing', ['n1', 'n2', 'n3', 'n4'], [['n1', 'n2'], ['n2', 'n3']])
    )
    const { id } = await runner.startRun(path)
    const run = await settled(id)
    expect(statuses(run)).toEqual({ n1: 'done', n2: 'failed', n3: 'skipped', n4: 'done' })
    expect(run.status).toBe('failed')
    expect(run.nodes.find((n) => n.id === 'n2').exit).toBe(3)
    // The skipped node never ran — the file its script would have written is
    // the only proof that matters.
    await expect(readFile(join(root, 'ran.txt'), 'utf8')).rejects.toThrow()
    // A skipped node has no start/end time to show, and no log.
    const n3 = run.nodes.find((n) => n.id === 'n3')
    expect([n3.started, n3.ended, n3.exit]).toEqual([null, null, null])
    await expect(readFile(n3.log, 'utf8')).rejects.toThrow()
  })

  it('keeps scheduling when a node fails before its process ever exists', async () => {
    const root = await workspace()
    // Every other failure in this file arrives as an exit code, so the exit
    // handler re-enters the scheduler. This one never gets that far — the air
    // gap could not be built (a proxy port that would not bind is the real
    // case), so there is no child, and therefore no exit to carry the run
    // forward. Its descendants have to be written off by the same pass that
    // failed it, or the run sits at 'running' with a 'pending' node under it
    // for as long as the app is open.
    runner.init({
      canOpenFile: () => true,
      buildAgentEnv: async () => {
        throw new Error('proxy port exhausted')
      },
      closeAgentEnv: () => {},
      airgapDefault: async () => true,
      logEvent: () => {},
      spawn: () => expect.unreachable('nothing may be spawned without an environment'),
    })
    const path = await writeFlow(root, flowDoc('no-env', ['n1', 'n2'], [['n1', 'n2']]))
    const { id } = await runner.startRun(path)
    const run = await settled(id)
    expect(statuses(run)).toEqual({ n1: 'failed', n2: 'skipped' })
    expect(run.status).toBe('failed')
    // Why it failed belongs in the log — it is the only place the user looks.
    expect(await readFile(run.nodes[0].log, 'utf8')).toContain('proxy port exhausted')
    install()
  })

  it('records a missing CLI as a failed node instead of taking the run down', async () => {
    const root = await workspace()
    // The one substitution a test may make to a spawn: a command that is not
    // there. ENOENT arrives as an 'error' event, not as an exit code.
    runner.init({
      canOpenFile: () => true,
      buildAgentEnv: async () => ({ env: process.env, sandbox: null }),
      closeAgentEnv: () => {},
      airgapDefault: async () => false,
      logEvent: () => {},
      spawn: (cmd, args, opts) => realSpawn(join(root, 'definitely-not-installed'), [], opts),
    })
    const { id } = await runner.startRun(await writeFlow(root, flowDoc('missing', ['n1'])))
    const run = await settled(id)
    expect(run.status).toBe('failed')
    expect(await readFile(run.nodes[0].log, 'utf8')).toMatch(/ENOENT/)
    install()
  })
})

describe('cancelRun', () => {
  it('kills what is running, skips what has not started, and settles as canceled', async () => {
    const root = await workspace()
    // `exec` so the shell is REPLACED by sleep: SIGTERM then reaches the
    // process actually holding the pipe open, which is what the runner sends.
    install({ scripts: { n1: 'exec sleep 30', n2: 'echo n2-ran >> ran.txt' } })
    const path = await writeFlow(root, flowDoc('long', ['n1', 'n2'], [['n1', 'n2']]))
    const { id } = await runner.startRun(path)
    expect(runner.cancelRun(id)).toEqual({ ok: true })
    expect(runner.cancelRun(id)).toEqual({ ok: true }) // idempotent: no second SIGTERM, no second kill timer
    const run = await settled(id)
    expect(statuses(run)).toEqual({ n1: 'canceled', n2: 'skipped' })
    expect(run.status).toBe('canceled')
    await expect(readFile(join(root, 'ran.txt'), 'utf8')).rejects.toThrow()
  })

  it('takes the node’s whole process group down, not just the child it spawned', async () => {
    const root = await workspace()
    // What an agent CLI does all day: `claude -p` running a Bash tool call is
    // a GRANDCHILD of this runner, and a signal aimed at the CLI alone leaves
    // it running — writing to the workspace, and billing — long after the run
    // says 'canceled'. The background loop here is that grandchild; the `exec
    // sleep` keeps the node itself alive so the run cannot settle on its own.
    const ticks = join(root, 'ticks.txt')
    const size = async () => (await readFile(ticks, 'utf8').catch(() => '')).length
    install({
      scripts: { n1: 'sh -c "while :; do echo t >> ticks.txt; sleep 0.02; done" & exec sleep 30' },
    })
    const { id } = await runner.startRun(await writeFlow(root, flowDoc('grandchild', ['n1'])))
    // Poll rather than sleep a fixed beat: what matters is that ticks are
    // arriving, not how quickly this machine got there.
    for (let i = 0; i < 300 && !(await size()); i++) await new Promise((r) => setTimeout(r, 15))
    expect(await size()).toBeGreaterThan(0)
    runner.cancelRun(id)
    expect((await settled(id)).status).toBe('canceled')
    // Two reads a beat apart. A grandchild that outlived the cancel would put
    // a dozen more ticks in the file between them.
    await new Promise((r) => setTimeout(r, 200))
    const after = await size()
    await new Promise((r) => setTimeout(r, 200))
    expect(await size()).toBe(after)
  })

  it('escalates to SIGKILL when a node ignores SIGTERM', async () => {
    const root = await workspace()
    const child = fakeChild()
    install({ spawn: () => child })
    let id
    // Only the two functions the escalation itself uses, so Date, the polling
    // in settled() and the fs promises underneath startRun all stay real.
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    try {
      ;({ id } = await runner.startRun(await writeFlow(root, flowDoc('stubborn', ['n1']))))
      expect(runner.cancelRun(id)).toEqual({ ok: true })
      expect(child.kills).toEqual(['SIGTERM'])
      vi.advanceTimersByTime(4999) // SIGTERM is a request, and the grace is real
      expect(child.kills).toEqual(['SIGTERM'])
      // …and then the process is taken out of the machine's hands. Without
      // this the run never settles: `canceling` is already true, so the Cancel
      // button is gone, and the status bar reads "1 running" for the life of
      // the app with nothing left to press.
      vi.advanceTimersByTime(1)
      expect(child.kills).toEqual(['SIGTERM', 'SIGKILL'])
    } finally {
      vi.useRealTimers()
    }
    child.fire('close', null, 'SIGKILL')
    const run = await settled(id)
    expect(statuses(run)).toEqual({ n1: 'canceled' })
    expect(run.status).toBe('canceled')
  })

  it('never SIGKILLs a node that exited inside the grace period', async () => {
    const root = await workspace()
    const child = fakeChild()
    install({ spawn: () => child })
    let id
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    try {
      ;({ id } = await runner.startRun(await writeFlow(root, flowDoc('polite', ['n1']))))
      runner.cancelRun(id)
      expect(child.kills).toEqual(['SIGTERM'])
      child.fire('close', null, 'SIGTERM') // obeyed, well inside the grace
      // Cleared, not merely aimed at a child that has gone: a pending timer
      // per cancelled node is a leak, and five seconds is long enough for the
      // OS to have handed that pid to somebody else.
      expect(vi.getTimerCount()).toBe(0)
      vi.advanceTimersByTime(60_000)
      expect(child.kills).toEqual(['SIGTERM'])
    } finally {
      vi.useRealTimers()
    }
    expect((await settled(id)).status).toBe('canceled')
  })

  it('never spawns into a run cancelled while its air gap was still coming up', async () => {
    const root = await workspace()
    // buildAgentEnv genuinely awaits a proxy bind in production, so this
    // window is real — and Cancel lands in it exactly when a user changes
    // their mind about a run they just started. Without the guard in launch()
    // a headless agent is spawned into an already-stopped run and works to
    // completion, spending money and touching the workspace after the UI said
    // the run was over.
    const closed = []
    let bound
    const binding = new Promise((resolve) => (bound = resolve))
    runner.init({
      canOpenFile: () => true,
      buildAgentEnv: async () => {
        await binding
        return { env: process.env, sandbox: null }
      },
      closeAgentEnv: (paneId) => closed.push(paneId),
      airgapDefault: async () => true,
      logEvent: () => {},
      spawn: () => expect.unreachable('a cancelled run must not spawn anything'),
    })
    const path = await writeFlow(root, flowDoc('raced', ['n1', 'n2'], [['n1', 'n2']]))
    const starting = runner.startRun(path)
    const id = await appears('raced')
    // The node is up as far as every reader is concerned — its proxy is not.
    const live = runner.snapshotAll().find((r) => r.id === id)
    expect(statuses(live)).toEqual({ n1: 'running', n2: 'pending' })
    expect(runner.cancelRun(id)).toEqual({ ok: true })
    bound()
    await starting
    const run = await settled(id)
    expect(statuses(run)).toEqual({ n1: 'canceled', n2: 'skipped' })
    expect(run.status).toBe('canceled')
    // The proxy that finished binding behind the cancel is torn down rather
    // than left listening for the life of the app.
    expect(closed).toEqual([`run:${id}:n1`])
    install()
  })

  it('is a no-op on a finished run and an error on an unknown one', async () => {
    const root = await workspace()
    install({ scripts: { n1: 'exit 0' } })
    const { id } = await runner.startRun(await writeFlow(root, flowDoc('short', ['n1'])))
    const run = await settled(id)
    expect(run.status).toBe('done')
    expect(runner.cancelRun(id)).toEqual({ ok: true }) // already over — not an error
    expect(runner.snapshotAll().find((r) => r.id === id).status).toBe('done') // still done
    expect(runner.cancelRun('no-such-run')).toEqual({ error: 'no such run' })
  })
})

describe('killAll — the app going away', () => {
  it('kills every live node, and the run settles as canceled', async () => {
    const root = await workspace()
    // The app's only reaping point for these children: they belong to no pane,
    // so nothing else in main would ever touch them. A regression that emptied
    // this would leave a headless agent working — and billing — with the
    // window it was started from already gone.
    const spawned = []
    const stub = stubSpawn({ n1: 'exec sleep 30', n2: 'echo n2-ran >> ran.txt' })
    install({
      spawn: (cmd, args, opts) => {
        const child = stub(cmd, args, opts)
        spawned.push(child)
        return child
      },
    })
    const path = await writeFlow(root, flowDoc('quitting', ['n1', 'n2'], [['n1', 'n2']]))
    const { id } = await runner.startRun(path)
    runner.killAll()
    runner.killAll() // idempotent: index.js hooks both will-quit and window-all-closed
    const run = await settled(id)
    expect(statuses(run)).toEqual({ n1: 'canceled', n2: 'skipped' })
    expect(run.status).toBe('canceled')
    // The bookkeeping is not the claim — the process is. Signal 0 probes for
    // existence and throws once the process is gone and reaped.
    expect(() => process.kill(spawned[0].pid, 0)).toThrow()
    // …and nothing downstream started in the gap.
    await expect(readFile(join(root, 'ran.txt'), 'utf8')).rejects.toThrow()
  })
})

describe('run.json and the logs', () => {
  it('is rewritten by the runner on every transition and matches the final snapshot', async () => {
    const root = await workspace()
    install({ scripts: { n1: 'echo hello-from-n1', n2: 'echo hello-from-n2 >&2' } })
    const path = await writeFlow(root, flowDoc('bookkeeping', ['n1', 'n2'], [['n1', 'n2']]))
    const { id } = await runner.startRun(path)
    const run = await settled(id)
    // The write chain is one behind the last transition by construction —
    // poll for the settled shape rather than assuming it has landed.
    const file = join(run.dir, 'run.json')
    let onDisk
    for (let i = 0; i < 100; i++) {
      onDisk = JSON.parse(await readFile(file, 'utf8'))
      if (onDisk.status !== 'running') break
      await new Promise((r) => setTimeout(r, 15))
    }
    expect(onDisk).toEqual(run)
    expect(onDisk.id).toBe(id)
    expect(onDisk.flow).toBe('bookkeeping')
    expect(onDisk.layers).toEqual([['n1'], ['n2']])
    // The runs pane draws one connector per entry, so these are the
    // scheduler's own dependencies rather than a second reading of the file.
    expect(onDisk.nodes.map((n) => n.parents)).toEqual([[], ['n1']])
    expect(onDisk.nodes.map((n) => n.exit)).toEqual([0, 0])
    expect(run.dir).toBe(join(root, '.tome', 'flows', 'bookkeeping', 'runs', id))
  })

  it('captures stdout and stderr in one log per node', async () => {
    const root = await workspace()
    install({ scripts: { n1: 'echo to-stdout; echo to-stderr >&2; exit 0' } })
    const { id } = await runner.startRun(await writeFlow(root, flowDoc('logs', ['n1'])))
    const run = await settled(id)
    const log = await readFile(run.nodes[0].log, 'utf8')
    expect(log).toContain('to-stdout')
    expect(log).toContain('to-stderr')
    expect(log).toContain('# exit 0')
    expect(run.nodes[0].log).toBe(join(run.dir, '1-n1.log'))
  })

  it('never lets a hand-edited node id aim a log write out of the run folder', async () => {
    const root = await workspace()
    install()
    const doc = flowDoc('traversal', ['../../../escaped', 'n2'])
    const { id } = await runner.startRun(await writeFlow(root, doc))
    const run = await settled(id)
    for (const node of run.nodes) expect(node.log.startsWith(run.dir + '/')).toBe(true)
    expect(run.nodes[0].log).toBe(join(run.dir, '1-.._.._.._escaped.log'))
  })
})

describe('the event log', () => {
  it('records the run and every node transition, identifiers only', async () => {
    const root = await workspace()
    const events = []
    install({ events, scripts: { n1: 'exit 0', n2: 'exit 7' } })
    const path = await writeFlow(root, flowDoc('audited', ['n1', 'n2'], [['n1', 'n2']]))
    const { id } = await runner.startRun(path)
    await settled(id)
    const logged = events.filter((e) => e.kind === 'flow-run')
    expect(logged.every((e) => e.run === id && e.flow === 'audited')).toBe(true)
    // makeEvent spreads these fields over { ts, kind }, so no field of theirs
    // may be called `kind` — one that was would file this whole family in the
    // log under the agent's name and hide it from the pane's summary.
    expect(logged.filter((e) => e.event === 'node').map((e) => e.agent)).toEqual([
      'claude',
      'claude',
      'claude',
      'claude',
    ])
    expect(logged.map((e) => [e.event, e.node, e.status])).toEqual([
      ['run', undefined, 'running'],
      ['node', 'n1', 'running'],
      ['node', 'n1', 'done'],
      ['node', 'n2', 'running'],
      ['node', 'n2', 'failed'],
      ['run', undefined, 'failed'],
    ])
    // The brief is never in the log — it embeds whatever the flow author put
    // in it, and the log records actions, not payloads.
    expect(JSON.stringify(logged)).not.toContain('You are')
  })

  it('logs the cancellation itself, not just the fallout', async () => {
    const root = await workspace()
    const events = []
    install({ events, scripts: { n1: 'exec sleep 30' } })
    const { id } = await runner.startRun(await writeFlow(root, flowDoc('stopped', ['n1'])))
    runner.cancelRun(id)
    await settled(id)
    expect(events.filter((e) => e.kind === 'flow-run').map((e) => e.event)).toContain('cancel')
  })
})

describe('runs:changed — the push the whole pane is built on', () => {
  it('sends the full snapshot array to the window on every transition', async () => {
    const root = await workspace()
    install({ scripts: { n1: 'exit 0', n2: 'exit 0' } })
    // The window is an argument (index.js hands over its one BrowserWindow),
    // so a test that omits it turns push() into a no-op via the optional
    // chain — which is how a channel the entire runs pane depends on can be
    // deleted with every other test still green.
    const sent = []
    const win = { webContents: { send: (channel, payload) => sent.push([channel, payload]) } }
    const path = await writeFlow(root, flowDoc('pushed', ['n1', 'n2'], [['n1', 'n2']]))
    const { id } = await runner.startRun(path, win)
    await settled(id)
    expect(sent.every(([channel]) => channel === 'runs:changed')).toBe(true)
    // One per transition and no more: the run starting, each node going
    // running then done, and the run settling. A missed push is a pane that
    // quietly stops matching disk.
    expect(sent).toHaveLength(6)
    // The first one carries the run before any of it had happened…
    const first = sent[0][1].find((r) => r.id === id)
    expect([first.status, ...first.nodes.map((n) => n.status)]).toEqual([
      'running',
      'pending',
      'pending',
    ])
    expect(sent.at(-1)[1]).toEqual(runner.snapshotAll())
    // …and every payload is the WHOLE array, not the run that moved. The pane
    // replaces its list from it, so a single-run payload would erase every
    // other row on every transition — which a one-run test cannot see.
    const two = await runner.startRun(await writeFlow(root, flowDoc('pushed-again', ['n1'])), win)
    await settled(two.id)
    expect(sent.at(-1)[1].map((r) => r.id)).toEqual(expect.arrayContaining([id, two.id]))
    expect(sent.at(-1)[1]).toEqual(runner.snapshotAll())
  })
})

describe('snapshotAll', () => {
  it('is plain, structured-cloneable data — no child processes cross IPC', async () => {
    const root = await workspace()
    install({ scripts: { n1: 'exit 0' } })
    const { id } = await runner.startRun(await writeFlow(root, flowDoc('cloneable', ['n1'])))
    await settled(id)
    const all = runner.snapshotAll()
    expect(() => structuredClone(all)).not.toThrow()
    expect(JSON.stringify(all)).not.toContain('"spawn"')
    // Newest first — the run you just started belongs at the top of the pane.
    expect(all[0].id).toBe(id)
  })
})
