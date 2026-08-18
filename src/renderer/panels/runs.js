// Flow runs pane — the window onto background runs
// (docs/FEATURE-PLAN-background-flow-runs.md §2). A run executed in the
// background has no pane of its own by design; this is where it becomes
// visible again: one row per run, and the selected run drawn as its pipeline
// — topological layers as columns, left to right, the way a CI pipeline
// reads — with a node's log one click away.
//
// Read-only, like the event log next to it. Cancel is the single control,
// because everything else about a run was decided when Run was pressed; a
// pane that could re-target or re-brief a live run would be a second way to
// submit work, and the whole point of the narrowed contract is that there is
// exactly one.
//
// Main is the only writer: every field rendered here comes from a snapshot
// pushed over runs:changed (or fetched by runs.list()), and the layers and
// parents in it are the scheduler's own, so the picture cannot disagree with
// what actually ran.
//
// Below the local list, a second "Remote" section (plan phase 3, remote.rs)
// gives the same read-only visibility into ANOTHER consented machine's flow
// runs, fetched fresh over ssh — see the "remote run visibility" methods
// near the bottom of this class for that half; it shares this pane only for
// screen real estate, not any state with the local list above it.

import { tome, el, toast } from '../util.js'
import { choiceModal } from '../modals.js'
import { runningCount, elapsedMs, formatElapsed } from '../../shared/flow-run-plan.js'

const SVG_NS = 'http://www.w3.org/2000/svg'

// Fixed pill footprint, so the pipeline can be laid out arithmetically
// instead of measured. Same reasoning as the flow canvas's NODE_W/NODE_H: a
// measured layout needs the elements on screen and laid out, and this pane
// re-renders while its tab may be inactive (dockview keeps hidden panels in
// the DOM at zero size), where every getBoundingClientRect reads 0.
const PILL_W = 178
const PILL_H = 30
const COL_GAP = 46 // room for a connector that reads as a curve, not a kink
const ROW_GAP = 12

// One glyph per node status. The empty ones are drawn with borders instead —
// see .runs-dot in style.css, where pending is a hollow ring, skipped a
// dashed one, and running an accent ring that spins (and holds still under
// prefers-reduced-motion, where the bright quarter alone still reads).
const NODE_ICON = { pending: '', running: '', done: '✓', failed: '✕', canceled: '⊘', skipped: '' }
// What the status badge on a run row says. Deliberately the same words the
// event log uses for the same transitions.
const RUN_TEXT = { running: 'running', done: 'done', failed: 'failed', canceled: 'canceled' }

// A cubic from one pill's right edge to the next pill's left edge. The
// control points are pulled horizontally by half the span, which is what
// makes a fan-out read as a bundle of parallel curves rather than a star of
// straight lines — and keeps a connector that skips a layer from cutting
// through the pill it passes.
function connector(x1, y1, x2, y2) {
  const k = Math.max(18, (x2 - x1) / 2)
  const p = document.createElementNS(SVG_NS, 'path')
  p.setAttribute('class', 'runs-conn')
  p.setAttribute('d', `M ${x1} ${y1} C ${x1 + k} ${y1}, ${x2 - k} ${y2}, ${x2} ${y2}`)
  return p
}

const shortTime = (iso) => {
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
}

export class RunsPanel {
  constructor() {
    this.element = el('div', 'panel-runs')
    this.element.innerHTML = `
      <div class="runs-bar">
        <span class="runs-title">flow runs</span>
        <button class="runs-refresh" title="Refresh">⟳</button>
      </div>
      <div class="runs-list"></div>
      <div class="runs-remote"></div>`
    this.runs = []
    // Rows are reconciled rather than rebuilt (see render): a run's log is a
    // scrolled element inside its row, and re-creating the row on every push
    // would throw the reader back to the top of it a few times a second.
    this.rows = new Map() // run id -> the elements of one row (see buildRow)
    this.openRun = null // which run's pipeline is expanded
    this.openNode = null // which node's log is showing, inside that run
    this.logPath = null // the log we are tailing, if any
    this.logWatched = false // …and whether main actually gave us a watch on it
    this.tailPinned = true // stick to the bottom until the reader scrolls up
    // Remote run visibility (plan phase 3, remote.rs): one group per
    // consented source, fetched on pane open and on that group's own
    // Refresh button only — see loadRemoteSources's doc comment for why
    // there is deliberately no push here.
    this.remoteGroups = new Map() // source id -> { source, runs, root, rowsEl, rows, openRunId }
  }

