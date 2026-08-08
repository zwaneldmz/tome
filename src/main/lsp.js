// Language servers. One process per (workspace root, server), spawned lazily
// the first time a matching file is opened and reused for every file after.
//
// Deliberately small: this speaks just enough LSP for what the editor pane
// shows — diagnostics, hover, go-to-definition — with full-text document sync
// rather than incremental. Full sync costs a string per keystroke-batch and
// removes a whole class of desync bugs; incremental is the optimisation to
// reach for only if a large file measurably drags.
//
// Servers are never bundled. If the binary is not on PATH the language simply
// has no diagnostics, which is reported once and then left alone — a missing
// optional tool must not nag on every keystroke.
import { spawn } from 'node:child_process'
import { pathToFileURL, fileURLToPath } from 'node:url'
import { dirname, join, resolve, sep } from 'node:path'

// stdio commands that are the conventional way to run each server
const SERVERS = [
  {
    id: 'typescript',
    langs: ['typescript', 'typescriptreact', 'javascript', 'javascriptreact'],
    cmd: 'typescript-language-server',
    args: ['--stdio'],
  },
  { id: 'python', langs: ['python'], cmd: 'pyright-langserver', args: ['--stdio'] },
  { id: 'go', langs: ['go'], cmd: 'gopls', args: [] },
  { id: 'rust', langs: ['rust'], cmd: 'rust-analyzer', args: [] },
  { id: 'json', langs: ['json'], cmd: 'vscode-json-language-server', args: ['--stdio'] },
  { id: 'css', langs: ['css', 'scss', 'less'], cmd: 'vscode-css-language-server', args: ['--stdio'] },
  { id: 'html', langs: ['html'], cmd: 'vscode-html-language-server', args: ['--stdio'] },
]

const LANG_BY_EXT = {
  ts: 'typescript',
  tsx: 'typescriptreact',
  mts: 'typescript',
  cts: 'typescript',
  js: 'javascript',
  jsx: 'javascriptreact',
  mjs: 'javascript',
  cjs: 'javascript',
  py: 'python',
  pyi: 'python',
  go: 'go',
  rs: 'rust',
  json: 'json',
  jsonc: 'json',
  css: 'css',
  scss: 'scss',
  less: 'less',
  html: 'html',
  htm: 'html',
}

export function languageIdFor(path) {
  const ext = path.split('.').pop()?.toLowerCase()
  return (ext && LANG_BY_EXT[ext]) || null
}
const serverFor = (langId) => SERVERS.find((s) => s.langs.includes(langId)) || null

const uriOf = (path) => pathToFileURL(path).href
const pathOf = (uri) => {
  try {
    return fileURLToPath(uri)
  } catch {
    return null
  }
}

// ---- one server process ----
class Server {
  constructor(spec, root, onDiagnostics, onExit) {
    this.spec = spec
    this.root = root
    this.onDiagnostics = onDiagnostics
    this.onExit = onExit
    this.seq = 0
    this.pending = new Map() // request id -> {resolve, reject, timer}
    this.docs = new Map() // path -> version
    this.buf = Buffer.alloc(0)
    this.ready = null
    this.dead = false
  }

