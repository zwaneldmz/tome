// Bottom status bar: active root, open-pane count, and air-gap network state.
// Updated by panes.js (pane add/remove), workspaces/tree (active root), and
// airgap-ui.js (per-pane network mode). Pure presentation — reads shared state.
import { wsState, agState } from './state.js'

const rootEl = document.getElementById('sb-root')
const panesEl = document.getElementById('sb-panes')
const airgapEl = document.getElementById('sb-airgap')

// panes.js injects the dock after creating it — avoids a panes<->statusbar
// import cycle at module-evaluation time.
let dock = null
export function setStatusbarDock(d) {
  dock = d
}

export function renderStatusbar() {
  // active root — the folder new panes and the git widget follow
  const root = wsState.activeRoot
  rootEl.textContent = root ? `▸ ${root.split('/').pop() || root}` : ''
  rootEl.title = root ? `Active root — ${root}` : 'Active root — new panes and git follow this folder'

  // open pane count
  const n = dock ? dock.panels.length : 0
  panesEl.textContent = n ? `${n} pane${n === 1 ? '' : 's'}` : ''

  // air-gap network state: count panes currently open to the internet
  const panes = Object.values(agState.panes || {})
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
