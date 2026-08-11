// Pins the proxy lifecycle the leak review flagged: a pane proxy must come
// down when its pane closes, and closeAll must reap whatever quit reaches —
// a proxy is a listening loopback server with no window to die with.
import { describe, it, expect, afterEach, vi } from 'vitest'
import { get, request as httpRequest } from 'node:http'
import { createServer } from 'node:net'
import { mkdtemp, writeFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { DEFAULT_ALLOW } from '../src/main/lib/allowlist.js'
import {
  createPaneProxy,
  closePane,
  closeAll,
  getState,
  unlockPane,
  relockPane,
  loadAllowlist,
  ALLOWED_UNLOCK_MINUTES,
} from '../src/main/airgap.js'

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

// TOME-019: airgap:unlock forwards the renderer's `minutes` verbatim into
// unlockPane, which used to trust it completely — Date.now() + minutes*60_000
// and setTimeout(..., minutes*60_000) both ran on whatever arrived, so a
// forged '15', NaN, Infinity, 0, negative, or out-of-menu value flipped the
// pane to mode 'open' (with a bogus or near-immediate expiry) before the
// value was ever checked. unlockPane now validates against a main-owned
// allowlist BEFORE any state mutation; ALLOWED_UNLOCK_MINUTES is exactly the
// menu the UI offers, so a legitimate renderer is never affected.
describe('unlockPane minutes validation', () => {
  it('ALLOWED_UNLOCK_MINUTES matches the menu the UI offers', () => {
    expect(ALLOWED_UNLOCK_MINUTES).toEqual([15, 30, 60])
  })

  it.each(['15', NaN, Infinity, 0, -1, 999])(
    'rejects minutes=%p without mutating pane state',
    async (bad) => {
      const id = pane()
      await createPaneProxy(id)
      expect(unlockPane(id, bad)).toBe(false)
      expect(getState().panes[id].mode).toBe('providers')
      expect(getState().panes[id].expiresAt).toBeNull()
    }
  )

  it.each(ALLOWED_UNLOCK_MINUTES)(
    'accepts %i minutes, opens the pane, and relocks by the deadline',
    async (minutes) => {
      const id = pane()
      await createPaneProxy(id)
      vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
      try {
        expect(unlockPane(id, minutes)).toBe(true)
        expect(getState().panes[id].mode).toBe('open')
        vi.advanceTimersByTime(minutes * 60_000 - 1)
        expect(getState().panes[id].mode).toBe('open') // not yet — deadline is exclusive
        vi.advanceTimersByTime(1)
        expect(getState().panes[id].mode).toBe('providers') // relocked itself
      } finally {
        vi.useRealTimers()
      }
    }
  )
})

// TOME-002: an established CONNECT tunnel used to outlive the mode change
// that was supposed to cut it off. relockPane only ever flipped st.mode, and
// closePane/closeAll only called server.close() — which stops ACCEPTING new
// connections but does nothing to sockets already piping. A tunnel opened
// while a pane was unlocked (mode 'open', any host allowed) kept piping
// bytes to its host forever, straight through a relock or even a pane close.
// createPaneProxy now tracks every established tunnel per pane so relock/
// close can find and destroy the ones that only got through because mode
// was 'open' — a tunnel to a host that's allowed on its own merits (provider
// allowlist / consented repo hosts) survives a relock, matching what the UI
// promises: relock narrows egress, it doesn't kill legitimate in-flight
// provider traffic.
describe('CONNECT tunnel teardown on relock/close', () => {
  // DEFAULT_ALLOW is all real internet hostnames (api.anthropic.com etc.)
  // that a test can't bind locally, so this block points the allowlist at a
  // loopback address via loadAllowlist — the same file the app itself loads
  // from at boot — then reloads the shipped defaults afterward so nothing
  // leaks into a test that runs later.
  const scratchDirs = []
  const echoServers = []
  afterEach(async () => {
    for (const srv of echoServers.splice(0)) await new Promise((r) => srv.close(r))
    for (const dir of scratchDirs.splice(0)) await rm(dir, { recursive: true, force: true }).catch(() => {})
    const resetDir = await mkdtemp(join(tmpdir(), 'tome-airgap-reset-'))
    await writeFile(join(resetDir, 'airgap.json'), JSON.stringify({ allow: DEFAULT_ALLOW }))
    await loadAllowlist(resetDir)
    await rm(resetDir, { recursive: true, force: true }).catch(() => {})
  })

  // A bare TCP echo server — bytes in, bytes out — so a live tunnel can be
  // proven live (and a dead one proven dead) by round-tripping a payload.
  const echoServer = (address) =>
    new Promise((resolve) => {
      const srv = createServer((sock) => sock.pipe(sock))
      srv.listen(0, address, () => resolve(srv))
    })

  // Raw CONNECT through the pane's proxy, the same handshake a real HTTPS
  // client performs — built on nothing but Node's own http client.
  const openTunnel = (proxyPort, host, targetPort) =>
    new Promise((resolve, reject) => {
      const req = httpRequest({
        host: '127.0.0.1',
        port: proxyPort,
        method: 'CONNECT',
        path: `${host}:${targetPort}`,
      })
      req.on('connect', (res, socket) => {
        if (res.statusCode !== 200) return reject(new Error(`CONNECT ${res.statusCode}`))
        resolve(socket)
      })
      req.on('error', reject)
      req.end()
    })

  // Writes a payload and waits for its echo; a destroyed tunnel can deliver
  // neither, so this doubles as the "is this tunnel still alive" probe. A
  // socket that already closed once won't re-emit 'close'/'error' to a
  // freshly attached listener, so a live probe always settles via 'data'
  // (fast) and only a genuinely-dead one ever rides the fallback timer —
  // callers that already know the socket is destroyed pass a short one.
  const echoRoundTrip = (socket, payload, timeoutMs = 500) =>
    new Promise((resolve) => {
      const done = (v) => {
        clearTimeout(timer)
        resolve(v)
      }
      const timer = setTimeout(() => done(null), timeoutMs)
      socket.once('data', (d) => done(d.toString()))
      socket.once('error', () => done(null))
      socket.once('close', () => done(null))
      socket.write(payload)
    })

  it('relock destroys an open-mode-only tunnel but spares a provider-allowlisted one', async () => {
    // Point the allowlist at a loopback address a test can actually bind.
    const dir = await mkdtemp(join(tmpdir(), 'tome-airgap-allow-'))
    scratchDirs.push(dir)
    await writeFile(join(dir, 'airgap.json'), JSON.stringify({ allow: ['127.0.0.1'] }))
    await loadAllowlist(dir)

    // Two real, reachable loopback endpoints: 127.0.0.1 matches the
    // allowlist just loaded, ::1 does not — neither is in DEFAULT_ALLOW
    // either, so this holds even if the reset above hasn't run yet.
    const allowedSrv = await echoServer('127.0.0.1')
    const blockedSrv = await echoServer('::1')
    echoServers.push(allowedSrv, blockedSrv)

    const id = pane()
    const { port } = await createPaneProxy(id)
    expect(unlockPane(id, ALLOWED_UNLOCK_MINUTES[0])).toBe(true) // mode 'open' — any host tunnels

    const allowedSocket = await openTunnel(port, '127.0.0.1', allowedSrv.address().port)
    const blockedSocket = await openTunnel(port, '::1', blockedSrv.address().port)
    expect(await echoRoundTrip(allowedSocket, 'pre-relock')).toBe('pre-relock')
    expect(await echoRoundTrip(blockedSocket, 'pre-relock')).toBe('pre-relock')

    relockPane(id)
    // getState() must keep exposing only the shape the renderer has always
    // seen — the tunnel registry added for this fix must never appear in it.
    expect(getState().panes[id]).toEqual({ mode: 'providers', expiresAt: null })
    await new Promise((r) => setTimeout(r, 50)) // let the destroy() reach the client-side socket

    // Non-allowlisted tunnel: only ever admitted because mode was 'open',
    // so relock destroys it outright — it cannot exchange another byte.
    expect(blockedSocket.destroyed).toBe(true)
    expect(await echoRoundTrip(blockedSocket, 'post-relock', 100)).toBeNull()

    // Provider-allowlisted tunnel: allowed on its own merits, so relock
    // leaves it running — fresh traffic still round-trips after relock.
    expect(allowedSocket.destroyed).toBe(false)
    expect(await echoRoundTrip(allowedSocket, 'post-relock')).toBe('post-relock')

    allowedSocket.destroy()
  })

  it('closePane destroys every live tunnel for that pane', async () => {
    const srv = await echoServer('127.0.0.1')
    echoServers.push(srv)

    const id = pane()
    const { port } = await createPaneProxy(id)
    expect(unlockPane(id, ALLOWED_UNLOCK_MINUTES[0])).toBe(true)
    const socket = await openTunnel(port, '127.0.0.1', srv.address().port)
    expect(await echoRoundTrip(socket, 'alive')).toBe('alive')

    closePane(id)
    expect(getState().panes[id]).toBeUndefined() // pre-existing invariant, still holds
    await new Promise((r) => setTimeout(r, 50))
    expect(socket.destroyed).toBe(true)
    expect(await echoRoundTrip(socket, 'dead', 100)).toBeNull()
  })
})
