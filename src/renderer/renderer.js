import { createDockview } from 'dockview-core'
import 'dockview-core/dist/styles/dockview.css'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import { basicSetup } from 'codemirror'
import { Decoration, EditorView, MatchDecorator, ViewPlugin, keymap } from '@codemirror/view'
import { EditorState } from '@codemirror/state'
import { LanguageDescription } from '@codemirror/language'
import { languages } from '@codemirror/language-data'
import { oneDark } from '@codemirror/theme-one-dark'
import { HistoryPanel } from './history.js'
import { bootAuth } from './lock.js'
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
tome.chat.onTool(({ id, tool, hint }) => chats.get(id)?.toolNote(tool, hint))
const brains = new Map() // ws name -> BrainPanel instance
tome.brain.onChanged(({ ws: bws, index }) => brains.get(bws)?.onChanged(index))

// ---------- air gap state ----------
let airgapDefault = true // spawn agents air-gapped (persisted)
let conductorRun = false // assistant may press Enter in terminals (persisted)
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
      ws: params.ws,
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
        <button type="button" class="chat-brain-toggle" title="Inject workspace brain context">◈ brain</button>
        <textarea rows="2" placeholder="Ask the assistant… (Enter to send · Shift+Enter newline · dictate with the 🎤 key)"></textarea>
        <button type="button" class="chat-speak" title="Speak replies aloud">🔊</button>
        <button type="submit">Send</button>
      </form>`
  }
  init({ params }) {
    this.chatId = params.chatId
    this.history = []
    this.busy = false
    this.brainOn = false
    chats.set(this.chatId, this)
    this.log = this.element.querySelector('.chat-log')
    this.input = this.element.querySelector('textarea')
    this.brainToggle = this.element.querySelector('.chat-brain-toggle')
    this.brainToggle.addEventListener('click', () => {
      this.brainOn = !this.brainOn
      this.brainToggle.classList.toggle('active', this.brainOn)
    })
    this.speak = false
    this.speakBtn = this.element.querySelector('.chat-speak')
    this.speakBtn.addEventListener('click', () => {
      this.speak = !this.speak
      this.speakBtn.classList.toggle('active', this.speak)
      if (!this.speak) speechSynthesis.cancel()
    })
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
    this.segText = ''
    let brainWs
    if (this.brainOn) {
      const w = activeWorkspace()
      if (w) brainWs = w.name
      else toast('no workspace for brain context')
    }
    tome.chat.send(this.chatId, this.history, brainWs)
  }
  appendDelta(text) {
    this.currentText += text
    this.segText += text
    if (this.current) {
      this.current.textContent = this.segText
      this.log.scrollTop = this.log.scrollHeight
    }
  }
  // a conductor tool ran between text segments: chip it, start a fresh bubble
  toolNote(tool, hint) {
    if (this.current && !this.segText) this.current.remove()
    this.bubble('tool', `⚙ ${tool}${hint ? ' · ' + hint : ''}`)
    this.current = this.bubble('ai', '')
    this.segText = ''
  }
  finish(error) {
    this.busy = false
    if (this.current && !this.segText) this.current.remove()
    if (error) {
      this.bubble('err', error)
      this.history.pop()
    } else {
      this.history.push({ role: 'assistant', content: this.currentText })
      if (this.speak && this.currentText) {
        speechSynthesis.cancel()
        speechSynthesis.speak(new SpeechSynthesisUtterance(this.currentText.slice(0, 1500)))
      }
    }
    this.current = null
  }
  dispose() {
    chats.delete(this.chatId)
  }
}

// markdown language mode, loaded once and shared by every BrainPanel editor
let mdLangExtPromise = null
function markdownLangExt() {
  if (!mdLangExtPromise) {
    const lang = LanguageDescription.matchFilename(languages, 'x.md')
    mdLangExtPromise = lang ? lang.load() : Promise.resolve([])
  }
  return mdLangExtPromise
}

// [[wikilink]] highlighting — stateless, shared across every BrainPanel editor
const wikilinkMatcher = new MatchDecorator({
  regexp: /\[\[[^\]]+\]\]/g,
  decoration: Decoration.mark({ class: 'cm-wikilink' }),
})
const wikilinkDeco = ViewPlugin.fromClass(
  class {
    constructor(view) {
      this.deco = wikilinkMatcher.createDeco(view)
    }
    update(u) {
      this.deco = wikilinkMatcher.updateDeco(u, this.deco)
    }
  },
  { decorations: (v) => v.deco }
)
const WIKILINK_TARGET_RE = /\[\[([^\]|#]+)(?:[#|][^\]]*)?\]\]/g

function matchesBrainQuery(note, q) {
  return (
    note.name.toLowerCase().includes(q) ||
    note.tags.some((t) => t.toLowerCase().includes(q)) ||
    note.body.toLowerCase().includes(q)
  )
}

function stubNote(name) {
  const created = new Date().toISOString().slice(0, 10)
  return `---\ntags: []\ncreated: ${created}\nstatus: draft\n---\n\n# ${name}\n`
}

