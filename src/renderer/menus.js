// Topbar menus: shared dropdown plumbing plus the workspace menu and the
// ＋ (new pane) menu. The git menu lives in git.js.
import { tome, toast, el, notifLog } from './util.js'
import { prefs, wsState } from './state.js'
import { activeWorkspace, saveWs, renderWsChip } from './workspaces.js'
import { addTerminal, addChat, addBrain, openFile } from './panes.js'
import { renderTree } from './tree.js'
import { refreshGit } from './git.js'

const allMenus = []
export function closeMenus(except) {
  for (const m of allMenus) if (m !== except) m.classList.add('hidden')
}
document.addEventListener('click', () => closeMenus())
export function wireMenu(btnId, menuId, onOpen) {
  const btn = document.getElementById(btnId)
  const menu = document.getElementById(menuId)
  allMenus.push(menu)
  btn.addEventListener('click', async (e) => {
    e.stopPropagation()
    const willOpen = menu.classList.contains('hidden')
    closeMenus()
    if (willOpen) {
      if (onOpen) await onOpen(menu)
      menu.classList.remove('hidden')
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

function switchWorkspace(i) {
  wsState.ws.active = i
  wsState.activeRoot = activeWorkspace()?.folders[0] || null
  saveWs()
  renderAll()
}

wireMenu('ws-chip', 'ws-menu', (menu) => {
  menu.innerHTML = ''
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
    menuItem(menu, { label: 'Add folder to workspace…', onClick: addFolderToActive })
    menuItem(menu, {
      label: `Delete “${activeWorkspace().name}”`,
      onClick: () => {
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
wireMenu('btn-add', 'add-menu', async (menu) => {
  menu.innerHTML = ''
  const agents = await tome.agents.list()
  for (const a of agents) {
    menuItem(menu, {
      label: (prefs.airgapDefault ? '⛨ ' : '') + a.name,
      hint: a.available ? 'agent' : 'not installed',
      disabled: !a.available,
      onClick: () => addTerminal(a.name),
    })
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
  menuItem(menu, { label: 'Assistant chat', hint: 'API', onClick: addChat })
  menuItem(menu, { label: 'Terminal', hint: 'zsh', onClick: () => addTerminal('terminal') })
  menuItem(menu, {
    label: 'Brain',
    hint: activeWorkspace() ? 'vault' : 'needs a workspace',
    disabled: !activeWorkspace(),
    onClick: addBrain,
  })
  menuItem(menu, {
    label: 'Open file…',
    onClick: async () => {
      const p = await tome.pickFile()
      if (p) openFile(p)
    },
  })
})
