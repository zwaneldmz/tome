import { app, BrowserWindow, ipcMain, dialog, protocol, net, shell, systemPreferences, nativeTheme, Menu } from 'electron'
import { join, resolve, extname, sep } from 'node:path'
import { readdir, readFile, writeFile, mkdir, realpath } from 'node:fs/promises'
import { watch as watchFile } from 'node:fs'
import { homedir } from 'node:os'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)
import { pathToFileURL, fileURLToPath } from 'node:url'
import pty from 'node-pty'
import Anthropic from '@anthropic-ai/sdk'
import mammoth from 'mammoth'
import * as XLSX from 'xlsx'
import * as airgap from './airgap'
import * as authlock from './authlock'
import * as brain from './brain'
import * as conductor from './conductor'
import * as lsp from './lsp'
import { AGENTS } from '../shared/pane-kinds.js'

const ptys = new Map()

// ---- pty data coalescing ----
// node-pty emits one onData per read() chunk; forwarding each as its own IPC
// message floods the renderer under heavy output (build logs, cat of a big
// file). Buffer per pane and flush on a ~4 ms window or at 64 KB buffered,
// whichever comes first. xterm.write() accepts arbitrarily large batched
// strings, so the renderer side is unchanged.
const PTY_FLUSH_MS = 4
const PTY_FLUSH_BYTES = 64 * 1024
const ptyBuffers = new Map() // id -> { data, timer }
function queuePtyData(id, data) {
  let buf = ptyBuffers.get(id)
  if (!buf) {
    buf = { data: '', timer: null }
    ptyBuffers.set(id, buf)
  }
  buf.data += data
  if (buf.data.length >= PTY_FLUSH_BYTES) {
    flushPtyData(id)
  } else if (!buf.timer) {
    buf.timer = setTimeout(() => flushPtyData(id), PTY_FLUSH_MS)
  }
}
function flushPtyData(id) {
  const buf = ptyBuffers.get(id)
  if (!buf) return
  ptyBuffers.delete(id)
  if (buf.timer) clearTimeout(buf.timer)
  if (buf.data) win?.webContents.send('pty:data', { id, data: buf.data })
}

let win = null
let anthropic = null

// ---- file-open confinement ----
// Anything that opens or parses a file in main on behalf of the renderer —
// the conductor's open_file tool (model-driven!) and doc:read's
// mammoth/SheetJS parsers — stays inside the open workspace folders, or a
// brain vault. Otherwise a prompt-injected chat reply could make the main
// process parse ~/.ssh/… with libraries that have CVE histories.
// (fs:readFile/fs:writeFile/fs:mkdir/fs:createFile/shell:openPath stay
// unvetted by design: the editor and tree are user-driven; the trust
// boundary is documented in the review — renderer compromise ≈
// user-privileged file access.)
let openFolders = [] // absolute paths of open workspace folders
// "Never told" is not the same answer as "told, and it is empty". Both deny,
// but only one is a bug — a silent deny during startup looks exactly like a
// genuine confinement violation, which is how the missing boot-time ws:sync
// went unnoticed. Keeping the distinction explicit also means a future change
// cannot let an unpopulated list read as "no restrictions".
let foldersSynced = false
function setOpenFolders(list) {
  openFolders = Array.isArray(list) ? list.filter((f) => typeof f === 'string' && f) : []
  foldersSynced = true
}
// Why a confined path was refused, for error messages that can be acted on.
const confinementError = (what) =>
  foldersSynced
    ? `${what}: path is outside the open workspace folders`
    : `${what}: workspace folders have not been reported yet`
