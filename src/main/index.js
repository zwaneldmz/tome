import { app, BrowserWindow, ipcMain, dialog } from 'electron'
import { join } from 'node:path'
import { readdir, readFile, writeFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { execFile } from 'node:child_process'
import pty from 'node-pty'
import Anthropic from '@anthropic-ai/sdk'

const ptys = new Map()
let win = null
let anthropic = null

const SHELL = process.env.SHELL || '/bin/zsh'
const AGENTS = ['claude', 'opencode', 'pi']
const CHAT_MODEL = process.env.TOME_CHAT_MODEL || 'claude-opus-5'
const CHAT_SYSTEM =
  'You are the assistant pane inside Tome, a desktop coding harness. ' +
  'Keep responses focused, brief, and concise. Plain text only — no markdown tables.'

function createWindow() {
  win = new BrowserWindow({
    width: 1440,
    height: 900,
    minWidth: 800,
    minHeight: 500,
    titleBarStyle: 'hiddenInset',
    backgroundColor: '#14161d',
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
}

app.whenReady().then(() => {
  createWindow()

  // ---- pty ----
  ipcMain.handle('pty:create', (e, { id, cmd, args, cwd }) => {
    const p = pty.spawn(cmd || SHELL, args || ['-l'], {
      name: 'xterm-256color',
      cols: 80,
      rows: 24,
      cwd: cwd || homedir(),
      env: { ...process.env, TERM: 'xterm-256color', COLORTERM: 'truecolor' },
    })
    ptys.set(id, p)
    p.onData((data) => win?.webContents.send('pty:data', { id, data }))
    p.onExit(({ exitCode }) => {
      ptys.delete(id)
      win?.webContents.send('pty:exit', { id, exitCode })
    })
  })
  ipcMain.on('pty:write', (e, { id, data }) => ptys.get(id)?.write(data))
  ipcMain.on('pty:resize', (e, { id, cols, rows }) => ptys.get(id)?.resize(cols, rows))
  ipcMain.on('pty:kill', (e, { id }) => {
    ptys.get(id)?.kill()
    ptys.delete(id)
  })

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
      // Zero-arg client: resolves ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN,
      // or an `ant auth login` profile.
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
