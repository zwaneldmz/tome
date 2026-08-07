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
import { AGENTS } from '../shared/pane-kinds.js'

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
  if (kind === 'terminal' || AGENTS.includes(kind)) return addTerminal(kind)
  toast(`assistant asked for unknown pane: ${kind}`)
})
tome.conductor.onActed(({ pane, ran }) =>
  toast(`assistant ${ran ? 'ran a command in' : 'typed into'} ${pane}`, 'ok')
)

// ---------- layout persistence ----------
// The dockview grid is serialized with toJSON() and stored per workspace,
// keyed by the workspace's folder list (falls back to the name) so renaming a
// workspace keeps its layout. Saved on every layout change (debounced) and
// once more via the main-process quit handshake.
//
// Terminals/agents are the exception: a pty is a live process and cannot be
// resumed. On restore we recreate each terminal/agent pane as a FRESH SHELL
// in its saved position (same kind/cwd/airgap), rather than skipping it — the
// grid shape survives even though scrollback and running processes don't.
let restoring = false
let layoutLoaded = false
let layoutSaveTimer = null

const layoutKey = (w) => {
  if (!w) return 'layout:none'
  const basis = w.folders.length ? w.folders.join('|') : 'name:' + w.name
  const slug = basis.replace(/[^a-z0-9-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 90)
  return 'layout-' + (slug || 'unnamed')
}

function scheduleLayoutSave() {
  if (restoring || !layoutLoaded) return
  clearTimeout(layoutSaveTimer)
  layoutSaveTimer = setTimeout(saveLayoutNow, 800)
}

async function saveLayoutNow() {
  if (!layoutLoaded) return
  clearTimeout(layoutSaveTimer)
  try {
    await tome.store.set(layoutKey(activeWorkspace()), dock.toJSON())
  } catch {}
}

dock.onDidLayoutChange(scheduleLayoutSave)
tome.app.onBeforeQuit(() => {
  saveLayoutNow().finally(() => tome.app.quitReady())
})

const DOC_MODES = new Set(['pdf', 'img', 'doc', 'binary'])

// fromJSON gives us the panel shell (id, title, params, position) but not the
// component instance — infer what to respawn from the params we persisted.
function componentOf(panel) {
  const params = panel.params || {}
  if (params.ptyId) return 'terminal'
  if (params.chatId) return 'chat'
  if (params.ws) return 'brain'
  if (params.dir) return 'history'
  if (params.path && params.mode) return 'doc'
  if (params.path) return 'editor'
  return null
}

async function fileExists(path) {
  try {
    await tome.fs.readFile(path)
    return true
  } catch {
    return false
  }
}
async function dirExists(path) {
  try {
    await tome.fs.readDir(path)
    return true
  } catch {
    return false
  }
}

export async function restoreLayout() {
  const w = activeWorkspace()
  const saved = await tome.store.get(layoutKey(w))
  layoutLoaded = true
  if (!saved || !Array.isArray(saved.panels) || !Object.keys(saved.panels).length) return
  restoring = true
  try {
    dock.fromJSON(saved)
  } catch (err) {
    console.warn('layout restore failed, starting empty:', err)
    restoring = false
    try {
      dock.clear()
    } catch {}
    return
  }
  // Panels that failed to deserialize (e.g. a doc iframe with a null content
  // element) come back without a renderer-side instance — drop them.
  const stale = []
  for (const p of dock.panels) {
    const el = p.view?.content?.element
    if (!el || !el.isConnected) stale.push(p)
  }
  for (const p of stale) {
    try {
      dock.removePanel(p)
    } catch {}
  }
  try {
    await Promise.all(
      dock.panels.map(async (p) => {
        const params = p.params || {}
        const component = componentOf(p)
        if (component === 'terminal') {
          const kind = AGENTS.includes(params.kind) || params.kind === 'terminal' ? params.kind : 'terminal'
          spawnTerminal({ kind, cwd: params.cwd, airgap: params.airgap, wsName: params.ws, saved: p })
        } else if (component === 'chat') {
          spawnChat(p)
        } else if (component === 'brain') {
          if (wsState.ws.workspaces.some((x) => x.name === params.ws)) spawnBrain(params.ws, p)
          else dock.removePanel(p) // workspace gone — skip
        } else if (component === 'history') {
          const dir = typeof params.dir === 'string' && (await dirExists(params.dir)) ? params.dir : null
          if (dir) spawnHistory(dir, p)
          else dock.removePanel(p)
        } else if (component === 'editor' || component === 'doc') {
          if (typeof params.path === 'string' && (await fileExists(params.path))) {
            if (component === 'doc' && !DOC_MODES.has(params.mode)) dock.removePanel(p)
            else await openFile(params.path, p)
          } else {
            dock.removePanel(p) // file no longer exists — skip
          }
        } else {
          dock.removePanel(p) // unknown component from an older build — skip
        }
      })
    )
  } finally {
    restoring = false
  }
}

function place() {
  const n = dock.panels.length
  if (n === 0) return undefined
  return { referencePanel: dock.panels[n - 1], direction: n % 2 ? 'right' : 'below' }
}

export function addTerminal(kind) {
  spawnTerminal({ kind })
}

// Shared by the ＋ menu and layout restore. `saved` carries the persisted
// id/title when recreating a pane from a stored layout.
function spawnTerminal({ kind, cwd, airgap, wsName, saved }) {
  const id = `pty-${++counters.seq}`
  cwd = cwd || paneCwd()
  const name = cwd.split('/').pop() || cwd
  const isAgent = kind !== 'terminal'
  const gapped = airgap !== undefined ? !!airgap : isAgent && prefs.airgapDefault
  dock.addPanel({
    id,
    component: 'terminal',
    title: saved?.title || (isAgent ? `${gapped ? '⛨ ' : ''}${kind} — ${name}` : `zsh — ${name}`),
    position: saved ? { referencePanel: saved.id } : place(),
    params: { ptyId: id, kind, cwd, airgap: gapped, ws: wsName ?? activeWorkspace()?.name },
  })
}

export function addChat() {
  spawnChat()
}

function spawnChat(saved) {
  const id = `chat-${++counters.seq}`
  dock.addPanel({
    id,
    component: 'chat',
    title: saved?.title || 'assistant',
    position: saved ? { referencePanel: saved.id } : place(),
    params: { chatId: id },
  })
}

export function addBrain() {
  const w = activeWorkspace()
  if (!w) return
  spawnBrain(w.name)
}

function spawnBrain(wsName, saved) {
  const id = `brain:${wsName}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  dock.addPanel({
    id,
    component: 'brain',
    title: saved?.title || `⌬ brain — ${wsName}`,
    position: saved ? { referencePanel: saved.id } : place(),
    params: { ws: wsName },
  })
}

export function addHistory() {
  if (!wsState.activeRoot) return
  spawnHistory(wsState.activeRoot)
}

function spawnHistory(dir, saved) {
  const id = `history:${dir}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  dock.addPanel({
    id,
    component: 'history',
    title: saved?.title || `⎇ history — ${dir.split('/').pop()}`,
    position: saved ? { referencePanel: saved.id } : place(),
    params: { dir },
  })
}

const IMG_EXT = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'ico'])
const CONV_EXT = new Set(['docx', 'xlsx', 'xls'])

export async function openFile(path, saved) {
  const id = `file:${path}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  const name = saved?.title || path.split('/').pop()
  const pos = saved ? { referencePanel: saved.id } : place()
  const ext = (name.includes('.') ? name.split('.').pop() : '').toLowerCase()
  const docPanel = (mode) =>
    dock.addPanel({ id, component: 'doc', title: name, position: pos, params: { mode, path } })

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
  dock.addPanel({ id, component: 'editor', title: name, position: pos, params: { path } })
}
