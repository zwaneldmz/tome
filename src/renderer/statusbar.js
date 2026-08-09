// Bottom status bar: active root, active-pane context, open-pane count,
// background flow runs, and air-gap network state. Updated by panes.js (pane
// add/remove/focus), workspaces/tree (active root), airgap-ui.js (per-pane
// network mode), and — for runs alone — a subscription of its own below.
// Otherwise pure presentation: it reads shared state.  Panels may expose
// statusMeta() returning { icon, text } for contextual info (editor line:col,
// terminal cwd).
import { tome } from './util.js'
import { wsState, agState } from './state.js'
import { addRuns } from './panes.js'
import { runningCount, RUN_PANE_PREFIX } from '../shared/flow-run-plan.js'

const rootEl = document.getElementById('sb-root')
const contextEl = document.getElementById('sb-context')
const panesEl = document.getElementById('sb-panes')
const runsEl = document.getElementById('sb-runs')
const airgapEl = document.getElementById('sb-airgap')

// panes.js injects the dock after creating it — avoids a panes<->statusbar
// import cycle at module-evaluation time.
let dock = null
export function setStatusbarDock(d) {
  dock = d
}

// The active panel's contextual metadata (icon + text), if it provides any.
function renderContext() {
  contextEl.replaceChildren()
  contextEl.title = 'Active pane'
  const panel = dock?.activePanel
  const view = panel?.view?.content
  if (typeof view?.statusMeta !== 'function') return
  const meta = view.statusMeta()
  if (!meta || !meta.text) return
  if (typeof meta.icon === 'function') {
    const wrap = document.createElement('span')
    wrap.className = 'sb-ctx-icon'
    wrap.appendChild(meta.icon())
    contextEl.appendChild(wrap)
  } else if (typeof meta.icon === 'string' && meta.icon) {
    // A glyph rather than an SVG factory — a panel whose icon is one
    // character shouldn't have to ship a function to draw it (runs.js).
    const wrap = document.createElement('span')
    wrap.className = 'sb-ctx-icon'
    wrap.textContent = meta.icon
    contextEl.appendChild(wrap)
  }
  contextEl.appendChild(document.createTextNode(meta.text))
  if (meta.title) contextEl.title = meta.title
}

export function renderStatusbar() {
  // active root — the folder new panes and the git widget follow
  const root = wsState.activeRoot
  rootEl.textContent = root ? `▸ ${root.split('/').pop() || root}` : ''
  rootEl.title = root ? `Active root — ${root}` : 'Active root — new panes and git follow this folder'

  // active-pane context (editor line:col, terminal cwd, …)
  renderContext()

  // open pane count
  const n = dock ? dock.panels.length : 0
  panesEl.textContent = n ? `${n} pane${n === 1 ? '' : 's'}` : ''

  // background flow runs — the only thing in this app that keeps working with
  // no pane to show for it, which is exactly why it gets a permanent seat
  // here. Empty when nothing is running, and `.sb-item:empty` hides it.
  runsEl.textContent = runsLive ? `▶ ${runsLive} running` : ''
  runsEl.title = `${runsLive} flow run${runsLive === 1 ? '' : 's'} in the background — open the runs page`

  // air-gap network state: count panes currently open to the internet.
  //
  // PANES, and only panes. A background flow node opens an air-gap proxy of
  // its own under a `run:` pane id it invented for the purpose
  // (flow-run-plan.js's RUN_PANE_PREFIX) — main's airgap map cannot tell the
  // two apart, but this item says "pane" and a run has none: no strip, no
  // unlock UI, no window. Counted here, pressing Run on a three-node flow
  // would light a previously blank chip up as "⛨ 2 gated" and flicker the
  // number for the length of the run, for panes the user cannot find. A run's
  // own gap state is rendered on its row in the runs pane instead.
  const panes = Object.entries(agState.panes || {})
    .filter(([id]) => !id.startsWith(RUN_PANE_PREFIX))
    .map(([, p]) => p)
  const gapped = panes.length
  const open = panes.filter((p) => p?.mode === 'open').length
  airgapEl.classList.remove('sb-open', 'sb-shut')
  if (!gapped) {
    airgapEl.textContent = ''
  } else if (open) {
    airgapEl.textContent = `⛉ ${open} open`
    airgapEl.classList.add('sb-open')
    airgapEl.title = `${open} of ${gapped} air-gapped pane${gapped === 1 ? '' : 's'} on open internet`
  } else {
    airgapEl.textContent = `⛨ ${gapped} gated`
    airgapEl.classList.add('sb-shut')
    airgapEl.title = `${gapped} air-gapped pane${gapped === 1 ? '' : 's'} — model APIs only`
  }
}

// ---------- background flow runs ----------
// The one item here that isn't fed by whoever changed the state: a run
// transitions in main, with no renderer action behind it, so this subscribes
// to the same push the runs pane reads. Module-level, and so is its disposer —
// there is exactly one status bar for the life of the window, and a second
// subscription would just double-count the same snapshot.
let runsLive = 0
let offRuns = null

function countRuns(list) {
  runsLive = runningCount(list)
  renderStatusbar()
}

function watchRuns() {
  // The disposer is kept rather than dropped on the floor: re-arming has to
  // drop the previous listener, or two subscriptions would count the same
  // snapshot twice and the bar would read "4 running" for two runs.
  offRuns?.()
  offRuns = tome.runs.onChanged(countRuns)
  // The push only fires on a transition, so a window that opened while a run
  // was already going would show nothing until the next one. Lock-gated like
  // every other channel: while the lock screen is still up this rejects, and
  // there is nothing to count yet anyway.
  tome.runs.list().then(countRuns, () => {})
}
watchRuns()

// panes.js imports this module, so importing addRuns back out of it is a
// cycle — and a sharper one than flow.js's openAsText, which is worth being
// precise about because the header above says this file deliberately took the
// dock by injection to AVOID exactly this.
//
// Two things keep it safe, and only the first is obvious. (1) addRuns is a
// hoisted function declaration read inside a click handler, so a half-
// evaluated panes.js is never a problem here. (2) The order runs the other
// way too: panes.js calls setStatusbarDock at ITS top level, and `dock` below
// is a `let` — so this module must finish evaluating BEFORE panes.js's body,
// or that call lands in the temporal dead zone and takes the whole renderer
// down at boot. It does finish first, because renderer.js reaches panes.js
// before anything else that pulls this file in (util/state/regs, the three
// imports ahead of it, are leaves), and statusbar.js is therefore always
// entered from inside panes.js's own import phase.
//
// So: this file may keep reading panes.js from callbacks, but it must never
// read one at module-evaluation time, and renderer.js must keep importing
// panes.js ahead of any other module that reaches this one.
runsEl.addEventListener('click', () => addRuns())
