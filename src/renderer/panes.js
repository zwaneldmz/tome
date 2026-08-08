// The dockview grid and the pane-opening actions (terminal, chat, brain,
// history, file). Also the conductor bridge: keeps main's pane snapshot
// fresh and honors assistant open requests.
import { createDockview } from 'dockview-core'
import 'dockview-core/dist/styles/dockview.css'
import { tome, toast, el } from './util.js'
import { prefs, counters } from './state.js'
import { activeWorkspace, paneCwd } from './workspaces.js'
import { wsState } from './state.js'
import { floatingMenu, populateAddMenu } from './menus.js'
import { confirmModal } from './modals.js'
import { trackThemedDocument } from './theme.js'
import { TerminalPanel } from './panels/terminal.js'
import { EditorPanel } from './panels/editor.js'
import { DocPanel } from './panels/doc.js'
import { ChatPanel } from './panels/chat.js'
import { BrainPanel } from './panels/brain.js'
import { HistoryPanel } from './history.js'
import { renderStatusbar, setStatusbarDock } from './statusbar.js'
import { plusIcon, popoutIcon } from './icons.js'
import { AGENTS } from '../shared/pane-kinds.js'

class Watermark {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'watermark'
    this.element.textContent = '＋ open a pane — agents · terminal · editor · chat'
  }
  init() {}
}

// The shell document a pane gets when it is dragged out into its own OS
// window. Resolved against the current page so it works both under the dev
// server (http://localhost/popout.html) and in a packaged build
// (file://…/out/renderer/popout.html).
const POPOUT_URL = new URL('popout.html', location.href).href

// Per-group header buttons: ＋ opens a new pane as a *tab in this group*, ⧉
// tears the group off into its own window.
class GroupActions {
  constructor(group) {
    this.group = group
    this.element = el('div', 'grp-actions')
    const add = el('button', 'grp-btn')
    add.appendChild(plusIcon())
    add.title = 'New pane as a tab in this group'
    add.setAttribute('aria-label', 'New pane in this group')
    add.setAttribute('aria-haspopup', 'true')
    add.setAttribute('aria-expanded', 'false')
    add.addEventListener('click', (e) => {
      e.stopPropagation()
      floatingMenu(add, (menu) => populateAddMenu(menu, { group }))
    })
    this.pop = el('button', 'grp-btn')
    this.pop.appendChild(popoutIcon())
    this.pop.title = 'Open this group in its own window'
    this.pop.setAttribute('aria-label', 'Open this group in its own window')
    this.pop.addEventListener('click', (e) => {
      e.stopPropagation()
      popout(group)
    })
    this.element.append(add, this.pop)
  }
  init({ api }) {
    const sync = () => {
      // already its own window (or a floating group) — nothing to tear off
      this.pop.style.display = api.location?.type === 'grid' ? '' : 'none'
    }
    this.disposable = api.onDidLocationChange?.(sync)
    sync()
  }
  dispose() {
    this.disposable?.dispose?.()
  }
}

