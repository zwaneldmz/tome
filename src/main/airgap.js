// Air gap for agent panes: macOS seatbelt denies all direct egress (DNS included);
// the only way out is loopback to a per-pane CONNECT proxy that enforces the
// provider allowlist. Unlocking widens the proxy, never the sandbox.
import http from 'node:http'
import { connect } from 'node:net'
import { createHash } from 'node:crypto'
import { readFile, writeFile, chmod } from 'node:fs/promises'
import { join } from 'node:path'
import {
  DEFAULT_ALLOW,
  compileAllowlist,
  parseRepoAllowlist,
  validateRepoAllowlist,
} from './lib/allowlist.js'

const DEFAULT_UNLOCK_MINUTES = 15

// The effective matcher list is the union of three sources, recompiled
// whenever one changes: the shipped provider defaults, the user's own
// userData/airgap.json (which REPLACES the defaults when present — same as
// it always has), and the consented repo lists (only ever ADDED on top).
let userAllow = null // null = no user file yet, defaults are in force
// Consent authority lives HERE, not in the renderer-writable store: root →
// { hash, hosts }. A compromised renderer can ask main to re-check a repo
// file, but it cannot widen egress without main re-reading and re-hashing
// the file itself. Persisted 0600 like the auth file; the seatbelt denies
// agents all of userData, so neither side of the sandbox can forge consent.
const repoConsents = new Map()
const appliedRepos = new Map() // root → hosts currently in the matcher list
let consentsFile = null
// index.js hands over its symlink-aware confinement (same pattern as
// setLogger) so repo files are resolved inside the open workspace folders.
let confinedRealPath = async () => null
let allowMatchers = compileAllowlist(DEFAULT_ALLOW)
let onEvent = () => {}
let logEvent = () => {} // main's persistent event log (events.js)
const panes = new Map() // paneId -> { mode, expiresAt, timer, server }

function recompile() {
  allowMatchers = compileAllowlist([
    ...(userAllow || DEFAULT_ALLOW),
    ...[...appliedRepos.values()].flat(),
  ])
}

export function setEventSink(fn) {
  onEvent = (type, payload) => {
    // Quit tears the window down while teardown code (closeAll, pty kills)
    // may still push state — a send into a destroyed webContents throws
    // "Object has been destroyed" as an uncaught main-process exception.
    try {
      fn(type, payload)
    } catch {}
  }
}

// onEvent pushes to the live renderer; logEvent persists to the event log.
// Separate setters so the no-throw guarantee of logging never depends on a
// renderer being up.
export function setLogger(fn) {
  logEvent = typeof fn === 'function' ? fn : () => {}
}

export async function loadAllowlist(userData) {
  const file = join(userData, 'airgap.json')
  try {
    const cfg = JSON.parse(await readFile(file, 'utf8'))
    if (Array.isArray(cfg.allow)) userAllow = cfg.allow
  } catch {
    // seed defaults; loaded into memory once at boot — file edits apply on restart
    await writeFile(file, JSON.stringify({ allow: DEFAULT_ALLOW }, null, 2)).catch(() => {})
  }
  recompile()
}

export function setConfinedRealPath(fn) {
  confinedRealPath = typeof fn === 'function' ? fn : async () => null
}

// Same 0600 discipline as authlock: the consent file proves user intent, so
// it must not be world-readable even outside the sandbox.
async function saveRepoConsents() {
  if (!consentsFile) return
  await writeFile(consentsFile, JSON.stringify(Object.fromEntries(repoConsents)))
  await chmod(consentsFile, 0o600)
}

export async function loadRepoConsents(userData) {
  consentsFile = join(userData, 'airgap-repo-consents.json')
  try {
    const stored = JSON.parse(await readFile(consentsFile, 'utf8'))
    for (const [root, c] of Object.entries(stored || {})) {
      if (typeof c?.hash === 'string' && Array.isArray(c?.hosts)) repoConsents.set(root, c)
    }
  } catch {
    // missing/corrupt consent file = no consents — the safe default
  }
}

// Reads ${root}/.tome/airgap.json through the confined resolver and reports
// what main WOULD apply — the renderer's modal must show exactly this set,
// never its own parse. Missing/unreadable/malformed all report 'absent':
// a file main cannot read is a file main cannot apply.
export async function readRepoAllowlist(root) {
  if (typeof root !== 'string' || !root) return { state: 'absent' }
  const real = await confinedRealPath(`${root}/.tome/airgap.json`)
  if (!real) return { state: 'absent' }
  let text
  try {
    text = await readFile(real, 'utf8')
  } catch {
    return { state: 'absent' }
  }
  let hosts
  try {
    hosts = parseRepoAllowlist(text).hosts
  } catch {
    return { state: 'absent' }
  }
  // Hash the raw text, not the parsed array: ANY edit — even whitespace —
  // must re-prompt, or a commit could smuggle a semantic change past an
  // existing consent.
  const hash = createHash('sha1').update(text).digest('hex')
  const { ok, rejected } = validateRepoAllowlist(hosts)
  return { state: 'present', hash, hosts: ok, rejected, consented: repoConsents.get(root)?.hash === hash }
}

