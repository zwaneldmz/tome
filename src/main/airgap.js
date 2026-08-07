// Air gap for agent panes: macOS seatbelt denies all direct egress (DNS included);
// the only way out is loopback to a per-pane CONNECT proxy that enforces the
// provider allowlist. Unlocking widens the proxy, never the sandbox.
import http from 'node:http'
import { connect } from 'node:net'
import { readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const DEFAULT_ALLOW = [
  'api.anthropic.com',
  'claude.ai',
  'console.anthropic.com',
  'statsig.anthropic.com',
  'api.openai.com',
  'auth.openai.com',
  'generativelanguage.googleapis.com',
  'oauth2.googleapis.com',
  'openrouter.ai',
  'router.requesty.ai',
  'api.deepseek.com',
  'api.moonshot.ai',
  'api.groq.com',
  'api.mistral.ai',
  'api.x.ai',
  'bedrock-runtime.*.amazonaws.com',
]

const DEFAULT_UNLOCK_MINUTES = 15

let allowMatchers = compile(DEFAULT_ALLOW)
let onEvent = () => {}
const panes = new Map() // paneId -> { mode, expiresAt, timer, server }

function compile(patterns) {
  return patterns.map((p) => {
    const re = p
      .split('*')
      .map((s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
      .join('[a-z0-9-]+')
    return new RegExp(`^${re}$`, 'i')
  })
}

export function setEventSink(fn) {
  onEvent = fn
}

export async function loadAllowlist(userData) {
  const file = join(userData, 'airgap.json')
  try {
    const cfg = JSON.parse(await readFile(file, 'utf8'))
    if (Array.isArray(cfg.allow)) allowMatchers = compile(cfg.allow)
  } catch {
    // seed defaults; loaded into memory once at boot — file edits apply on restart
    await writeFile(file, JSON.stringify({ allow: DEFAULT_ALLOW }, null, 2)).catch(() => {})
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

export function createPaneProxy(paneId) {
  const blocked = (host) => onEvent('blocked', { paneId, host })

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
}

export function closePane(paneId) {
  const st = panes.get(paneId)
  if (!st) return
  clearTimeout(st.timer)
  st.server.close()
  panes.delete(paneId)
  pushState()
}

export function getState() {
  const out = {}
  for (const [id, st] of panes) out[id] = { mode: st.mode, expiresAt: st.expiresAt }
  return { panes: out, defaultMinutes: DEFAULT_UNLOCK_MINUTES }
}
