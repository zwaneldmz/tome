// The pre-auth ipcMain.on gate (TOME-003): which fire-and-forget/sendSync IPC
// channels may run before login succeeds, and the block/pass decision itself.
// Extracted so both are testable without a bundled Electron main process —
// index.js is the only caller, and owns no policy of its own beyond wiring
// these into ipcMain.on/authlock/app.isPackaged.
//
// ipcMain.handle channels get their own gate (installed once whenReady
// fires); ipcMain.on channels are fire-and-forget or sendSync, so without a
// matching wrapper ws:sync (resets the fs-confinement roots) and lsp:didOpen
// (spawns a language server) ran straight through a locked app. Only the
// lock screen's own needs stay open: homedir (for the workspace picker) and
// theming — widening this set re-opens exactly that bypass, so it is pinned
// by test/ipc-lock-gate.test.js.
export const OPEN_ON = new Set(['app:home', 'theme:set'])

// True when the app currently requires login before a gated channel may run.
// `shotMode` is TOME_SHOT dev/screenshot mode, which index.js computes as
// `!!process.env.TOME_SHOT && !app.isPackaged` — it must never bypass the
// gate in a packaged build, so it is threaded through as plain input here
// rather than read from the environment/app object directly.
export function isLocked({ configured, unlocked, shotMode }) {
  return !!configured && !unlocked && !shotMode
}

// True when `channel` must be refused because the app is locked and the
// channel is not on the pre-auth allowlist. Kept as a standalone predicate
// (rather than inlined in the ipcMain.on wrapper) so a future edit that
// widens OPEN_ON, or a regression in isLocked's composition, fails a pinned
// test here instead of shipping silently.
export function shouldBlockIpcOn(channel, locked) {
  return locked && !OPEN_ON.has(channel)
}
