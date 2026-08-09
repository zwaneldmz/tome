// Topbar menus: shared dropdown plumbing plus the workspace menu and the
// ＋ (new pane) menu. The git menu lives in git.js.
import { tome, toast, el, notifLog } from './util.js'
import { prefs, wsState } from './state.js'
import { activeWorkspace, saveWs, renderWsChip } from './workspaces.js'
import { addTerminal, addChat, addBrain, addEvents, addRuns, openFile, createFlowFile } from './panes.js'
import { confirmModal, promptModal } from './modals.js'
import { renderStatusbar } from './statusbar.js'
import { renderTree, createFileIn, createFolderIn } from './tree.js'
import { validateRelPath } from './tree-create.js'
import { refreshGit } from './git.js'
import { shortcutsModal } from './keys.js'
import { preferencesModal } from './preferences.js'

const allMenus = []
// Which button opened which menu — used to flip aria-expanded and to hand
// focus back when a menu closes from the keyboard.
const triggers = new Map() // menu element -> trigger button
let kbMenu = null // the menu keyboard navigation is currently armed on

function setExpanded(menu, open) {
  triggers.get(menu)?.setAttribute('aria-expanded', String(open))
}

// Keyboard navigation is a highlight (.kb-focus, styled like :hover), not DOM
// focus — menu inputs keep their caret and the trigger keeps its focus ring.
function kbItems(menu) {
  return [...menu.querySelectorAll(':scope > button:not(:disabled)')]
}
function kbMove(menu, pick) {
  const items = kbItems(menu)
  if (!items.length) return
  const at = items.findIndex((b) => b.classList.contains('kb-focus'))
  const next = items[(pick(at, items.length) + items.length) % items.length]
  for (const b of items) b.classList.remove('kb-focus')
  next.classList.add('kb-focus')
  next.scrollIntoView({ block: 'nearest' })
}
function kbClear(menu) {
  for (const b of menu.querySelectorAll('.kb-focus')) b.classList.remove('kb-focus')
  if (kbMenu === menu) kbMenu = null
}

function menuKeydown(e) {
  if (!kbMenu) {
    if (e.key === 'Escape') closeMenus()
    return
  }
  const items = kbItems(kbMenu)
  if (e.key === 'ArrowDown') {
    e.preventDefault()
    kbMove(kbMenu, (at) => at + 1)
  } else if (e.key === 'ArrowUp') {
    e.preventDefault()
    kbMove(kbMenu, (at, n) => (at < 0 ? n - 1 : at - 1))
  } else if (e.key === 'Home') {
    e.preventDefault()
    kbMove(kbMenu, () => 0)
  } else if (e.key === 'End') {
    e.preventDefault()
    kbMove(kbMenu, (_, n) => n - 1)
  } else if (e.key === 'Enter' || e.key === ' ') {
    const b = kbMenu.querySelector('.kb-focus')
    if (b && items.includes(b)) {
      e.preventDefault()
      b.click()
    }
  } else if (e.key === 'Escape') {
    e.preventDefault()
    const t = triggers.get(kbMenu)
    closeMenus()
    t?.focus()
  }
}
document.addEventListener('keydown', menuKeydown)

export function closeMenus(except) {
  for (const m of allMenus)
    if (m !== except && !m.classList.contains('hidden')) {
      m.classList.add('hidden')
      setExpanded(m, false)
      kbClear(m)
    }
}
document.addEventListener('click', () => closeMenus())

// ---------- floating menus ----------
// The topbar menus are anchored in markup; a pane's header ＋ has no such
// slot, so it borrows a single reusable popover per document. Per document,
// because a popped-out pane lives in its own window and must not open its
// menu back in the main one.
const floaters = new Map() // Document -> menu element
function floaterFor(doc) {
  let menu = floaters.get(doc)
  if (!menu) {
    menu = doc.createElement('div')
    menu.className = 'menu floating hidden'
    menu.setAttribute('role', 'menu')
    menu.addEventListener('click', (e) => e.stopPropagation())
    doc.body.appendChild(menu)
    if (doc !== document) {
      doc.addEventListener('click', () => closeMenus())
      doc.addEventListener('keydown', menuKeydown)
    }
    allMenus.push(menu)
    floaters.set(doc, menu)
  }
  return menu
}

