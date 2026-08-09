// Native menu bar bridge: main sends one 'menu:action' event per custom
// menu item (roles never leave main), and this switch routes each action id
// to the same function the topbar buttons and ⌘ keys already call. No logic
// lives here — it is a dispatch table, nothing more. Imported once from
// renderer.js.
import { tome, toast } from './util.js'
import { addTerminal, addChat, addBrain, openFile } from './panes.js'
import { activeWorkspace } from './workspaces.js'
import { toggleSidebar, openThemeMenu } from './chrome.js'
import { quickOpen, shortcutsModal, closeActivePanel, saveActivePanel } from './keys.js'
import { preferencesModal } from './preferences.js'
import { toggleVoice } from './voice.js'
import { showOnboarding } from './onboarding.js'
import { saveAllEditors } from './panels/editor.js'
import { addFolderToActive, renderAll } from './menus.js'
import { createFileIn } from './tree.js'
import { promptModal } from './modals.js'
import { wsState } from './state.js'
import { saveWs } from './workspaces.js'
import { AGENTS } from '../shared/pane-kinds.js'

// 'New Pane' sends the kind through blindly (the menu is static); check the
// agent is actually installed here, where tome.agents.list() lives.
async function newPane(kind) {
  if (kind === 'terminal') return addTerminal('terminal')
  if (kind === 'chat') return addChat()
  if (kind === 'brain') {
    if (!activeWorkspace()) return toast('a brain pane needs an active workspace')
    return addBrain()
  }
  if (AGENTS.includes(kind)) {
    const agents = await tome.agents.list()
    if (!agents.find((a) => a.name === kind)?.available)
      return toast(`${kind} is not installed`)
    return addTerminal(kind)
  }
  toast(`unknown pane kind: ${kind}`)
}

tome.menu.onAction((action) => {
  switch (action?.id) {
    case 'open-preferences':
      preferencesModal()
      break
    case 'open-onboarding':
      showOnboarding()
      break
    case 'toggle-sidebar':
      toggleSidebar()
      break
    case 'toggle-voice':
      // The native menu item (⌘⇧V) toggles the same ambient session the
      // topbar mic button does.
      toggleVoice()
      break
    case 'set-theme':
      // The native Appearance submenu can't render live radio state (the
      // menu is static), so it opens the same appearance picker the ☾/☀
      // button uses — that one reflects the current pref.
      openThemeMenu()
      break
    case 'quick-open':
      quickOpen()
      break
    case 'shortcuts':
      shortcutsModal()
      break
    case 'new-pane':
      newPane(action.kind)
      break
    case 'close-pane':
      closeActivePanel()
      break
    case 'save':
      saveActivePanel()
      break
    case 'save-all':
      saveAllEditors().then((n) => n && toast(`saved ${n} file${n === 1 ? '' : 's'}`, 'ok'))
      break
    case 'open-file':
      tome.pickFile().then((p) => p && openFile(p))
      break
    case 'open-folder':
      // Adds the picked folder to the ACTIVE workspace (same as the ws-chip
      // menu's 'Add folder to workspace…'); with no workspace yet it offers
      // to create one first, which is the flow new users actually want from
      // File → Open Folder.
      if (activeWorkspace()) addFolderToActive()
      else newWorkspace()
      break
    case 'new-file':
      if (!wsState.activeRoot) return toast('new file needs a workspace folder — open a folder first')
      createFileIn(wsState.activeRoot)
      break
    case 'new-workspace':
      newWorkspace()
      break
  }
})

// Prompt for a name, create the workspace, then drop straight into the
// folder picker — a workspace without a folder is an empty shell.
async function newWorkspace() {
  const name = await promptModal('New workspace', 'Workspace name', '', 'Create')
  if (!name?.trim()) return
  wsState.ws.workspaces.push({ name: name.trim(), folders: [] })
  wsState.ws.active = wsState.ws.workspaces.length - 1
  wsState.activeRoot = null
  saveWs()
  renderAll()
  addFolderToActive()
}
