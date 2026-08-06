import { app, BrowserWindow, ipcMain, dialog, protocol, net, shell } from 'electron'
import { join, extname } from 'node:path'
import { readdir, readFile, writeFile, mkdir } from 'node:fs/promises'
import { homedir } from 'node:os'
import { execFile } from 'node:child_process'
import { pathToFileURL } from 'node:url'
import pty from 'node-pty'
import Anthropic from '@anthropic-ai/sdk'
import mammoth from 'mammoth'
import * as XLSX from 'xlsx'
import * as airgap from './airgap'
import * as authlock from './authlock'

const ptys = new Map()
let win = null
let anthropic = null

const SHELL = process.env.SHELL || '/bin/zsh'
const AGENTS = ['claude', 'opencode', 'pi']
const CHAT_MODEL = process.env.TOME_CHAT_MODEL || 'claude-opus-5'
const CHAT_SYSTEM =
  'You are the assistant pane inside Tome, a desktop coding harness. ' +
  'Keep responses focused, brief, and concise. Plain text only — no markdown tables.'

// local-file protocol so panes can embed PDFs/images without file:// cross-origin blocks
protocol.registerSchemesAsPrivileged([
  { scheme: 'tome', privileges: { standard: true, secure: true, supportFetchAPI: true, stream: true } },
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
      sandbox: false,
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
  protocol.handle('tome', (req) => {
    const p = decodeURIComponent(new URL(req.url).searchParams.get('p') || '')
    return net.fetch(pathToFileURL(p).toString())
  })

  const userData = app.getPath('userData')
  await airgap.loadAllowlist(userData)
  await authlock.initAuth(userData)
  airgap.setEventSink((type, payload) => win?.webContents.send('airgap:' + type, payload))

  createWindow()

  // ---- pty ----
  ipcMain.handle('pty:create', async (e, { id, cmd, args, cwd, airgap: gapped }) => {
    let spawnCmd = cmd || SHELL
    let spawnArgs = args || ['-l']
    const env = { ...process.env, TERM: 'xterm-256color', COLORTERM: 'truecolor' }
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
    p.onData((data) => win?.webContents.send('pty:data', { id, data }))
    p.onExit(({ exitCode }) => {
      ptys.delete(id)
      airgap.closePane(id)
      win?.webContents.send('pty:exit', { id, exitCode })
    })
  })
  ipcMain.on('pty:write', (e, { id, data }) => ptys.get(id)?.write(data))
  ipcMain.on('pty:resize', (e, { id, cols, rows }) => ptys.get(id)?.resize(cols, rows))
  ipcMain.on('pty:kill', (e, { id }) => {
    ptys.get(id)?.kill()
    ptys.delete(id)
    airgap.closePane(id)
  })

  // ---- air gap ----
  ipcMain.handle('airgap:state', () => ({ ...airgap.getState(), auth: authlock.authStatus() }))
  ipcMain.handle('airgap:unlock', (e, { paneId, passphrase, code, minutes }) => {
    if (!authlock.verifyPassphrase(passphrase)) return { ok: false, error: 'Wrong passphrase.' }
    if (authlock.totpActive() && !authlock.verifyTotp(code))
      return { ok: false, error: 'Wrong 2FA code.' }
    airgap.unlockPane(paneId, minutes)
    return { ok: true }
  })
  ipcMain.handle('airgap:relock', (e, paneId) => airgap.relockPane(paneId))
  ipcMain.handle('airgap:setup', async (e, { passphrase }) => {
    if (authlock.authStatus().configured) return { ok: false, error: 'Already configured.' }
    await authlock.setPassphrase(passphrase)
    return { ok: true }
  })
  ipcMain.handle('airgap:enrollTotp', () => authlock.enrollTotp())
  ipcMain.handle('airgap:confirmTotp', (e, { code }) => authlock.confirmTotp(code))

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
  const storeDir = app.getPath('userData')
  ipcMain.handle('store:get', async (e, key) => {
    try {
      return JSON.parse(await readFile(join(storeDir, key + '.json'), 'utf8'))
    } catch {
      return null
    }
  })
  ipcMain.handle('store:set', async (e, { key, value }) => {
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

  // ---- document conversion (docx/xlsx → sandboxed html) ----
  ipcMain.handle('doc:read', async (e, path) => {
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
  ipcMain.handle('chat:send', async (e, { id, messages }) => {
    try {
      anthropic ??= new Anthropic()
      const stream = anthropic.beta.messages.stream({
        model: CHAT_MODEL,
        max_tokens: 64000,
        system: CHAT_SYSTEM,
        messages,
        betas: ['server-side-fallback-2026-07-01'],
        fallbacks: 'default',
      })
      stream.on('text', (text) => win?.webContents.send('chat:delta', { id, text }))
      const final = await stream.finalMessage()
      if (final.stop_reason === 'refusal') {
        win?.webContents.send('chat:done', { id, error: 'Request declined by safety classifiers.' })
      } else {
        win?.webContents.send('chat:done', { id })
      }
    } catch (err) {
      const msg = err?.message || String(err)
      const authy = err?.status === 401 || /api.key|auth/i.test(msg)
      win?.webContents.send('chat:done', {
        id,
        error: authy
          ? 'No Claude API credentials found. Set ANTHROPIC_API_KEY in your shell (or run `ant auth login`) and restart Tome.'
          : msg,
      })
    }
  })
})

app.on('window-all-closed', () => {
  for (const p of ptys.values()) p.kill()
  app.quit()
})