  start() {
    if (this.ready) return this.ready
    this.ready = (async () => {
      // A project-local server wins over a global one: language servers are
      // usually a dev dependency, and the project's pinned version is the one
      // that matches its config.
      const binDir = join(this.root, 'node_modules', '.bin')
      this.proc = spawn(this.spec.cmd, this.spec.args, {
        cwd: this.root,
        env: { ...process.env, PATH: `${binDir}:${process.env.PATH || ''}` },
        stdio: ['pipe', 'pipe', 'pipe'],
      })
      // A missing binary raises 'error' and may never raise 'exit', so both
      // paths must settle the in-flight requests — otherwise `initialize`
      // sits on its timeout and the pane waits seconds to learn the server
      // simply is not installed.
      const fail = (err) => {
        if (this.dead) return
        this.dead = true
        for (const { reject, timer } of this.pending.values()) {
          clearTimeout(timer)
          reject(err)
        }
        this.pending.clear()
        this.onExit?.(this)
      }
      this.proc.on('error', (err) => fail(err)) // not installed / not executable
      this.proc.on('exit', () => fail(new Error('language server exited')))
      this.proc.stdout.on('data', (chunk) => this.onData(chunk))
      this.proc.stderr.resume() // drain; server logs are not ours to surface

      await this.request('initialize', {
        processId: process.pid,
        rootUri: uriOf(this.root),
        workspaceFolders: [{ uri: uriOf(this.root), name: this.root.split(sep).pop() }],
        capabilities: {
          textDocument: {
            synchronization: { dynamicRegistration: false },
            publishDiagnostics: { relatedInformation: false },
            hover: { contentFormat: ['plaintext', 'markdown'] },
            definition: { linkSupport: false },
          },
          workspace: { workspaceFolders: true, configuration: true },
        },
      })
      this.notify('initialized', {})
      return true
    })().catch((err) => {
      this.kill()
      throw err
    })
    return this.ready
  }

  // ---- framing ----
  onData(chunk) {
    this.buf = Buffer.concat([this.buf, chunk])
    for (;;) {
      const split = this.buf.indexOf('\r\n\r\n')
      if (split === -1) return
      const header = this.buf.subarray(0, split).toString('ascii')
      const match = /content-length:\s*(\d+)/i.exec(header)
      if (!match) {
        this.buf = this.buf.subarray(split + 4) // unparseable header, skip it
        continue
      }
      const len = Number(match[1])
      const start = split + 4
      if (this.buf.length < start + len) return // wait for the rest
      const body = this.buf.subarray(start, start + len).toString('utf8')
      this.buf = this.buf.subarray(start + len)
      let msg
      try {
        msg = JSON.parse(body)
      } catch {
        continue
      }
      this.dispatch(msg)
    }
  }

  dispatch(msg) {
    if (msg.id !== undefined && this.pending.has(msg.id)) {
      const { resolve: res, reject, timer } = this.pending.get(msg.id)
      this.pending.delete(msg.id)
      clearTimeout(timer)
      msg.error ? reject(new Error(msg.error.message || 'lsp error')) : res(msg.result)
      return
    }
    // server -> client request: answer the few that block startup if ignored
    if (msg.id !== undefined && msg.method) {
      const result = msg.method === 'workspace/configuration' ? [{}] : null
      this.send({ jsonrpc: '2.0', id: msg.id, result })
      return
    }
    if (msg.method === 'textDocument/publishDiagnostics') {
      const path = pathOf(msg.params?.uri)
      if (path) this.onDiagnostics(path, msg.params.diagnostics || [])
    }
  }

  send(payload) {
    if (this.dead || !this.proc?.stdin.writable) return
    const body = Buffer.from(JSON.stringify(payload), 'utf8')
    this.proc.stdin.write(`Content-Length: ${body.length}\r\n\r\n`)
    this.proc.stdin.write(body)
  }

  notify(method, params) {
    this.send({ jsonrpc: '2.0', method, params })
  }

  request(method, params, timeoutMs = 15000) {
    const id = ++this.seq
    return new Promise((res, reject) => {
      // a wedged server must not leak a pending promise per keystroke
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`${method} timed out`))
      }, timeoutMs)
      this.pending.set(id, { resolve: res, reject, timer })
      this.send({ jsonrpc: '2.0', id, method, params })
    })
  }

  // ---- documents ----
  didOpen(path, langId, text) {
    if (this.docs.has(path)) return this.didChange(path, text)
    this.docs.set(path, 1)
    this.notify('textDocument/didOpen', {
      textDocument: { uri: uriOf(path), languageId: langId, version: 1, text },
    })
  }
  didChange(path, text) {
    const version = (this.docs.get(path) || 1) + 1
    this.docs.set(path, version)
    this.notify('textDocument/didChange', {
      textDocument: { uri: uriOf(path), version },
      contentChanges: [{ text }], // full sync
    })
  }
  didClose(path) {
    if (!this.docs.delete(path)) return
    this.notify('textDocument/didClose', { textDocument: { uri: uriOf(path) } })
  }

  kill() {
    this.dead = true
    try {
      this.proc?.kill()
    } catch {
      /* already gone */
    }
  }
}

