// The dockview grid and the pane-opening actions (terminal, chat, brain,
// history, file). Also the conductor bridge: keeps main's pane snapshot
// fresh and honors assistant open requests.
import { createDockview } from 'dockview-core'
import 'dockview-core/dist/styles/dockview.css'
import { tome, toast } from './util.js'
import { prefs, counters } from './state.js'
import { activeWorkspace, paneCwd } from './workspaces.js'
import { wsState } from './state.js'
import { TerminalPanel } from './panels/terminal.js'
import { EditorPanel } from './panels/editor.js'
import { DocPanel } from './panels/doc.js'
import { ChatPanel } from './panels/chat.js'
import { BrainPanel } from './panels/brain.js'
import { HistoryPanel } from './history.js'

class Watermark {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'watermark'
    this.element.textContent = '＋ open a pane — agents · terminal · editor · chat'
  }
  init() {}
}

export const dock = createDockview(document.getElementById('dock'), {
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

export function addTerminal(kind) {
  const id = `pty-${++counters.seq}`
  const cwd = paneCwd()
  const name = cwd.split('/').pop() || cwd
  const isAgent = kind !== 'terminal'
  const gapped = isAgent && prefs.airgapDefault
  dock.addPanel({
    id,
    component: 'terminal',
    title: isAgent ? `${gapped ? '⛨ ' : ''}${kind} — ${name}` : `zsh — ${name}`,
    position: place(),
    params: { ptyId: id, kind, cwd, airgap: gapped, ws: activeWorkspace()?.name },
  })
}

export function addChat() {
  const id = `chat-${++counters.seq}`
  dock.addPanel({
    id,
    component: 'chat',
    title: 'assistant',
    position: place(),
    params: { chatId: id },
  })
}

export function addBrain() {
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

export function addHistory() {
  if (!wsState.activeRoot) return
  const id = `history:${wsState.activeRoot}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  dock.addPanel({
    id,
    component: 'history',
    title: `⎇ history — ${wsState.activeRoot.split('/').pop()}`,
    position: place(),
    params: { dir: wsState.activeRoot },
  })
}

const IMG_EXT = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'ico'])
const CONV_EXT = new Set(['docx', 'xlsx', 'xls'])

export async function openFile(path) {
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
