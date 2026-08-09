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
import { choiceModal, confirmModal } from './modals.js'
import { trackThemedDocument } from './theme.js'
import { setOpenFile } from './lsp-editor.js'
import { TerminalPanel } from './panels/terminal.js'
import { EditorPanel } from './panels/editor.js'
import { DocPanel } from './panels/doc.js'
import { ChatPanel } from './panels/chat.js'
import { BrainPanel } from './panels/brain.js'
import { FlowPanel } from './panels/flow.js'
import { EventsPanel } from './panels/events.js'
import { RunsPanel } from './panels/runs.js'
import { HistoryPanel } from './history.js'
import { renderStatusbar, setStatusbarDock } from './statusbar.js'
import { plusIcon, popoutIcon } from './icons.js'
import { AGENTS } from '../shared/pane-kinds.js'
import { createFlow } from '../shared/flow-model.js'
import { stripControlChars } from '../shared/terminal-text.js'

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
  // dndOverlayMounting must be 'absolute' — it is what leaves dockview's
  // drop-target anchor container enabled. On the default 'relative' the
  // anchor a popped-out group gets is created disabled, so nothing but the
  // tab strip accepts a drop from another window. Every built-in theme with
  // a gap sets this pair for the same reason.
  theme: {
    name: 'tome',
    className: 'dockview-theme-tome',
    gap: 8,
    dndOverlayMounting: 'absolute',
    dndPanelOverlay: 'group',
  },
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
      case 'events':
        return new EventsPanel()
      case 'runs':
        return new RunsPanel()
      case 'flow':
        return new FlowPanel()
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