export const dock = createDockview(document.getElementById('dock'), {
  theme: { name: 'tome', className: 'dockview-theme-tome', gap: 8 },
  popoutUrl: POPOUT_URL,
  createWatermarkComponent: () => new Watermark(),
  createRightHeaderActionComponent: (group) => new GroupActions(group),
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
// no manual resize wiring: createDockview returns the api, which has no
// `element`, so this threw on every resize — and dockview already watches its
// own container with a ResizeObserver unless disableAutoResizing is set.

// ---------- close guard ----------
// Panels expose isDirty() (editors do); closing one asks before discarding.
// Programmatic removes (layout restore, conductor) set bypassCloseGuard first.
let bypassCloseGuard = false
export const removePanel = (p) => {
  bypassCloseGuard = true
  try {
    dock.removePanel(p)
  } finally {
    bypassCloseGuard = false
  }
}

// Close a panel with the same guard the tab's ✕ uses: a dirty editor asks
// before discarding. Clean panels go straight through removePanel.
// ---------- drag-and-drop file open ----------
// Dropping OS files anywhere over the window opens them as panes. The
// highlight is counter-based because dragenter/dragleave fire per nested
// element; dockview's own tab drags carry no 'Files' type and pass through.
const dockEl = document.getElementById('dock')
let dropDepth = 0
const isFileDrag = (e) => [...(e.dataTransfer?.types || [])].includes('Files')

window.addEventListener('dragenter', (e) => {
  if (!isFileDrag(e)) return
  dropDepth++
  dockEl.classList.add('drop-target')
})
window.addEventListener('dragleave', (e) => {
  if (!isFileDrag(e)) return
  dropDepth = Math.max(0, dropDepth - 1)
  if (!dropDepth) dockEl.classList.remove('drop-target')
})
window.addEventListener('dragover', (e) => {
  if (!isFileDrag(e)) return
  e.preventDefault() // allow the drop
  e.dataTransfer.dropEffect = 'copy'
})
window.addEventListener('drop', (e) => {
  if (!isFileDrag(e)) return
  e.preventDefault()
  dropDepth = 0
  dockEl.classList.remove('drop-target')
  for (const file of e.dataTransfer.files) {
    // File.path is the classic Electron renderer field; newer versions gate
    // it behind webUtils.getPathForFile in the preload.
    const path = file.path || tome.webUtils?.pathForFile?.(file)
    if (typeof path === 'string' && path) openFile(path)
    else toast(`cannot open dropped item: ${file.name}`)
  }
})

export function closePanel(panel) {
  if (!panel) return
  const view = panel.view?.content
  if (typeof view?.isDirty === 'function' && view.isDirty()) {
    confirmModal(
      'Discard unsaved changes?',
      `“${panel.title.replace(/^● /, '')}” has changes that have not been saved. Closing it discards them.`,
      'Discard'
    ).then((ok) => ok && removePanel(panel))
    return
  }
  removePanel(panel)
}

// The default tab's close affordance is .dv-default-tab-action (it honours
// defaultPrevented, so a capture-phase preventDefault vetoes the close).
// Tab DOM order matches group.panels order.
function dirtyPanelFromTabAction(action) {
  const tabEl = action.closest('.dv-default-tab')?.parentElement
  if (!tabEl?.classList.contains('dv-tab')) return null
  const groupEl = tabEl.closest('.dv-groupview')
  const group = dock.groups.find((g) => groupEl?.contains(g.element) || g.element === groupEl)
  if (!group) return null
  const idx = [...tabEl.parentElement.querySelectorAll(':scope > .dv-tab')].indexOf(tabEl)
  return group.panels[idx] || null
}

document.addEventListener(
  'click',
  (e) => {
    if (bypassCloseGuard || !(e.target instanceof Element)) return
    const action = e.target.closest('.dv-default-tab-action')
    if (!action) return
    const panel = dirtyPanelFromTabAction(action)
    const view = panel?.view?.content
    if (typeof view?.isDirty !== 'function' || !view.isDirty()) return
    e.preventDefault()
    confirmModal(
      'Discard unsaved changes?',
      `“${panel.title.replace(/^● /, '')}” has changes that have not been saved. Closing it discards them.`,
      'Discard'
    ).then((ok) => ok && removePanel(panel))
  },
  true
)

// ---------- tear-off: drag a pane past the window edge ----------
// Inside the window dockview already handles the drop (rearrange or stack as
// a tab). If the drag ends outside the window — another display, or just the
// desktop — we take that as "give this its own window" and pop the group out
// where it was dropped.
const POPOUT_SIZE = { width: 940, height: 640 }

function popout(item, at) {
  // Only a pane still sitting in the main grid can be torn off. A drag that
  // ended in one of our own popout windows reports dropEffect 'none' back to
  // this document and lands outside this window's bounds, so the tear-off
  // fires for a pane dockview has already moved — popping it out again throws
  // 'invalid operation' inside dockview and desyncs the grid.
  if (item.api?.location?.type !== 'grid') return
  const position = at
    ? {
        left: Math.round(at.x - POPOUT_SIZE.width / 2),
        top: Math.round(at.y - 20),
        ...POPOUT_SIZE,
      }
    : undefined
  Promise.resolve(
    dock.addPopoutGroup(item, {
      popoutUrl: POPOUT_URL,
      position,
      onDidOpen: ({ window: w }) => {
        w.document.body.classList.add('tome-popout')
        const untrack = trackThemedDocument(w.document)
        w.addEventListener('pagehide', untrack, { once: true })
      },
    })
  )
    // dockview catches its own failures and resolves false rather than
    // rejecting, so without this a failed tear-off is silent to the user
    .then((ok) => ok === false && toast('could not open a window for that pane'))
    .catch((err) => toast(`could not open a window for that pane: ${err?.message || err}`))
}

const outsideWindow = (x, y) =>
  x < window.screenX ||
  y < window.screenY ||
  x > window.screenX + window.outerWidth ||
  y > window.screenY + window.outerHeight

// One listener, one pending item — not a listener per drag. A drag that ends
// inside a popout window fires its dragend on *that* document, so a per-drag
// listener on this one is never removed: they pile up and later fire with
// stale items, which is what produced the 'invalid operation' bursts.
let tearOffItem = null
const armTearOff = (item) => {
  tearOffItem = item
}

document.addEventListener(
  'dragend',
  (e) => {
    const item = tearOffItem
    tearOffItem = null
    if (!item) return
    // a drop dockview handled sets a dropEffect; 'none' means it landed nowhere
    if (e.dataTransfer && e.dataTransfer.dropEffect !== 'none') return
    if (!outsideWindow(e.screenX, e.screenY)) return
    popout(item, { x: e.screenX, y: e.screenY })
  },
  true
)

dock.onWillDragPanel(({ panel }) => armTearOff(panel))
dock.onWillDragGroup(({ group }) => armTearOff(group))

// conductor: keep the pane snapshot fresh; let the assistant open panes; toast its actions
setStatusbarDock(dock)
const syncPanes = () => {
  tome.panes.sync(dock.panels.map((p) => ({ id: p.id, title: p.title })))
  renderStatusbar()
}
dock.onDidAddPanel(syncPanes)
dock.onDidRemovePanel(syncPanes)
// active-pane context in the status bar (editor line:col, terminal cwd, …)
dock.onDidActivePanelChange(() => renderStatusbar())

// `source` is the assistant pane that asked. Panes it opens join that pane's
// group as tabs rather than carving up the grid behind the user's back.
const groupTarget = (paneId) => {
  const p = paneId ? dock.getPanel(paneId) : null
  return p?.group ? { group: p.group } : undefined
}

tome.conductor.onOpen(({ kind, file, source }) => {
  const target = groupTarget(source)
  if (file) return openFile(file, undefined, target)
  if (kind === 'chat') return addChat(target)
  if (kind === 'brain') return addBrain(target)
  if (kind === 'terminal' || AGENTS.includes(kind)) return addTerminal(kind, target)
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
    const node = p.view?.content?.element
    if (!node || !node.isConnected) stale.push(p)
  }
  for (const p of stale) {
    try {
      removePanel(p)
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
          else removePanel(p) // workspace gone — skip
        } else if (component === 'history') {
          const dir = typeof params.dir === 'string' && (await dirExists(params.dir)) ? params.dir : null
          if (dir) spawnHistory(dir, p)
          else removePanel(p)
        } else if (component === 'editor' || component === 'doc') {
          if (typeof params.path === 'string' && (await fileExists(params.path))) {
            if (component === 'doc' && !DOC_MODES.has(params.mode)) removePanel(p)
            else await openFile(params.path, p)
          } else {
            removePanel(p) // file no longer exists — skip
          }
        } else {
          removePanel(p) // unknown component from an older build — skip
        }
      })
    )
  } finally {
    restoring = false
  }
}

