// Pins the proxy lifecycle the leak review flagged: a pane proxy must come
// down when its pane closes, and closeAll must reap whatever quit reaches —
// a proxy is a listening loopback server with no window to die with.
import { describe, it, expect, afterEach } from 'vitest'
import { get } from 'node:http'
import { createPaneProxy, closePane, closeAll, getState } from '../src/main/airgap.js'

// Every test gets a fresh pane id so the module-level map never leaks state
// between cases; afterEach sweeps regardless of outcome.
let seq = 0
const ids = []
const pane = () => {
  const id = `test-pane-${++seq}`
  ids.push(id)
  return id
}
afterEach(() => {
  for (const id of ids.splice(0)) closePane(id)
  closeAll()
})

const portOpen = (port) =>
  new Promise((resolve) => {
    // CONNECT proxies answer any TCP connection; a refused connect means closed.
    const req = get({ host: '127.0.0.1', port, path: 'http://example.com/', timeout: 500 })
    req.on('response', () => resolve(true))
    req.on('timeout', () => resolve(false))
    req.on('error', () => resolve(false))
  })

describe('pane proxy lifecycle', () => {
  it('closePane stops the server and drops the state entry', async () => {
    const id = pane()
    const { port } = await createPaneProxy(id)
    expect(getState().panes[id]).toBeTruthy()
    expect(await portOpen(port)).toBe(true)
    closePane(id)
    expect(getState().panes[id]).toBeUndefined()
    expect(await portOpen(port)).toBe(false)
  })

  it('closePane on an unknown id is a no-op', () => {
    expect(() => closePane('never-existed')).not.toThrow()
  })

  it('closeAll reaps every proxy — the spawn-failed / quit path', async () => {
    const a = pane()
    const b = pane()
    const pa = await createPaneProxy(a)
    const pb = await createPaneProxy(b)
    expect(Object.keys(getState().panes).length).toBeGreaterThanOrEqual(2)
    closeAll()
    expect(getState().panes).toEqual({})
    expect(await portOpen(pa.port)).toBe(false)
    expect(await portOpen(pb.port)).toBe(false)
  })

  it('closeAll is idempotent (will-quit AND window-all-closed both call it)', () => {
    expect(() => {
      closeAll()
      closeAll()
    }).not.toThrow()
  })
})
