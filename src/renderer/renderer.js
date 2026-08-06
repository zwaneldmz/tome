import { createDockview } from 'dockview-core'
import 'dockview-core/dist/styles/dockview.css'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import { basicSetup } from 'codemirror'
import { EditorView, keymap } from '@codemirror/view'
import { LanguageDescription } from '@codemirror/language'
import { languages } from '@codemirror/language-data'
import { oneDark } from '@codemirror/theme-one-dark'
import './style.css'

const tome = window.tome
let seq = 0

// ---------- workspaces ----------
// state: { workspaces: [{ name, folders: [] }], active: index }
let ws = { workspaces: [], active: -1 }
let activeRoot = null // folder whose git repo the branch widget follows

const activeWorkspace = () => ws.workspaces[ws.active] || null
const saveWs = () => tome.store.set('workspaces', ws)
const paneCwd = () => activeRoot || activeWorkspace()?.folders[0] || tome.home

// ---------- toasts ----------
const toasts = document.getElementById('toasts')
function toast(msg, kind = 'err') {
  const t = document.createElement('div')
  t.className = 'toast ' + kind
  t.textContent = msg
  toasts.appendChild(t)
  setTimeout(() => t.classList.add('out'), 4200)
  setTimeout(() => t.remove(), 4800)
}

// ---------- pty / chat fan-out ----------
const terms = new Map()
tome.pty.onData(({ id, data }) => terms.get(id)?.write(data))
tome.pty.onExit(({ id, exitCode }) =>
  terms.get(id)?.write(`\r\n\x1b[2m[process exited ${exitCode}]\x1b[0m\r\n`)
)
const chats = new Map()
tome.chat.onDelta(({ id, text }) => chats.get(id)?.appendDelta(text))
tome.chat.onDone(({ id, error }) => chats.get(id)?.finish(error))

// ---------- air gap state ----------
let airgapDefault = true // spawn agents air-gapped (persisted)
let agState = { panes: {}, defaultMinutes: 15, auth: { configured: false, totp: false } }
const strips = new Map() // paneId -> strip element
const blockedThrottle = new Map()

function stripRender(paneId) {
  const strip = strips.get(paneId)
  if (!strip) return
  const st = agState.panes[paneId]
  const label = strip.querySelector('.ag-label')
  if (!st || st.mode === 'providers') {
    strip.classList.remove('open')
    label.textContent = '⛨ providers only — click to free'
  } else {
    strip.classList.add('open')
    const left = Math.max(0, st.expiresAt - Date.now())
    const m = Math.floor(left / 60000)
    const s = String(Math.floor((left % 60000) / 1000)).padStart(2, '0')
    label.textContent = `⛉ open internet · relocks in ${m}:${s} — click to relock`
  }
}
setInterval(() => {
  for (const id of strips.keys()) {
    if (agState.panes[id]?.mode === 'open') stripRender(id)
  }
}, 1000)

tome.airgap.onState((s) => {
  agState = { ...agState, ...s }
  for (const id of strips.keys()) stripRender(id)
})
tome.airgap.onBlocked(({ paneId, host }) => {
  const key = paneId + host
  if (Date.now() - (blockedThrottle.get(key) || 0) < 5000) return
  blockedThrottle.set(key, Date.now())
  const strip = strips.get(paneId)
  if (strip) {
    const f = strip.querySelector('.ag-flash')
    f.textContent = `✕ ${host}`
    f.classList.remove('show')
    void f.offsetWidth
    f.classList.add('show')
  }
  toast(`airgap blocked: ${host}`, 'err')
})

