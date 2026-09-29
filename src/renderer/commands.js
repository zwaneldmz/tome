// ⌘K — the command palette. Everything Tome can do is one row here: start
// an agent, jump to a pane, flip a security switch, open a file. Type to
// filter everything; start with ">" to search commands only.
//
// Commands are built fresh each time the palette opens, so the list always
// reflects live state (installed agents, open panes, current toggles).
import { tome, el, toast } from './util.js'
import { prefs, wsState } from './state.js'
import { dock, addTerminal, addChat, addBrain, addGraphify, addRuns, addEvents, addReport, addHistory, addFlow, openFile } from './panes.js'
import { activeWorkspace } from './workspaces.js'
import { closeMenus, switchWorkspace, addFolderToActive } from './menus.js'
import { spawnPolicy } from './spawn-policy.js'
import { setTheme, themeState } from './theme.js'
import { toggleSidebar } from './chrome.js'
import { showSideView, jumpToWaiting } from './sidebar.js'
import { renderSeal } from './seal.js'
import { pushFlow } from './git.js'
import { zoomTerminals } from './panels/terminal.js'

const isMac = navigator.platform.startsWith('Mac')
export const MOD = isMac ? '⌘' : 'Ctrl+'
const SHIFT = isMac ? '⇧' : 'Shift+'

// ---------- file index ----------
const SKIP_DIRS = new Set(['node_modules', '.git', 'out', 'dist', '.venv', '__pycache__', '.next', 'target'])
const MAX_DEPTH = 8
const MAX_DIRS = 400
const MAX_FILES = 4000
const MAX_RESULTS = 50

// Lazily walked; the palette is usable (and keeps filling in) while big
// trees are still being scanned.
class FileIndex {
  constructor(roots) {
    this.files = []
    this.roots = roots
    this.dirs = 0
    this.cancelled = false
    this.done = false
    this.version = 0
  }
  cancel() {
    this.cancelled = true
  }
  async start() {
    const walk = async (dir, depth) => {
      if (this.cancelled || this.files.length >= MAX_FILES) return
      if (depth > MAX_DEPTH || ++this.dirs > MAX_DIRS) return
      let entries
      try {
        entries = await tome.fs.readDir(dir)
      } catch {
        return
      }
      for (const e of entries) {
        if (this.cancelled || this.files.length >= MAX_FILES) return
        const path = dir + '/' + e.name
        if (e.dir) {
          if (!SKIP_DIRS.has(e.name) && !e.name.startsWith('.')) await walk(path, depth + 1)
        } else this.files.push(path)
      }
      this.version++
    }
    await Promise.all(this.roots.map((r) => walk(r, 0)))
    this.done = true
    this.version++
  }
}

// Subsequence match: consecutive runs and word starts rank higher; null when
// the query is not a subsequence of the text.
export function fuzzy(query, text) {
  const q = query.toLowerCase()
  const t = text.toLowerCase()
  let ti = 0
  let score = 0
  let run = 0
  for (let qi = 0; qi < q.length; qi++) {
    if (q[qi] === ' ') continue
    const found = t.indexOf(q[qi], ti)
    if (found === -1) return null
    run = found === ti ? run + 1 : 0
    score += run * 4
    if (found === 0 || '/._- '.includes(t[found - 1])) score += 6
    score -= (found - ti) * 0.3
    ti = found + 1
  }
  return score - text.length * 0.01
}

function relToWorkspace(path) {
  for (const root of activeWorkspace()?.folders || []) if (path.startsWith(root + '/')) return path.slice(root.length + 1)
  return path
}