const cssVar = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim()

// deterministic 0..1 hash so a node's seed position is stable across panel
// opens (same name -> same seed) until the force sim nudges it
function graphHash(s) {
  let h = 5381
  for (let i = 0; i < s.length; i++) h = ((h * 33) ^ s.charCodeAt(i)) | 0
  return (h >>> 0) / 4294967295
}

class BrainPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-brain'
    this.element.innerHTML = `
      <div class="brain-side">
        <input class="brain-search" placeholder="search name, tags, body…" />
        <div class="brain-list"></div>
        <div class="brain-foot">
          <button class="brain-new">＋ note</button>
          <button class="brain-graph">◉ graph</button>
        </div>
      </div>
      <div class="brain-main">
        <div class="brain-head">
          <span class="brain-title"></span>
          <button class="brain-promote hidden">↑ promote</button>
          <button class="brain-del" title="Delete note">✕</button>
        </div>
        <div class="brain-editor"></div>
        <canvas class="brain-graph"></canvas>
        <div class="brain-back"></div>
      </div>`
  }
  async init({ params, api }) {
    this.ws = params.ws
    this.openRel = null
    this.dirty = false
    this.loading = false
    this.graphMode = false
    this.graphPos = new Map() // name(lower) -> {x,y,vx,vy,name,rel,phantom,backlinks}
    this.graphEdges = []
    this.graphRunning = false
    this.dragKey = null
    this.hoverKey = null
    brains.set(this.ws, this)

    this.searchInput = this.element.querySelector('.brain-search')
    this.listEl = this.element.querySelector('.brain-list')
    this.headTitle = this.element.querySelector('.brain-title')
    this.promoteBtn = this.element.querySelector('.brain-promote')
    this.delBtn = this.element.querySelector('.brain-del')
    this.editorHost = this.element.querySelector('.brain-editor')
    this.backEl = this.element.querySelector('.brain-back')
    this.graphCanvas = this.element.querySelector('canvas.brain-graph')
    this.graphCtx = this.graphCanvas.getContext('2d')
    this.graphColors = { void: cssVar('--void'), muted: cssVar('--muted'), bright: cssVar('--bright'), mono: cssVar('--mono') }

    this.searchInput.addEventListener('input', () => this.renderList(this.index))
    this.element.querySelector('.brain-new').addEventListener('click', () => this.newNoteModal())
    this.element
      .querySelector('button.brain-graph')
      .addEventListener('click', () => this.setGraphMode(!this.graphMode))
    this.promoteBtn.addEventListener('click', () => this.promoteModal())
    this.delBtn.addEventListener('click', () => this.deleteOpen())
    this.graphCanvas.addEventListener('mousedown', (e) => this.graphMouseDown(e))
    this.graphCanvas.addEventListener('mousemove', (e) => this.graphMouseMove(e))
    this.onGraphMouseUp = () => this.graphMouseUp()
    window.addEventListener('mouseup', this.onGraphMouseUp)
    api.onDidDimensionsChange(() => {
      if (!this.graphMode) return
      this.resizeGraphCanvas()
      this.draw()
    })

    const coreInfo = await tome.brain.coreInfo()
    this.promoteBtn.classList.toggle('hidden', !coreInfo.configured)

    const langExt = await markdownLangExt()
    this.extensions = [
      basicSetup,
      oneDark,
      langExt,
      wikilinkDeco,
      EditorView.domEventHandlers({ mousedown: (e, view) => this.handleWikiClick(e, view) }),
      keymap.of([
        {
          key: 'Mod-s',
          run: () => {
            this.save()
            return true
          },
        },
      ]),
      EditorView.updateListener.of((u) => this.handleUpdate(u)),
    ]
    this.view = new EditorView({ doc: '', parent: this.editorHost, extensions: this.extensions })

    const { index } = await tome.brain.open(this.ws)
    this.index = index
    this.renderList(index)
    const first = index.notes.find((n) => n.rel === 'AGENTS.md') || index.notes[0]
    if (first) await this.loadNote(first.rel)
  }
  handleUpdate(u) {
    if (!u.docChanged || this.loading) return
    this.dirty = true
    this.updateHead()
    clearTimeout(this.saveTimer)
    this.saveTimer = setTimeout(() => this.save(), 800)
  }
  save() {
    if (!this.openRel) return
    clearTimeout(this.saveTimer)
    const rel = this.openRel
    const content = this.view.state.doc.toString()
    tome.brain.write(this.ws, rel, content).then(() => {
      if (this.openRel === rel && this.view.state.doc.toString() === content) {
        this.dirty = false
        this.updateHead()
      }
    })
  }
  handleWikiClick(e, view) {
    if (!(e.metaKey || e.ctrlKey)) return false
    const pos = view.posAtCoords({ x: e.clientX, y: e.clientY })
    if (pos == null) return false
    const line = view.state.doc.lineAt(pos)
    WIKILINK_TARGET_RE.lastIndex = 0
    let m
    while ((m = WIKILINK_TARGET_RE.exec(line.text))) {
      const from = line.from + m.index
      const to = from + m[0].length
      if (pos >= from && pos <= to) {
        this.openNote(m[1].trim())
        return true
      }
    }
    return false
  }
  // lowercased basename -> shallowest-rel note; shared with syncGraph's node
  // resolution so a colliding basename opens the same note from either UI.
  byNameMap() {
    const byName = new Map()
    for (const n of this.index.notes) {
      const key = n.name.toLowerCase()
      const cur = byName.get(key)
      if (!cur || n.rel.split('/').length < cur.rel.split('/').length) byName.set(key, n)
    }
    return byName
  }
  async openNote(target) {
    const name = target.trim()
    if (!name) return
    const found = this.byNameMap().get(name.toLowerCase())
    if (found) return this.loadNote(found.rel)
    const rel = name + '.md'
    try {
      await tome.brain.write(this.ws, rel, stubNote(name), true)
    } catch (err) {
      toast(`brain: ${err.message}`)
      return
    }
    this.loadNote(rel)
  }
  newNoteModal() {
    const m = modalShell('＋ new note')
    const input = m.input('note name')
    const go = () => {
      const v = input.value.trim()
      if (!v) return
      m.close()
      this.openNote(v)
    }
    m.button('Create', go)
    input.addEventListener('keydown', (e) => e.key === 'Enter' && go())
    setTimeout(() => input.focus(), 0)
  }
  async loadNote(rel) {
    let text
    try {
      text = await tome.brain.read(this.ws, rel)
    } catch (err) {
      toast(`brain: could not read ${rel}: ${err.message}`)
      return
    }
    clearTimeout(this.saveTimer)
    this.openRel = rel
    this.dirty = false
    this.loading = true
    this.view.setState(EditorState.create({ doc: text, extensions: this.extensions }))
    this.loading = false
    this.updateHead()
    this.renderList(this.index)
    this.renderBacklinks()
  }
  updateHead() {
    const note = this.index.notes.find((n) => n.rel === this.openRel)
    const name = note ? note.name : (this.openRel || '').replace(/\.md$/, '')
    this.headTitle.textContent = (this.dirty ? '● ' : '') + name
    this.delBtn.disabled = this.openRel === 'AGENTS.md'
  }
  renderList(index) {
    if (!index) return
    const q = this.searchInput.value.trim().toLowerCase()
    this.listEl.innerHTML = ''
    const notes = [...index.notes].sort((a, b) => a.name.localeCompare(b.name))
    for (const n of notes) {
      if (q && !matchesBrainQuery(n, q)) continue
      const row = document.createElement('div')
      row.className = 'brain-row' + (n.rel === this.openRel ? ' active' : '')
      const name = document.createElement('span')
      name.textContent = n.name
      const hint = document.createElement('span')
      hint.className = 'hint'
      const parts = []
      if (n.status) parts.push(n.status)
      if (n.tags.length) parts.push(n.tags.join(', '))
      hint.textContent = parts.join(' · ')
      row.append(name, hint)
      row.addEventListener('click', () => this.loadNote(n.rel))
      this.listEl.appendChild(row)
    }
  }
  renderBacklinks() {
    this.backEl.innerHTML = ''
    if (!this.openRel) return
    const note = this.index.notes.find((n) => n.rel === this.openRel)
    const key = (note ? note.name : this.openRel.replace(/\.md$/, '')).toLowerCase()
    const rels = this.index.backlinks[key] || []
    for (const rel of rels) {
      const linking = this.index.notes.find((n) => n.rel === rel)
      const chip = document.createElement('button')
      chip.className = 'brain-chip'
      chip.textContent = linking ? linking.name : rel
      chip.addEventListener('click', () => this.loadNote(rel))
      this.backEl.appendChild(chip)
    }
  }
  async deleteOpen() {
    if (!this.openRel || this.openRel === 'AGENTS.md') return
    const rel = this.openRel
    try {
      await tome.brain.delete(this.ws, rel)
    } catch (err) {
      toast(`brain: ${err.message}`)
      return
    }
    this.index.notes = this.index.notes.filter((n) => n.rel !== rel)
    this.loadNote('AGENTS.md')
  }
  async promoteModal() {
    if (!this.openRel) return
    const rel = this.openRel
    const info = await tome.brain.coreInfo()
    if (!info.configured) return
    const note = this.index.notes.find((n) => n.rel === rel)
    const m = modalShell('↑ promote to core vault')
    m.note(info.root)
    const attempt = async (folder, overwrite, rename) => {
      let r
      try {
        r = await tome.brain.promote(this.ws, rel, folder, overwrite, rename)
      } catch (err) {
        toast(`brain: ${err.message}`)
        return
      }
      if (r.collision) return collide(folder)
      m.close()
      toast(`promoted to ${folder || 'vault root'}`, 'ok')
      this.markPromoted()
    }
    const collide = (folder) => {
      m.body.innerHTML = ''
      m.note(`“${note ? note.name : rel}” already exists in ${folder || 'vault root'}.`)
      m.button('Overwrite', () => attempt(folder, true, false))
      m.button('Keep both', () => attempt(folder, false, true), 'ghost')
      m.button('Cancel', () => m.close(), 'ghost')
    }
    m.button('vault root', () => attempt('', false, false))
    for (const folder of info.folders) m.button(folder, () => attempt(folder, false, false))
  }
  // frontmatter status -> promoted on the live buffer, then persist + refresh
  markPromoted() {
    const text = this.view.state.doc.toString()
    const updated = text.replace(/^status:\s*.*$/m, 'status: promoted')
    if (updated === text) return
    clearTimeout(this.saveTimer)
    this.loading = true
    this.view.setState(EditorState.create({ doc: updated, extensions: this.extensions }))
    this.loading = false
    this.dirty = false
    this.updateHead()
    tome.brain.write(this.ws, this.openRel, updated)
  }
  setGraphMode(on) {
    this.graphMode = on
    this.element.classList.toggle('graph-mode', on)
    if (on) {
      this.resizeGraphCanvas()
      this.syncGraph()
    } else {
      cancelAnimationFrame(this.graphRaf)
      this.graphRunning = false
    }
  }
  resizeGraphCanvas() {
    const canvas = this.graphCanvas
    const rect = canvas.getBoundingClientRect()
    const dpr = window.devicePixelRatio || 1
    this.graphW = Math.max(1, Math.round(rect.width))
    this.graphH = Math.max(1, Math.round(rect.height))
    canvas.width = this.graphW * dpr
    canvas.height = this.graphH * dpr
    this.graphCtx.setTransform(dpr, 0, 0, dpr, 0, 0)
  }
  // rebuild nodes/edges from the current index, reusing positions from the
  // name-keyed this.graphPos so layout survives brain:changed rebuilds
  syncGraph() {
    const byName = this.byNameMap()
    const seen = new Set()
    const nodeFor = (key, name) => {
      seen.add(key)
      let node = this.graphPos.get(key)
      if (!node) {
        node = { x: graphHash('x:' + key) * this.graphW, y: graphHash('y:' + key) * this.graphH, vx: 0, vy: 0 }
        this.graphPos.set(key, node)
      }
      const note = byName.get(key)
      node.name = name
      node.rel = note ? note.rel : null
      node.phantom = !note
      node.backlinks = (this.index.backlinks[key] || []).length
    }
    for (const [key, note] of byName) nodeFor(key, note.name)
    const edges = []
    for (const n of this.index.notes) {
      const fromKey = n.name.toLowerCase()
      for (const link of n.links) {
        const key = link.toLowerCase()
        if (key === fromKey) continue // self-links aren't edges, mirrors backlinks build
        if (!seen.has(key)) nodeFor(key, byName.get(key)?.name || link.trim())
        edges.push([fromKey, key])
      }
    }
    for (const key of [...this.graphPos.keys()]) if (!seen.has(key)) this.graphPos.delete(key)
    this.graphEdges = edges
    this.reheat()
  }
  simTick() {
    const nodes = [...this.graphPos.values()]
    const cx = this.graphW / 2
    const cy = this.graphH / 2
    for (let i = 0; i < nodes.length; i++) {
      for (let j = i + 1; j < nodes.length; j++) {
        const a = nodes[i]
        const b = nodes[j]
        const dx = a.x - b.x
        const dy = a.y - b.y
        const d2 = Math.max(dx * dx + dy * dy, 1)
        const f = Math.min(1200 / d2, 40) // capped so near-coincident nodes don't explode
        const d = Math.sqrt(d2)
        const fx = (dx / d) * f
        const fy = (dy / d) * f
        a.vx += fx
        a.vy += fy
        b.vx -= fx
        b.vy -= fy
      }
    }
    for (const [ak, bk] of this.graphEdges) {
      const a = this.graphPos.get(ak)
      const b = this.graphPos.get(bk)
      if (!a || !b) continue
      const dx = b.x - a.x
      const dy = b.y - a.y
      const d = Math.sqrt(dx * dx + dy * dy) || 1
      const f = (d - 90) * 0.02
      const fx = (dx / d) * f
      const fy = (dy / d) * f
      a.vx += fx
      a.vy += fy
      b.vx -= fx
      b.vy -= fy
    }
    let totalV = 0
    for (const [key, n] of this.graphPos) {
      if (key === this.dragKey) {
        n.vx = 0
        n.vy = 0
        continue // pinned to the cursor by graphMouseMove, not the sim
      }
      n.vx = (n.vx + (cx - n.x) * 0.01) * 0.85
      n.vy = (n.vy + (cy - n.y) * 0.01) * 0.85
      n.x += n.vx
      n.y += n.vy
      totalV += Math.abs(n.vx) + Math.abs(n.vy)
    }
    return totalV
  }
  reheat() {
    if (!this.graphMode) return
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      for (let i = 0; i < 300; i++) this.simTick()
      this.draw()
      return
    }
    if (this.graphRunning) return
    this.graphRunning = true
    const step = () => {
      const v = this.simTick()
      this.draw()
      if (v < 0.1) {
        this.graphRunning = false
        return
      }
      this.graphRaf = requestAnimationFrame(step)
    }
    this.graphRaf = requestAnimationFrame(step)
  }
  draw() {
    const ctx = this.graphCtx
    const { graphW: w, graphH: h, graphColors: c } = this
    ctx.fillStyle = c.void
    ctx.fillRect(0, 0, w, h)
    ctx.strokeStyle = 'rgba(0,229,255,0.18)'
    ctx.lineWidth = 1
    ctx.beginPath()
    for (const [ak, bk] of this.graphEdges) {
      const a = this.graphPos.get(ak)
      const b = this.graphPos.get(bk)
      if (!a || !b) continue
      ctx.moveTo(a.x, a.y)
      ctx.lineTo(b.x, b.y)
    }
    ctx.stroke()
    for (const [key, n] of this.graphPos) {
      const r = 3 + 2 * Math.log(1 + n.backlinks)
      ctx.beginPath()
      if (n.phantom) {
        ctx.setLineDash([3, 3])
        ctx.strokeStyle = c.muted
        ctx.arc(n.x, n.y, r, 0, Math.PI * 2)
        ctx.stroke()
        ctx.setLineDash([])
      } else if (n.rel === this.openRel) {
        ctx.shadowColor = '#ff2ea6'
        ctx.shadowBlur = 12
        ctx.fillStyle = '#ff2ea6'
        ctx.arc(n.x, n.y, r, 0, Math.PI * 2)
        ctx.fill()
        ctx.shadowBlur = 0
      } else {
        ctx.fillStyle = '#00e5ff'
        ctx.arc(n.x, n.y, r, 0, Math.PI * 2)
        ctx.fill()
      }
      ctx.font = `10px ${c.mono}`
      ctx.fillStyle = key === this.hoverKey ? c.bright : c.muted
      ctx.fillText(n.name, n.x + r + 4, n.y + 3)
    }
  }
  graphHit(x, y) {
    let best = null
    let bestD = 10 // px
    for (const [key, n] of this.graphPos) {
      const d = Math.hypot(n.x - x, n.y - y)
      if (d <= bestD) {
        bestD = d
        best = key
      }
    }
    return best
  }
  graphPoint(e) {
    const rect = this.graphCanvas.getBoundingClientRect()
    return { x: e.clientX - rect.left, y: e.clientY - rect.top }
  }
  graphMouseDown(e) {
    const { x, y } = this.graphPoint(e)
    const key = this.graphHit(x, y)
    if (!key) return
    this.dragKey = key
    this.dragMoved = false
    this.reheat()
  }
  graphMouseMove(e) {
    const { x, y } = this.graphPoint(e)
    if (this.dragKey) {
      const n = this.graphPos.get(this.dragKey)
      n.x = x
      n.y = y
      this.dragMoved = true
      this.draw()
      return
    }
    const hit = this.graphHit(x, y)
    if (hit !== this.hoverKey) {
      this.hoverKey = hit
      this.draw()
    }
  }
  graphMouseUp() {
    if (!this.dragKey) return
    const key = this.dragKey
    const moved = this.dragMoved
    this.dragKey = null
    if (moved) {
      this.reheat()
    } else {
      const n = this.graphPos.get(key)
      this.setGraphMode(false)
      if (n.phantom) this.openNote(n.name)
      else this.loadNote(n.rel)
    }
  }
  // watcher-driven reindex: always refresh list/backlinks, only touch the
  // open buffer's content when the user has no unsaved edits in it
  async onChanged(index) {
    this.index = index
    this.renderList(index)
    this.renderBacklinks()
    if (this.graphMode) this.syncGraph()
    if (!this.openRel || this.dirty) return
    if (!index.notes.some((n) => n.rel === this.openRel)) return
    let text
    try {
      text = await tome.brain.read(this.ws, this.openRel)
    } catch {
      return
    }
    if (text === this.view.state.doc.toString()) return
    this.loading = true
    this.view.setState(EditorState.create({ doc: text, extensions: this.extensions }))
    this.loading = false
    this.updateHead()
  }
  dispose() {
    clearTimeout(this.saveTimer)
    if (this.dirty && this.openRel) tome.brain.write(this.ws, this.openRel, this.view.state.doc.toString())
    cancelAnimationFrame(this.graphRaf)
    window.removeEventListener('mouseup', this.onGraphMouseUp)
    tome.brain.close(this.ws)
    brains.delete(this.ws)
    this.view?.destroy()
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
  theme: { name: 'tome', className: 'dockview-theme-tome', gap: 7 },
  createWatermarkComponent: () => new Watermark(),
  createComponent: (opts) => {
    switch (opts.name) {
      case 'editor':
        return new EditorPanel()
      case 'chat':
        return new ChatPanel()
      case 'doc':
        return new DocPanel()
      case 'brain':
        return new BrainPanel()
      case 'history':
        return new HistoryPanel()
      default:
        return new TerminalPanel()
    }
  },
})
window.addEventListener('resize', () =>
  dock.layout(dock.element.parentElement.clientWidth, dock.element.parentElement.clientHeight)
)