// ---------- panels ----------
class TerminalPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-terminal'
  }
  init({ params, api }) {
    this.ptyId = params.ptyId
    let termHost = this.element
    if (params.airgap) {
      this.element.classList.add('airgapped')
      const strip = document.createElement('div')
      strip.className = 'airgap-strip'
      const label = document.createElement('span')
      label.className = 'ag-label'
      const flash = document.createElement('span')
      flash.className = 'ag-flash'
      strip.append(label, flash)
      strip.addEventListener('click', () => airgapModal(this.ptyId))
      termHost = document.createElement('div')
      termHost.className = 'termbox'
      this.element.append(strip, termHost)
      strips.set(this.ptyId, strip)
      stripRender(this.ptyId)
    }
    const term = new Terminal({
      fontSize: 12.5,
      fontFamily: "'MesloLGS NF', 'JetBrainsMono Nerd Font', ui-monospace, Menlo, monospace",
      cursorBlink: true,
      theme: {
        background: '#060609',
        foreground: '#c9d4e3',
        cursor: '#ff2ea6',
        cursorAccent: '#060609',
        selectionBackground: 'rgba(0,229,255,0.22)',
        black: '#11131c',
        red: '#ff3b5c',
        green: '#3dff9e',
        yellow: '#ffd23e',
        blue: '#00a6ff',
        magenta: '#ff2ea6',
        cyan: '#00e5ff',
        white: '#c9d4e3',
        brightBlack: '#566179',
        brightRed: '#ff6b84',
        brightGreen: '#7dffbe',
        brightYellow: '#ffe37e',
        brightBlue: '#57c4ff',
        brightMagenta: '#ff7ec9',
        brightCyan: '#7ef2ff',
        brightWhite: '#eef4fb',
      },
    })
    const fit = new FitAddon()
    term.loadAddon(fit)
    term.open(termHost)
    terms.set(this.ptyId, term)
    tome.pty.create({
      id: this.ptyId,
      kind: params.kind,
      cwd: params.cwd,
      airgap: params.airgap,
    })
    term.onData((d) => tome.pty.write(this.ptyId, d))
    term.onResize(({ cols, rows }) => tome.pty.resize(this.ptyId, cols, rows))
    const refit = () => {
      try {
        fit.fit()
      } catch {}
    }
    api.onDidDimensionsChange(refit)
    api.onDidActiveChange(({ isActive }) => isActive && setTimeout(() => term.focus(), 0))
    requestAnimationFrame(refit)
    this.term = term
  }
  dispose() {
    tome.pty.kill(this.ptyId)
    terms.delete(this.ptyId)
    strips.delete(this.ptyId)
    this.term?.dispose()
  }
}

class EditorPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-editor'
  }
  async init({ params, api }) {
    const path = params.path
    const name = path.split('/').pop()
    let text = ''
    try {
      text = await tome.fs.readFile(path)
    } catch (err) {
      this.element.textContent = `Could not read ${path}: ${err.message}`
      return
    }
    const lang = LanguageDescription.matchFilename(languages, name)
    const langExt = lang ? await lang.load() : []
    const save = (view) => {
      tome.fs.writeFile(path, view.state.doc.toString()).then(() => api.setTitle(name))
      return true
    }
    this.view = new EditorView({
      doc: text,
      parent: this.element,
      extensions: [
        basicSetup,
        oneDark,
        langExt,
        keymap.of([{ key: 'Mod-s', run: save }]),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) api.setTitle('● ' + name)
        }),
      ],
    })
  }
  dispose() {
    this.view?.destroy()
  }
}

// pdf (Chromium's viewer), images, docx/xlsx (converted in main), binary fallback
class DocPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-doc'
  }
  async init({ params }) {
    const { mode, path } = params
    const url = 'tome://local/?p=' + encodeURIComponent(path)
    if (mode === 'pdf') {
      const f = document.createElement('iframe')
      f.className = 'doc-frame'
      f.src = url
      this.element.appendChild(f)
    } else if (mode === 'img') {
      const wrap = document.createElement('div')
      wrap.className = 'doc-img'
      const img = document.createElement('img')
      img.src = url
      wrap.appendChild(img)
      this.element.appendChild(wrap)
    } else if (mode === 'doc') {
      try {
        const { html } = await tome.doc.read(path)
        const f = document.createElement('iframe')
        f.className = 'doc-frame'
        f.sandbox = '' // converted content: no scripts, no navigation
        f.srcdoc = html
        this.element.appendChild(f)
      } catch (err) {
        this.fallback(path, err.message)
      }
    } else {
      this.fallback(path, 'No built-in viewer for this file type.')
    }
  }
  fallback(path, why) {
    const box = document.createElement('div')
    box.className = 'doc-fallback'
    const p = document.createElement('p')
    p.textContent = why
    const b = document.createElement('button')
    b.textContent = 'Open in default app'
    b.addEventListener('click', () => tome.openPath(path))
    box.append(p, b)
    this.element.appendChild(box)
  }
}

class ChatPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-chat'
    this.element.innerHTML = `
      <div class="chat-log"></div>
      <form class="chat-form">
        <textarea rows="2" placeholder="Ask the assistant… (Enter to send, Shift+Enter for newline)"></textarea>
        <button type="submit">Send</button>
      </form>`
  }
  init({ params }) {
    this.chatId = params.chatId
    this.history = []
    this.busy = false
    chats.set(this.chatId, this)
    this.log = this.element.querySelector('.chat-log')
    this.input = this.element.querySelector('textarea')
    this.element.querySelector('form').addEventListener('submit', (e) => {
      e.preventDefault()
      this.send()
    })
    this.input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault()
        this.send()
      }
    })
  }
  bubble(cls, text) {
    const div = document.createElement('div')
    div.className = 'msg ' + cls
    div.textContent = text
    this.log.appendChild(div)
    this.log.scrollTop = this.log.scrollHeight
    return div
  }
  send() {
    const text = this.input.value.trim()
    if (!text || this.busy) return
    this.busy = true
    this.input.value = ''
    this.bubble('me', text)
    this.history.push({ role: 'user', content: text })
    this.current = this.bubble('ai', '')
    this.currentText = ''
    tome.chat.send(this.chatId, this.history)
  }
  appendDelta(text) {
    this.currentText += text
    if (this.current) {
      this.current.textContent = this.currentText
      this.log.scrollTop = this.log.scrollHeight
    }
  }
  finish(error) {
    this.busy = false
    if (error) {
      if (this.current && !this.currentText) this.current.remove()
      this.bubble('err', error)
      this.history.pop()
    } else {
      this.history.push({ role: 'assistant', content: this.currentText })
    }
    this.current = null
  }
  dispose() {
    chats.delete(this.chatId)
  }
}

// ---------- dockview ----------
class Watermark {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'watermark'
    this.element.textContent = '＋ open a pane — agents · terminal · editor · chat'
  }
  init() {}
}

const dock = createDockview(document.getElementById('dock'), {
  theme: { name: 'tome', className: 'dockview-theme-tome' },
  createWatermarkComponent: () => new Watermark(),
  createComponent: (opts) => {
    switch (opts.name) {
      case 'editor':
        return new EditorPanel()
      case 'chat':
        return new ChatPanel()
      case 'doc':
        return new DocPanel()
      default:
        return new TerminalPanel()
    }
  },
})
window.addEventListener('resize', () =>
  dock.layout(dock.element.parentElement.clientWidth, dock.element.parentElement.clientHeight)
)

function place() {
  const n = dock.panels.length
  if (n === 0) return undefined
  return { referencePanel: dock.panels[n - 1], direction: n % 2 ? 'right' : 'below' }
}

function addTerminal(kind) {
  const id = `pty-${++seq}`
  const cwd = paneCwd()
  const name = cwd.split('/').pop() || cwd
  const isAgent = kind !== 'terminal'
  const gapped = isAgent && airgapDefault
  dock.addPanel({
    id,
    component: 'terminal',
    title: isAgent ? `${gapped ? '⛨ ' : ''}${kind} — ${name}` : `zsh — ${name}`,
    position: place(),
    params: { ptyId: id, kind, cwd, airgap: gapped },
  })
}

function addChat() {
  const id = `chat-${++seq}`
  dock.addPanel({
    id,
    component: 'chat',
    title: 'assistant',
    position: place(),
    params: { chatId: id },
  })
}

const IMG_EXT = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'ico'])
const CONV_EXT = new Set(['docx', 'xlsx', 'xls'])