// TOCTOU-safe consent: the file is re-read and re-hashed NOW, and the
// presented hash must match what main just computed — the renderer never
// supplies hosts, only proof that the user saw this exact file.
export async function consentRepoAllowlist(root, hash) {
  if (typeof root !== 'string' || typeof hash !== 'string')
    return { ok: false, error: 'bad request' }
  const r = await readRepoAllowlist(root)
  if (r.state !== 'present') return { ok: false, error: 'file changed' }
  if (r.hash !== hash) return { ok: false, error: 'file changed' }
  repoConsents.set(root, { hash, hosts: r.hosts })
  appliedRepos.set(root, r.hosts)
  recompile()
  await saveRepoConsents().catch(() => {}) // persistence must not break consent
  return { ok: true, applied: r.hosts, rejected: r.rejected }
}

export async function revokeRepoAllowlist(root) {
  repoConsents.delete(root)
  appliedRepos.delete(root)
  recompile()
  await saveRepoConsents().catch(() => {})
  return { ok: true }
}

// Boot/workspace-sync re-apply: stored consents ARE the applied set, but
// only while the file they pin still matches. A consent whose file changed
// or vanished is dropped (and the drop persisted) — that is what makes
// "file changed ⇒ re-prompt" and revoke-by-delete true main-side.
// confinedRealPath refuses until the renderer's first ws:sync, so this is
// called again from the ws:sync handler; the guard keeps the two from
// racing each other.
let reapplying = false
export async function reapplyRepoConsents() {
  if (reapplying) return
  reapplying = true
  try {
    let changed = false
    for (const [root, c] of repoConsents) {
      const r = await readRepoAllowlist(root)
      if (r.state === 'present' && r.hash === c.hash) {
        appliedRepos.set(root, c.hosts)
      } else {
        repoConsents.delete(root)
        appliedRepos.delete(root)
        changed = true
      }
    }
    recompile()
    if (changed) await saveRepoConsents().catch(() => {})
  } finally {
    reapplying = false
  }
}

export function seatbeltProfile(userData) {
  // Later rules win: default-allow, kill egress, re-allow loopback only.
  // Hardening: an agent may read/write project files, but not tome's own
  // config (allowlist tamper) nor the auth file (TOTP secret).
  return [
    '(version 1)',
    '(allow default)',
    '(deny network-outbound)',
    '(allow network-outbound (remote ip "localhost:*"))',
    `(deny file-write* (subpath "${userData}"))`,
    `(deny file-read* (literal "${join(userData, 'airgap-auth.json')}"))`,
  ].join('\n')
}

// RFC 9110 hop-by-hop headers must not be forwarded verbatim: a pane could
// otherwise smuggle Proxy-Authorization or abuse Connection/Upgrade
// semantics through the proxy. Node lowercases incoming header names.
const HOP_BY_HOP = new Set([
  'connection',
  'keep-alive',
  'proxy-authenticate',
  'proxy-authorization',
  'te',
  'trailers',
  'transfer-encoding',
  'upgrade',
])
function forwardHeaders(headers) {
  const out = {}
  for (const [k, v] of Object.entries(headers)) if (!HOP_BY_HOP.has(k)) out[k] = v
  return out
}

function hostAllowed(paneId, host) {
  if (panes.get(paneId)?.mode === 'open') return true
  return allowMatchers.some((re) => re.test(host))
}

function pushState() {
  const out = {}
  for (const [id, st] of panes) out[id] = { mode: st.mode, expiresAt: st.expiresAt }
  onEvent('state', { panes: out })
}

// A retrying agent can hammer a blocked host for minutes; one log line per
// attempt would bury the audit trail. The live renderer push stays
// uncoalesced (the toast throttle owns UX), but the persistent log writes
// the first attempt immediately and then one trailing "× N" record per
// (pane, host) once a minute of quiet has passed.
const BLOCKED_COALESCE_MS = 60_000
const blockedPending = new Map() // `${paneId}|${host}` → { count, first, timer }

function logBlocked(paneId, host) {
  const key = `${paneId}|${host}`
  const now = Date.now()
  const p = blockedPending.get(key)
  if (p && now - p.first < BLOCKED_COALESCE_MS) {
    p.count++
    clearTimeout(p.timer)
    p.timer = setTimeout(() => flushBlocked(key), BLOCKED_COALESCE_MS - (now - p.first))
    p.timer.unref?.()
    return
  }
  if (p) clearTimeout(p.timer) // window expired without a flush — log fresh
  logEvent('airgap:blocked', { paneId, host })
  blockedPending.set(key, {
    count: 1,
    first: now,
    timer: (() => {
      const t = setTimeout(() => flushBlocked(key), BLOCKED_COALESCE_MS)
      t.unref?.()
      return t
    })(),
  })
}

