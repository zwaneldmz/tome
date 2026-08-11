// The allowed top-level navigation target for every webContents the app
// owns: our own packaged renderer pages (index.html, popout.html), or the
// electron-vite dev server origin in development. Backs main's
// will-navigate/will-redirect guard (TOME-006) — before it, a renderer-driven
// top-level navigation or a server redirect had no application-owned policy
// at all, just whatever Electron defaults to. Extracted so it is testable
// without a bundled Electron main process — index.js is the only caller.
//
// Same shape as index.js's isPopoutUrl (dev: protocol+host match against
// ELECTRON_RENDERER_URL; packaged: exact resolved file: path match), which
// stays narrower — popout.html only — for window.open. This one is broader
// (either packaged entry point) because a normal top-level navigation inside
// either window is legitimate; parameterized on the renderer directory
// instead of resolving __dirname itself, since __dirname means something
// different under this file's own path than under index.js's once bundled.
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export function isAppUrl(raw, { devOrigin, rendererDir } = {}) {
  let u
  try {
    u = new URL(raw)
  } catch {
    return false
  }
  if (devOrigin) {
    let base
    try {
      base = new URL(devOrigin)
    } catch {
      return false
    }
    return u.protocol === base.protocol && u.host === base.host
  }
  if (u.protocol !== 'file:') return false
  try {
    const path = resolve(fileURLToPath(u))
    return path === resolve(rendererDir, 'index.html') || path === resolve(rendererDir, 'popout.html')
  } catch {
    return false
  }
}