// conductor: keep the pane snapshot fresh; let the assistant open panes; toast its actions
const syncPanes = () => tome.panes.sync(dock.panels.map((p) => ({ id: p.id, title: p.title })))
dock.onDidAddPanel(syncPanes)
dock.onDidRemovePanel(syncPanes)
tome.conductor.onOpen(({ kind, file }) => {
  if (file) return openFile(file)
  if (kind === 'chat') return addChat()
  if (kind === 'brain') return addBrain()
  if (kind === 'terminal' || kind === 'claude' || kind === 'opencode' || kind === 'pi')
    return addTerminal(kind)
  toast(`assistant asked for unknown pane: ${kind}`)
})
tome.conductor.onActed(({ pane, ran }) =>
  toast(`assistant ${ran ? 'ran a command in' : 'typed into'} ${pane}`, 'ok')
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
    params: { ptyId: id, kind, cwd, airgap: gapped, ws: activeWorkspace()?.name },
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

function addBrain() {
  const w = activeWorkspace()
  if (!w) return
  const id = `brain:${w.name}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  dock.addPanel({
    id,
    component: 'brain',
    title: `⌬ brain — ${w.name}`,
    position: place(),
    params: { ws: w.name },
  })
}

function addHistory() {
  if (!activeRoot) return
  const id = `history:${activeRoot}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  dock.addPanel({
    id,
    component: 'history',
    title: `⎇ history — ${activeRoot.split('/').pop()}`,
    position: place(),
    params: { dir: activeRoot },
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
  menuRule(menu)
  menuItem(menu, {
    label: 'Set core vault…',
    onClick: async () => {
      const dir = await tome.pickFolder()
      if (!dir) return
      await tome.store.set('core-vault', dir)
      toast('core vault set', 'ok')
    },
  })
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
  menuItem(menu, { label: 'History', hint: 'commit log', onClick: addHistory })
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
  menuItem(menu, {
    label: 'assistant may run commands',
    hint: conductorRun ? 'on' : 'off',
    active: conductorRun,
    onClick: () => {
      conductorRun = !conductorRun
      tome.store.set('conductor-run', conductorRun)
      tome.conductor.allowRun(conductorRun)
    },
  })
  menuRule(menu)
  menuItem(menu, { label: 'Assistant chat', hint: 'API', onClick: addChat })
  menuItem(menu, { label: 'Terminal', hint: 'zsh', onClick: () => addTerminal('terminal') })
  menuItem(menu, {
    label: 'Brain',
    hint: activeWorkspace() ? 'vault' : 'needs a workspace',
    disabled: !activeWorkspace(),
    onClick: addBrain,
  })
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
  // app login already proved the passphrase — freeing a pane wants the second
  // factor: the authenticator code when enrolled, the passphrase otherwise
  let pass = null
  let code = null
  if (state.auth.totp) code = m.input('2FA code (6 digits)', 'text')
  else pass = m.input('passphrase')
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
      passphrase: pass?.value,
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
  const field = code || pass
  field.addEventListener('keydown', (e) => e.key === 'Enter' && go())
  setTimeout(() => field.focus(), 0)
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
  await bootAuth(tome, toast) // main gates the sensitive IPC until this resolves
  const saved = await tome.store.get('workspaces')
  if (saved && Array.isArray(saved.workspaces)) {
    ws = saved
    if (ws.active >= ws.workspaces.length) ws.active = ws.workspaces.length - 1
  }
  const agPref = await tome.store.get('airgap-default')
  if (agPref !== null) airgapDefault = !!agPref
  if (await tome.store.get('conductor-run')) {
    conductorRun = true
    tome.conductor.allowRun(true)
  }
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
    addBrain()
  }
})()