  init() {
    this.listEl = this.element.querySelector('.runs-list')
    this.remoteEl = this.element.querySelector('.runs-remote')
    this.element.querySelector('.runs-refresh').addEventListener('click', () => this.load())
    // Same shape as the event pane's subscription: an unsubscribe kept for
    // dispose, so a closed pane stops receiving pushes.
    this.offChanged = tome.runs.onChanged((list) => this.apply(list))
    runsPanels.add(this)
    // Elapsed is wall time, not a field of the snapshot — nothing pushes when
    // a second passes. Ticked in place (text only, no re-render) so a live run
    // counts up without disturbing a log the user is reading.
    this.timer = setInterval(() => this.tick(), 1000)
    this.load()
    this.loadRemoteSources()
  }

  async load() {
    let list
    try {
      list = await tome.runs.list()
    } catch (err) {
      this.listEl.replaceChildren(el('div', 'runs-err', err.message))
      this.rows.clear()
      return
    }
    this.apply(list)
  }

  apply(list) {
    this.runs = Array.isArray(list) ? list : []
    this.render()
    // A snapshot arrives because a node changed state — which is exactly when
    // the log of a node that had not started yet comes into existence. Re-read
    // (and re-try the watch) rather than trusting that fs.watch was armable at
    // the moment the pill was clicked.
    if (this.logPath) {
      this.armWatch()
      this.readLog()
    }
  }

  render() {
    this.listEl.querySelector('.runs-empty')?.remove()
    if (!this.runs.length) {
      this.listEl.replaceChildren(el('div', 'runs-empty', 'no flow runs yet — press Run on a flow'))
      this.rows.clear()
      this.openRun = null
      this.closeLog() // whatever we were tailing went with them
      return
    }
    // Snapshots come newest-first and a run's start time never changes, so
    // the only ordering event is a new run appearing — which belongs on top.
    // Walking oldest-first and prepending anything missing lands both the
    // first load and every later arrival in the right order, without moving
    // rows that are already on screen (a move resets the log's scroll).
    for (let i = this.runs.length - 1; i >= 0; i--) {
      const run = this.runs[i]
      let row = this.rows.get(run.id)
      if (!row) {
        row = this.buildRow(run)
        this.rows.set(run.id, row)
        this.listEl.prepend(row.root)
      }
      this.updateRow(row, run)
    }
    // A run only disappears when main forgets it (window reload), but a stale
    // row would then keep ticking against a run nobody can cancel.
    const live = new Set(this.runs.map((r) => r.id))
    for (const [id, row] of this.rows) {
      if (live.has(id)) continue
      row.root.remove()
      this.rows.delete(id)
      if (this.openRun !== id) continue
      this.openRun = null
      this.closeLog()
    }
  }

