// Brain pane: per-workspace note vault — list, editor with [[wikilinks]],
// backlinks, promote-to-core, and a force-directed graph view.
import { basicSetup } from 'codemirror'
import { Decoration, EditorView, MatchDecorator, ViewPlugin, keymap } from '@codemirror/view'
import { EditorState } from '@codemirror/state'
import { LanguageDescription } from '@codemirror/language'
import { languages } from '@codemirror/language-data'
import { oneDark } from '@codemirror/theme-one-dark'
import { tome, toast } from '../util.js'
import { brains } from '../regs.js'
import { modalShell } from '../modals.js'

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

export class BrainPanel {
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
