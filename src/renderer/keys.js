// The keyboard spine: global key handling for pane management (close,
// focus-by-number, cycle), the ⌘P quick-open palette, terminal zoom, and
// the shortcut reference modal. Imported once from renderer.js.
//
// Typing is sacred: shortcuts that are clearly global (⌘W, ⌘P, ⌘1–9, zoom)
// fire even from inside inputs; everything else bails when an editable
// element has focus. The quick-open palette installs its own keydown
// listener while it is open.
import { tome, el, toast } from './util.js'
import { dock, closePanel, openFile } from './panes.js'
import { activeWorkspace } from './workspaces.js'
import { zoomTerminals } from './panels/terminal.js'
import { closeMenus } from './menus.js'

const isMac = navigator.platform.startsWith('Mac')
const MOD = isMac ? '⌘' : 'Ctrl+'

// ---------- pane helpers ----------
const activeGroup = () =>
  dock.activeGroup || dock.groups.find((g) => g.panels.length) || null

export function closeActivePanel() {
  const panel = dock.activePanel || activeGroup()?.activePanel
  if (panel) closePanel(panel) // goes through the dirty close-guard in panes.js
}

// ⌘S is a native menu accelerator, which consumes the key before the page
// sees it — so the menu, not CodeMirror's own binding, drives save on mac.
export function saveActivePanel() {
  const panel = dock.activePanel || activeGroup()?.activePanel
  const view = panel?.view?.content
  if (typeof view?.save === 'function') view.save()
}

function focusNthPanel(n) {
  const panel = activeGroup()?.panels[n]
  panel?.api.setActive()
}

function cyclePanel(step) {
  const group = activeGroup()
  const panels = group?.panels || []
  if (panels.length < 2) return
  const idx = Math.max(0, panels.indexOf(group.activePanel))
  panels[(idx + step + panels.length) % panels.length].api.setActive()
}

// ---------- quick-open palette ----------
const SKIP_DIRS = new Set(['node_modules', '.git', 'out', 'dist', '.venv', '__pycache__', '.next', 'target'])
const MAX_DEPTH = 8
const MAX_DIRS = 400
const MAX_FILES = 4000
const MAX_RESULTS = 40

// Lazily-walked file index for the active workspace's folders. Rebuilt each
// time the palette opens, but the walk is incremental so the palette is
// usable (and keeps filling in) while big trees are still being scanned.
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
        } else {
          this.files.push(path)
        }
      }
      this.version++
    }
    await Promise.all(this.roots.map((r) => walk(r, 0)))
    this.done = true
    this.version++
  }
}

// Subsequence match with a small score: consecutive runs and matches at
// word starts (path separators, camelCase) rank higher. Returns null when
// the query isn't a subsequence of the candidate.
function fuzzy(query, text) {
  const q = query.toLowerCase()
  const t = text.toLowerCase()
  let ti = 0
  let score = 0
  let run = 0
  for (let qi = 0; qi < q.length; qi++) {
    const found = t.indexOf(q[qi], ti)
    if (found === -1) return null
    run = found === ti ? run + 1 : 0
    score += run * 4
    if (found === 0 || '/._- '.includes(t[found - 1])) score += 6
    score -= (found - ti) * 0.3 // gap penalty
    ti = found + 1
  }
  return score - text.length * 0.01
}

function relToWorkspace(path) {
  const w = activeWorkspace()
  for (const root of w?.folders || []) {
    if (path.startsWith(root + '/')) return path.slice(root.length + 1)
  }
  return path
}

const isEditable = (n) =>
  !!n && (n.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(n.tagName))

