import { createDockview, themeAbyss } from 'dockview-core'
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
let root = tome.home
let seq = 0

// ---------- pty event fan-out ----------
const terms = new Map() // ptyId -> Terminal
tome.pty.onData(({ id, data }) => terms.get(id)?.write(data))
tome.pty.onExit(({ id, exitCode }) =>
  terms.get(id)?.write(`\r\n\x1b[2m[process exited ${exitCode}]\x1b[0m\r\n`)
)

// ---------- chat event fan-out ----------
const chats = new Map() // chatId -> ChatPanel
tome.chat.onDelta(({ id, text }) => chats.get(id)?.appendDelta(text))
tome.chat.onDone(({ id, error }) => chats.get(id)?.finish(error))

// ---------- panels ----------
class TerminalPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-terminal'
  }
  init({ params, api }) {
    this.ptyId = params.ptyId
    const term = new Terminal({
      fontSize: 12.5,
      fontFamily: 'ui-monospace, Menlo, monospace',
      cursorBlink: true,
      theme: { background: '#171a21' },
    })
    const fit = new FitAddon()
    term.loadAddon(fit)
    term.open(this.element)
    terms.set(this.ptyId, term)
    tome.pty.create({ id: this.ptyId, cmd: params.cmd, args: params.args, cwd: params.cwd })
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
    const form = this.element.querySelector('form')
    form.addEventListener('submit', (e) => {
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
      this.history.pop() // failed turn: drop the user message so history stays valid
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
const dock = createDockview(document.getElementById('dock'), {
  theme: themeAbyss,
  createComponent: (opts) => {
    switch (opts.name) {
      case 'editor':
        return new EditorPanel()
      case 'chat':
        return new ChatPanel()
      default:
        return new TerminalPanel()
    }
  },
})
window.addEventListener('resize', () =>
  dock.layout(dock.element.parentElement.clientWidth, dock.element.parentElement.clientHeight)
)

// grid-ish placement: alternate right / below of the last panel
function place() {
  const n = dock.panels.length
  if (n === 0) return undefined
  return {
    referencePanel: dock.panels[n - 1],
    direction: n % 2 ? 'right' : 'below',
  }
}

function addTerminal(kind) {
  // kind: 'terminal' or an agent name ('claude' | 'opencode' | 'pi')
  const id = `pty-${++seq}`
  const name = root.split('/').pop() || root
  const isAgent = kind !== 'terminal'
  dock.addPanel({
    id,
    component: 'terminal',
    title: isAgent ? `${kind} — ${name}` : `zsh — ${name}`,
    position: place(),
    params: {
      ptyId: id,
      // login shell so PATH (~/.local/bin, homebrew) resolves for agent CLIs
      args: isAgent ? ['-l', '-c', kind] : ['-l'],
      cwd: root,
    },
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

function openFile(path) {
  const id = `file:${path}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  dock.addPanel({
    id,
    component: 'editor',
    title: path.split('/').pop(),
    position: place(),
    params: { path },
  })
}

// ---------- ＋ menu ----------
const btnAdd = document.getElementById('btn-add')
const menu = document.getElementById('add-menu')

async function buildMenu() {
  const agents = await tome.agents.list()
  menu.innerHTML = ''
  const item = (label, hint, onClick, disabled = false) => {
    const b = document.createElement('button')
    b.setAttribute('role', 'menuitem')
    b.disabled = disabled
    const l = document.createElement('span')
    l.textContent = label
    const h = document.createElement('span')
    h.className = 'hint'
    h.textContent = hint
    b.append(l, h)
    if (onClick) b.addEventListener('click', onClick)
    menu.appendChild(b)
  }
  for (const a of agents) {
    item(a.name, a.available ? 'agent' : 'not installed', () => addTerminal(a.name), !a.available)
  }
  menu.appendChild(document.createElement('hr'))
  item('Assistant chat', 'API', addChat)
  item('Terminal', 'zsh', () => addTerminal('terminal'))
  item('Open file…', '', async () => {
    const p = await tome.pickFile()
    if (p) openFile(p)
  })
}

btnAdd.addEventListener('click', (e) => {
  e.stopPropagation()
  const open = menu.classList.toggle('hidden')
  btnAdd.setAttribute('aria-expanded', String(!open))
})
document.addEventListener('click', () => {
  menu.classList.add('hidden')
  btnAdd.setAttribute('aria-expanded', 'false')
})

// ---------- file tree ----------
const treeEl = document.getElementById('tree')
const rootName = document.getElementById('root-name')

async function renderDir(dir, container, depth) {
  let entries
  try {
    entries = await tome.fs.readDir(dir)
  } catch {
    return
  }
  for (const e of entries) {
    const full = `${dir}/${e.name}`
    const row = document.createElement('div')
    row.className = 'entry ' + (e.dir ? 'dir' : 'file')
    row.style.paddingLeft = 10 + depth * 13 + 'px'
    row.textContent = (e.dir ? '▸ ' : '') + e.name
    container.appendChild(row)
    if (e.dir) {
      let open = false
      let kids = null
      row.addEventListener('click', () => {
        open = !open
        row.textContent = (open ? '▾ ' : '▸ ') + e.name
        if (open && !kids) {
          kids = document.createElement('div')
          row.after(kids)
          renderDir(full, kids, depth + 1)
        } else if (kids) {
          kids.style.display = open ? '' : 'none'
        }
      })
    } else {
      row.addEventListener('click', () => openFile(full))
    }
  }
}

async function setRoot(dir) {
  root = dir
  rootName.textContent = dir.split('/').pop() || dir
  treeEl.innerHTML = ''
  renderDir(dir, treeEl, 0)
}

document.getElementById('pick-root').addEventListener('click', async () => {
  const dir = await tome.pickFolder()
  if (dir) setRoot(dir)
})

// ---------- boot ----------
buildMenu()
setRoot(root)