function flushBlocked(key) {
  const p = blockedPending.get(key)
  blockedPending.delete(key)
  if (!p || p.count < 2) return // the first occurrence was already logged
  const i = key.indexOf('|')
  logEvent('airgap:blocked', { paneId: key.slice(0, i), host: key.slice(i + 1), count: p.count })
}

export function createPaneProxy(paneId) {
  const blocked = (host) => {
    onEvent('blocked', { paneId, host })
    logBlocked(paneId, host)
  }

  const server = http.createServer((req, res) => {
    // plain-HTTP proxy request (absolute URI)
    let u
    try {
      u = new URL(req.url)
    } catch {
      res.writeHead(400)
      res.end()
      return
    }
    if (!hostAllowed(paneId, u.hostname)) {
      blocked(u.hostname)
      res.writeHead(403)
      res.end(`airgap: ${u.hostname} is blocked (providers-only mode)\n`)
      return
    }
    const up = http.request(
      {
        host: u.hostname,
        port: u.port || 80,
        path: u.pathname + u.search,
        method: req.method,
        headers: forwardHeaders(req.headers),
      },
      (ur) => {
        res.writeHead(ur.statusCode, ur.headers)
        ur.pipe(res)
      }
    )
    up.on('error', () => {
      try {
        res.writeHead(502)
        res.end()
      } catch {}
    })
    req.pipe(up)
  })

  server.on('clientError', (err, socket) => socket.destroy())
  server.on('connection', (socket) => socket.on('error', () => socket.destroy()))

  server.on('connect', (req, socket, head) => {
    const i = req.url.lastIndexOf(':')
    const host = i > 0 ? req.url.slice(0, i) : req.url
    const port = i > 0 ? +req.url.slice(i + 1) : 443
    if (!hostAllowed(paneId, host)) {
      blocked(host)
      socket.end(`HTTP/1.1 403 Forbidden\r\n\r\nairgap: ${host} is blocked\r\n`)
      return
    }
    const up = connect(port, host, () => {
      socket.write('HTTP/1.1 200 Connection Established\r\n\r\n')
      if (head?.length) up.write(head)
      up.pipe(socket)
      socket.pipe(up)
    })
    up.on('error', () => socket.destroy())
    socket.on('error', () => up.destroy())
  })

  return new Promise((resolve, reject) => {
    server.on('error', reject)
    server.listen(0, '127.0.0.1', () => {
      panes.set(paneId, { mode: 'providers', expiresAt: null, timer: null, server })
      pushState()
      resolve({ port: server.address().port })
    })
  })
}

export function unlockPane(paneId, minutes = DEFAULT_UNLOCK_MINUTES) {
  const st = panes.get(paneId)
  if (!st) return false
  clearTimeout(st.timer)
  st.mode = 'open'
  st.expiresAt = Date.now() + minutes * 60_000
  st.timer = setTimeout(() => relockPane(paneId), minutes * 60_000)
  pushState()
  logEvent('airgap:unlock', { paneId, minutes })
  return true
}

export function relockPane(paneId) {
  const st = panes.get(paneId)
  if (!st) return
  clearTimeout(st.timer)
  st.mode = 'providers'
  st.expiresAt = null
  st.timer = null
  pushState()
  logEvent('airgap:relock', { paneId })
}

export function closePane(paneId) {
  const st = panes.get(paneId)
  if (!st) return
  clearTimeout(st.timer)
  st.server.close()
  panes.delete(paneId)
  pushState()
}

// Quit-time teardown: pane proxies are children of no window, so without
// this an unclosed proxy (spawn failed after createPaneProxy, onExit never
// fired) would keep its loopback port bound until the process exits.
// server.close() only stops accepting — in-flight CONNECT tunnels die with
// the process itself, which is where this is called from. No pushState:
// closeAll runs from will-quit/window-all-closed, where the renderer is
// already gone — pushing would throw "Object has been destroyed".
export function closeAll() {
  for (const [id, st] of panes) {
    clearTimeout(st.timer)
    st.server.close()
    panes.delete(id)
  }
}

export function getState() {
  const out = {}
  for (const [id, st] of panes) out[id] = { mode: st.mode, expiresAt: st.expiresAt }
  return {
    panes: out,
    defaultMinutes: DEFAULT_UNLOCK_MINUTES,
    // One entry per consented repo currently in the matcher list — a
    // workspace can span several folders, each with its own consent.
    repo: [...appliedRepos.entries()].map(([root, hosts]) => ({ root, hosts: hosts.length })),
  }
}
