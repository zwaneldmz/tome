// The keyboard spine: global key handling for pane management (close,
// focus-by-number, cycle), the ⌘P quick-open palette, terminal zoom, and
// the shortcut reference modal. Imported once from renderer.js.
//
// Typing is sacred: shortcuts that are clearly global (⌘W, ⌘P, ⌘1–9, zoom)
// fire even from inside inputs; everything else bails when an editable
// element has focus. The quick-open palette installs its own keydown
// listener while it is open.
import { el } from './util.js'
import { openPalette } from './commands.js'
import { dock, closePanel } from './panes.js'
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

const isEditable = (n) =>
  !!n && (n.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(n.tagName))

// ⌘K / ⌘P — the palette lives in commands.js. Opened synchronously so the
// keystrokes typed right after ⌘K land in its input.
export function quickOpen(initial) {
  openPalette(initial)
}

// ---------- shortcut reference ----------
const SHORTCUTS = [
  ['Find anything', [
    [MOD + 'K', 'Command palette — files, panes, commands'],
    [MOD + 'P', 'Same palette (VS Code habit)'],
    [MOD + 'J', 'Jump to the next agent waiting for you'],
    [MOD + 'T', 'New… (agents, terminal, chat)'],
  ]],
  ['Sidebar', [
    [MOD + 'B', 'Show / hide the sidebar'],
    [MOD + '⇧A', 'Open panes'],
    [MOD + '⇧E', 'Files'],
    [MOD + '⇧G', 'Changes — review and commit'],
  ]],
  ['Panes', [
    [MOD + 'W', 'Close the active pane (asks if unsaved)'],
    [MOD + '1–9', 'Focus the Nth tab of the active group'],
    [MOD + '⇧[ / ' + MOD + '⇧]', 'Previous / next tab'],
    [MOD + '= / ' + MOD + '- / ' + MOD + '0', 'Terminal text bigger / smaller / reset'],
  ]],
  ['Editing', [
    [MOD + 'S', 'Save'],
    [MOD + '⌥S', 'Save all'],
    ['Enter / ⇧Enter', 'Send / new line in chat'],
    [MOD + 'Enter', 'Commit, from the Changes message box'],
  ]],
  ['App', [
    [MOD + ',', 'Settings'],
    [MOD + '/', 'This list'],
    ['Esc', 'Close menus, the palette, and dialogs'],
  ]],
]

export function shortcutsModal() {
  document.getElementById('keys-overlay')?.remove()
  closeMenus()
  const overlay = el('div')
  overlay.id = 'keys-overlay'
  const box = el('div', 'ag-box keys-box')
  box.append(el('h3', '', 'Keyboard shortcuts'))
  for (const [title, rows] of SHORTCUTS) {
    box.appendChild(el('div', 'keys-section', title))
    const grid = el('div', 'keys-grid')
    for (const [keys, desc] of rows) {
      const k = el('span', 'keys-col')
      for (const part of keys.split(' / ')) k.append(el('kbd', '', part))
      grid.append(k, el('span', 'keys-desc', desc))
    }
    box.appendChild(grid)
  }
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

  // ⌘W (close pane) and ⌘P (quick open) are native menu accelerators — the
  // menu-bridge routes them here; the renderer must not also handle them or
  // they would fire twice. ⌘, (Preferences) is also a native menu accelerator
  // routed via menu-bridge, so it is likewise NOT handled here.
  // ⌘⇧A/E/G (sidebar views), ⌘J (next waiting agent) and ⌘/ are native
  // menu accelerators routed through menu-bridge — not handled here.
  // ⌘K — the command palette, from anywhere (terminals included: the
  // palette is the advertised way around the app, so it outranks a shell's
  // clear-scrollback habit).
  if (!e.shiftKey && e.key.toLowerCase() === 'k') {
    e.preventDefault()
    e.stopPropagation()
    quickOpen()
    return
  }
  if (!e.shiftKey && e.key.toLowerCase() === 't') {
    e.preventDefault()
    document.getElementById('btn-add')?.click()
    return
  }
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
