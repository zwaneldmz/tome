import { app, BrowserWindow, ipcMain, dialog, protocol, net, shell, systemPreferences } from 'electron'
import { join, resolve, extname, sep } from 'node:path'
import { readdir, readFile, writeFile, mkdir, realpath } from 'node:fs/promises'
import { homedir } from 'node:os'
import { execFile, execFileSync } from 'node:child_process'
import { pathToFileURL } from 'node:url'
import pty from 'node-pty'
import Anthropic from '@anthropic-ai/sdk'
import mammoth from 'mammoth'
import * as XLSX from 'xlsx'
import * as airgap from './airgap'
import * as authlock from './authlock'
import * as brain from './brain'
import * as conductor from './conductor'

const ptys = new Map()
let win = null
let anthropic = null

// ---- file-open confinement ----
// Anything that opens or parses a file in main on behalf of the renderer —
// the conductor's open_file tool (model-driven!) and doc:read's
// mammoth/SheetJS parsers — stays inside the open workspace folders, or a
// brain vault. Otherwise a prompt-injected chat reply could make the main
// process parse ~/.ssh/… with libraries that have CVE histories.
// (fs:readFile/fs:writeFile/shell:openPath stay unvetted by design: the
// editor and tree are user-driven; the trust boundary is documented in the
// review — renderer compromise ≈ user-privileged file access.)
let openFolders = [] // absolute paths of open workspace folders
function setOpenFolders(list) {
  openFolders = Array.isArray(list) ? list.filter((f) => typeof f === 'string' && f) : []
}
function isBrainPath(p) {
  return p.startsWith(brain.BRAINS_ROOT + sep)
}
function isConfinedPath(p) {
  if (typeof p !== 'string' || !p) return false
  const abs = resolve(p)
  return openFolders.some((f) => abs === f || abs.startsWith(f + sep)) || isBrainPath(abs)
}
// Symlink-aware variant for main-process parsing: resolve the real path so a
// symlink inside a workspace can't aim the parser outside it.
async function confinedRealPath(p) {
  if (!isConfinedPath(p)) return null
  try {
    const real = await realpath(p)
    return isConfinedPath(real) ? real : null
  } catch {
    return null
  }
}

const SHELL = process.env.SHELL || '/bin/zsh'
const AGENTS = ['claude', 'opencode', 'pi']
// Assistant provider: the Requesty router by default (REQUESTY_API_KEY, pulled
// from the login shell like the agent-pane secrets — Finder launches don't see
// .zshrc). Without a Requesty key, fall back to direct Anthropic.
// No /v1 suffix: the SDK appends /v1/messages itself, and /v1/v1/messages 404s.
const REQUESTY_BASE = process.env.TOME_CHAT_BASE_URL || 'https://router.requesty.ai'
// Requesty routes Claude via vertex/bedrock; bare anthropic/* ids 403 unless the
// key's Model Library approves them.
const REQUESTY_MODEL = process.env.TOME_CHAT_MODEL || 'vertex/claude-opus-4-8@eu'
const ANTHROPIC_MODEL = process.env.TOME_CHAT_MODEL || 'claude-opus-5'
const CHAT_SYSTEM =
  'You are the assistant pane inside Tome, a desktop coding harness. ' +
  'Keep responses focused, brief, and concise. Plain text only — no markdown tables.'

app.setName('Tome')