  buildRow(run) {
    const root = el('div', 'runs-run')
    const head = el('button', 'runs-row')
    head.type = 'button'
    head.setAttribute('aria-expanded', 'false')
    // The only thing that says "there is more behind this row" while it is
    // shut. Without it the cues are cursor:pointer and a hover background —
    // both of which have to be stumbled onto — and a user who never clicks
    // concludes that background runs are simply not visualised, on the one
    // page whose entire value is the pipeline underneath.
    const caret = el('span', 'runs-caret', '›')
    caret.setAttribute('aria-hidden', 'true')
    const badge = el('span', 'runs-badge')
    const flow = el('span', 'runs-flow', run.flow)
    const id = el('span', 'runs-id', `#${run.id}`)
    // Whether this run's agents were sandboxed. A background node with the gap
    // off is otherwise invisible: no pane strip, no per-pane unlock UI, no
    // relock timer, and no seat in the status bar (it creates no proxy for the
    // bar to count). This row is the only place in the app that can say five
    // unsandboxed agents are on the network, so it says it.
    const gap = el('span', 'runs-gap')
    const when = el('span', 'runs-when')
    head.append(caret, badge, flow, id, gap, when)
    head.addEventListener('click', () => this.toggleRun(run.id))

    const cancel = el('button', 'runs-cancel', 'Cancel')
    cancel.type = 'button'
    cancel.title = 'Stop this run — nodes still waiting are skipped'
    cancel.addEventListener('click', async (e) => {
      e.stopPropagation() // the row's own click would collapse the pipeline
      const res = await tome.runs.cancel(run.id)
      if (res?.error) toast(res.error)
    })

    // Only a settled run has products to copy anywhere (updateRow hides
    // this until run.status === 'done') — captures run.id, not run itself:
    // a row is built once per id and never rebuilt (see the class doc
    // comment), so the id below never goes stale even as later snapshots
    // repaint everything else about this row in place.
    const exportBtn = el('button', 'runs-export', 'Export…')
    exportBtn.type = 'button'
    exportBtn.title = "Copy this run's promoted products to a destination or folder"
    exportBtn.addEventListener('click', (e) => {
      e.stopPropagation() // the row's own click would collapse the pipeline
      this.exportRun(run.id)
    })

    const bar = el('div', 'runs-rowbar')
    bar.append(head, cancel, exportBtn)
    const body = el('div', 'runs-body') // pipeline + log, only while expanded
    // Run ids are base36 timestamps with an optional `-2` suffix, so they are
    // already safe as an id — and unique, which is what aria-controls needs to
    // point the row at the thing it opens.
    body.id = `runs-body-${run.id}`
    head.setAttribute('aria-controls', body.id)
    root.append(bar, body)
    return { root, head, badge, gap, when, cancel, exportBtn, body, pills: null, logBox: null, logNode: null }
  }

  // Everything below is deliberately in-place. A run pushes a snapshot on
  // every node transition, and rebuilding the expanded body on each one would
  // scroll a log the reader is holding open back to the top a dozen times a
  // run. The graph itself never changes mid-run — only statuses do — so the
  // pipeline is built once per expansion and then repainted.
  updateRow(row, run) {
    const open = this.openRun === run.id
    // 'canceling…' is a state of its own, not a state to hide. run.status only
    // flips when the last child exits, and a node that ignores SIGTERM gets a
    // five-second grace before the runner takes it out — so a row that still
    // reads 'running' with its Cancel button silently gone reads as a misclick
    // or a broken button, and the natural next move is to press Run again.
    const canceling = run.canceling && run.status === 'running'
    row.badge.className = `runs-badge runs-st-${canceling ? 'canceling' : run.status}`
    row.badge.textContent = canceling ? 'canceling…' : RUN_TEXT[run.status] || run.status
    row.gap.className = `runs-gap${run.egress ? '' : ' open'}`
    row.gap.textContent = run.egress ? '⛨' : '⛉ open internet'
    row.gap.title = run.egress
      ? 'Contained — the same sandbox and model-APIs-only proxy a fresh agent pane gets'
      : 'Ran on the open internet — the ＋ menu’s egress default was off when Run was pressed'
    // The gated state is a bare shield, which is the right amount of visual
    // noise and the wrong amount of spoken one — the row is a button, so its
    // name is built from its contents and a lone glyph would read as nothing
    // useful in the middle of it.
    row.gap.setAttribute('aria-label', run.egress ? 'gapped' : 'on the open internet')
    row.when.textContent = this.whenText(run)
    // Disabled rather than hidden: the control stays where the user last
    // clicked it, saying what it is now doing.
    row.cancel.style.display = run.status === 'running' ? '' : 'none'
    row.cancel.disabled = canceling
    row.cancel.textContent = canceling ? 'Canceling…' : 'Cancel'
    // Only a settled run has a products directory to export at all — see
    // ipc::runs::runs_export's own doc comment for the exact convention.
    row.exportBtn.style.display = run.status === 'done' ? '' : 'none'
    row.head.setAttribute('aria-expanded', open ? 'true' : 'false')
    row.head.classList.toggle('open', open)
    row.head.title = run.flowPath || ''
    if (!open) {
      row.body.replaceChildren()
      row.pills = null
      row.logBox = null
      row.logNode = null
      return
    }
    if (!row.pills) {
      row.body.replaceChildren()
      row.body.appendChild(this.buildPipeline(run, row))
    } else {
      this.repaintPills(row, run)
    }
    const node = this.openNode ? run.nodes.find((n) => n.id === this.openNode) : null
    if (this.openNode && !node) this.closeLog() // the node it belonged to is gone
    if (node && row.logNode !== node.id) {
      row.logBox?.remove()
      row.logBox = this.buildLog(node)
      row.logNode = node.id
      row.body.appendChild(row.logBox)
    } else if (!node && row.logBox) {
      row.logBox.remove()
      row.logBox = null
      row.logNode = null
    }
  }