function isBrainPath(p) {
  return p.startsWith(brain.BRAINS_ROOT + sep)
}
function isConfinedPath(p) {
  if (!foldersSynced) return false // refuse until the renderer has reported
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

// One Tome at a time: a second launch focuses the existing window and exits.
const gotLock = app.requestSingleInstanceLock()
if (!gotLock) {
  app.quit()
} else {
  app.on('second-instance', () => {
    if (!win) return
    if (win.isMinimized()) win.restore()
    win.focus()
  })
}

// When launched from Finder/Spotlight the app gets launchd's bare PATH, and a
// non-interactive login shell never reads .zshrc — where PATH additions like
// ~/.local/bin (claude) usually live. Resolve the user's real interactive
// PATH once, with well-known agent bins as a fallback.
//
// ASYNC + CACHED: this used to be an execFileSync on the launch path, blocking
// the event loop for up to 8 s on a cold shell. Now it fires once, in the
// background, and every consumer (pty spawn, chat provider, agents:list)
// awaits the same in-flight promise. Only the first caller pays the shell-out.
let loginEnvPromise = null
function ensureLoginEnv() {
  if (loginEnvPromise) return loginEnvPromise
  loginEnvPromise = (async () => {
    // PATH and provider secrets come from the same login shell — spawn it
    // once, not twice.
    const [pathRes, envRes] = await Promise.allSettled([
      execFileAsync(SHELL, ['-ilc', 'echo -n "$PATH"'], { timeout: 8000, encoding: 'utf8' }),
      execFileAsync(SHELL, ['-ilc', 'env'], { timeout: 8000, encoding: 'utf8' }),
    ])
    if (pathRes.status === 'fulfilled') {
      const line = pathRes.value.stdout
        .split('\n')
        .map((l) => l.replace(/\x1b\[[0-9;]*[A-Za-z]/g, '').trim())
        .filter((l) => l.includes('/usr/bin'))
        .pop()
      if (line) process.env.PATH = line
    }
    // Well-known agent bins as a fallback, whether or not the shell answered.
    const cur = process.env.PATH ? process.env.PATH.split(':') : []
    const extras = [
      join(homedir(), '.local/bin'),
      join(homedir(), '.opencode/bin'),
      '/opt/homebrew/bin',
      '/usr/local/bin',
    ].filter((e) => !cur.includes(e))
    process.env.PATH = [...cur, ...extras].join(':')
    const secrets = {}
    if (envRes.status === 'fulfilled') {
      for (const line of envRes.value.stdout.split('\n')) {
        // Least-privilege forwarding: the old suffix match handed EVERY
        // *_API_KEY/*_KEY/*_TOKEN in the login shell (GITHUB_TOKEN, NPM_TOKEN,
        // DIGITALOCEAN_TOKEN, …) to every agent pane — air-gapped ones
        // included — when a pane needs exactly its provider's key.
        const i = line.indexOf('=')
        if (i < 1) continue
        const key = line.slice(0, i)
        const val = line.slice(i + 1)
        if (AGENT_SECRET_KEYS.has(key) && val) secrets[key] = val
      }
    }
    return { secrets }
  })()
  return loginEnvPromise
}
// Kick off at boot so the shell-out overlaps window creation instead of
// sitting in front of the first pane spawn.
ensureLoginEnv()

// Same .zshrc blind spot as PATH, for provider credentials: agent CLIs are
// spawned with `-l -c`, which never reads .zshrc, so keys exported there are
// invisible and the CLI fails auth. Read them from an interactive login shell
// once, and hand them only to agent panes — a plain terminal pane inherits
// nothing, and tome's own process env is left untouched.
// Forward only the credentials the supported providers actually consume.
// New provider? Add its key here.
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
// LAZY: resolved via the shared ensureLoginEnv() promise — off the boot path,
// cached, and paid for only by the first agent spawn / chat send.
async function resolveAgentSecrets() {
  const { secrets } = await ensureLoginEnv()
  return secrets
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

// ---- appearance ----
// The renderer owns the choice (system / light / dark) and reports the
// resolved mode here, because main needs it for two things it paints itself:
// window backgrounds and the CSS injected into converted-document iframes.
// nativeTheme cannot be read before the app is ready, so this starts on the
// old default and is corrected in createWindow().
let uiTheme = 'dark'
const WINDOW_BG = { dark: '#050508', light: '#eeeef2' }

// styles injected into sandboxed doc-viewer iframes (docx/xlsx conversions)
const docCss = () => {
  const d = uiTheme === 'dark'
  const bg = d ? '#0b0b11' : '#ffffff'
  const fg = d ? '#c9d4e3' : '#35353d'
  const head = d ? '#eef4fb' : '#101014'
  const link = d ? '#00e5ff' : '#0071e3'
  const line = d ? 'rgba(255,255,255,0.12)' : 'rgba(0,0,0,0.12)'
  const zebra = d ? '#151723' : '#f1f1f5'
  return (
    `<style>body{font:14px/1.65 -apple-system,BlinkMacSystemFont,system-ui,sans-serif;` +
    `background:${bg};color:${fg};padding:30px;max-width:840px;margin:0 auto}` +
    `h1,h2,h3{color:${head}}a{color:${link}}` +
    'table{border-collapse:collapse;font-size:12.5px;font-family:ui-monospace,Menlo,monospace}' +
    `td,th{border:1px solid ${line};padding:4px 10px;white-space:nowrap}th{background:${zebra}}` +
    'img{max-width:100%}</style>'
  )
}

// A popout window is only ever our own renderer's popout.html — same dev
// server in development, the bundled file next to index.html when packaged.
function isPopoutUrl(raw) {
  let u
  try {
    u = new URL(raw)
  } catch {
    return false
  }
  if (!u.pathname.endsWith('/popout.html')) return false
  if (process.env.ELECTRON_RENDERER_URL) {
    const base = new URL(process.env.ELECTRON_RENDERER_URL)
    return u.protocol === base.protocol && u.host === base.host
  }
  if (u.protocol !== 'file:') return false
  try {
    return resolve(fileURLToPath(u)) === resolve(join(__dirname, '../renderer/popout.html'))
  } catch {
    return false
  }
}

// ---- popped-out window close ----
// Closing a popout asks first (move its panes to the main window, or close
// them), so the OS close is vetoed until the renderer answers. dockview names
// each child window after its popout group, and window.open's frameName
// carries that name here — which is how the renderer maps a BrowserWindow
// back to the panes inside it.
const popoutApproved = new Set() // BrowserWindow ids cleared to close

function watchPopout(child, frameName) {
  child.on('close', (e) => {
    // never veto during a quit, or once the main window is gone — there would
    // be nothing left to show the prompt
    if (popoutApproved.has(child.id) || quitting) return
    if (!win || win.isDestroyed() || win.webContents.isDestroyed()) return
    e.preventDefault()
    win.webContents.send('popout:close-request', { id: child.id, name: frameName })
  })
  child.on('closed', () => popoutApproved.delete(child.id))
}

function createWindow() {
  uiTheme = nativeTheme.shouldUseDarkColors ? 'dark' : 'light'
  win = new BrowserWindow({
    width: 1440,
    height: 900,
    minWidth: 800,
    minHeight: 500,
    titleBarStyle: 'hiddenInset',
    backgroundColor: WINDOW_BG[uiTheme],
    webPreferences: {
      preload: join(__dirname, '../preload/index.js'),
      sandbox: true,
    },
  })
  // ---- popped-out panes ----
  // dockview tears a pane group off into its own OS window with window.open()
  // on a same-origin popout.html and then moves the live DOM across. Only
  // that document is allowed through; anything else a renderer tries to open
  // is refused rather than silently becoming a chromeless window.
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (!isPopoutUrl(url)) return { action: 'deny' }
    return {
      action: 'allow',
      overrideBrowserWindowOptions: {
        minWidth: 320,
        minHeight: 200,
        // A real title bar, unlike the main window's hiddenInset. popout.html
        // has no topbar to inset the traffic lights or to offer as a drag
        // region, so hiding the bar left dockview's tab strip covering the
        // window buttons with no way to move the window. Keeping the bar also
        // leaves the whole tab strip free as a drop target for panes dragged
        // in from another window — a drag region there would swallow them.
        backgroundColor: WINDOW_BG[uiTheme],
        webPreferences: { preload: join(__dirname, '../preload/index.js'), sandbox: true },
      },
    }
  })
  win.webContents.on('did-create-window', (child, { frameName }) => watchPopout(child, frameName))
  if (process.env.ELECTRON_RENDERER_URL) {
    win.loadURL(process.env.ELECTRON_RENDERER_URL)
    if (process.env.TOME_DEVTOOLS) win.webContents.openDevTools({ mode: 'detach' })
    win.webContents.on('console-message', (event) => {
      console.log(`[renderer] ${event.message}`)
    })
  } else {
    win.loadFile(join(__dirname, '../renderer/index.html'))
  }
  if (!app.isPackaged && process.env.TOME_SHOT) {
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
    // Closing a window the user already asked to close is not privileged, and
    // gating it deadlocks: main vetoes the close until the renderer answers,
    // so a locked app would leave a popout window that cannot be closed.
    'popout:close',
  ])
  // TOME_SHOT is a dev/screenshot affordance — an env var that bypasses the
  // lock gate must never ship in packaged builds.
  const shotMode = !!process.env.TOME_SHOT && !app.isPackaged
  const isLockedNow = () =>
    authlock.authStatus().configured && !authlock.isUnlocked() && !shotMode
  const rawHandle = ipcMain.handle.bind(ipcMain)
  ipcMain.handle = (channel, fn) =>
    rawHandle(channel, (e, ...args) => {
      if (!OPEN_CHANNELS.has(channel) && isLockedNow()) throw new Error('Tome is locked.')
      return fn(e, ...args)
    })

  protocol.handle('tome', async (req) => {
    const p = decodeURIComponent(new URL(req.url).searchParams.get('p') || '')
    const deny = () => new Response(confinementError('tome'), { status: 403 })
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
  buildMenu()
  conductor.init({
    ptys,
    send: (channel, payload) => win?.webContents.send(channel, payload),
    canOpenFile: isConfinedPath,
  })
  // The renderer resolves 'system' itself and reports both the preference
  // (so native chrome can keep following the OS) and the resolved mode.
  ipcMain.on('theme:set', (e, msg) => {
    const pref = msg?.pref === 'light' || msg?.pref === 'dark' ? msg.pref : 'system'
    uiTheme = msg?.mode === 'dark' ? 'dark' : 'light'
    nativeTheme.themeSource = pref
    for (const w of BrowserWindow.getAllWindows()) w.setBackgroundColor(WINDOW_BG[uiTheme])
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
    // Await the login shell before spawning so the agent lands in the user's
    // real PATH (first spawn pays for the shell-out; later spawns get the cache).
    await ensureLoginEnv()
    const env = { ...process.env, TERM: 'xterm-256color', COLORTERM: 'truecolor' }
    if (isAgent) Object.assign(env, await resolveAgentSecrets())
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
      queuePtyData(id, data)
    })
    p.onExit(({ exitCode }) => {
      flushPtyData(id) // don't strand buffered output on exit
      ptys.delete(id)
      conductor.markExited(id)
      airgap.closePane(id)
      win?.webContents.send('pty:exit', { id, exitCode })
    })
  }
  ipcMain.on('pty:write', (e, { id, data }) => ptys.get(id)?.write(data))
  ipcMain.on('chat:abort', (e, id) => conductor.abortChat(id))
  ipcMain.on('pty:resize', (e, { id, cols, rows }) => ptys.get(id)?.resize(cols, rows))
  ipcMain.on('pty:kill', (e, { id }) => {
    flushPtyData(id)
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
  ipcMain.handle('fs:mkdir', (e, p) => mkdir(p, { recursive: true }))
  ipcMain.handle('fs:createFile', async (e, p) => {
    // 'wx' fails rather than clobbering an existing file
    await writeFile(p, '', { flag: 'wx' })
  })

  // ---- open-file watching ----
  // Editors ask to be told when a file changes underneath them. Refcounted,
  // because the same path can be open in more than one pane. Nothing here
  // tries to tell our own writes apart from someone else's — the renderer
  // compares content, which is the only check that cannot race.
  const watched = new Map() // path -> { watcher, count, timer }
  ipcMain.handle('fs:watch', (e, p) => {
    const entry = watched.get(p)
    if (entry) {
      entry.count++
      return true
    }
    let watcher
    try {
      watcher = watchFile(p, () => {
        const w = watched.get(p)
        if (!w) return
        // editors save in bursts and fs.watch is chatty; one event is enough
        clearTimeout(w.timer)
        w.timer = setTimeout(() => win?.webContents.send('fs:changed', p), 120)
      })
    } catch {
      return false // unwatchable (deleted, permissions) — not worth failing over
    }
    watcher.on('error', () => {})
    watched.set(p, { watcher, count: 1, timer: null })
    return true
  })
  // ---- language servers ----
  // Diagnostics are pushed by the server whenever it feels like it, so they
  // ride an event rather than a request/response.
  lsp.init({
    onDiagnostics: (path, diagnostics) => win?.webContents.send('lsp:diagnostics', { path, diagnostics }),
    onMissing: (cmd, langId) => win?.webContents.send('lsp:missing', { cmd, langId }),
  })
  ipcMain.on('lsp:didOpen', (e, { path, text }) => lsp.didOpen(path, text, openFolders))
  ipcMain.on('lsp:didChange', (e, { path, text }) => lsp.didChange(path, text, openFolders))
  ipcMain.on('lsp:didClose', (e, path) => lsp.didClose(path, openFolders))
  ipcMain.handle('lsp:hover', (e, { path, line, character }) =>
    lsp.hover(path, line, character, openFolders)
  )
  ipcMain.handle('lsp:definition', (e, { path, line, character }) =>
    lsp.definition(path, line, character, openFolders)
  )

  // ---- format on save ----
  // Prettier runs in main: it is a node module, and the renderer is sandboxed.
  // Its own config wins, so a project's .prettierrc is respected. A file type
  // Prettier has no parser for resolves to null and the save proceeds
  // unformatted rather than failing.
  ipcMain.handle('fmt:format', async (e, { path, content }) => {
    try {
      const prettier = await import('prettier')
      const info = await prettier.getFileInfo(path, { resolveConfig: true })
      if (!info.inferredParser) return null
      const config = (await prettier.resolveConfig(path)) || {}
      return await prettier.format(content, { ...config, filepath: path })
    } catch (err) {
      // a syntax error mid-edit is normal — report it, never block the save
      return { error: String(err.message || err).split('\n')[0] }
    }
  })

  ipcMain.handle('fs:unwatch', (e, p) => {
    const entry = watched.get(p)
    if (!entry) return
    if (--entry.count > 0) return
    clearTimeout(entry.timer)
    entry.watcher.close()
    watched.delete(p)
  })

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
    if (!real) throw new Error(confinementError('doc:read'))
    path = real
    const ext = extname(path).toLowerCase()
    if (ext === '.docx') {
      const { value } = await mammoth.convertToHtml({ path })
      return { html: docCss() + value }
    }
    if (ext === '.xlsx' || ext === '.xls') {
      const wb = XLSX.readFile(path)
      const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;')
      const parts = wb.SheetNames.map(
        (n) => `<h3>${esc(n)}</h3>` + XLSX.utils.sheet_to_html(wb.Sheets[n], { header: '', footer: '' })
      )
      return { html: docCss() + parts.join('') }
    }
    throw new Error('No viewer for ' + ext)
  })
  ipcMain.handle('shell:openPath', (e, p) => shell.openPath(p))

  // The renderer answered a popout close prompt: let that window go. Not
  // calling this is how "cancel" works — the window simply stays open.
  ipcMain.handle('popout:close', (e, id) => {
    const child = BrowserWindow.fromId(id)
    if (!child || child.isDestroyed()) return
    popoutApproved.add(id)
    child.close()
  })

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
    // Wait for the login-shell PATH (async, cached) so Finder launches still
    // find agents installed in ~/.local/bin etc.
    await ensureLoginEnv()
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
  async function chatProvider() {
    const secrets = await resolveAgentSecrets()
    const reqKey = process.env.REQUESTY_API_KEY || secrets.REQUESTY_API_KEY
    if (reqKey)
      return { opts: { apiKey: reqKey, baseURL: REQUESTY_BASE }, model: REQUESTY_MODEL, beta: false }
    return { opts: {}, model: ANTHROPIC_MODEL, beta: true }
  }

  ipcMain.handle('chat:send', async (e, { id, messages, brainWs }) => {
    try {
      const provider = await chatProvider()
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
        aborted: false,
        error: authy
          ? 'No chat credentials found. Set REQUESTY_API_KEY (router) or ANTHROPIC_API_KEY (direct) in your shell and restart Tome.'
          : msg,
      })
    }
  })
})