// When launched from Finder/Spotlight the app gets launchd's bare PATH, and a
// non-interactive login shell never reads .zshrc — where PATH additions like
// ~/.local/bin (claude) usually live. Resolve the user's real interactive
// PATH once, with well-known agent bins as a fallback.
function resolveLoginPath() {
  try {
    const out = execFileSync(SHELL, ['-ilc', 'echo -n "$PATH"'], {
      timeout: 8000,
      encoding: 'utf8',
    })
    const line = out
      .split('\n')
      .map((l) => l.replace(/\x1b\[[0-9;]*[A-Za-z]/g, '').trim())
      .filter((l) => l.includes('/usr/bin'))
      .pop()
    if (line) process.env.PATH = line
  } catch {}
  const cur = process.env.PATH ? process.env.PATH.split(':') : []
  const extras = [
    join(homedir(), '.local/bin'),
    join(homedir(), '.opencode/bin'),
    '/opt/homebrew/bin',
    '/usr/local/bin',
  ].filter((e) => !cur.includes(e))
  process.env.PATH = [...cur, ...extras].join(':')
}
resolveLoginPath()

// Same .zshrc blind spot as PATH, for provider credentials: agent CLIs are
// spawned with `-l -c`, which never reads .zshrc, so keys exported there are
// invisible and the CLI fails auth. Read them from an interactive login shell
// once, and hand them only to agent panes — a plain terminal pane inherits
// nothing, and tome's own process env is left untouched.
let agentSecrets = null
// Least-privilege forwarding: the old suffix match handed EVERY
// *_API_KEY/*_KEY/*_TOKEN in the login shell (GITHUB_TOKEN, NPM_TOKEN,
// DIGITALOCEAN_TOKEN, …) to every agent pane — air-gapped ones included —
// when a pane needs exactly its provider's key. Forward only the credentials
// the supported providers actually consume. New provider? Add its key here.
const AGENT_SECRET_KEYS = new Set([
  'ANTHROPIC_API_KEY',
  'OPENAI_API_KEY',
  'REQUESTY_API_KEY',
  'OPENROUTER_API_KEY',
  'DEEPSEEK_API_KEY',
  'MOONSHOT_API_KEY',
  'GROQ_API_KEY',
  'MISTRAL_API_KEY',
  'XAI_API_KEY',
  'GOOGLE_API_KEY',
  'GEMINI_API_KEY',
  // bedrock
  'AWS_ACCESS_KEY_ID',
  'AWS_SECRET_ACCESS_KEY',
  'AWS_REGION',
  'AWS_DEFAULT_REGION',
])
function resolveAgentSecrets() {
  if (agentSecrets) return agentSecrets
  agentSecrets = {}
  try {
    const out = execFileSync(SHELL, ['-ilc', 'env'], { timeout: 8000, encoding: 'utf8' })
    for (const line of out.split('\n')) {
      const i = line.indexOf('=')
      if (i < 1) continue
      const key = line.slice(0, i)
      const val = line.slice(i + 1)
      if (AGENT_SECRET_KEYS.has(key) && val) agentSecrets[key] = val
    }
  } catch {}
  return agentSecrets
}

// local-file protocol so panes can embed PDFs/images without file:// cross-origin blocks.
// Embedding (img/iframe) is all this scheme is for, so it gets no fetch/CORS
// privileges — renderer JS cannot read tome:// bodies by design, not by CSP
// accident (today's CSP omits tome: from connect-src, but one CSP edit used
// to silently turn this into a read-any-file primitive). The handler itself
// is confined to workspace folders / brain vaults + an extension allowlist.
protocol.registerSchemesAsPrivileged([
  {
    scheme: 'tome',
    privileges: { standard: true, secure: true, stream: true },
  },
])

// tome:// serves displayable content only: images, pdf, plain text/markdown,
// and common source files. Executables/archives/documents that parse in main
// (docx/xlsx go through doc:read instead) stay out.
const TOME_SERVE_EXT = new Set([
  'png',
  'jpg',
  'jpeg',
  'gif',
  'webp',
  'svg',
  'bmp',
  'ico',
  'avif',
  'pdf',
  'md',
  'markdown',
  'txt',
  'json',
  'js',
  'mjs',
  'cjs',
  'ts',
  'tsx',
  'jsx',
  'css',
  'html',
  'py',
  'rb',
  'go',
  'rs',
  'c',
  'h',
  'cpp',
  'java',
  'sh',
  'yml',
  'yaml',
  'toml',
  'xml',
  'csv',
])

const git = (dir, args) =>
  new Promise((resolve, reject) => {
    execFile('git', ['-C', dir, ...args], { timeout: 10000 }, (err, stdout, stderr) => {
      if (err) reject(new Error((stderr || err.message).trim()))
      else resolve(stdout)
    })
  })

// styles injected into sandboxed doc-viewer iframes (docx/xlsx conversions)
const DOC_CSS =
  '<style>body{font:14px/1.65 system-ui,sans-serif;background:#0c0d15;color:#c9d4e3;' +
  'padding:30px;max-width:840px;margin:0 auto}h1,h2,h3{color:#eef4fb}a{color:#00e5ff}' +
  'table{border-collapse:collapse;font-size:12.5px;font-family:ui-monospace,Menlo,monospace}' +
  'td,th{border:1px solid #1b1e2f;padding:4px 10px;white-space:nowrap}th{background:#12141f}' +
  'img{max-width:100%}</style>'

function createWindow() {
  win = new BrowserWindow({
    width: 1440,
    height: 900,
    minWidth: 800,
    minHeight: 500,
    titleBarStyle: 'hiddenInset',
    backgroundColor: '#060609',
    webPreferences: {
      preload: join(__dirname, '../preload/index.js'),
      sandbox: true,
    },
  })
  if (process.env.ELECTRON_RENDERER_URL) {
    win.loadURL(process.env.ELECTRON_RENDERER_URL)
    win.webContents.on('console-message', (event) => {
      console.log(`[renderer] ${event.message}`)
    })
  } else {
    win.loadFile(join(__dirname, '../renderer/index.html'))
  }
  if (process.env.TOME_SHOT) {
    win.webContents.once('did-finish-load', () => {
      setTimeout(async () => {
        const img = await win.webContents.capturePage()
        await writeFile(process.env.TOME_SHOT, img.toPNG())
        console.log('shot saved:', process.env.TOME_SHOT)
      }, 2500)
    })
  }
}

app.whenReady().then(async () => {
  // ---- app lock ----
  // Every invoke channel refuses until login succeeds; only the door itself
  // (auth + first-run setup) and the ui-state store stay open. Registered
  // handlers below inherit the guard because this wraps ipcMain.handle first.
  const OPEN_CHANNELS = new Set([
    'auth:status',
    'auth:login',
    'auth:touchid',
    'airgap:setup',
    'airgap:enrollTotp',
    'airgap:confirmTotp',
    'airgap:state',
    'store:get',
    'store:set',
  ])
  const isLockedNow = () =>
    authlock.authStatus().configured && !authlock.isUnlocked() && !process.env.TOME_SHOT
  const rawHandle = ipcMain.handle.bind(ipcMain)
  ipcMain.handle = (channel, fn) =>
    rawHandle(channel, (e, ...args) => {
      if (!OPEN_CHANNELS.has(channel) && isLockedNow()) throw new Error('Tome is locked.')
      return fn(e, ...args)
    })

  protocol.handle('tome', async (req) => {
    const p = decodeURIComponent(new URL(req.url).searchParams.get('p') || '')
    const deny = () => new Response('tome: path not allowed', { status: 403 })
    const ext = extname(p).slice(1).toLowerCase()
    if (!TOME_SERVE_EXT.has(ext)) return deny()
    const real = await confinedRealPath(p)
    if (!real) return deny()
    return net.fetch(pathToFileURL(real).toString())
  })

  if (process.platform === 'darwin' && !app.isPackaged && app.dock) {
    try {
      app.dock.setIcon(join(__dirname, '../../build/icon.png'))
    } catch {}
  }

  ipcMain.on('app:home', (e) => {
    e.returnValue = homedir()
  })

  const userData = app.getPath('userData')
  await airgap.loadAllowlist(userData)
  await authlock.initAuth(userData)
  airgap.setEventSink((type, payload) => win?.webContents.send('airgap:' + type, payload))
  brain.setEventSink((ws, index) => win?.webContents.send('brain:changed', { ws, index }))

  createWindow()
  conductor.init({
    ptys,
    send: (channel, payload) => win?.webContents.send(channel, payload),
    canOpenFile: isConfinedPath,
  })
  ipcMain.on('panes:sync', (e, list) => conductor.setPanes(list))
  ipcMain.on('ws:sync', (e, folders) => setOpenFolders(folders))
  ipcMain.on('conductor:allowRun', (e, v) => conductor.setAllowRun(v))

  // ---- pty ----
  // The renderer names a vetted pane kind; the command line is built HERE so a
  // compromised renderer can't request arbitrary binaries or arguments.
  ipcMain.handle('pty:create', async (e, { id, kind, cwd, airgap: gapped, ws }) => {
    try {
      return await createPty({ id, kind, cwd, gapped, ws })
    } catch (err) {
      // The renderer fires this without awaiting, so a throw here used to
      // surface as nothing but a blank pane. Say what broke.
      console.error(
        `pty:create failed (kind=${kind}, airgap=${!!gapped}, cwd=${cwd}, ws=${ws}):`,
        err
      )
      win?.webContents.send('pty:data', {
        id,
        data: `\r\n\x1b[31mpane failed to start: ${String(err?.message || err)}\x1b[0m\r\n`,
      })
      throw err
    }
  })

  async function createPty({ id, kind, cwd, gapped, ws }) {
    const isAgent = AGENTS.includes(kind)
    if (!isAgent && kind !== 'terminal') return
    let spawnCmd = SHELL
    let spawnArgs = isAgent ? ['-l', '-c', kind] : ['-l']
    const env = { ...process.env, TERM: 'xterm-256color', COLORTERM: 'truecolor' }
    if (isAgent) Object.assign(env, resolveAgentSecrets())
    if (ws) {
      env.TOME_BRAIN = await brain.ensureBrain(ws)
      const info = await brain.coreInfo(await readStore('core-vault'))
      if (info.configured) env.TOME_CORE_VAULT = info.root
    }
    if (gapped && process.platform === 'darwin') {
      const { port } = await airgap.createPaneProxy(id)
      const proxy = `http://127.0.0.1:${port}`
      for (const k of ['HTTP_PROXY', 'HTTPS_PROXY', 'http_proxy', 'https_proxy', 'ALL_PROXY'])
        env[k] = proxy
      env.NO_PROXY = env.no_proxy = 'localhost,127.0.0.1'
      spawnArgs = ['-p', airgap.seatbeltProfile(userData), spawnCmd, ...spawnArgs]
      spawnCmd = '/usr/bin/sandbox-exec'
    }
    const p = pty.spawn(spawnCmd, spawnArgs, {
      name: 'xterm-256color',
      cols: 80,
      rows: 24,
      cwd: cwd || homedir(),
      env,
    })
    ptys.set(id, p)
    conductor.register(id, { kind, cwd: cwd || homedir(), airgap: !!gapped })
    p.onData((data) => {
      conductor.record(id, data)
      win?.webContents.send('pty:data', { id, data })
    })
    p.onExit(({ exitCode }) => {
      ptys.delete(id)
      conductor.markExited(id)
      airgap.closePane(id)
      win?.webContents.send('pty:exit', { id, exitCode })
    })
  }
  ipcMain.on('pty:write', (e, { id, data }) => ptys.get(id)?.write(data))
  ipcMain.on('pty:resize', (e, { id, cols, rows }) => ptys.get(id)?.resize(cols, rows))
  ipcMain.on('pty:kill', (e, { id }) => {
    ptys.get(id)?.kill()
    ptys.delete(id)
    conductor.forget(id)
    airgap.closePane(id)
  })

  // ---- air gap ----
  ipcMain.handle('airgap:state', () => ({ ...airgap.getState(), auth: authlock.authStatus() }))
  ipcMain.handle('airgap:unlock', (e, { paneId, passphrase, code, minutes }) => {
    // The app login already proved the passphrase (or Touch ID) — this channel
    // is gated, so nobody reaches it locked. Freeing a pane still demands a
    // second factor: the TOTP code when enrolled, the passphrase otherwise.
    const wait = authlock.throttleRetryIn('airgap:unlock')
    if (wait) return { ok: false, error: `Too many attempts — try again in ${Math.ceil(wait / 1000)}s.` }
    const ok = authlock.totpActive()
      ? authlock.verifyTotp(code)
      : authlock.verifyPassphrase(passphrase)
    if (!ok) {
      authlock.recordFailure('airgap:unlock')
      return { ok: false, error: authlock.totpActive() ? 'Wrong 2FA code.' : 'Wrong passphrase.' }
    }
    authlock.recordSuccess('airgap:unlock')
    airgap.unlockPane(paneId, minutes)
    return { ok: true }
  })
  ipcMain.handle('airgap:relock', (e, paneId) => airgap.relockPane(paneId))
  ipcMain.handle('airgap:setup', async (e, { passphrase }) => {
    if (authlock.authStatus().configured) return { ok: false, error: 'Already configured.' }
    try {
      await authlock.setPassphrase(passphrase)
    } catch (err) {
      return { ok: false, error: err.message }
    }
    authlock.markUnlocked() // first-run setup happens at the lock screen
    return { ok: true }
  })
  ipcMain.handle('airgap:enrollTotp', () => authlock.enrollTotp())
  ipcMain.handle('airgap:confirmTotp', (e, { code }) => authlock.confirmTotp(code))

  // ---- app login (Touch ID or passphrase + TOTP; arms the whole workspace) ----
  ipcMain.handle('auth:status', () => ({
    ...authlock.authStatus(),
    unlocked: authlock.isUnlocked(),
    touchId: process.platform === 'darwin' && systemPreferences.canPromptTouchID(),
  }))
  ipcMain.handle('auth:touchid', async () => {
    try {
      await systemPreferences.promptTouchID('unlock the Tome workspace')
      authlock.markUnlocked()
      return { ok: true }
    } catch (err) {
      return { ok: false, error: err.message || 'Touch ID failed.' }
    }
  })
  ipcMain.handle('auth:login', (e, { passphrase, code }) => {
    const wait = authlock.throttleRetryIn('auth:login')
    if (wait) return { ok: false, error: `Too many attempts — try again in ${Math.ceil(wait / 1000)}s.` }
    const passOk = authlock.verifyPassphrase(passphrase)
    const totpOk = !authlock.totpActive() || authlock.verifyTotp(code)
    if (!passOk || !totpOk) {
      authlock.recordFailure('auth:login')
      return { ok: false, error: passOk ? 'Wrong 2FA code.' : 'Wrong passphrase.' }
    }
    authlock.recordSuccess('auth:login')
    authlock.markUnlocked()
    return { ok: true }
  })

  // ---- brain (per-workspace note vault) ----
  ipcMain.handle('brain:open', (e, { ws }) => brain.open(ws))
  ipcMain.handle('brain:close', (e, { ws }) => brain.close(ws))
  ipcMain.handle('brain:index', (e, { ws }) => brain.getIndex(ws))
  ipcMain.handle('brain:read', (e, { ws, rel }) => brain.readNote(ws, rel))
  ipcMain.handle('brain:write', (e, { ws, rel, content, exclusive }) =>
    brain.writeNote(ws, rel, content, exclusive)
  )
  ipcMain.handle('brain:delete', (e, { ws, rel }) => brain.deleteNote(ws, rel))
  ipcMain.handle('brain:coreInfo', async () => brain.coreInfo(await readStore('core-vault')))
  ipcMain.handle('brain:promote', async (e, { ws, rel, folder, overwrite, rename }) =>
    brain.promote(await readStore('core-vault'), ws, rel, folder, { overwrite, rename })
  )

  // ---- fs ----
  ipcMain.handle('fs:readDir', async (e, dir) => {
    const entries = await readdir(dir, { withFileTypes: true })
    return entries
      .filter((d) => d.name !== '.git' && d.name !== '.DS_Store')
      .map((d) => ({ name: d.name, dir: d.isDirectory() }))
      .sort((a, b) => b.dir - a.dir || a.name.localeCompare(b.name))
  })
  ipcMain.handle('fs:readFile', (e, p) => readFile(p, 'utf8'))
  ipcMain.handle('fs:writeFile', (e, { path, content }) => writeFile(path, content))

  // ---- json store (workspaces, ui state) ----
  // store:get/set stay open pre-login for the lock screen, so keys are strictly
  // vetted: plain slugs only (no traversal) and never the files that hold
  // credentials (airgap-auth) or the egress allowlist (airgap).
  const storeDir = app.getPath('userData')
  const RESERVED_KEYS = new Set(['airgap', 'airgap-auth'])
  function vetKey(key) {
    if (typeof key !== 'string' || !/^[a-z0-9][a-z0-9-]*$/.test(key) || RESERVED_KEYS.has(key))
      throw new Error('Bad store key.')
    return key
  }
  async function readStore(key) {
    try {
      return JSON.parse(await readFile(join(storeDir, vetKey(key) + '.json'), 'utf8'))
    } catch {
      return null
    }
  }
  ipcMain.handle('store:get', (e, key) => readStore(key))
  ipcMain.handle('store:set', async (e, { key, value }) => {
    vetKey(key)
    await mkdir(storeDir, { recursive: true })
    await writeFile(join(storeDir, key + '.json'), JSON.stringify(value, null, 2))
  })

  // ---- git ----
  ipcMain.handle('git:info', async (e, dir) => {
    try {
      const branch = (await git(dir, ['rev-parse', '--abbrev-ref', 'HEAD'])).trim()
      let added = 0
      let modified = 0
      let deleted = 0
      for (const line of (await git(dir, ['status', '--porcelain'])).split('\n')) {
        if (!line) continue
        const x = line[0]
        const y = line[1]
        if (x === '?' || x === 'A') added++
        else if (x === 'D' || y === 'D') deleted++
        else modified++
      }
      let ahead = 0
      let behind = 0
      try {
        const ab = (await git(dir, ['rev-list', '--left-right', '--count', '@{u}...HEAD']))
          .trim()
          .split(/\s+/)
        behind = +ab[0] || 0
        ahead = +ab[1] || 0
      } catch {} // no upstream
      return { repo: true, branch, added, modified, deleted, ahead, behind }
    } catch {
      return { repo: false }
    }
  })
  ipcMain.handle('git:branches', async (e, dir) =>
    (await git(dir, ['branch', '--sort=-committerdate', '--format=%(refname:short)']))
      .split('\n')
      .filter(Boolean)
  )
  ipcMain.handle('git:checkout', async (e, { dir, branch, create }) => {
    try {
      await git(dir, create ? ['checkout', '-b', branch] : ['checkout', branch])
      return { ok: true }
    } catch (err) {
      return { ok: false, error: err.message }
    }
  })
  const LOG_SEP = '\x1f'
  ipcMain.handle('git:log', async (e, { dir, limit }) => {
    const out = await git(dir, [
      'log',
      `-${limit || 250}`,
      '--date=format-local:%Y-%m-%d %H:%M',
      `--pretty=format:%H${LOG_SEP}%h${LOG_SEP}%an${LOG_SEP}%ad${LOG_SEP}%D${LOG_SEP}%s`,
    ])
    return out
      .split('\n')
      .filter(Boolean)
      .map((l) => {
        const [hash, short, author, date, refs, subject] = l.split(LOG_SEP)
        return { hash, short, author, date, refs: refs ? refs.split(', ').filter(Boolean) : [], subject }
      })
  })
  ipcMain.handle('git:commit', async (e, { dir, hash }) => {
    const body = (await git(dir, ['show', '-s', '--format=%B', hash])).trim()
    let raw
    try {
      // vs first parent, so merge commits list files too
      raw = await git(dir, ['diff', '--name-status', '-M', `${hash}^`, hash])
    } catch {
      // root commit has no parent
      raw = await git(dir, ['diff-tree', '--no-commit-id', '--name-status', '-r', '-M', '--root', hash])
    }
    const files = raw
      .split('\n')
      .filter(Boolean)
      .map((l) => {
        const parts = l.split('\t')
        return { status: parts[0][0], path: parts[parts.length - 1] }
      })
    return { body, files }
  })
  ipcMain.handle('git:diff', async (e, { dir, hash, file }) => {
    try {
      return await git(dir, ['diff', `${hash}^`, hash, '--', file])
    } catch {
      return git(dir, ['show', '--format=', hash, '--', file])
    }
  })

  // ---- document conversion (docx/xlsx → sandboxed html) ----
  ipcMain.handle('doc:read', async (e, path) => {
    // Parsing is the dangerous part (mammoth/SheetJS CVE histories) — only
    // parse files inside the open workspace folders or a brain vault.
    const real = await confinedRealPath(path)
    if (!real) throw new Error('doc:read: path is outside the open workspace folders')
    path = real
    const ext = extname(path).toLowerCase()
    if (ext === '.docx') {
      const { value } = await mammoth.convertToHtml({ path })
      return { html: DOC_CSS + value }
    }
    if (ext === '.xlsx' || ext === '.xls') {
      const wb = XLSX.readFile(path)
      const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;')
      const parts = wb.SheetNames.map(
        (n) => `<h3>${esc(n)}</h3>` + XLSX.utils.sheet_to_html(wb.Sheets[n], { header: '', footer: '' })
      )
      return { html: DOC_CSS + parts.join('') }
    }
    throw new Error('No viewer for ' + ext)
  })
  ipcMain.handle('shell:openPath', (e, p) => shell.openPath(p))

  // ---- dialogs ----
  ipcMain.handle('dialog:pickFolder', async () => {
    const r = await dialog.showOpenDialog(win, { properties: ['openDirectory'] })
    return r.canceled ? null : r.filePaths[0]
  })
  ipcMain.handle('dialog:pickFile', async () => {
    const r = await dialog.showOpenDialog(win, { properties: ['openFile'] })
    return r.canceled ? null : r.filePaths[0]
  })

  // ---- agents ----
  ipcMain.handle('agents:list', async () => {
    const check = (name) =>
      new Promise((resolve) => {
        execFile(SHELL, ['-l', '-c', `command -v ${name}`], (err) =>
          resolve({ name, available: !err })
        )
      })
    return Promise.all(AGENTS.map(check))
  })

  // ---- chat (Claude API, streamed from main so the key never enters the renderer) ----
  // Resolved per send so a key added to the shell after boot is picked up on
  // the next message; the SDK client itself is built once (anthropic ??=).
  function chatProvider() {
    const reqKey = process.env.REQUESTY_API_KEY || resolveAgentSecrets().REQUESTY_API_KEY
    if (reqKey)
      return { opts: { apiKey: reqKey, baseURL: REQUESTY_BASE }, model: REQUESTY_MODEL, beta: false }
    return { opts: {}, model: ANTHROPIC_MODEL, beta: true }
  }

  ipcMain.handle('chat:send', async (e, { id, messages, brainWs }) => {
    try {
      const provider = chatProvider()
      anthropic ??= new Anthropic(provider.opts)
      // conductor system prompt (workspace tools) + the brain vault context
      let system = conductor.SYSTEM
      if (brainWs) system += await brain.contextFor(brainWs, messages[messages.length - 1]?.content || '')
      await conductor.runChat(anthropic, {
        id,
        model: provider.model,
        system,
        messages,
        // server-side fallback betas are Anthropic-only; routers 400 on them
        betas: provider.beta ? ['server-side-fallback-2026-07-01'] : undefined,
        fallbacks: provider.beta ? 'default' : undefined,
      })
    } catch (err) {
      const msg = err?.message || String(err)
      const authy = err?.status === 401 || /api.key|auth/i.test(msg)
      win?.webContents.send('chat:done', {
        id,
        error: authy
          ? 'No chat credentials found. Set REQUESTY_API_KEY (router) or ANTHROPIC_API_KEY (direct) in your shell and restart Tome.'
          : msg,
      })
    }
  })
})

app.on('window-all-closed', () => {
  for (const p of ptys.values()) p.kill()
  app.quit()
})