export function quickOpen() {
  if (document.getElementById('qo-overlay')) return
  const w = activeWorkspace()
  if (!w?.folders.length) {
    toast('quick open needs a workspace with a folder')
    return
  }
  closeMenus()
  const overlay = el('div')
  overlay.id = 'qo-overlay'
  const box = el('div', 'qo-box')
  const input = el('input', 'qo-input')
  input.placeholder = 'type to find a file…'
  input.spellcheck = false
  const list = el('div', 'qo-list')
  box.append(input, list)
  overlay.appendChild(box)
  overlay.addEventListener('mousedown', (e) => e.target === overlay && close())
  document.body.appendChild(overlay)

  const index = new FileIndex(w.folders)
  index.start()
  let results = []
  let sel = 0
  let shownVersion = -1

  function close() {
    index.cancel()
    overlay.remove()
  }

  function refresh() {
    const q = input.value.trim()
    const scored = []
    if (q) {
      for (const f of index.files) {
        const s = fuzzy(q, relToWorkspace(f))
        if (s !== null) scored.push([s, f])
      }
      scored.sort((a, b) => b[0] - a[0])
    } else {
      for (const f of index.files) scored.push([0, f])
    }
    results = scored.slice(0, MAX_RESULTS).map((r) => r[1])
    sel = Math.min(sel, Math.max(0, results.length - 1))
    shownVersion = index.version
    list.innerHTML = ''
    results.forEach((path, i) => {
      const row = el('div', 'qo-row' + (i === sel ? ' sel' : ''))
      const rel = relToWorkspace(path)
      const slash = rel.lastIndexOf('/')
      row.append(
        el('span', 'qo-name', slash === -1 ? rel : rel.slice(slash + 1)),
        el('span', 'qo-dir', slash === -1 ? '' : rel.slice(0, slash))
      )
      row.addEventListener('click', () => pick(path))
      row.addEventListener('mousemove', () => {
        if (sel !== i) {
          sel = i
          paintSel()
        }
      })
      list.appendChild(row)
    })
    if (!results.length) {
      list.appendChild(
        el('div', 'qo-empty', index.done ? 'no matches' : 'scanning… ' + index.files.length + ' files so far')
      )
    } else if (!index.done) {
      list.appendChild(el('div', 'qo-empty', 'scanning… ' + index.files.length + ' files'))
    }
    list.querySelector('.qo-row.sel')?.scrollIntoView({ block: 'nearest' })
  }

  const paintSel = () => {
    list.querySelectorAll('.qo-row').forEach((row, i) => row.classList.toggle('sel', i === sel))
  }

  function move(step) {
    if (!results.length) return
    sel = (sel + step + results.length) % results.length
    paintSel()
    list.querySelector('.qo-row.sel')?.scrollIntoView({ block: 'nearest' })
  }

  function pick(path) {
    close()
    openFile(path)
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
  // Keep filling in while the lazy walk is still discovering files.
  const poll = setInterval(() => {
    if (!overlay.isConnected) return clearInterval(poll)
    if (index.version !== shownVersion) refresh()
  }, 250)
  refresh()
  input.focus()
}

// ---------- shortcut reference ----------
const SHORTCUTS = [
  [MOD + 'B', 'Toggle the sidebar'],
  [MOD + 'S', 'Save the active editor'],
  [MOD + '⌥S', 'Save every editor with unsaved changes'],
  [MOD + 'W', 'Close the active pane (asks if unsaved)'],
  [MOD + 'P', 'Quick-open a file'],
  [MOD + ',', 'Preferences'],
  [MOD + '1–9', 'Focus the Nth tab of the active group'],
  [MOD + '⇧[ / ' + MOD + '⇧]', 'Previous / next tab (also Ctrl+PageUp/PageDown)'],
  [MOD + '= / ' + MOD + '-', 'Zoom terminal text in / out'],
  [MOD + '0', 'Reset terminal text size'],
  ['Enter / Shift+Enter', 'Send / newline in the assistant chat'],
  ['Esc', 'Close menus, the palette, and modals'],
]

export function shortcutsModal() {
  document.getElementById('keys-overlay')?.remove()
  closeMenus()
  const overlay = el('div')
  overlay.id = 'keys-overlay'
  const box = el('div', 'ag-box keys-box')
  box.append(el('h3', '', 'Keyboard shortcuts'))
  const grid = el('div', 'keys-grid')
  for (const [keys, desc] of SHORTCUTS) {
    const k = el('span', 'keys-col')
    for (const part of keys.split(' / ')) k.append(el('kbd', '', part))
    grid.append(k, el('span', 'keys-desc', desc))
  }
  box.appendChild(grid)
  overlay.appendChild(box)
  overlay.addEventListener('mousedown', (e) => e.target === overlay && overlay.remove())
  document.body.appendChild(overlay)
}

// ---------- global key handling ----------
const DIGITS = ['1', '2', '3', '4', '5', '6', '7', '8', '9']

window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    // Menus/modals close on Esc even from an input; the palette handles its
    // own Esc (and stops propagation) before this runs.
    const modal = document.getElementById('keys-overlay') || document.getElementById('ag-overlay')
    if (modal) {
      e.preventDefault()
      modal.remove()
    } else {
      closeMenus()
    }
    return
  }
  const mod = e.metaKey || e.ctrlKey
  if (!mod || e.altKey) return
  const key = e.key.toLowerCase()

  // ⌘W (close pane) and ⌘P (quick open) are native menu accelerators — the
  // menu-bridge routes them here; the renderer must not also handle them or
  // they would fire twice. ⌘, (Preferences) is also a native menu accelerator
  // routed via menu-bridge, so it is likewise NOT handled here.
  if (!e.shiftKey && DIGITS.includes(e.key)) {
    e.preventDefault()
    focusNthPanel(DIGITS.indexOf(e.key))
    return
  }
  // Terminal zoom (⌘= / ⌘- / ⌘0, also with Ctrl).
  if (e.key === '=' || e.key === '+') {
    e.preventDefault()
    zoomTerminals(1)
    return
  }
  if (e.key === '-' || e.key === '_') {
    e.preventDefault()
    zoomTerminals(-1)
    return
  }
  if (e.key === '0') {
    e.preventDefault()
    zoomTerminals(0)
    return
  }
  // Tab cycling: ⌘⇧[/⌘⇧] on mac, Ctrl+PageUp/PageDown everywhere. Not
  // global inside inputs — plain PageUp/Down there belong to the field.
  if (isEditable(e.target)) return
  const prevKey = e.key === '[' || e.key === '{' || e.key === 'PageUp'
  const nextKey = e.key === ']' || e.key === '}' || e.key === 'PageDown'
  if ((e.metaKey && e.shiftKey && (prevKey || nextKey)) || (e.ctrlKey && !e.metaKey && (prevKey || nextKey))) {
    e.preventDefault()
    cyclePanel(prevKey ? -1 : 1)
  }
})