// ---- native menu bar ----
// mac-first: standard roles cover the free/correct items; every custom Tome
// action goes over ONE channel ('menu:action') with an id the renderer's
// menu-bridge switches on. The renderer owns the features — the menu is
// just a discoverable shortcut surface in front of the same code paths the
// topbar buttons and ⌘ keys already use. Agent kinds are sent through
// blindly: the renderer checks tome.agents.list() and toasts when the CLI
// isn't installed.
function buildMenu() {
  if (process.platform !== 'darwin') return
  const send = (action) => () => win?.webContents.send('menu:action', action)
  const template = [
    {
      label: app.name,
      submenu: [
        { role: 'about' },
        { type: 'separator' },
        {
          label: 'Preferences…',
          accelerator: 'CmdOrCtrl+,',
          click: send({ id: 'open-preferences' }),
        },
        { type: 'separator' },
        { role: 'hide' },
        { role: 'hideOthers' },
        { role: 'unhide' },
        { type: 'separator' },
        { role: 'quit' },
      ],
    },
    {
      label: 'File',
      submenu: [
        {
          label: 'Save',
          accelerator: 'CmdOrCtrl+S',
          click: send({ id: 'save' }),
        },
        {
          label: 'Save All',
          accelerator: 'CmdOrCtrl+Alt+S',
          click: send({ id: 'save-all' }),
        },
      ],
    },
    {
      label: 'Edit',
      submenu: [
        { role: 'undo' },
        { role: 'redo' },
        { type: 'separator' },
        { role: 'cut' },
        { role: 'copy' },
        { role: 'paste' },
        { role: 'selectAll' },
      ],
    },
    {
      label: 'View',
      submenu: [
        {
          label: 'Toggle Sidebar',
          accelerator: 'CmdOrCtrl+B',
          click: send({ id: 'toggle-sidebar' }),
        },
        {
          label: 'Appearance',
          submenu: [
            {
              label: 'Light',
              type: 'radio',
              checked: uiTheme === 'light',
              click: send({ id: 'set-theme', pref: 'light' }),
            },
            {
              label: 'Dark',
              type: 'radio',
              checked: uiTheme === 'dark',
              click: send({ id: 'set-theme', pref: 'dark' }),
            },
            {
              label: 'Match System',
              type: 'radio',
              click: send({ id: 'set-theme', pref: 'system' }),
            },
          ],
        },
        { type: 'separator' },
        {
          label: 'Quick Open',
          accelerator: 'CmdOrCtrl+P',
          click: send({ id: 'quick-open' }),
        },
        {
          label: 'Keyboard Shortcuts',
          accelerator: 'CmdOrCtrl+/',
          click: send({ id: 'shortcuts' }),
        },
        { type: 'separator' },
        ...(app.isPackaged
          ? []
          : [{ role: 'reload' }, { role: 'toggleDevTools' }, { type: 'separator' }]),
        { role: 'resetZoom' },
        { role: 'zoomIn' },
        { role: 'zoomOut' },
        { type: 'separator' },
        { role: 'togglefullscreen' },
      ],
    },
    {
      label: 'Pane',
      submenu: [
        {
          label: 'New Pane',
          submenu: [
            {
              label: 'Terminal',
              click: send({ id: 'new-pane', kind: 'terminal' }),
            },
            {
              label: 'Assistant Chat',
              click: send({ id: 'new-pane', kind: 'chat' }),
            },
            {
              label: 'Brain',
              click: send({ id: 'new-pane', kind: 'brain' }),
            },
            { type: 'separator' },
            ...AGENTS.map((name) => ({
              label: name,
              click: send({ id: 'new-pane', kind: name }),
            })),
          ],
        },
        { type: 'separator' },
        {
          label: 'Close Pane',
          accelerator: 'CmdOrCtrl+W',
          click: send({ id: 'close-pane' }),
        },
      ],
    },
    {
      label: 'Window',
      submenu: [
        { role: 'minimize' },
        { role: 'zoom' },
        { role: 'close' },
        { type: 'separator' },
        { role: 'front' },
      ],
    },
  ]
  Menu.setApplicationMenu(Menu.buildFromTemplate(template))
}

// ---- quit handshake ----
// Give the renderer one beat to persist the dockview layout before the
// process goes away. Before-quit fires on every quit path (Cmd+Q, menu,
// window-all-closed below), so this is the single place to hook.
let quitting = false
app.on('before-quit', (e) => {
  if (quitting || !win || win.isDestroyed() || !win.webContents || win.webContents.isDestroyed())
    return
  e.preventDefault()
  quitting = true // re-entry from quitNow() must not loop
  win.webContents.send('app:before-quit')
  setTimeout(() => app.quit(), 1500) // hard cap: never hang the quit
})
ipcMain.on('app:quit-ready', () => {
  if (quitting) app.quit()
})

app.on('window-all-closed', () => {
  lsp.shutdownAll()
  for (const p of ptys.values()) p.kill()
  app.quit()
})