  repaintPills(row, run) {
    for (const node of run.nodes) {
      const pill = row.pills.get(node.id)
      if (!pill) continue
      pill.className = `runs-pill runs-st-${node.status}${this.openNode === node.id ? ' open' : ''}`
      const dot = pill.querySelector('.runs-dot')
      dot.className = `runs-dot runs-st-${node.status}`
      dot.textContent = NODE_ICON[node.status] ?? ''
      pill.title = this.pillTitle(node)
      pill.setAttribute('aria-label', pill.title)
    }
  }

  whenText(run) {
    const at = shortTime(run.started)
    const took = formatElapsed(elapsedMs(run))
    return `${at ? at + ' · ' : ''}${took}`
  }

  // Ticks the elapsed text only. A full render every second would rebuild the
  // open pipeline and re-fetch the open log for no new information.
  tick() {
    if (!this.runs.some((r) => r.status === 'running')) return
    for (const run of this.runs) {
      const row = this.rows.get(run.id)
      if (row) row.when.textContent = this.whenText(run)
    }
  }

  // The pipeline: one column per topological layer, laid out from the plan
  // the scheduler used. Absolute positions over a fixed footprint, with the
  // connectors in one SVG underneath — the same split the flow canvas uses.
  buildPipeline(run, row) {
    const wrap = el('div', 'runs-pipeline')
    const layers = Array.isArray(run.layers) && run.layers.length ? run.layers : [run.nodes.map((n) => n.id)]
    const at = new Map() // node id -> its top-left corner
    layers.forEach((layer, col) =>
      layer.forEach((id, slot) => at.set(id, { x: col * (PILL_W + COL_GAP), y: slot * (PILL_H + ROW_GAP) }))
    )
    const tallest = Math.max(1, ...layers.map((l) => l.length))
    const width = layers.length * PILL_W + (layers.length - 1) * COL_GAP
    const height = tallest * PILL_H + (tallest - 1) * ROW_GAP
    wrap.style.width = `${width}px`
    wrap.style.height = `${height}px`

    const svg = document.createElementNS(SVG_NS, 'svg')
    svg.setAttribute('class', 'runs-edges')
    svg.setAttribute('width', String(width))
    svg.setAttribute('height', String(height))
    for (const node of run.nodes) {
      const to = at.get(node.id)
      if (!to) continue
      for (const parentId of node.parents || []) {
        const from = at.get(parentId)
        // Kahn layers put every parent in a strictly earlier column, so this
        // always runs left to right — no back-edge case to handle.
        if (from) svg.appendChild(connector(from.x + PILL_W, from.y + PILL_H / 2, to.x, to.y + PILL_H / 2))
      }
    }
    wrap.appendChild(svg)

    row.pills = new Map()
    for (const node of run.nodes) {
      const pos = at.get(node.id)
      if (!pos) continue
      const pill = this.buildPill(node, pos)
      row.pills.set(node.id, pill)
      wrap.appendChild(pill)
    }
    return wrap
  }

  pillTitle(node) {
    const exit = node.exit == null ? '' : ` · exit ${node.exit}`
    return `${node.name} · ${node.kind}${node.model ? ' · ' + node.model : ''} — ${node.status}${exit}`
  }

  buildPill(node, pos) {
    const pill = el('button', `runs-pill runs-st-${node.status}${this.openNode === node.id ? ' open' : ''}`)
    pill.type = 'button'
    pill.style.left = `${pos.x}px`
    pill.style.top = `${pos.y}px`
    pill.style.width = `${PILL_W}px`
    pill.style.height = `${PILL_H}px`
    const dot = el('span', `runs-dot runs-st-${node.status}`, NODE_ICON[node.status] ?? '')
    dot.setAttribute('aria-hidden', 'true')
    const name = el('span', 'runs-pill-name', node.name)
    pill.append(dot, name)
    // A pinned model changes what this node actually ran, so it rides on the
    // pill exactly as it rides on the canvas card.
    if (node.model) pill.appendChild(el('span', 'runs-pill-model', `· ${node.model}`))
    pill.title = this.pillTitle(node)
    pill.setAttribute('aria-label', pill.title)
    pill.addEventListener('click', () => this.toggleNode(node))
    return pill
  }