/** Open (or toggle shut) a popover anchored under `anchor`, built by `build`. */
export function floatingMenu(anchor, build) {
  const doc = anchor.ownerDocument
  const menu = floaterFor(doc)
  const key = anchor.dataset.menuKey || (anchor.dataset.menuKey = String(allMenus.length) + Math.random())
  const reopen = menu.classList.contains('hidden') || menu.dataset.openFor !== key
  closeMenus()
  if (!reopen) return
  menu.innerHTML = ''
  menu.dataset.openFor = key
  build(menu)
  const r = anchor.getBoundingClientRect()
  const view = doc.defaultView
  menu.style.top = `${Math.round(r.bottom + 6)}px`
  menu.style.right = `${Math.max(8, Math.round(view.innerWidth - r.right))}px`
  menu.classList.remove('hidden')
  triggers.set(menu, anchor)
  anchor.setAttribute('aria-expanded', 'true')
  kbMenu = menu
}
export function wireMenu(btnId, menuId, onOpen) {
  const btn = document.getElementById(btnId)
  const menu = document.getElementById(menuId)
  allMenus.push(menu)
  triggers.set(menu, btn)
  btn.setAttribute('aria-expanded', 'false')
  btn.addEventListener('click', async (e) => {
    e.stopPropagation()
    const willOpen = menu.classList.contains('hidden')
    closeMenus()
    if (willOpen) {
      // A builder that throws must still leave an open menu: it half-built the
      // markup either way, and a button that silently does nothing reads as a
      // dead app rather than as one failed call.
      if (onOpen)
        try {
          await onOpen(menu)
        } catch (err) {
          menuLabel(menu, `couldn't load: ${err.message}`)
        }
      menu.classList.remove('hidden')
      if (menu.dataset.openFor) delete menu.dataset.openFor
      setExpanded(menu, true)
      kbMenu = menu
    }
  })
  menu.addEventListener('click', (e) => e.stopPropagation())
  return menu
}
export function menuItem(menu, { label, hint = '', onClick, disabled = false, active = false }) {
  const b = el('button')
  b.setAttribute('role', 'menuitem')
  b.disabled = disabled
  if (active) b.classList.add('active')
  const l = el('span', '', label)
  const h = el('span', 'hint', hint)
  b.append(l, h)
  if (onClick) {
    b.addEventListener('click', () => {
      closeMenus()
      onClick()
    })
  }
  menu.appendChild(b)
  return b
}
export function menuInput(menu, placeholder, submitLabel, onSubmit) {
  const row = el('div', 'menu-input')
  const input = el('input')
  input.placeholder = placeholder
  const go = el('button', '', submitLabel)
  const submit = () => {
    const v = input.value.trim()
    if (!v) return
    closeMenus()
    onSubmit(v)
  }
  go.addEventListener('click', submit)
  input.addEventListener('keydown', (e) => e.key === 'Enter' && submit())
  row.append(input, go)
  menu.appendChild(row)
}
export const menuRule = (menu) => menu.appendChild(el('hr'))
export const menuLabel = (menu, text) => menu.appendChild(el('div', 'menu-label', text))

// ---------- notification log (toast history) ----------
wireMenu('btn-notifs', 'notifs-menu', (menu) => {
  document.getElementById('btn-notifs').classList.remove('unseen')
  menu.innerHTML = ''
  if (!notifLog.length) {
    menuLabel(menu, 'No notifications yet')
    return
  }
  menuItem(menu, {
    label: 'Clear',
    hint: `${notifLog.length} entr${notifLog.length === 1 ? 'y' : 'ies'}`,
    onClick: () => {
      notifLog.length = 0
    },
  })
  menuRule(menu)
  for (const n of [...notifLog].reverse()) {
    const row = el('div', 'notif-row ' + (n.kind === 'ok' ? 'ok' : 'err'))
    row.append(
      el('span', 'notif-time', new Date(n.ts).toLocaleTimeString([], { hour12: false })),
      el('span', 'notif-msg', n.msg)
    )
    menu.appendChild(row)
  }
})

