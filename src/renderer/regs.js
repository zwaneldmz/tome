// Live panel registries. Main-process events (pty data, chat deltas, brain
// reindexes) fan out through these maps to the panel instances.
export const terms = new Map() // ptyId -> xterm Terminal
export const chats = new Map() // chatId -> ChatPanel
export const brains = new Map() // ws name -> BrainPanel
export const strips = new Map() // ptyId -> air-gap strip element