// Where a new pane lands. `target.group` means "as a tab in that group" —
// dockview treats a referenceGroup with no direction as 'within'.
function place(target) {
  if (target?.group) return { referenceGroup: target.group }
  const n = dock.panels.length
  if (n === 0) return undefined
  return { referencePanel: dock.panels[n - 1], direction: n % 2 ? 'right' : 'below' }
}

// A pane spawned into an existing group is a helper for what is already there,
// so it inherits that pane's working directory when it has one.
function targetCwd(target) {
  const params = target?.group?.activePanel?.params
  return typeof params?.cwd === 'string' ? params.cwd : null
}

export function addTerminal(kind, target) {
  spawnTerminal({ kind, cwd: targetCwd(target), target })
}

// Shared by the ＋ menus and layout restore. `saved` carries the persisted
// id/title when recreating a pane from a stored layout.
function spawnTerminal({ kind, cwd, airgap, wsName, saved, target }) {
  const id = `pty-${++counters.seq}`
  cwd = cwd || paneCwd()
  const name = cwd.split('/').pop() || cwd
  const isAgent = kind !== 'terminal'
  const gapped = airgap !== undefined ? !!airgap : isAgent && prefs.airgapDefault
  dock.addPanel({
    id,
    component: 'terminal',
    title: saved?.title || (isAgent ? `${gapped ? '⛨ ' : ''}${kind} — ${name}` : `zsh — ${name}`),
    position: saved ? { referencePanel: saved.id } : place(target),
    params: { ptyId: id, kind, cwd, airgap: gapped, ws: wsName ?? activeWorkspace()?.name },
  })
}

export function addChat(target) {
  spawnChat(undefined, target)
}

function spawnChat(saved, target) {
  // A restored pane keeps its saved chatId so the persisted transcript
  // (chat-log-<chatId> in the store) lines back up with this pane.
  const id = typeof saved?.params?.chatId === 'string' ? saved.params.chatId : `chat-${++counters.seq}`
  dock.addPanel({
    id,
    component: 'chat',
    title: saved?.title || 'assistant',
    position: saved ? { referencePanel: saved.id } : place(target),
    params: { chatId: id },
  })
}

export function addBrain(target) {
  const w = activeWorkspace()
  if (!w) return
  spawnBrain(w.name, undefined, target)
}

function spawnBrain(wsName, saved, target) {
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
    position: saved ? { referencePanel: saved.id } : place(target),
    params: { ws: wsName },
  })
}

export function addHistory(target) {
  if (!wsState.activeRoot) return
  spawnHistory(wsState.activeRoot, undefined, target)
}

function spawnHistory(dir, saved, target) {
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
    position: saved ? { referencePanel: saved.id } : place(target),
    params: { dir },
  })
}

const IMG_EXT = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'ico'])
const CONV_EXT = new Set(['docx', 'xlsx', 'xls'])

export async function openFile(path, saved, target) {
  const id = `file:${path}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  const name = saved?.title || path.split('/').pop()
  const pos = saved ? { referencePanel: saved.id } : place(target)
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