async function openFile(path) {
  const id = `file:${path}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  const name = path.split('/').pop()
  const ext = (name.includes('.') ? name.split('.').pop() : '').toLowerCase()
  const docPanel = (mode) =>
    dock.addPanel({ id, component: 'doc', title: name, position: place(), params: { mode, path } })

  if (ext === 'pdf') return docPanel('pdf')
  if (IMG_EXT.has(ext)) return docPanel('img')
  if (CONV_EXT.has(ext)) return docPanel('doc')

  // text vs binary: sniff the decoded content
  try {
    const text = await tome.fs.readFile(path)
    if (text.slice(0, 8000).includes('�') || text.includes('\u0000')) return docPanel('binary')
  } catch {
    return docPanel('binary')
  }
  dock.addPanel({ id, component: 'editor', title: name, position: place(), params: { path } })
}

// ---------- menus (shared behaviour) ----------
const allMenus = []
function closeMenus(except) {
  for (const m of allMenus) if (m !== except) m.classList.add('hidden')
}
document.addEventListener('click', () => closeMenus())
function wireMenu(btnId, menuId, onOpen) {
  const btn = document.getElementById(btnId)
  const menu = document.getElementById(menuId)
  allMenus.push(menu)
  btn.addEventListener('click', async (e) => {
    e.stopPropagation()
    const willOpen = menu.classList.contains('hidden')
    closeMenus()
    if (willOpen) {
      if (onOpen) await onOpen(menu)
      menu.classList.remove('hidden')
    }
  })
  menu.addEventListener('click', (e) => e.stopPropagation())
  return menu
}
function menuItem(menu, { label, hint = '', onClick, disabled = false, active = false }) {
  const b = document.createElement('button')
  b.setAttribute('role', 'menuitem')
  b.disabled = disabled
  if (active) b.classList.add('active')
  const l = document.createElement('span')
  l.textContent = label
  const h = document.createElement('span')
  h.className = 'hint'
  h.textContent = hint
  b.append(l, h)
  if (onClick) {
    b.addEventListener('click', () => {
      closeMenus()
      onClick()
    })
  }
  menu.appendChild(b)
  return b
}
function menuInput(menu, placeholder, submitLabel, onSubmit) {
  const row = document.createElement('div')
  row.className = 'menu-input'
  const input = document.createElement('input')
  input.placeholder = placeholder
  const go = document.createElement('button')
  go.textContent = submitLabel
  const submit = () => {
    const v = input.value.trim()
    if (!v) return
    closeMenus()
    onSubmit(v)
  }
  go.addEventListener('click', submit)
  input.addEventListener('keydown', (e) => e.key === 'Enter' && submit())
  row.append(input, go)
  menu.appendChild(row)
}
const menuRule = (menu) => menu.appendChild(document.createElement('hr'))
const menuLabel = (menu, text) => {
  const d = document.createElement('div')
  d.className = 'menu-label'
  d.textContent = text
  menu.appendChild(d)
}

// ---------- workspace UI ----------
const wsName = document.getElementById('ws-name')

function renderWsChip() {
  wsName.textContent = activeWorkspace()?.name || 'no workspace'
  document.title = activeWorkspace() ? `tome — ${activeWorkspace().name}` : 'tome'
}

async function addFolderToActive() {
  const w = activeWorkspace()
  if (!w) return
  const dir = await tome.pickFolder()
  if (!dir || w.folders.includes(dir)) return
  w.folders.push(dir)
  activeRoot = dir
  saveWs()
  renderAll()
}

function createWorkspace(name) {
  ws.workspaces.push({ name, folders: [] })
  ws.active = ws.workspaces.length - 1
  activeRoot = null
  saveWs()
  renderAll()
  addFolderToActive()
}

function switchWorkspace(i) {
  ws.active = i
  activeRoot = activeWorkspace()?.folders[0] || null
  saveWs()
  renderAll()
}

wireMenu('ws-chip', 'ws-menu', (menu) => {
  menu.innerHTML = ''
  menuInput(menu, 'new workspace name…', 'Create', createWorkspace)
  if (ws.workspaces.length) {
    menuRule(menu)
    menuLabel(menu, 'Workspaces')
    ws.workspaces.forEach((w, i) =>
      menuItem(menu, {
        label: w.name,
        hint: `${w.folders.length} folder${w.folders.length === 1 ? '' : 's'}`,
        active: i === ws.active,
        onClick: () => switchWorkspace(i),
      })
    )
  }
  if (activeWorkspace()) {
    menuRule(menu)
    menuItem(menu, { label: 'Add folder to workspace…', onClick: addFolderToActive })
    menuItem(menu, {
      label: `Delete “${activeWorkspace().name}”`,
      onClick: () => {
        ws.workspaces.splice(ws.active, 1)
        ws.active = ws.workspaces.length ? 0 : -1
        activeRoot = activeWorkspace()?.folders[0] || null
        saveWs()
        renderAll()
      },
    })
  }
})

// ---------- git widget ----------
const gitChip = document.getElementById('git-chip')
const gitBranch = document.getElementById('git-branch')
const gitStats = document.getElementById('git-stats')

async function refreshGit() {
  if (!activeRoot) {
    gitChip.classList.add('hidden')
    gitStats.textContent = ''
    return
  }
  const info = await tome.git.info(activeRoot)
  if (!info.repo) {
    gitChip.classList.add('hidden')
    gitStats.textContent = ''
    return
  }
  gitChip.classList.remove('hidden')
  gitBranch.textContent = info.branch
  gitChip.title = `Git — ${activeRoot}`
  gitStats.textContent = ''
  const stat = (n, cls, sym) => {
    if (!n) return
    const s = document.createElement('span')
    s.className = cls
    s.textContent = sym + n
    gitStats.appendChild(s)
  }
  stat(info.added, 'g-add', '+')
  stat(info.modified, 'g-mod', '~')
  stat(info.deleted, 'g-del', '−')
  stat(info.ahead, 'g-sync', '↑')
  stat(info.behind, 'g-sync', '↓')
}
setInterval(refreshGit, 5000)

wireMenu('git-chip', 'git-menu', async (menu) => {
  menu.innerHTML = ''
  menuLabel(menu, activeRoot.split('/').pop())
  menuInput(menu, 'new branch from HEAD…', 'Create', async (name) => {
    const r = await tome.git.checkout(activeRoot, name, true)
    r.ok ? checkoutPulse(name) : toast(r.error)
  })
  menuRule(menu)
  let branches = []
  try {
    branches = await tome.git.branches(activeRoot)
  } catch {}
  const current = gitBranch.textContent
  for (const b of branches) {
    menuItem(menu, {
      label: b,
      active: b === current,
      hint: b === current ? 'current' : '',
      onClick: async () => {
        if (b === current) return
        const r = await tome.git.checkout(activeRoot, b, false)
        r.ok ? checkoutPulse(b) : toast(r.error)
      },
    })
  }
})

function checkoutPulse(branch) {
  gitBranch.textContent = branch
  gitChip.classList.remove('pulse')
  void gitChip.offsetWidth // restart animation
  gitChip.classList.add('pulse')
  toast(`Switched to ${branch}`, 'ok')
  refreshGit()
}

// ---------- ＋ menu ----------
wireMenu('btn-add', 'add-menu', async (menu) => {
  menu.innerHTML = ''
  const agents = await tome.agents.list()
  for (const a of agents) {
    menuItem(menu, {
      label: (airgapDefault ? '⛨ ' : '') + a.name,
      hint: a.available ? 'agent' : 'not installed',
      disabled: !a.available,
      onClick: () => addTerminal(a.name),
    })
  }
  menuItem(menu, {
    label: 'spawn agents air-gapped',
    hint: airgapDefault ? 'on' : 'off',
    active: airgapDefault,
    onClick: () => {
      airgapDefault = !airgapDefault
      tome.store.set('airgap-default', airgapDefault)
    },
  })
  menuRule(menu)
  menuItem(menu, { label: 'Assistant chat', hint: 'API', onClick: addChat })
  menuItem(menu, { label: 'Terminal', hint: 'zsh', onClick: () => addTerminal('terminal') })
  menuItem(menu, {
    label: 'Open file…',
    onClick: async () => {
      const p = await tome.pickFile()
      if (p) openFile(p)
    },
  })
})

// ---------- air gap modal ----------
function modalShell(title) {
  document.getElementById('ag-overlay')?.remove()
  const overlay = document.createElement('div')
  overlay.id = 'ag-overlay'
  const box = document.createElement('div')
  box.className = 'ag-box'
  const h = document.createElement('h3')
  h.textContent = title
  const body = document.createElement('div')
  body.className = 'ag-body'
  const err = document.createElement('div')
  err.className = 'ag-err'
  box.append(h, body, err)
  overlay.appendChild(box)
  overlay.addEventListener('click', (e) => e.target === overlay && overlay.remove())
  document.body.appendChild(overlay)
  return {
    body,
    err,
    close: () => overlay.remove(),
    input(placeholder, type = 'password') {
      const i = document.createElement('input')
      i.type = type
      i.placeholder = placeholder
      body.appendChild(i)
      return i
    },
    button(label, onClick, cls = 'primary') {
      const b = document.createElement('button')
      b.className = 'ag-btn ' + cls
      b.textContent = label
      b.addEventListener('click', onClick)
      body.appendChild(b)
      return b
    },
    note(text) {
      const p = document.createElement('p')
      p.className = 'ag-note'
      p.textContent = text
      body.appendChild(p)
      return p
    },
  }
}

async function airgapModal(paneId) {
  const state = await tome.airgap.state()
  agState = { ...agState, ...state }
  if (!state.auth.configured) return setupModal(paneId)
  const st = state.panes[paneId]

  if (st?.mode === 'open') {
    const m = modalShell('⛉ pane is on open internet')
    m.note('Relock now to return this pane to providers-only mode.')
    m.button('Relock now', async () => {
      await tome.airgap.relock(paneId)
      m.close()
      toast('Pane relocked', 'ok')
    })
    return
  }

  const m = modalShell('⛨ free this pane')
  m.note(`Grants this pane open internet for a limited time, then relocks itself.`)
  const pass = m.input('passphrase')
  let code = null
  if (state.auth.totp) code = m.input('2FA code (6 digits)', 'text')
  const mins = document.createElement('select')
  for (const v of [15, 30, 60]) {
    const o = document.createElement('option')
    o.value = v
    o.textContent = `${v} minutes`
    mins.appendChild(o)
  }
  m.body.appendChild(mins)
  if (!state.auth.totp) {
    m.note('Tip: enroll an authenticator app for 2FA below.')
    m.button(
      'Enable 2FA…',
      async () => {
        m.close()
        totpModal()
      },
      'ghost'
    )
  }
  const go = async () => {
    const r = await tome.airgap.unlock({
      paneId,
      passphrase: pass.value,
      code: code?.value,
      minutes: +mins.value,
    })
    if (r.ok) {
      m.close()
      toast(`Pane freed for ${mins.value} min`, 'ok')
    } else {
      m.err.textContent = r.error
    }
  }
  m.button('Unlock', go)
  pass.addEventListener('keydown', (e) => e.key === 'Enter' && go())
  setTimeout(() => pass.focus(), 0)
}

function setupModal(paneId) {
  const m = modalShell('⛨ set up air-gap unlock')
  m.note('Choose the passphrase that frees air-gapped panes. Stored as a salted hash.')
  const p1 = m.input('passphrase')
  const p2 = m.input('repeat passphrase')
  m.button('Set passphrase', async () => {
    if (p1.value.length < 4) return (m.err.textContent = 'Too short.')
    if (p1.value !== p2.value) return (m.err.textContent = 'Passphrases differ.')
    const r = await tome.airgap.setup(p1.value)
    if (!r.ok) return (m.err.textContent = r.error)
    m.close()
    toast('Passphrase set', 'ok')
    if (paneId) airgapModal(paneId)
  })
}

function totpModal() {
  const m = modalShell('⛉ enroll authenticator (TOTP)')
  m.note('Add this secret to your authenticator app, then confirm a code.')
  tome.airgap.enrollTotp().then(({ secret, uri }) => {
    const s = m.note(secret)
    s.classList.add('ag-secret')
    m.note(uri)
    const code = m.input('code from the app', 'text')
    m.button('Confirm', async () => {
      if (await tome.airgap.confirmTotp(code.value)) {
        m.close()
        toast('2FA enabled', 'ok')
      } else {
        m.err.textContent = 'Code did not match — try the next one.'
      }
    })
  })
}

// ---------- file tree ----------
const treeEl = document.getElementById('tree')
const JUNK_DIRS = new Set(['node_modules', 'out', 'dist', '.venv', '__pycache__', '.next', 'target'])

async function renderDir(dir, container, depth, rootPath) {
  let entries
  try {
    entries = await tome.fs.readDir(dir)
  } catch {
    return
  }
  for (const e of entries) {
    const full = `${dir}/${e.name}`
    const row = document.createElement('div')
    const junk = e.dir && JUNK_DIRS.has(e.name)
    row.className = 'entry ' + (e.dir ? 'dir' : 'file') + (junk ? ' junk' : '')
    row.style.paddingLeft = 10 + depth * 13 + 'px'
    row.textContent = (e.dir ? '▸ ' : '') + e.name
    container.appendChild(row)
    if (e.dir) {
      let open = false
      let kids = null
      row.addEventListener('click', () => {
        setActiveRoot(rootPath)
        open = !open
        row.textContent = (open ? '▾ ' : '▸ ') + e.name
        if (open && !kids) {
          kids = document.createElement('div')
          row.after(kids)
          renderDir(full, kids, depth + 1, rootPath)
        } else if (kids) {
          kids.style.display = open ? '' : 'none'
        }
      })
    } else {
      row.addEventListener('click', () => {
        setActiveRoot(rootPath)
        openFile(full)
      })
    }
  }
}

function setActiveRoot(rootPath) {
  if (activeRoot === rootPath) return
  activeRoot = rootPath
  for (const h of treeEl.querySelectorAll('.root-head')) {
    h.classList.toggle('active', h.dataset.path === rootPath)
  }
  refreshGit()
}

function emptyState(text, btnLabel, onClick) {
  const box = document.createElement('div')
  box.className = 'tree-empty'
  const p = document.createElement('p')
  p.textContent = text
  const b = document.createElement('button')
  b.textContent = btnLabel
  b.addEventListener('click', onClick)
  box.append(p, b)
  treeEl.appendChild(box)
}

function renderTree() {
  treeEl.innerHTML = ''
  const w = activeWorkspace()
  if (!w) {
    emptyState('A workspace groups the folders you are working across.', '▚ New workspace', () =>
      document.getElementById('ws-chip').click()
    )
    return
  }
  if (!w.folders.length) {
    emptyState(`“${w.name}” has no folders yet.`, '＋ Add folder', addFolderToActive)
    return
  }
  for (const folder of w.folders) {
    const head = document.createElement('div')
    head.className = 'root-head' + (folder === activeRoot ? ' active' : '')
    head.dataset.path = folder
    const label = document.createElement('span')
    label.textContent = folder.split('/').pop() || folder
    label.title = folder
    const rm = document.createElement('button')
    rm.className = 'root-rm'
    rm.title = 'Remove folder from workspace'
    rm.textContent = '×'
    rm.addEventListener('click', (e) => {
      e.stopPropagation()
      w.folders = w.folders.filter((f) => f !== folder)
      if (activeRoot === folder) activeRoot = w.folders[0] || null
      saveWs()
      renderAll()
    })
    head.append(label, rm)
    const kids = document.createElement('div')
    head.addEventListener('click', () => setActiveRoot(folder))
    treeEl.append(head, kids)
    renderDir(folder, kids, 0, folder)
  }
}

function renderAll() {
  renderWsChip()
  renderTree()
  refreshGit()
}

// ---------- boot ----------
;(async () => {
  const saved = await tome.store.get('workspaces')
  if (saved && Array.isArray(saved.workspaces)) {
    ws = saved
    if (ws.active >= ws.workspaces.length) ws.active = ws.workspaces.length - 1
  }
  const agPref = await tome.store.get('airgap-default')
  if (agPref !== null) airgapDefault = !!agPref
  tome.airgap.state().then((s) => (agState = { ...agState, ...s }))
  activeRoot = activeWorkspace()?.folders[0] || null
  renderAll()
  if (tome.shotMode && activeRoot) {
    // screenshot/demo mode: open a representative set of panes
    const id = `pty-${++seq}`
    dock.addPanel({
      id,
      component: 'terminal',
      title: `⛨ zsh — demo`,
      params: { ptyId: id, kind: 'terminal', cwd: activeRoot, airgap: true },
    })
    openFile(`${activeRoot}/package.json`)
    addChat()
  }
})()