// ---- pool ----
const servers = new Map() // `${root} ${serverId}` -> Server
const missing = new Set() // `${root} ${cmd}` already reported absent, so we say it once
let notifyDiagnostics = () => {}
let notifyMissing = () => {}

export function init({ onDiagnostics, onMissing }) {
  notifyDiagnostics = onDiagnostics || (() => {})
  notifyMissing = onMissing || (() => {})
}

// The workspace folder the file sits in — the server's root. Falls back to the
// file's own directory so a file opened outside any workspace still gets one.
function rootFor(path, folders) {
  const abs = resolve(path)
  const hit = (folders || [])
    .filter((f) => abs === f || abs.startsWith(f + sep))
    .sort((a, b) => b.length - a.length)[0]
  return hit || dirname(abs)
}

async function serverOf(path, folders) {
  const langId = languageIdFor(path)
  if (!langId) return null
  const spec = serverFor(langId)
  if (!spec) return null
  const root = rootFor(path, folders)
  if (missing.has(`${root} ${spec.cmd}`)) return null
  const key = `${root} ${spec.id}`
  let server = servers.get(key)
  if (server?.dead) {
    servers.delete(key)
    server = null
  }
  if (!server) {
    server = new Server(spec, root, notifyDiagnostics, (s) => {
      for (const [k, v] of servers) if (v === s) servers.delete(k)
    })
    servers.set(key, server)
  }
  try {
    await server.start()
  } catch {
    // treat a server that will not start as absent: report once, then stay quiet
    servers.delete(key)
    const mark = `${root} ${spec.cmd}`
    if (!missing.has(mark)) {
      missing.add(mark)
      notifyMissing(spec.cmd, langId)
    }
    return null
  }
  return { server, langId }
}

export async function didOpen(path, text, folders) {
  const s = await serverOf(path, folders)
  s?.server.didOpen(path, s.langId, text)
}
export async function didChange(path, text, folders) {
  const s = await serverOf(path, folders)
  s?.server.didChange(path, text)
}
export async function didClose(path, folders) {
  const s = await serverOf(path, folders)
  s?.server.didClose(path)
}

export async function hover(path, line, character, folders) {
  const s = await serverOf(path, folders)
  if (!s) return null
  try {
    const res = await s.server.request('textDocument/hover', {
      textDocument: { uri: uriOf(path) },
      position: { line, character },
    })
    const c = res?.contents
    if (!c) return null
    const text = Array.isArray(c)
      ? c.map((x) => (typeof x === 'string' ? x : x.value)).join('\n')
      : typeof c === 'string'
        ? c
        : c.value
    return text?.trim() || null
  } catch {
    return null
  }
}

export async function definition(path, line, character, folders) {
  const s = await serverOf(path, folders)
  if (!s) return null
  try {
    const res = await s.server.request('textDocument/definition', {
      textDocument: { uri: uriOf(path) },
      position: { line, character },
    })
    const first = Array.isArray(res) ? res[0] : res
    if (!first) return null
    const uri = first.uri || first.targetUri
    const range = first.range || first.targetSelectionRange || first.targetRange
    const target = pathOf(uri)
    if (!target || !range) return null
    return { path: target, line: range.start.line, character: range.start.character }
  } catch {
    return null
  }
}

export function shutdownAll() {
  for (const s of servers.values()) s.kill()
  servers.clear()
}
