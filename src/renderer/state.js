// Renderer-wide mutable state, shared across modules. Grouped so the split
// stays a pure move; the objects are mutated in place (never reassigned).

// workspaces — shape: { workspaces: [{ name, folders: [] }], active: index }
export const wsState = { ws: { workspaces: [], active: -1 }, activeRoot: null }

// egress / conductor preferences (persisted via the store)
export const prefs = { egressDefault: true, conductorRun: false }

// live egress state mirrored from main
export const agState = {
  panes: {},
  defaultMinutes: 15,
  auth: { configured: false, totp: false },
}

// pane id sequences (pty-1, chat-2, …)
export const counters = { seq: 0 }
