// Workspace state helpers: the active workspace, its folders, and which
// folder the git branch widget follows (activeRoot).
import { tome } from './util.js'
import { wsState } from './state.js'
// The workspaces <-> repo-egress import cycle is safe for the same reason as
// the menus <-> tree one noted in tree.js: neither side calls the other at
// module-evaluation time.
import { checkRepoEgress } from './repo-egress.js'

export const activeWorkspace = () => wsState.ws.workspaces[wsState.ws.active] || null
// main confines conductor open_file / doc:read / tome:// to these folders, and
// roots language servers at them. Must run at boot as well as on every edit:
// a session that only ever loads a stored workspace never calls saveWs, which
// used to leave main's list empty for the whole run.
export const syncFolders = () =>
  tome.ws.syncFolders(wsState.ws.workspaces.flatMap((w) => w.folders))
// The assistant's working root follows the ACTIVE workspace root (not the
// first folder of the first workspace): every activeRoot mutation should
// end in this call — boot, workspace switches, folder opens/closes. Null
// clears it (main falls back to the first open folder). Fire-and-forget:
// a missed sync only degrades to the old default, never blocks the UI.
export const syncAssistantRoot = () => {
  tome.conductor.setCwd(wsState.activeRoot || null).catch(() => {})
}
export const saveWs = () => {
  tome.store.set('workspaces', wsState.ws)
  syncFolders()
  syncAssistantRoot()
  // A workspace mutation can add a folder carrying .tome/egress.json — the
  // consent check is fire-and-forget here; it guards against re-entrancy.
  checkRepoEgress()
}
export const paneCwd = () => wsState.activeRoot || activeWorkspace()?.folders[0] || tome.home

const wsName = document.getElementById('ws-name')

export function renderWsChip() {
  wsName.textContent = activeWorkspace()?.name || 'no workspace'
  document.title = activeWorkspace() ? `tome — ${activeWorkspace().name}` : 'tome'
}