// Awaitable so callers closing several panes can do it one at a time — the
// discard prompt is a modal, and only one modal exists at a time.
export async function closePanel(panel, doc) {
  if (!panel) return
  const view = panel.view?.content
  if (typeof view?.isDirty === 'function' && view.isDirty()) {
    const ok = await confirmModal(
      'Discard unsaved changes?',
      `“${panel.title.replace(/^● /, '')}” has changes that have not been saved. Closing it discards them.`,
      'Discard',
      doc
    )
    if (!ok) return
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
        // a drag released in this window ends here, not in the main document
        watchDragCleanup(w.document)
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

// A popped-out window asked to close. Main vetoed the close and is holding
// the window open until we call popout.close(id) — so cancelling is simply
// never calling it. dockview names each popout window `${dockId}-${groupId}`,
// which is how the window maps back to the panes inside it.
tome.popout.onCloseRequest(async ({ id, name }) => {
  const group = dock.groups.find(
    (g) => g.api.location.type === 'popout' && name.endsWith(`-${g.api.id}`)
  )
  const panels = group ? [...group.panels] : []
  if (!panels.length) return tome.popout.close(id) // nothing to lose, just go
  // The prompt is about *that* window, so it belongs in it — asking back in
  // the main window points at the wrong place, and may be behind it. A popout
  // group's location carries its own window, so no id bookkeeping is needed.
  const doc = group.api.location.getWindow?.().document

  const one = panels.length === 1
  const what = one ? `“${panels[0].title.replace(/^● /, '')}”` : `${panels.length} panes`
  const choice = await choiceModal(
    one ? 'Close this window?' : `Close this window and its ${panels.length} panes?`,
    `${what} ${one ? 'is' : 'are'} open in this window.`,
    [
      { label: one ? 'Move pane to main window' : 'Move panes to main window', value: 'move' },
      { label: one ? 'Close pane' : `Close ${panels.length} panes`, value: 'close', cls: 'danger' },
    ],
    doc
  )
  if (!choice) return // dismissed — leave the window exactly as it was

  if (choice === 'close') {
    // Sequential: closePanel can raise its own discard prompt, and only one
    // modal exists at a time. A pane whose discard is refused stays open and
    // rides back to the main window when the window closes below.
    for (const p of panels) await closePanel(p, doc)
  }
  tome.popout.close(id)
})

// dockview clears an anchored drop overlay on dragend/drop, but only in the
// document that owns it — and onDragLeave deliberately skips the clear while
// an anchor container is in use. A drag that crosses windows ends in whichever
// window you released over, so the window you merely passed through keeps its
// highlight painted. Clearing every group is the same call dockview makes, and
// is a no-op for groups that have no overlay up.
function clearDropOverlays() {
  // The root container backs drops onto the dock's own edges; each group has
  // its own for tab and content drops. A leftover can be in either.
  const containers = [
    dock.component?.rootDropTargetContainer,
    ...dock.groups.map((g) => g.model?.dropTargetContainer),
  ]
  for (const c of containers) {
    try {
      c?.model?.clear?.()
    } catch {
      /* internal shape changed — a stale highlight is not worth throwing over */
    }
  }
}
// Deferred so dockview's own drop handling finishes first: clearing ahead of
// it would just be re-shown. `drop` as well as `dragend`, because a drag that
// crosses windows does not reliably deliver dragend to the window that was
// only passed through.
const clearDropOverlaysSoon = () => setTimeout(clearDropOverlays, 0)

export function watchDragCleanup(doc) {
  doc.addEventListener('dragend', clearDropOverlaysSoon, true)
  doc.addEventListener('drop', clearDropOverlaysSoon, true)
}
watchDragCleanup(document)

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
  if (kind === 'flow') return addFlow(target)
  // Read-only, like the event log the assistant can already be asked about:
  // opening the runs page shows what is running, it starts nothing.
  if (kind === 'runs') return addRuns(target)
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
  if (params.events) return 'events'
  if (params.runs) return 'runs'
  // Must precede the generic path&&mode / bare-path fallthroughs below, or a
  // flow's params (which are just { path }, same shape as an editor's) get
  // classified as 'editor' and the panel silently opens the raw JSON on
  // restore instead of the flow canvas (plan §5).
  //
  // The flow canvas's own "Open as text" escape hatch (FlowPanel.openAsText)
  // deliberately reuses this exact { path } shape and the same .flow.json
  // path — it's the raw-JSON view of the file the canvas renders — but is
  // saved under a different id (`text:<path>`, not `file:<path>`; see
  // openFile). Without excluding that id prefix here, restoring a workspace
  // that persisted ONLY the text escape hatch (the canvas tab was closed)
  // would still classify it as 'flow', and openFile() would then dedupe to
  // the fixed id `file:<path>` — which doesn't exist yet — and spawn an
  // uninvited flow-canvas tab nobody asked to see again.
  if (params.path?.endsWith('.flow.json') && !panel.id?.startsWith('text:')) return 'flow'
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
          // `model` passes through unchecked on purpose, unlike `kind`: kind
          // decides what this side builds, while a model only ever reaches a
          // command line in main, which vets it against the same allowlist and
          // falls back to the CLI default on a miss (lib/agent-spawn.js).
          // Screening it here too would fork that rule into two copies.
          spawnTerminal({ kind, cwd: params.cwd, airgap: params.airgap, wsName: params.ws, model: params.model, saved: p })
        } else if (component === 'chat') {
          spawnChat(p)
        } else if (component === 'brain') {
          if (wsState.ws.workspaces.some((x) => x.name === params.ws)) spawnBrain(params.ws, p)
          else removePanel(p) // workspace gone — skip
        } else if (component === 'history') {
          const dir = typeof params.dir === 'string' && (await dirExists(params.dir)) ? params.dir : null
          if (dir) spawnHistory(dir, p)
          else removePanel(p)
        } else if (component === 'events') {
          // main owns the log file (userData) — nothing to existence-check.
          spawnEvents(p)
        } else if (component === 'runs') {
          // Runs live in main's memory for the life of the app, so a restored
          // pane comes back empty after a restart rather than stale — which is
          // the honest state: those child processes died with the last window.
          spawnRuns(p)
        } else if (component === 'editor' || component === 'doc' || component === 'flow') {
          // openFile() re-routes a .flow.json path to the flow component on
          // its own (see the check added there) — including the flow
          // canvas's "Open as text" escape hatch, which restores as
          // component 'editor' with a .flow.json path (componentOf keys off
          // its `text:` id prefix) and which openFile special-cases back to
          // a no-op reactivation via that same prefix. Neither case needs a
          // branch of its own here beyond being let through this condition.
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
  // dock.panels spans every window — popped-out and floating groups included.
  // Placing against the most recent pane overall meant that once anything was
  // torn off, the main window's ＋ opened panes into that other window.
  const grid = dock.panels.filter((p) => p.api.location.type === 'grid')
  const n = grid.length
  if (n === 0) return undefined
  return { referencePanel: grid[n - 1], direction: n % 2 ? 'right' : 'below' }
}

// A pane spawned into an existing group is a helper for what is already there,
// so it inherits that pane's working directory when it has one.
function targetCwd(target) {
  const params = target?.group?.activePanel?.params
  return typeof params?.cwd === 'string' ? params.cwd : null
}

export function addTerminal(kind, target) {
  return spawnTerminal({ kind, cwd: targetCwd(target), target })
}

// Shared by the ＋ menus, layout restore, and flow Run (flow.js — it needs
// the returned panel to read back .group so subsequent nodes in the same run
// stack as tabs alongside the first). `saved` carries the persisted id/title
// when recreating a pane from a stored layout.
export function spawnTerminal({ kind, cwd, airgap, wsName, saved, target, model }) {
  const id = `pty-${++counters.seq}`
  cwd = cwd || paneCwd()
  const name = cwd.split('/').pop() || cwd
  const isAgent = kind !== 'terminal'
  const gapped = airgap !== undefined ? !!airgap : isAgent && prefs.airgapDefault
  return dock.addPanel({
    id,
    component: 'terminal',
    title: saved?.title || (isAgent ? `${gapped ? '⛨ ' : ''}${kind} — ${name}` : `zsh — ${name}`),
    position: saved ? { referencePanel: saved.id } : place(target),
    // `model` (a flow node's pinned model) rides in params rather than being
    // passed straight to the pty, because params is what survives layout
    // persistence: a reopened window respawns its ptys from these and would
    // otherwise silently restore an agent on the wrong model. Written only
    // when set, so a persisted layout carries no key for the default case —
    // absent is the schema's spelling of "the CLI's own default".
    params: { ptyId: id, kind, cwd, airgap: gapped, ws: wsName ?? activeWorkspace()?.name, ...(model ? { model } : {}) },
  })
}

// A flow's "Run in terminals" types a node's bootstrap prompt into its
// freshly spawned terminal via this — and ONLY this. It never appends '\r'.
// This is the same no-auto-submit contract as the conductor's
// type_in_terminal with auto-run off (see conductor.js's allowRun gate): the
// user reviews what landed in the prompt and presses Enter themselves.
// Nothing typed into an interactive pane is ever submitted for the user, by
// a flow or by anything else.
//
// The narrowed contract, since background runs landed
// (docs/FEATURE-PLAN-background-flow-runs.md): the plain Run no longer comes
// through here at all — it hands the flow to main's runner, which submits the
// composed brief and nothing else, on an explicit Run click and nothing else,
// headless (`-p`, a process that answers and exits, leaving no session to
// type into), inside the same air gap a pane gets, with every transition in
// the event log. This path is what stayed: an interactive agent, driven by
// the user, with the brief pre-typed and unsubmitted.
//
// composeBootstrapPrompt embeds several hand-editable flow.json string
// fields verbatim (instructions/expects/produces, edge labels, output
// names) — a literal "\n" inside any of them would submit whatever text
// came before it the instant that byte reaches the pty (a bare embedded LF
// submits a shell line on its own, same as a trailing "\r" does), which
// would defeat the no-auto-submit contract above for exactly the case it
// exists to protect: a `kind: 'terminal'` node's pty is a plain shell.
// stripControlChars is conductor's own equivalent guard for model-typed text
// with auto-run off (see runTool's type_in_terminal in conductor.js) —
// reusing it here closes the same hole for flows.
export function typeIntoPanel(panel, text) {
  tome.pty.write(panel.params.ptyId, stripControlChars(text))
}

// `opts.chatId` pins the pane to a fixed id — the ambient voice session
// uses it to open its transcript ('chat-voice'), which must dedupe like the
// brain pane does: a second request focuses the existing tab instead of
// forking the shared chat-log-store key across two panels.
export function addChat(target, opts) {
  spawnChat(undefined, target, opts)
}

function spawnChat(saved, target, opts) {
  // A restored pane keeps its saved chatId so the persisted transcript
  // (chat-log-<chatId> in the store) lines back up with this pane.
  const id =
    typeof saved?.params?.chatId === 'string'
      ? saved.params.chatId
      : typeof opts?.chatId === 'string'
        ? opts.chatId
        : `chat-${++counters.seq}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return existing
  }
  return dock.addPanel({
    id,
    component: 'chat',
    title: saved?.title || opts?.title || 'assistant',
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

export function addEvents(target) {
  spawnEvents(undefined, target)
}

// One log pane at a time: fixed id, so a second request just activates the
// existing tab (same dedupe as brain/history).
function spawnEvents(saved, target) {
  const id = 'events:log'
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  dock.addPanel({
    id,
    component: 'events',
    title: saved?.title || 'event log',
    position: saved ? { referencePanel: saved.id } : place(target),
    params: { events: true },
  })
}

export function addRuns(target) {
  spawnRuns(undefined, target)
}

// One runs pane at a time, same fixed-id dedupe as the event log: both are
// app-wide views of main's state rather than views of a file, so a second
// request means "show me that", not "give me another one".
function spawnRuns(saved, target) {
  const id = 'runs:list'
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    return
  }
  dock.addPanel({
    id,
    component: 'runs',
    title: saved?.title || '▶ flow runs',
    position: saved ? { referencePanel: saved.id } : place(target),
    params: { runs: true },
  })
}

// Scans .tome/flows for existing `untitled-<n>.flow.json` files so a second
// (or third…) assistant-created flow doesn't collide with the first — same
// "lowest unused integer" rationale as flow-model.js's lowestUnusedId,
// applied to filenames instead of node/edge ids because a brand-new flow has
// no open panel yet to hold that bookkeeping in memory.
async function lowestUnusedFlowName(root) {
  let entries = []
  try {
    entries = await tome.fs.readDir(`${root}/.tome/flows`)
  } catch {
    // .tome/flows doesn't exist yet — every name is unused
  }
  const used = new Set()
  const re = /^untitled-(\d+)\.flow\.json$/
  for (const e of entries) {
    const m = re.exec(e.name)
    if (m) used.add(Number(m[1]))
  }
  let n = 1
  while (used.has(n)) n++
  return `untitled-${n}`
}

// Shared by the conductor's 'flow' open request (below) and the ＋ menu's
// "Flow diagram…" entry (menus.js) — both need to create a brand-new
// flow.json on disk before they can open it. A flow panel's params are just
// { path } (plan §2.4/§5): there's no such thing as a pathless flow pane the
// way addChat/addBrain mint a bare id and let the panel invent its own
// content, so componentOf() and restoreLayout would have nothing to point at
// if the file didn't exist yet — it has to be created eagerly, not lazily on
// first save.
export async function createFlowFile(root, name, target) {
  const path = `${root}/.tome/flows/${name}.flow.json`
  try {
    await tome.fs.mkdir(`${root}/.tome/flows`)
    // Exclusive create first (same 'wx' guard tree.js's createFileIn uses)
    // so a name collision surfaces as a clean toast instead of silently
    // clobbering another flow's file; the real JSON body goes in via a
    // second, ordinary write, since a brand-new flow isn't meant to start
    // out empty the way a new text file is.
    await tome.fs.createFile(path)
  } catch (err) {
    if (String(err.message).includes('EEXIST')) toast(`“${name}.flow.json” already exists`)
    else toast(`couldn't create “${name}.flow.json”: ${err.message}`)
    return
  }
  try {
    await tome.fs.writeFile(path, JSON.stringify(createFlow(name), null, 2) + '\n')
  } catch (err) {
    toast(`created “${name}.flow.json” but couldn't write its contents: ${err.message}`)
    return
  }
  openFile(path, undefined, target)
}