  buildLog(node) {
    const box = el('div', 'runs-log')
    const head = el('div', 'runs-log-head')
    head.append(el('span', 'runs-log-name', `${node.name} — ${node.log.split('/').pop()}`))
    const close = el('button', 'runs-log-close', '✕')
    close.type = 'button'
    close.title = 'Close this log'
    close.setAttribute('aria-label', 'Close this log')
    close.addEventListener('click', () => {
      this.closeLog()
      this.render()
    })
    head.appendChild(close)
    const body = el('pre', 'runs-log-body')
    body.tabIndex = 0 // a scrollable region has to be reachable from the keyboard
    // Autoscroll only while the reader is at the bottom. Anything else means
    // they went looking for something earlier in the log, and yanking them
    // back on the next write is the fastest way to make a tail unreadable.
    body.addEventListener('scroll', () => {
      this.tailPinned = body.scrollHeight - body.scrollTop - body.clientHeight < 24
    })
    box.append(head, body)
    this.logEl = body
    this.readLog()
    return box
  }

  toggleRun(id) {
    if (this.openRun === id) {
      this.openRun = null
      this.closeLog()
    } else {
      this.openRun = id
      this.closeLog() // a different run's node log has nothing to do with this one
    }
    this.render()
  }

  toggleNode(node) {
    const same = this.openNode === node.id
    this.closeLog()
    if (!same) {
      this.openNode = node.id
      this.logPath = node.log
      this.armWatch()
    }
    this.render()
  }

  // Export…: a choice of every consented destination plus "Save to
  // folder…", then main does the actual copy — this pane never sees (or
  // needs) a host/url/target itself, only an id or a folder the user just
  // picked. Read-only otherwise (the class doc comment), and this is no
  // exception: it doesn't change what ran, only where a copy of what
  // already ran ends up.
  async exportRun(runId) {
    let destinations = []
    try {
      destinations = await tome.exportDest.list()
    } catch (err) {
      toast(err.message)
      return
    }
    const choices = destinations.map((d) => ({
      label: `${d.label} (${d.kind})`,
      value: { destinationId: d.id },
    }))
    choices.push({ label: 'Save to folder…', value: { local: true }, cls: 'ghost' })
    const picked = await choiceModal('Export run', 'Where should this run’s products go?', choices)
    if (!picked) return
    let localPath = null
    if (picked.local) {
      localPath = await tome.pickFolder()
      if (!localPath) return
    }
    try {
      await tome.runs.export(runId, picked.destinationId, localPath)
      toast('run exported', 'ok')
    } catch (err) {
      toast(err.message)
    }
  }

  // ---- remote run visibility (plan phase 3) ----
  //
  // A second, independent section under the local runs list: one group per
  // consented remote source (Settings → Remote sources), each holding its
  // own flattened run list fetched fresh over ssh. Deliberately NOT wired to
  // any push/onChanged the way the local list above is — main never watches
  // a remote host, so there is nothing to push, and re-fetching on every
  // pane open plus an explicit per-group Refresh button (mirroring the
  // local pane's own .runs-refresh) is the whole contract. Remote rows never
  // get a Cancel or Export button: nothing here can be cancelled (main isn't
  // running it) and a remote run's products already live on the machine
  // that made them.

  // Sources first (remote_sources never touches the network — it just lists
  // main's own consented, hash-verified records), then one runs() fetch per
  // source. A source with nothing consented yet renders an empty-state hint
  // pointing at Settings rather than an empty section.
  async loadRemoteSources() {
    let sources = []
    try {
      sources = await tome.remote.sources()
    } catch (err) {
      this.remoteEl.replaceChildren(el('div', 'runs-err', err.message))
      return
    }
    this.remoteGroups.clear()
    this.remoteEl.replaceChildren()
    if (!sources.length) {
      this.remoteEl.appendChild(
        el('div', 'runs-remote-empty', 'no remote sources yet — add one in Settings → Remote sources')
      )
      return
    }
    for (const source of sources) {
      const group = this.buildRemoteGroup(source)
      this.remoteGroups.set(source.id, group)
      this.remoteEl.appendChild(group.root)
      this.refreshRemoteGroup(source.id)
    }
  }

