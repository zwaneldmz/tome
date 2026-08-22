// Renderer-wide mutable state, shared across modules. Grouped so the split
// stays a pure move; the objects are mutated in place (never reassigned).

// workspaces — shape: { workspaces: [{ name, folders: [] }], active: index }
export const wsState = { ws: { workspaces: [], active: -1 }, activeRoot: null }

// egress / conductor preferences (persisted via the store)
export const prefs = {
  egressDefault: true,
  conductorRun: false,
  // Sandboxed Docker: the global master (persisted) and the per-pane
  // spawn-mode (session-only — whether the NEXT agent pane asks for the
  // filtered gateway). Both must be on for a pane to get DOCKER_HOST.
  dockerGateway: false,
  dockerPanes: false,
  // Containment-only mode (P2.1): a CEILING, not a default. Removes the
  // unsandboxed spawn entry from the ＋ menu (see spawn-policy.js), and the
  // backend refuses unsandboxed spawns at the IPC layer regardless — this
  // flag only mirrors the stored pref for the UI.
  containmentOnly: false,
}

// live egress state mirrored from main
export const agState = {
  panes: {},
  defaultMinutes: 15,
  auth: { configured: false, totp: false },
}

// pane id sequences (pty-1, chat-2, …)
export const counters = { seq: 0 }
