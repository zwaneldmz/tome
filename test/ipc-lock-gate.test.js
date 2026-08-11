// Pins the pre-auth ipcMain.on gate (TOME-003) that index.js wires up but
// cannot itself be unit-tested — index.js boots a real Electron main process
// (BrowserWindow, protocol.handle, app.whenReady, ...) and is never imported
// by any test in this repo. Before this gate existed, ipcMain.on channels
// (fire-and-forget/sendSync, so the separate ipcMain.handle gate never
// covered them) ran straight through a locked app: ws:sync reset the
// fs-confinement roots and lsp:didOpen spawned a language server with no
// login required at all. The two regressions this exists to catch — OPEN_ON
// silently widening to include a channel like 'ws:sync' or 'lsp:didOpen',
// and isLocked's AND composition breaking (e.g. flipping to OR, or dropping
// the shotMode guard) — would otherwise ship with no failing test anywhere.
import { describe, it, expect } from 'vitest'
import { OPEN_ON, isLocked, shouldBlockIpcOn } from '../src/main/lib/ipc-lock-gate.js'

describe('OPEN_ON', () => {
  it('is exactly the lock screen’s own needs — homedir and theming', () => {
    // A bypass here is a widen, so pin the exact set rather than just
    // membership: any addition (or removal) must be a deliberate edit here.
    expect(OPEN_ON).toEqual(new Set(['app:home', 'theme:set']))
  })

  it('does not include channels that must never run pre-auth', () => {
    // ws:sync resets the fs-confinement roots; lsp:didOpen spawns a language
    // server — both were the original TOME-003 bypass and must stay gated.
    for (const channel of ['ws:sync', 'lsp:didOpen', 'panes:sync', 'conductor:allowRun'])
      expect(OPEN_ON.has(channel)).toBe(false)
  })
})

describe('isLocked()', () => {
  it('is true only when configured, not unlocked, and not in shot mode', () => {
    expect(isLocked({ configured: true, unlocked: false, shotMode: false })).toBe(true)
  })

  it('is false when never configured (first run — no passphrase to unlock)', () => {
    expect(isLocked({ configured: false, unlocked: false, shotMode: false })).toBe(false)
  })

  it('is false once unlocked', () => {
    expect(isLocked({ configured: true, unlocked: true, shotMode: false })).toBe(false)
  })

  it('is false in TOME_SHOT dev/screenshot mode even if configured and locked', () => {
    // shotMode must win regardless of the other two — index.js computes it
    // as `!!process.env.TOME_SHOT && !app.isPackaged`, so this can only ever
    // be true in an unpackaged dev build, never a shipped one.
    expect(isLocked({ configured: true, unlocked: false, shotMode: true })).toBe(false)
  })

  it('requires every condition, not just a majority — the AND this pins', () => {
    // A regression that swaps && for || would make any single true input
    // enough; assert each pairwise-true / one-false combination independently
    // so that specific mistake fails here.
    expect(isLocked({ configured: false, unlocked: true, shotMode: true })).toBe(false)
    expect(isLocked({ configured: true, unlocked: true, shotMode: true })).toBe(false)
    expect(isLocked({ configured: true, unlocked: false, shotMode: true })).toBe(false)
  })
})

describe('shouldBlockIpcOn()', () => {
  it('blocks a non-allowlisted channel while locked', () => {
    expect(shouldBlockIpcOn('ws:sync', true)).toBe(true)
    expect(shouldBlockIpcOn('lsp:didOpen', true)).toBe(true)
  })

  it('never blocks an OPEN_ON channel, even while locked', () => {
    expect(shouldBlockIpcOn('app:home', true)).toBe(false)
    expect(shouldBlockIpcOn('theme:set', true)).toBe(false)
  })

  it('blocks nothing once unlocked, allowlisted or not', () => {
    expect(shouldBlockIpcOn('ws:sync', false)).toBe(false)
    expect(shouldBlockIpcOn('app:home', false)).toBe(false)
  })
})
