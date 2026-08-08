// Workspace state helpers: the active workspace, its folders, and which
// folder the git branch widget follows (activeRoot).
import { tome } from './util.js'
import { wsState } from './state.js'

export const activeWorkspace = () => wsState.ws.workspaces[wsState.ws.active] || null
// main confines conductor open_file / doc:read / tome:// to these folders, and
// roots language servers at them. Must run at boot as well as on every edit:
// a session that only ever loads a stored workspace never calls saveWs, which
// used to leave main's list empty for the whole run.
export const syncFolders = () =>
  tome.ws.syncFolders(wsState.ws.workspaces.flatMap((w) => w.folders))
export const saveWs = () => {
  tome.store.set('workspaces', wsState.ws)
  syncFolders()
}
export const paneCwd = () => wsState.activeRoot || activeWorkspace()?.folders[0] || tome.home

const wsName = document.getElementById('ws-name')

export function renderWsChip() {
  wsName.textContent = activeWorkspace()?.name || 'no workspace'
  document.title = activeWorkspace() ? `tome — ${activeWorkspace().name}` : 'tome'
}
