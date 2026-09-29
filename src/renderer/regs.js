// Live panel registries. Main-process events (pty data, chat deltas, brain
// reindexes) fan out through these maps to the panel instances.
export const terms = new Map() // ptyId -> xterm Terminal
export const chats = new Map() // chatId -> ChatPanel
export const brains = new Map() // ws name -> BrainPanel
export const strips = new Map() // ptyId -> egress strip element
// Pane → sidebar signals. terminal.js can't import sidebar.js (it would
// evaluate before panes.js has created the dock), so the sidebar plugs its
// handlers in here at boot; until then these are no-ops.
export const paneSignals = {
  report: (_ptyId, _state) => {}, // OSC 8663 from the pane (agent-reported state)
  input: (_ptyId, _data) => {}, // keys the user typed into the pane
}