  buildRemoteGroup(source) {
    const root = el('div', 'runs-remote-group')
    const head = el('div', 'runs-remote-head')
    head.append(el('span', 'runs-remote-label', `${source.label} — ${source.host}`))
    const refresh = el('button', 'runs-refresh', '⟳')
    refresh.type = 'button'
    refresh.title = 'Refresh'
    refresh.addEventListener('click', () => this.refreshRemoteGroup(source.id))
    head.appendChild(refresh)
    const rowsEl = el('div', 'runs-remote-rows')
    root.append(head, rowsEl)
    return { source, runs: [], root, rowsEl, rows: new Map(), openRunId: null }
  }

  // Re-fetches ONE group's run list. Rows are rebuilt wholesale on every
  // refresh (unlike the local list's reconciled rows) — there is no log
  // tailing or per-second tick to preserve here, and a remote fetch already
  // happens only on an explicit user action, never several times a second.
  async refreshRemoteGroup(sourceId) {
    const group = this.remoteGroups.get(sourceId)
    if (!group) return
    let runs
    try {
      runs = await tome.remote.runs(sourceId)
    } catch (err) {
      group.rowsEl.replaceChildren(el('div', 'runs-err', err.message))
      return
    }
    group.runs = Array.isArray(runs) ? runs : []
    group.rows.clear()
    group.openRunId = null
    group.rowsEl.replaceChildren()
    if (!group.runs.length) {
      group.rowsEl.appendChild(el('div', 'runs-empty', 'no runs yet'))
      return
    }
    for (const entry of group.runs) {
      const row = this.buildRemoteRow(group, entry)
      group.rows.set(entry.id, row)
      group.rowsEl.append(row.head, row.detail)
    }
  }

  // Same head/body split the local pane's buildRow/updateRow use, minus
  // Cancel/Export (the class doc comment: nothing here can be stopped, and a
  // remote run's products already live where they were made) and minus the
  // pipeline graph (main never sees this run live — layers are a scheduler
  // artifact of a LIVE run, not something a settled run.json carries).
  buildRemoteRow(group, entry) {
    const head = el('button', 'runs-row')
    head.type = 'button'
    head.setAttribute('aria-expanded', 'false')
    const caret = el('span', 'runs-caret', '›')
    caret.setAttribute('aria-hidden', 'true')
    const badge = el('span', `runs-badge runs-st-${entry.status}`, RUN_TEXT[entry.status] || entry.status)
    const flow = el('span', 'runs-flow', entry.flow || '(unknown flow)')
    const id = el('span', 'runs-id', `#${entry.id}`)
    const when = el('span', 'runs-when', shortTime(entry.started))
    head.append(caret, badge, flow, id, when)
    const detail = el('div', 'runs-remote-detail')
    head.addEventListener('click', () => this.toggleRemoteRow(group, entry, head, detail))
    return { head, detail }
  }

  async toggleRemoteRow(group, entry, head, detail) {
    const wasOpen = group.openRunId === entry.id
    // Only one remote run open per group at a time — same "one pipeline
    // visible" simplicity the local pane's openRun enforces.
    if (group.openRunId && group.openRunId !== entry.id) {
      const prev = group.rows.get(group.openRunId)
      if (prev) {
        prev.head.setAttribute('aria-expanded', 'false')
        prev.head.classList.remove('open')
        prev.detail.replaceChildren()
      }
    }
    group.openRunId = wasOpen ? null : entry.id
    head.classList.toggle('open', !wasOpen)
    head.setAttribute('aria-expanded', String(!wasOpen))
    if (wasOpen) {
      detail.replaceChildren()
      return
    }
    detail.replaceChildren(el('div', 'runs-remote-loading', 'loading…'))
    let payload
    try {
      payload = await tome.remote.runDetail(group.source.id, entry.flow, entry.id)
    } catch (err) {
      // A row can close (or a different one open) while this was in flight —
      // never paint over whatever the reader is looking at now.
      if (group.openRunId === entry.id) detail.replaceChildren(el('div', 'runs-err', err.message))
      return
    }
    if (group.openRunId === entry.id) detail.replaceChildren(this.buildRemoteDetail(payload))
  }

