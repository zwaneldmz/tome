// Native menu bar bridge: main sends one 'menu:action' event per custom
// menu item (roles never leave main), and this switch routes each action id
// to the same function the topbar buttons and ⌘ keys already call. No logic
// lives here — it is a dispatch table, nothing more. Imported once from
// renderer.js.
import { tome, toast } from './util.js'
import { addTerminal, addChat, addBrain } from './panes.js'
import { activeWorkspace } from './workspaces.js'
import { toggleSidebar, openThemeMenu } from './chrome.js'
import { quickOpen, shortcutsModal, closeActivePanel } from './keys.js'
import { preferencesModal } from './preferences.js'
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
    case 'toggle-sidebar':
      toggleSidebar()
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
  }
})