// ---------- workspace UI ----------
export function renderAll() {
  renderWsChip()
  renderTree()
  refreshGit()
  renderStatusbar()
}

export async function addFolderToActive() {
  const w = activeWorkspace()
  if (!w) return
  const dir = await tome.pickFolder()
  if (!dir || w.folders.includes(dir)) return
  w.folders.push(dir)
  wsState.activeRoot = dir
  saveWs()
  renderAll()
}

function createWorkspace(name) {
  wsState.ws.workspaces.push({ name, folders: [] })
  wsState.ws.active = wsState.ws.workspaces.length - 1
  wsState.activeRoot = null
  saveWs()
  renderAll()
  addFolderToActive()
}

async function renameActiveWorkspace() {
  const w = activeWorkspace()
  if (!w) return
  const name = await promptModal('Rename workspace', 'Workspace name', w.name)
  if (!name || !name.trim() || name.trim() === w.name) return
  w.name = name.trim()
  saveWs()
  renderAll()
}

function switchWorkspace(i) {
  wsState.ws.active = i
  wsState.activeRoot = activeWorkspace()?.folders[0] || null
  saveWs()
  renderAll()
}

wireMenu('ws-chip', 'ws-menu', (menu) => {
  menu.innerHTML = ''
  // Zero workspaces: the capsule is how you create the first one, so the menu
  // must say what it does — a bare input reads as a dead button.
  if (!wsState.ws.workspaces.length)
    menuLabel(menu, 'Group the folders you work across into a workspace.')
  menuInput(menu, 'new workspace name…', 'Create', createWorkspace)
  if (wsState.ws.workspaces.length) {
    menuRule(menu)
    menuLabel(menu, 'Workspaces')
    wsState.ws.workspaces.forEach((w, i) =>
      menuItem(menu, {
        label: w.name,
        hint: `${w.folders.length} folder${w.folders.length === 1 ? '' : 's'}`,
        active: i === wsState.ws.active,
        onClick: () => switchWorkspace(i),
      })
    )
  }
  if (activeWorkspace()) {
    menuRule(menu)
    menuItem(menu, { label: 'Rename workspace…', onClick: renameActiveWorkspace })
    menuItem(menu, { label: 'Add folder to workspace…', onClick: addFolderToActive })
    menuItem(menu, {
      label: `Delete “${activeWorkspace().name}”`,
      onClick: async () => {
        const name = activeWorkspace().name
        const ok = await confirmModal(
          `Delete “${name}”?`,
          'The workspace and its saved layout are removed. Its folders on disk are not touched.',
          'Delete workspace'
        )
        if (!ok) return
        wsState.ws.workspaces.splice(wsState.ws.active, 1)
        wsState.ws.active = wsState.ws.workspaces.length ? 0 : -1
        wsState.activeRoot = activeWorkspace()?.folders[0] || null
        saveWs()
        renderAll()
      },
    })
  }
  menuRule(menu)
  menuItem(menu, {
    label: 'Set core vault…',
    onClick: async () => {
      const dir = await tome.pickFolder()
      if (!dir) return
      await tome.store.set('core-vault', dir)
      toast('core vault set', 'ok')
    },
  })
})

