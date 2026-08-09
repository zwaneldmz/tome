// Entry point: fan main-process events out to live panels, then boot
// (lock screen -> persisted state -> render). The pieces live in:
//   panels/   one file per dockview panel class
//   panes.js  the dockview grid + pane-opening actions + conductor bridge
//   menus.js  topbar menus (workspace, ＋)   tree.js  file tree sidebar
//   git.js    branch widget + polling        airgap-ui.js  strips + modals
//   modals.js modal shell   util.js  el()/toast/tome   state.js, regs.js
import { tome, toast } from './util.js'
import { prefs, wsState, agState, counters } from './state.js'
import { terms, chats, brains } from './regs.js'
import { dock, addChat, addBrain, openFile, restoreLayout } from './panes.js'
import { renderAll } from './menus.js'
import { startGitPolling, initGitMenu } from './git.js'
import { activeWorkspace, syncFolders } from './workspaces.js'
import { checkRepoAirgap } from './repo-airgap.js'
import { bootAuth } from './lock.js'
import { maybeShowOnboarding } from './onboarding.js'
import { bootTheme } from './theme.js'
import { bootChrome } from './chrome.js'
import { loadEditorPrefs } from './panels/editor.js'
import './airgap-ui.js' // wires the air-gap event listeners + strip ticker
import './keys.js' // the keyboard spine: pane keys, quick open, zoom, reference
import './menu-bridge.js' // native menu bar actions → the same functions the buttons use
import './style.css'

// ---------- pty / chat / brain fan-out ----------
tome.pty.onData(({ id, data }) => terms.get(id)?.write(data))
tome.pty.onExit(({ id, exitCode }) =>
  terms.get(id)?.write(`\r\n\x1b[2m[process exited ${exitCode}]\x1b[0m\r\n`)
)
tome.chat.onDelta(({ id, text }) => chats.get(id)?.appendDelta(text))
tome.chat.onDone(({ id, error, aborted }) => chats.get(id)?.finish(error, aborted))
tome.chat.onTool(({ id, tool, hint }) => chats.get(id)?.toolNote(tool, hint))
tome.brain.onChanged(({ ws: bws, index }) => brains.get(bws)?.onChanged(index))

// ---------- boot ----------
;(async () => {
  await bootTheme() // before the lock screen paints — store:get is open while locked
  await bootAuth(tome, toast) // main gates the sensitive IPC until this resolves
  await bootChrome()
  maybeShowOnboarding() // first run only — checks 'onboarded-v1' itself
  const saved = await tome.store.get('workspaces')
  if (saved && Array.isArray(saved.workspaces)) {
    wsState.ws = saved
    if (wsState.ws.active >= wsState.ws.workspaces.length)
      wsState.ws.active = wsState.ws.workspaces.length - 1
  }
  await loadEditorPrefs() // before restoreLayout, so reopened editors get them
  const agPref = await tome.store.get('airgap-default')
  if (agPref !== null) prefs.airgapDefault = !!agPref
  if (await tome.store.get('conductor-run')) {
    prefs.conductorRun = true
    tome.conductor.allowRun(true)
  }
  tome.airgap.state().then((s) => Object.assign(agState, s))
  syncFolders() // main starts with an empty confinement list
  wsState.activeRoot = activeWorkspace()?.folders[0] || null
  // After bootAuth, so the lock-gated apply channel is reachable; a repo's
  // .tome/airgap.json still needs the user's consent before it is honored.
  checkRepoAirgap()
  renderAll()
  initGitMenu() // deferred out of git.js's module body — see the note there
  startGitPolling() // gated on unlock: the IPC gate refuses while locked
  try {
    await restoreLayout()
  } catch (err) {
    console.warn('layout restore failed:', err)
  }
  if (tome.shotMode && wsState.activeRoot) {
    // screenshot/demo mode: open a representative set of panes
    const id = `pty-${++counters.seq}`
    dock.addPanel({
      id,
      component: 'terminal',
      title: `⛨ zsh — demo`,
      params: { ptyId: id, kind: 'terminal', cwd: wsState.activeRoot, airgap: true },
    })
    openFile(`${wsState.activeRoot}/package.json`)
    addChat()
    addBrain()
  }
})()