// ---------- the command list ----------
// { name, section, keys?, run, when? } — `when` false hides the row.
async function buildCommands() {
  const policy = spawnPolicy(prefs)
  const agents = await tome.agents.list().catch(() => [])
  const disabled = new Set((await tome.store.get('agents-disabled').catch(() => null)) || [])
  const hasRoot = !!wsState.activeRoot
  const cmds = []
  const add = (section, name, run, extra = {}) => cmds.push({ section, name, run, ...extra })

  for (const a of agents.filter((a) => a.available && !disabled.has(a.name)))
    add('Start', `New ${a.label || a.name} agent`, () => addTerminal(a.name), { hint: policy.agentsGapped ? 'sealed' : 'unsealed' })
  if (policy.showUnsandboxedTerminal) add('Start', 'New terminal', () => addTerminal('terminal'), { hint: 'host shell' })
  add('Start', 'New assistant chat', () => addChat())
  if (hasRoot) add('Start', 'New flow…', () => addFlow())

  add('Go', 'Next agent waiting for you', () => jumpToWaiting() || toast('No agent is waiting for you', 'ok'), { keys: MOD + 'J' })
  for (const p of dock.panels)
    add('Go', (p.title || p.id).replace(/^[^\p{L}\p{N}.]+/u, ''), () => p.api.setActive(), { hint: 'open' })

  add('View', 'Show open panes', () => showSideView('open'), { keys: MOD + SHIFT + 'A' })
  add('View', 'Show files', () => showSideView('files'), { keys: MOD + SHIFT + 'E' })
  add('View', 'Show changes', () => showSideView('changes'), { keys: MOD + SHIFT + 'G' })
  add('View', 'Toggle sidebar', toggleSidebar, { keys: MOD + 'B' })
  for (const [pref, label] of [['system', 'Match system'], ['light', 'Light'], ['dark', 'Dark']])
    if (themeState.pref !== pref) add('View', `Theme: ${label}`, () => setTheme(pref))
  add('View', 'Bigger terminal text', () => zoomTerminals(1), { keys: MOD + '=' })
  add('View', 'Smaller terminal text', () => zoomTerminals(-1), { keys: MOD + '-' })

  if (policy.showEgressDefaultToggle)
    add('Security', prefs.egressDefault ? 'Start agents unsealed (full network)' : 'Start agents sealed', () => {
      prefs.egressDefault = !prefs.egressDefault
      tome.store.set('egress-default', prefs.egressDefault)
      renderSeal()
      toast(prefs.egressDefault ? 'New agents start sealed' : 'New agents start unsealed', 'ok')
    })
  add('Security', prefs.conductorRun ? 'Stop the assistant running commands' : 'Let the assistant run commands', () => {
    prefs.conductorRun = !prefs.conductorRun
    tome.store.set('conductor-run', prefs.conductorRun)
    tome.conductor.allowRun(prefs.conductorRun)
    toast(prefs.conductorRun ? 'The assistant may run commands' : 'The assistant types; you press Enter', 'ok')
  })
  add('Security', 'Event log', () => addEvents(), { hint: 'audit' })

  if (hasRoot) {
    add('Git', 'Review changes', () => showSideView('changes'))
    add('Git', 'Push', () => pushFlow())
    add('Git', 'Commit history', () => addHistory())
    add('Git', 'Switch branch…', () => document.getElementById('git-chip')?.click())
  }

  wsState.ws.workspaces.forEach((w, i) => {
    if (i !== wsState.ws.active) add('Workspace', `Switch to ${w.name}`, () => switchWorkspace(i))
  })
  if (activeWorkspace()) add('Workspace', 'Add folder to workspace…', addFolderToActive)
  add('Workspace', 'New workspace…', () => document.getElementById('ws-chip')?.click())

  if (activeWorkspace()) add('Tools', 'Brain — notes vault', () => addBrain())
  if (hasRoot) add('Tools', 'Code graph', () => addGraphify())
  add('Tools', 'Flow runs', () => addRuns())
  add('Tools', 'Usage report', () => addReport())
  add('Tools', 'Open file…', async () => {
    const p = await tome.pickFile()
    if (p) openFile(p)
  })

  add('Tome', 'Settings', () => import('./preferences.js').then((m) => m.preferencesModal()), { keys: MOD + ',' })
  add('Tome', 'Keyboard shortcuts', () => import('./keys.js').then((m) => m.shortcutsModal()), { keys: MOD + '/' })
  add('Tome', 'Setup wizard', () => import('./onboarding.js').then((m) => m.showOnboarding()))
  return cmds
}