// ---------- ＋ menu ----------
// Shared by the topbar ＋ (opens panes wherever the grid has room) and each
// pane header's ＋ (`target = { group }` — the new pane joins that group as a
// tab, which is how an agent's helpers stay stacked with it).
export async function populateAddMenu(menu, target) {
  menu.innerHTML = ''
  const agents = await tome.agents.list()
  // The wizard's Agents step (and Settings → Agents) can deselect a CLI:
  // it stays installed, it just leaves this menu. pty:create in main still
  // accepts the kind — deselection is a UI convenience, not a sandbox rule.
  const disabled = new Set((await tome.store.get('agents-disabled')) || [])
  for (const a of agents.filter((a) => !a.custom && !disabled.has(a.name))) {
    menuItem(menu, {
      label: (prefs.airgapDefault ? '⛨ ' : '') + a.name,
      hint: a.available ? (target ? 'as a tab' : 'agent') : 'not installed',
      disabled: !a.available,
      onClick: () => addTerminal(a.name, target),
    })
  }
  // User-declared CLIs (Preferences → Agents) spawn exactly like built-ins —
  // same addTerminal path, same greyed-with-a-hint treatment when the bin
  // isn't on PATH; main re-vets the kind again at pty:create.
  const customs = agents.filter((a) => a.custom && !disabled.has(a.name))
  if (customs.length) {
    menuLabel(menu, 'Custom agents')
    for (const a of customs) {
      menuItem(menu, {
        label: (prefs.airgapDefault ? '⛨ ' : '') + (a.label || a.name),
        hint: a.available ? (target ? 'as a tab' : 'agent') : 'not installed',
        disabled: !a.available,
        onClick: () => addTerminal(a.name, target),
      })
    }
  }
  menuItem(menu, {
    label: 'spawn agents air-gapped',
    hint: prefs.airgapDefault ? 'on' : 'off',
    active: prefs.airgapDefault,
    onClick: () => {
      prefs.airgapDefault = !prefs.airgapDefault
      tome.store.set('airgap-default', prefs.airgapDefault)
    },
  })
  menuItem(menu, {
    label: 'assistant may run commands',
    hint: prefs.conductorRun ? 'on' : 'off',
    active: prefs.conductorRun,
    onClick: () => {
      prefs.conductorRun = !prefs.conductorRun
      tome.store.set('conductor-run', prefs.conductorRun)
      tome.conductor.allowRun(prefs.conductorRun)
    },
  })
  menuRule(menu)
  menuItem(menu, { label: 'Assistant chat', hint: 'API', onClick: () => addChat(target) })
  menuItem(menu, { label: 'Terminal', hint: 'zsh', onClick: () => addTerminal('terminal', target) })
  menuItem(menu, {
    label: 'Brain',
    hint: activeWorkspace() ? 'vault' : 'needs a workspace',
    disabled: !activeWorkspace(),
    onClick: () => addBrain(target),
  })
  menuItem(menu, {
    label: 'Open file…',
    onClick: async () => {
      const p = await tome.pickFile()
      if (p) openFile(p, undefined, target)
    },
  })
  menuItem(menu, {
    label: 'New file…',
    hint: wsState.activeRoot ? '' : 'needs a workspace folder',
    disabled: !wsState.activeRoot,
    onClick: () => createFileIn(wsState.activeRoot, target),
  })
  menuItem(menu, {
    label: 'New folder…',
    hint: wsState.activeRoot ? '' : 'needs a workspace folder',
    disabled: !wsState.activeRoot,
    onClick: () => createFolderIn(wsState.activeRoot, target),
  })
  menuItem(menu, {
    label: 'Flow diagram…',
    hint: wsState.activeRoot ? '' : 'needs a workspace folder',
    disabled: !wsState.activeRoot,
    onClick: async () => {
      const input = await promptModal('New flow', 'name — e.g. review-pipeline', '', 'Create')
      if (input == null) return // cancelled
      const check = validateRelPath(input)
      if (!check.ok) return toast(check.reason)
      // validateRelPath allows a multi-segment relative path (that's the
      // point for New file/New folder, which can create nested dirs) — but
      // flows live flat in .tome/flows/ (plan §2.3), so a name that still has
      // a "/" in it after that check needs its own refusal here rather than
      // silently nesting a .flow.json under a folder nothing else browses to.
      if (check.rel.includes('/')) return toast('flow names can\'t contain "/" — flows live flat in .tome/flows/')
      createFlowFile(wsState.activeRoot, check.rel, target)
    },
  })
  menuRule(menu)
  menuItem(menu, { label: 'Flow runs', onClick: () => addRuns(target) })
  menuItem(menu, { label: 'Event log', hint: 'audit', onClick: () => addEvents(target) })
  menuRule(menu)
  menuItem(menu, { label: 'Settings…', hint: '⌘,', onClick: () => preferencesModal() })
  menuItem(menu, { label: 'Keyboard shortcuts', hint: '⌘', onClick: () => shortcutsModal() })
}

wireMenu('btn-add', 'add-menu', (menu) => populateAddMenu(menu))
