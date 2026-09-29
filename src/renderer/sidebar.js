// The sidebar: three views behind one tab strip.
//   Open     every pane as a row — agents first, each with a live status
//            lamp (working / needs you / idle / exited) and its containment.
//   Files    the existing tree (tree.js renders into #tree-body).
//   Changes  the working tree of the active repo: click a file to review
//            its diff, commit everything from the box at the bottom.
import { tome, el } from './util.js'
import { agState } from './state.js'
import { dock, closePanel, showDiff, openFile } from './panes.js'
import { commitAll, pushFlow } from './git.js'
import { newActivity, noteOutput, markSeen, sessionStatus } from '../shared/session-status.js'
import { closeIcon } from './icons.js'

// ---------- tabs ----------
const tabs = [...document.querySelectorAll('#side-tabs [role=tab]')]
const views = { open: 'side-open', files: 'side-files', changes: 'side-changes' }
let current = 'open'

export function showSideView(name) {
  if (!views[name]) return
  current = name
  document.body.classList.remove('tree-collapsed') // asking for a view means showing the sidebar
  for (const t of tabs) t.setAttribute('aria-selected', String(t.dataset.view === name))
  for (const [k, id] of Object.entries(views)) document.getElementById(id).classList.toggle('hidden', k !== name)
  tome.store.set('sidebar-view', name)
  if (name === 'changes') loadChanges()
}
for (const t of tabs) t.addEventListener('click', () => showSideView(t.dataset.view))
// Arrow keys move between tabs, as a tablist should.
document.getElementById('side-tabs').addEventListener('keydown', (e) => {
  if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return
  const i = tabs.findIndex((t) => t.dataset.view === current)
  const next = tabs[(i + (e.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length]
  showSideView(next.dataset.view)
  next.focus()
})
tome.store.get('sidebar-view').then((v) => v && v !== 'open' && showSideView(v)).catch(() => {})

// ---------- activity (fed by renderer.js's pty fan-out) ----------
const activity = new Map() // ptyId -> activity record
const act = (id) => activity.get(id) || activity.set(id, newActivity()).get(id)
export const onPtyData = (id, data) => noteOutput(act(id), Date.now(), data)
export function onPtyExit(id) {
  act(id).exited = true
  scheduleRender()
}

// ---------- Open ----------
const openEl = document.getElementById('side-open')
const openCount = document.getElementById('open-count')

const GROUPS = [
  ['agents', 'Agents'],
  ['terminals', 'Terminals'],
  ['files', 'Files'],
  ['tools', 'Tools'],
]
const componentName = (p) => p.view?.contentComponent || ''
function groupOf(p) {
  const c = componentName(p)
  const params = p.params || {}
  if (params.ptyId) return params.kind && params.kind !== 'terminal' ? 'agents' : 'terminals'
  if (['editor', 'doc', 'flow', 'diff'].includes(c) || params.path || params.diff) return 'files'
  return 'tools'
}

// Pane titles carry a leading glyph and a "— folder" suffix for the tab
// strip; the sidebar has room to show those as structure instead.
function splitTitle(title) {
  const clean = (title || '').replace(/^[^\p{L}\p{N}.]+/u, '')
  const [name, ...rest] = clean.split(' — ')
  return { name: name || 'untitled', where: rest.join(' — ') }
}

const STATUS_WORD = { working: 'working', attention: 'needs you', idle: 'idle', exited: 'exited' }

function containment(params) {
  if (!params.ptyId) return null
  if (!params.egress) return { cls: 'host', word: 'host network' }
  const open = agState.panes?.[params.ptyId]?.mode === 'open'
  if (open) return { cls: 'open', word: 'internet open' }
  return { cls: 'sealed', word: params.docker ? 'sealed · docker' : 'sealed' }
}

let lastSig = ''
let renderQueued = false
export function scheduleRender() {
  if (renderQueued) return
  renderQueued = true
  requestAnimationFrame(() => {
    renderQueued = false
    renderOpen()
  })
}

function renderOpen(force) {
  const now = Date.now()
  const active = dock.activePanel
  if (active?.params?.ptyId && activity.has(active.params.ptyId)) markSeen(activity.get(active.params.ptyId))
  const rows = dock.panels.map((p) => {
    const ptyId = p.params?.ptyId
    const status = ptyId ? sessionStatus(act(ptyId), now) : null
    return { p, group: groupOf(p), status, seal: containment(p.params || {}), ...splitTitle(p.title) }
  })
  // Rebuild only when something visible changed — this runs on a timer.
  const sig = rows.map((r) => [r.p.id, r.name, r.where, r.status, r.seal?.cls, r.p === active].join('|')).join('\n')
  if (!force && sig === lastSig) return
  lastSig = sig
  const needYou = rows.filter((r) => r.status === 'attention').length
  openCount.textContent = rows.length ? String(rows.length) : ''
  openCount.classList.toggle('attn', needYou > 0)
  openCount.title = needYou ? `${needYou} waiting for you` : ''

  openEl.replaceChildren()
  if (!rows.length) {
    openEl.appendChild(el('p', 'side-empty', 'Nothing open yet. Start an agent with New, or press ⌘K.'))
    return
  }
  for (const [key, label] of GROUPS) {
    const inGroup = rows.filter((r) => r.group === key)
    if (!inGroup.length) continue
    const head = el('div', 'side-label')
    head.append(el('span', '', label), el('span', 'side-label-n', String(inGroup.length)))
    openEl.appendChild(head)
    for (const r of inGroup) openEl.appendChild(openRow(r, r.p === active))
  }
}

function openRow(r, isActive) {
  const row = el('div', 'open-row' + (isActive ? ' active' : '') + (r.status ? ` st-${r.status}` : ''))
  row.tabIndex = 0
  row.setAttribute('role', 'button')
  const lamp = el('span', 'lamp' + (r.status ? ` lamp-${r.status}` : ''))
  const text = el('span', 'open-text')
  text.appendChild(el('span', 'open-name', r.name))
  const sub = []
  if (r.status && r.status !== 'idle') sub.push(STATUS_WORD[r.status])
  if (r.where) sub.push(r.where)
  if (sub.length) text.appendChild(el('span', 'open-sub', sub.join(' · ')))
  row.append(lamp, text)
  if (r.seal) {
    const s = el('span', `seal-tag seal-${r.seal.cls}`, r.seal.cls === 'sealed' ? 'sealed' : r.seal.cls === 'open' ? 'open' : 'host')
    s.title = r.seal.word
    row.appendChild(s)
  }
  const x = el('button', 'open-close')
  x.appendChild(closeIcon())
  x.title = 'Close'
  x.setAttribute('aria-label', `Close ${r.name}`)
  x.addEventListener('click', (e) => {
    e.stopPropagation()
    closePanel(r.p)
  })
  row.appendChild(x)
  row.title = [r.name, r.where, r.status && STATUS_WORD[r.status], r.seal?.word].filter(Boolean).join(' · ')
  row.setAttribute('aria-label', row.title)
  const go = () => r.p.api.setActive()
  row.addEventListener('click', go)
  row.addEventListener('keydown', (e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), go()))
  row.addEventListener('auxclick', (e) => e.button === 1 && closePanel(r.p))
  return row
}

// Agents that finished while you were elsewhere, in order — ⌘J jumps to the
// next one (the Linear-inbox move: triage what is waiting, one key at a time).
export function jumpToWaiting() {
  const now = Date.now()
  const next = dock.panels.find((p) => p.params?.ptyId && sessionStatus(act(p.params.ptyId), now) === 'attention')
  next?.api.setActive()
  return !!next
}

dock.onDidAddPanel(scheduleRender)
dock.onDidRemovePanel((p) => {
  if (p.params?.ptyId) activity.delete(p.params.ptyId)
  scheduleRender()
})
dock.onDidActivePanelChange(scheduleRender)
dock.onDidLayoutChange(scheduleRender) // titles (dirty markers), moves
setInterval(() => renderOpen(), 1000)
renderOpen(true)

// ---------- Changes ----------
const changesEl = document.getElementById('side-changes')
const changesCount = document.getElementById('changes-count')
let repo = null // { dir, branch, ahead, behind, ... } from git.js's poll
let files = []
let infoSig = ''
let draft = ''

window.addEventListener('git:info', (e) => {
  repo = e.detail
  const n = repo ? (repo.added || 0) + (repo.modified || 0) + (repo.deleted || 0) : 0
  changesCount.textContent = n ? String(n) : ''
  const sig = repo ? [repo.dir, repo.branch, repo.added, repo.modified, repo.deleted, repo.ahead].join('|') : ''
  if (sig !== infoSig) {
    infoSig = sig
    if (current === 'changes') loadChanges()
  }
})

async function loadChanges() {
  if (!repo) {
    files = []
    return renderChanges()
  }
  try {
    files = (await tome.git.status(repo.dir))?.files || []
  } catch {
    files = []
  }
  renderChanges()
}

// porcelain XY → the one letter a reviewer cares about
function code(f) {
  if (f.x === '?' || f.y === '?') return ['U', 'g-add', 'untracked']
  const c = f.y !== ' ' ? f.y : f.x
  return { A: ['A', 'g-add', 'added'], D: ['D', 'g-del', 'deleted'], R: ['R', 'g-mod', 'renamed'] }[c] || ['M', 'g-mod', 'modified']
}

function renderChanges() {
  changesEl.replaceChildren()
  if (!repo) {
    changesEl.appendChild(el('p', 'side-empty', 'Open a folder that is a git repository to review changes here.'))
    return
  }
  const head = el('div', 'side-label')
  head.append(el('span', '', repo.branch || 'detached'), el('span', 'side-label-n', files.length ? `${files.length} changed` : ''))
  changesEl.appendChild(head)
  const list = el('div', 'changes-list')
  if (!files.length) list.appendChild(el('p', 'side-empty', 'Working tree clean.'))
  for (const f of files) {
    const [letter, cls, word] = code(f)
    const path = f.path.includes(' -> ') ? f.path.split(' -> ').pop() : f.path
    const at = path.lastIndexOf('/')
    const row = el('div', 'change-row')
    row.tabIndex = 0
    row.setAttribute('role', 'button')
    row.title = `${path} — ${word}. Click to review, double-click to open.`
    const name = el('span', 'change-name', path.slice(at + 1))
    const dir = el('span', 'change-dir', at > 0 ? path.slice(0, at) : '')
    row.append(el('span', `change-code ${cls}`, letter), name, dir)
    row.addEventListener('click', () => showDiff(repo.dir, path))
    row.addEventListener('dblclick', () => openFile(`${repo.dir}/${path}`))
    row.addEventListener('keydown', (e) => e.key === 'Enter' && showDiff(repo.dir, path))
    list.appendChild(row)
  }
  changesEl.appendChild(list)

  const box = el('div', 'commit-box')
  const msg = el('textarea', 'commit-msg')
  msg.rows = 2
  msg.placeholder = 'Commit message'
  msg.value = draft
  msg.setAttribute('aria-label', 'Commit message')
  const commit = el('button', 'btn-primary', files.length ? `Commit ${files.length} file${files.length === 1 ? '' : 's'}` : 'Commit')
  const sync = () => {
    draft = msg.value
    commit.disabled = !files.length || !msg.value.trim()
  }
  msg.addEventListener('input', sync)
  const doCommit = () =>
    !commit.disabled &&
    commitAll(repo.dir, msg.value.trim(), files, () => {
      draft = ''
      loadChanges()
    })
  msg.addEventListener('keydown', (e) => e.key === 'Enter' && (e.metaKey || e.ctrlKey) && (e.preventDefault(), doCommit()))
  commit.addEventListener('click', doCommit)
  commit.title = 'Stage everything and commit (⌘Enter)'
  const push = el('button', 'btn-quiet', repo.ahead ? `Push ↑${repo.ahead}` : 'Push')
  push.addEventListener('click', () => pushFlow())
  const actions = el('div', 'commit-actions')
  actions.append(commit, push)
  box.append(msg, actions)
  changesEl.appendChild(box)
  sync()
}
renderChanges()