// ---------- the palette ----------
export async function openPalette(initial = '') {
  if (document.getElementById('palette')) return
  closeMenus()
  const restoreFocus = document.activeElement
  const overlay = el('div', 'pal-overlay')
  overlay.id = 'palette'
  const box = el('div', 'pal-box')
  box.setAttribute('role', 'dialog')
  box.setAttribute('aria-label', 'Command palette')
  const input = el('input', 'pal-input')
  input.placeholder = 'Search files, panes, and commands — type > for commands only'
  input.spellcheck = false
  input.value = initial
  input.setAttribute('role', 'combobox')
  input.setAttribute('aria-expanded', 'true')
  input.setAttribute('aria-controls', 'pal-list')
  const list = el('div', 'pal-list')
  list.id = 'pal-list'
  list.setAttribute('role', 'listbox')
  const foot = el('div', 'pal-foot')
  foot.innerHTML = '<span><kbd>↑</kbd><kbd>↓</kbd> move</span><span><kbd>↵</kbd> run</span><span><kbd>esc</kbd> close</span>'
  box.append(input, list, foot)
  overlay.appendChild(box)
  overlay.addEventListener('mousedown', (e) => e.target === overlay && close())
  document.body.appendChild(overlay)
  input.focus()

  let commands = []
  const w = activeWorkspace()
  const index = w?.folders.length ? new FileIndex(w.folders) : null
  index?.start()
  let results = []
  let sel = 0
  let shownVersion = -1

  function close() {
    index?.cancel()
    overlay.remove()
    if (restoreFocus?.isConnected) restoreFocus.focus?.()
  }

  function refresh() {
    let q = input.value.trim()
    const cmdOnly = q.startsWith('>')
    if (cmdOnly) q = q.slice(1).trim()
    const scored = []
    for (const c of commands) {
      const s = q ? fuzzy(q, c.name + ' ' + c.section) : 0
      if (s !== null) scored.push([s, c])
    }
    if (index && q && !cmdOnly) {
      for (const f of index.files) {
        const rel = relToWorkspace(f)
        const s = fuzzy(q, rel)
        if (s === null) continue
        const slash = rel.lastIndexOf('/')
        scored.push([s - 2, { section: 'Files', name: slash === -1 ? rel : rel.slice(slash + 1), hint: slash === -1 ? '' : rel.slice(0, slash), run: () => openFile(f) }])
      }
    }
    // With a query: best match first. Without: the curated section order.
    if (q) scored.sort((a, b) => b[0] - a[0])
    results = scored.slice(0, q ? MAX_RESULTS : 200).map((r) => r[1])
    sel = Math.min(sel, Math.max(0, results.length - 1))
    shownVersion = index?.version ?? -1
    list.replaceChildren()
    let lastSection = null
    results.forEach((it, i) => {
      if (!q && it.section !== lastSection) {
        list.appendChild(el('div', 'pal-section', it.section))
        lastSection = it.section
      }
      const row = el('div', 'pal-row' + (i === sel ? ' sel' : ''))
      row.id = `pal-opt-${i}`
      row.setAttribute('role', 'option')
      row.setAttribute('aria-selected', String(i === sel))
      const main = el('span', 'pal-name', it.name)
      row.appendChild(main)
      if (q) row.appendChild(el('span', 'pal-tag', it.section))
      if (it.hint) row.appendChild(el('span', 'pal-hint', it.hint))
      if (it.keys) row.appendChild(el('kbd', 'pal-keys', it.keys))
      row.addEventListener('click', () => pick(it))
      row.addEventListener('mousemove', () => {
        if (sel !== i) {
          sel = i
          paintSel()
        }
      })
      list.appendChild(row)
    })
    if (!results.length)
      list.appendChild(el('div', 'pal-empty', index && !index.done ? `Searching… ${index.files.length} files so far` : 'No matches. Try fewer letters, or > for commands.'))
    paintSel()
  }

  function paintSel() {
    list.querySelectorAll('.pal-row').forEach((row, i) => {
      row.classList.toggle('sel', i === sel)
      row.setAttribute('aria-selected', String(i === sel))
    })
    input.setAttribute('aria-activedescendant', results.length ? `pal-opt-${sel}` : '')
    list.querySelector('.pal-row.sel')?.scrollIntoView({ block: 'nearest' })
  }
  function move(step) {
    if (!results.length) return
    sel = (sel + step + results.length) % results.length
    paintSel()
  }
  function pick(it) {
    close()
    it.run()
  }

  input.addEventListener('input', () => {
    sel = 0
    refresh()
  })
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) {
      e.preventDefault()
      move(1)
    } else if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) {
      e.preventDefault()
      move(-1)
    } else if (e.key === 'Enter') {
      e.preventDefault()
      if (results[sel]) pick(results[sel])
    } else if (e.key === 'Escape') {
      e.preventDefault()
      close()
    }
    e.stopPropagation()
  })
  const poll = setInterval(() => {
    if (!overlay.isConnected) return clearInterval(poll)
    if ((index?.version ?? -1) !== shownVersion && input.value.trim()) refresh()
  }, 250)
  refresh()
  commands = await buildCommands()
  if (overlay.isConnected) refresh()
}

document.getElementById('cmdk').addEventListener('click', () => openPalette())