  // A node table (id/kind/model/status/exit — the same facts each pill's
  // title string carries locally, laid out as rows instead since there is
  // no live pipeline graph to place pills on) plus the product list from
  // the manifest, if the run has been promoted yet (flow::products only
  // writes manifest.json once a run settles "done" AND every declared
  // output is confirmed on disk — see that module's doc comment; a run
  // still in progress on the remote end legitimately has none).
  buildRemoteDetail(payload) {
    const wrap = el('div', 'runs-remote-detail-body')
    const nodes = Array.isArray(payload?.run?.nodes) ? payload.run.nodes : []
    const table = el('table', 'runs-remote-nodes')
    const head = el('tr')
    head.append(el('th', '', 'node'), el('th', '', 'kind'), el('th', '', 'status'), el('th', '', 'exit'))
    table.append(head)
    for (const n of nodes) {
      const tr = el('tr')
      tr.append(
        el('td', '', n.name || n.id),
        el('td', '', n.model ? `${n.kind} · ${n.model}` : n.kind),
        el('td', `runs-remote-node-st runs-st-${n.status}`, n.status),
        el('td', '', n.exit == null ? '' : String(n.exit))
      )
      table.append(tr)
    }
    wrap.appendChild(table)

    const products = Array.isArray(payload?.manifest?.products) ? payload.manifest.products : null
    const productsBox = el('div', 'runs-remote-products')
    productsBox.appendChild(el('h5', '', 'Products'))
    if (!products) {
      productsBox.appendChild(el('div', 'runs-remote-hint', 'not promoted yet'))
    } else if (!products.length) {
      productsBox.appendChild(el('div', 'runs-remote-hint', 'this flow declares no terminal outputs'))
    } else {
      const list = el('ul', 'runs-remote-product-list')
      for (const p of products) {
        list.appendChild(el('li', '', `${p.file} — ${p.bytes.toLocaleString()} bytes`))
      }
      productsBox.appendChild(list)
    }
    wrap.appendChild(productsBox)
    return wrap
  }

  // Refcounted and debounced in main, so a tail is a watch plus a re-read
  // rather than a poll — the same plumbing editors use. A node that has not
  // started has no log file yet and cannot be watched at all, which is why
  // this is idempotent and re-tried from apply() on every snapshot.
  async armWatch() {
    const path = this.logPath
    if (this.logWatched || !path) return
    const ok = await tome.fs.watch(path)
    // Only record success: main refcounts watches, and claiming one we never
    // got would send an unwatch that decrements somebody else's.
    if (this.logPath === path && ok) this.logWatched = true
  }

  closeLog() {
    if (this.logPath && this.logWatched) tome.fs.unwatch(this.logPath)
    this.logPath = null
    this.logWatched = false
    this.openNode = null
    this.logEl = null
    this.tailPinned = true
  }

  // Re-read the whole log rather than tracking an offset: these are one file
  // per node of an agent's own output, read at human speed, and a length-based
  // tail would have to handle a file that was truncated and rewritten anyway.
  async readLog() {
    const path = this.logPath
    const body = this.logEl
    if (!path || !body) return
    let text
    try {
      text = await tome.fs.readFile(path)
    } catch {
      // Expected for a node that has not started: the runner opens the log
      // when it spawns, so there is genuinely nothing there yet.
      text = ''
    }
    // A late reply for a log the user already closed (or swapped) must not
    // paint over the current one.
    if (this.logPath !== path || this.logEl !== body) return
    body.textContent = text || 'no output yet'
    if (this.tailPinned) body.scrollTop = body.scrollHeight
  }

  // Status bar context: how much is running, from this pane's own snapshot.
  statusMeta() {
    const n = runningCount(this.runs)
    return { icon: '▶', text: n ? `${n} running` : '' }
  }

  dispose() {
    this.offChanged?.()
    clearInterval(this.timer)
    this.closeLog()
    runsPanels.delete(this)
  }
}

// One fs:changed listener for every runs pane, not one per instance — main
// sends a single event per changed path, so per-instance listeners would each
// re-read the file. Same shape as flow.js's flowPanels and editor.js's
// editors sets.
const runsPanels = new Set()
tome.fs.onChanged((p) => {
  for (const panel of runsPanels) if (panel.logPath === p) panel.readLog()
})