export async function addFlow(target) {
  if (!wsState.activeRoot) {
    toast('assistant asked for a flow pane, but no workspace folder is open')
    return
  }
  const name = await lowestUnusedFlowName(wsState.activeRoot)
  createFlowFile(wsState.activeRoot, name, target)
}

const IMG_EXT = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'ico'])
const CONV_EXT = new Set(['docx', 'xlsx', 'xls'])

export async function openFile(path, saved, target, reveal) {
  // Restoring the flow canvas's "Open as text" escape hatch (FlowPanel's
  // openAsText): it deliberately persists under `text:<path>`, not
  // `file:<path>`, precisely so it can coexist as its own tab alongside the
  // canvas. Every other caller either passes no `saved` at all, or a `saved`
  // whose id already IS the canonical `file:<path>` this function computes
  // just below — this is the one case where the persisted id doesn't match
  // that pattern, and falling through to the generic id/routing logic would
  // re-derive 'flow' from the .flow.json suffix and spawn/reattach an
  // uninvited flow canvas under `file:<path>`, instead of leaving this pane
  // alone (dock.fromJSON already recreated it under its own id — restoring
  // it is a no-op, exactly like the happy path below for every other kind).
  if (typeof saved?.id === 'string' && saved.id.startsWith('text:') && path.endsWith('.flow.json')) {
    dock.getPanel(saved.id)?.api.setActive()
    return
  }

  const id = `file:${path}`
  const existing = dock.getPanel(id)
  if (existing) {
    existing.api.setActive()
    // already open — jump to the requested position (go-to-definition)
    if (reveal) existing.view?.content?.reveal?.(reveal)
    return
  }
  const name = saved?.title || path.split('/').pop()
  const pos = saved ? { referencePanel: saved.id } : place(target)
  const ext = (name.includes('.') ? name.split('.').pop() : '').toLowerCase()

  // Compound extension — `ext` above is just 'json' — so test the full name.
  // Must precede the pdf/img/conv checks and the text/binary sniff below, or
  // a flow file opens as plain text (plan §5).
  if (name.endsWith('.flow.json')) {
    return dock.addPanel({ id, component: 'flow', title: name, position: pos, params: { path } })
  }

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
  dock.addPanel({ id, component: 'editor', title: name, position: pos, params: { path, reveal } })
}

setOpenFile(openFile)
