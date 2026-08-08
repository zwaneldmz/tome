// Flow pane: an editable `<name>.flow.json` graph (see flow-model.js and
// docs/FEATURE-PLAN-file-creation-and-flows.md §2.4, §5, and the "Slice 5"
// entry in §3's build order). Nodes are absolutely-positioned cards; edges
// are a single SVG layer underneath them; panning translates a content
// wrapper. This slice adds node drag, edge drawing, a node editor modal,
// add/delete, save + dirty guard, and a live disk-change watch. Run (§2.5)
// is still a later commit.
import { tome, el, toast } from '../util.js'
import { validateFlow, addNode, addEdge, edgeError } from '../flow-model.js'
import { dock } from '../panes.js'
import { modalShell, confirmModal } from '../modals.js'
import { AGENTS } from '../../shared/pane-kinds.js'

const SVG_NS = 'http://www.w3.org/2000/svg'

// Fixed card footprint. v1 has no auto-layout (plan §4 non-goal), so a node
// keeps exactly the x/y the file gives it — PAD is just margin around the
// computed bounding box so cards near (0,0) aren't flush against the
// viewport edge.
const NODE_W = 220
const NODE_H = 140
const PAD = 56

// Smallest bounding box that contains every node's card, plus the offset
// needed to shift a graph with negative/zero-based coordinates into
// positive space. Kept pure (no DOM) so it's easy to reason about — the
// caller applies `minX`/`minY` as a subtraction when placing each card.
function layoutBBox(nodes) {
  let minX = 0
  let minY = 0
  let maxX = NODE_W
  let maxY = NODE_H
  for (const n of nodes) {
    const x = Number.isFinite(n.x) ? n.x : 0
    const y = Number.isFinite(n.y) ? n.y : 0
    minX = Math.min(minX, x)
    minY = Math.min(minY, y)
    maxX = Math.max(maxX, x + NODE_W)
    maxY = Math.max(maxY, y + NODE_H)
  }
  return { minX, minY, width: maxX - minX + PAD, height: maxY - minY + PAD }
}

function anchorOf(dot, wrapperRect) {
  const r = dot.getBoundingClientRect()
  return { x: r.left + r.width / 2 - wrapperRect.left, y: r.top + r.height / 2 - wrapperRect.top }
}

// Horizontal cubic bezier: control points are pulled toward the midpoint so
// the curve always leaves an output port heading right and arrives at an
// input port heading in from the left, regardless of where the target sits
// (above, below, or to the left of the source).
function edgePathD(a, b) {
  const pull = Math.max(40, Math.abs(b.x - a.x) / 2)
  return `M ${a.x} ${a.y} C ${a.x + pull} ${a.y}, ${b.x - pull} ${b.y}, ${b.x} ${b.y}`
}

function buildEdgePath(a, b) {
  const path = document.createElementNS(SVG_NS, 'path')
  path.setAttribute('class', 'flow-edge-path')
  path.setAttribute('d', edgePathD(a, b))
  return path
}

// An SVG <text> rather than a positioned div: it lives in the exact same
// coordinate space as the path it labels (the content wrapper's untransformed
// pixel space — see recomputeEdges), so there's no separate offset math to
// keep in sync.
function buildEdgeLabel(a, b, text) {
  const t = document.createElementNS(SVG_NS, 'text')
  t.setAttribute('class', 'flow-edge-label')
  t.setAttribute('x', String((a.x + b.x) / 2))
  t.setAttribute('y', String((a.y + b.y) / 2 - 6))
  t.setAttribute('text-anchor', 'middle')
  t.textContent = text
  return t
}

export class FlowPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-flow'
  }

  async init({ params, api }) {
    this.path = params.path
    this.name = this.path.split('/').pop()
    this.api = api
    this.pan = { x: 0, y: 0 }
    this.dirty = false
    this.diskConflict = false
    this.watched = false

    let text
    try {
      text = await tome.fs.readFile(this.path)
    } catch (err) {
      return this.renderError(`could not read ${this.name}: ${err.message}`)
    }

    let flow
    try {
      flow = JSON.parse(text)
      if (!flow || typeof flow !== 'object') throw new Error('not a JSON object')
      // Hand-edited files may be missing these entirely — normalize before
      // validateFlow/rendering ever see the document, rather than have every
      // later `for (const n of flow.nodes)` need its own guard.
      if (!Array.isArray(flow.nodes)) flow.nodes = []
      if (!Array.isArray(flow.edges)) flow.edges = []
    } catch (err) {
      return this.renderError(`"${this.name}" is not a valid flow file: ${err.message}`)
    }

    let errors, warnings
    try {
      ;({ errors, warnings } = validateFlow(flow))
    } catch (err) {
      // Belt-and-suspenders: validateFlow assumes each node/edge is at least
      // an object with the expected fields. A hand-edited file that violates
      // that (e.g. a node that's a bare string) shouldn't take the panel down
      // with it — surface it the same way a parse failure is.
      return this.renderError(`"${this.name}" could not be read as a flow: ${err.message}`)
    }

    // Errors mean the graph itself can't be trusted (dangling ids, duplicate
    // ids, …) — rendering would mean guessing. Warnings mean only the
    // declared contract is off (a stale port name, an unknown kind); the
    // graph still stands, and hand-edited files must still open (plan §2.2).
    if (errors.length) {
      return this.renderError(
        `"${this.name}" has ${errors.length} structural ${errors.length === 1 ? 'problem' : 'problems'} and can't be rendered:`,
        errors
      )
    }

    this.flow = flow
    this.savedText = text
    this.renderGraph(warnings)
    // Recompute anchors whenever this panel's own box changes — including
    // the very first real layout pass. A pane restored into a background tab
    // (or a fresh one not yet painted) measures 0×0 at init time, which would
    // otherwise bake in degenerate edge paths.
    api.onDidDimensionsChange(() => this.recomputeEdges())

    // Editing surface: wired only once the graph actually renders — the
    // error/unrecoverable states above have nothing to save, select, or
    // delete, and dispose() checks this.onKeyDown before removing it, so
    // never setting it there is safe.
    const doc = this.element.ownerDocument
    this.onKeyDown = (e) => {
      // The canvas is plain divs, not focusable elements, so a keydown's
      // target is almost never inside this.element even right after
      // clicking a card to select it — DOM focus containment can't be the
      // scope check. api.isActive ("is this the currently selected pane in
      // the grid") is false for every other open pane, so a Backspace typed
      // while an unrelated editor tab is focused can no longer reach into a
      // background flow pane and delete whatever it last had selected, and a
      // ⌘S elsewhere can't silently re-save this one. The node editor modal
      // doesn't belong to any dockview panel's own content area, so opening
      // it never changes which pane is active — this stays true while it's
      // up, which is what lets ⌘S still save from inside it.
      if (!this.api?.isActive) return
      const mod = e.metaKey || e.ctrlKey
      if (mod && e.key.toLowerCase() === 's') {
        e.preventDefault()
        this.save()
        return
      }
      if (e.key !== 'Delete' && e.key !== 'Backspace') return
      // The node editor modal lives in this same document — Backspace while
      // typing a name/instructions/port name must edit the text, not delete
      // whatever node/edge happens to be selected underneath it.
      const tag = e.target?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return
      if (!this.selectedNodeId && !this.selectedEdgeId) return
      e.preventDefault()
      this.deleteSelection()
    }
    doc.addEventListener('keydown', this.onKeyDown)

    // Watch for external edits — a hand-edited flow.json, or a flow written
    // by another pane/process. Refcounted in main, same as editor.js.
    tome.fs.watch(this.path)
    this.watched = true
    flowPanels.add(this)
  }

  renderError(message, details) {
    const box = el('div', 'flow-error')
    box.appendChild(el('p', null, message))
    if (details && details.length) {
      const list = el('ul', 'flow-error-list')
      for (const d of details) list.appendChild(el('li', null, d))
      box.appendChild(list)
    }
    const btn = el('button', null, 'Open as text')
    btn.addEventListener('click', () => this.openAsText())
    box.appendChild(btn)
    this.element.appendChild(box)
  }

  // Builds the whole graph section (toolbar, warning strip, viewport/nodes/
  // edges) from this.flow — used both for the initial load and for a silent
  // reload() when the file changes on disk underneath a clean pane. Resets
  // every per-render lookup table so a reload can't leave stale entries
  // (a removed node's port dots, a selection that no longer exists) behind.
  renderGraph(warnings) {
    this.loadWarnings = warnings
    this.selectedNodeId = null
    this.selectedEdgeId = null
    // "dir:nodeId:portName" -> the port's dot element, filled in while
    // building node cards. Keyed rather than looked up via querySelector so
    // recomputeEdges never has to build a CSS attribute selector out of a
    // node/port name that came straight from a (possibly hand-edited, so
    // untrusted) flow.json file.
    this.portDots = new Map()
    // nodeId -> Set of the portDots keys that node currently owns, so a
    // rename/removal/delete can drop exactly its own stale entries without
    // parsing them back out of the composite key string.
    this.portKeysByNode = new Map()
    this.nodeCards = new Map() // nodeId -> its card element

    this.element.appendChild(this.buildToolbar())
    this.warningStripEl = null
    this.refreshWarningStrip()

    this.viewportEl = el('div', 'flow-viewport')
    this.contentEl = el('div', 'flow-content')
    this.viewportEl.appendChild(this.contentEl)
    this.element.appendChild(this.viewportEl)

    this.svg = document.createElementNS(SVG_NS, 'svg')
    this.svg.setAttribute('class', 'flow-edges')
    // Edges are one SVG layer, appended before any node card, so nodes paint
    // on top of it in normal DOM stacking order (plan §2.4).
    this.contentEl.appendChild(this.svg)

    // Origin is derived once here and then FROZEN for the life of this
    // render pass — buildNodeCard and every drag update below convert
    // between model (node.x/y) and card (left/top) coordinates through this
    // same fixed offset, so a node dragged mid-session can't have the ground
    // shift under it. layoutBBox only runs again on the next open (or the
    // next silent reload), which is when it's safe to renormalize.
    const box = layoutBBox(this.flow.nodes)
    this.origin = { x: box.minX, y: box.minY }
    this.contentEl.style.width = `${box.width}px`
    this.contentEl.style.height = `${box.height}px`
    this.svg.setAttribute('width', String(box.width))
    this.svg.setAttribute('height', String(box.height))

    for (const node of this.flow.nodes) this.contentEl.appendChild(this.buildNodeCard(node))
    if (this.flow.nodes.length === 0) {
      this.contentEl.appendChild(el('div', 'flow-empty', 'this flow has no nodes yet'))
    }

    this.wirePan()
    this.applyPan()
    this.recomputeEdges()
  }

  buildToolbar() {
    const bar = el('div', 'flow-toolbar')
    this.metaEl = el('span', 'flow-meta')
    bar.append(el('span', 'flow-name', this.flow.name || this.name), this.metaEl)
    this.updateToolbarCounts()

    const actions = el('div', 'flow-toolbar-actions')
    const addBtn = el('button', 'flow-add-node', '＋ node')
    addBtn.type = 'button'
    addBtn.addEventListener('click', () => this.addNodeAction())
    const saveBtn = el('button', 'flow-save', 'Save')
    saveBtn.type = 'button'
    saveBtn.addEventListener('click', () => this.save())
    const openText = el('button', 'flow-open-text', 'Open as text')
    openText.type = 'button'
    openText.addEventListener('click', () => this.openAsText())
    actions.append(addBtn, saveBtn, openText)
    bar.appendChild(actions)
    return bar
  }

  updateToolbarCounts() {
    if (!this.metaEl) return
    const n = this.flow.nodes.length
    const e = this.flow.edges.length
    this.metaEl.textContent = `${n} node${n === 1 ? '' : 's'} · ${e} edge${e === 1 ? '' : 's'}`
  }

  buildWarningStrip(warnings) {
    const strip = el('div', 'flow-warning-strip')
    const body = el('div', 'flow-warning-body')
    body.appendChild(
      el(
        'div',
        'flow-warning-head',
        `${warnings.length} ${warnings.length === 1 ? 'issue' : 'issues'} in this file — it still opened:`
      )
    )
    const list = el('ul', 'flow-warning-list')
    for (const w of warnings) list.appendChild(el('li', null, w))
    body.appendChild(list)
    const dismiss = el('button', 'flow-warning-dismiss', '✕')
    dismiss.title = 'Dismiss'
    dismiss.setAttribute('aria-label', 'Dismiss warnings')
    // Session-only: the warnings describe the file's state, not any state of
    // ours, so dismissing just hides the strip for now — refreshWarningStrip
    // (a reload, or a new disk conflict) rebuilds it from scratch and shows
    // it again, which is the point.
    dismiss.addEventListener('click', () => {
      strip.remove()
      if (this.warningStripEl === strip) this.warningStripEl = null
    })
    strip.append(body, dismiss)
    return strip
  }

  // Combines the file-parse warnings computed once at load/reload with the
  // live disk-conflict notice, which can appear after the fact (a dirty pane
  // whose file changed underneath it) — so this can be called again later
  // without redoing the parse.
  refreshWarningStrip() {
    const warnings = [...(this.loadWarnings || [])]
    if (this.diskConflict) warnings.push('file changed on disk — saving will overwrite the newer version')
    this.warningStripEl?.remove()
    this.warningStripEl = null
    if (!warnings.length) return
    const strip = this.buildWarningStrip(warnings)
    // this.viewportEl may not exist yet on the very first call (buildToolbar
    // has run, the viewport hasn't) — insertBefore(strip, null) is then just
    // an append, which lands the strip right after the toolbar either way.
    this.element.insertBefore(strip, this.viewportEl || null)
    this.warningStripEl = strip
  }

  buildNodeCard(node) {
    const card = el('div', 'flow-node')
    const x = (Number.isFinite(node.x) ? node.x : 0) - this.origin.x + PAD / 2
    const y = (Number.isFinite(node.y) ? node.y : 0) - this.origin.y + PAD / 2
    card.style.left = `${x}px`
    card.style.top = `${y}px`

    const head = el('div', 'flow-node-head')
    head.append(
      el('span', 'flow-kind-badge', node.kind || '?'),
      el('span', 'flow-node-name', node.name || node.id || '')
    )
    const body = el('div', 'flow-node-body', node.instructions || '')

    const portsIn = el('div', 'flow-ports flow-ports-in')
    for (const input of node.inputs || []) portsIn.appendChild(this.buildPort(node.id, input?.name, 'in'))

    const portsOut = el('div', 'flow-ports flow-ports-out')
    for (const output of node.outputs || []) portsOut.appendChild(this.buildPort(node.id, output?.name, 'out'))

    card.append(head, body, portsIn, portsOut)
    this.nodeCards.set(node.id, card)
    this.wireNodeInteraction(card, node)
    return card
  }

  buildPort(nodeId, name, dir) {
    const port = el('div', `flow-port flow-port-${dir}`)
    const dot = el('span', 'flow-port-dot')
    // Plain data attributes, not a reverse lookup structure — edge drawing
    // needs to go from "the element under the pointer" back to a node id +
    // port name (via elementFromPoint), which a Map keyed the other way
    // round can't answer directly.
    dot.dataset.dir = dir
    dot.dataset.nodeId = nodeId
    const label = el('span', 'flow-port-label', name || '')
    // Input ports read dot-then-label (dot flush on the card's left edge);
    // output ports read label-then-dot (dot flush on the right) — see the
    // .flow-ports-in/.flow-ports-out CSS, which relies on this DOM order
    // instead of flex-direction: row-reverse.
    if (dir === 'in') port.append(dot, label)
    else port.append(label, dot)
    if (name) {
      dot.dataset.port = name
      const key = `${dir}:${nodeId}:${name}`
      this.portDots.set(key, dot)
      let keys = this.portKeysByNode.get(nodeId)
      if (!keys) {
        keys = new Set()
        this.portKeysByNode.set(nodeId, keys)
      }
      keys.add(key)
      // Only an output port starts a drag — an input port is purely a drop
      // target, discovered via elementFromPoint on pointerup/pointermove.
      if (dir === 'out') dot.addEventListener('pointerdown', (e) => this.beginEdgeDrag(e, nodeId, name))
    }
    return port
  }

  // Drag-vs-click, mirroring brain.js's graphMouseDown/Move/Up: a pointerdown
  // starts tentatively and only becomes a drag once the pointer has moved
  // past a small threshold, so an ordinary click (which always jitters a
  // pixel or two) still opens the node editor instead of being swallowed as
  // a zero-distance drag.
  wireNodeInteraction(card, node) {
    const THRESH = 4
    card.addEventListener('pointerdown', (e) => {
      if (e.button !== 0) return
      if (e.target.closest('.flow-port-dot')) return // ports run their own gesture (edge drawing)
      e.stopPropagation() // don't let wirePan also treat this as a canvas pan
      const startClientX = e.clientX
      const startClientY = e.clientY
      const startLeft = parseFloat(card.style.left) || 0
      const startTop = parseFloat(card.style.top) || 0
      let dragging = false
      card.setPointerCapture(e.pointerId)

      const onMove = (e2) => {
        const dx = e2.clientX - startClientX
        const dy = e2.clientY - startClientY
        if (!dragging) {
          if (Math.abs(dx) < THRESH && Math.abs(dy) < THRESH) return
          dragging = true
          card.classList.add('dragging')
        }
        const left = startLeft + dx
        const top = startTop + dy
        card.style.left = `${left}px`
        card.style.top = `${top}px`
        // Convert back through the frozen origin (see renderGraph) — a drag
        // that ends up beyond the graph's original top-left can produce
        // negative model coordinates, which is fine: layoutBBox renormalizes
        // the whole graph the next time this file is opened (or reloaded).
        node.x = left + this.origin.x - PAD / 2
        node.y = top + this.origin.y - PAD / 2
        this.recomputeEdges()
      }
      const onUp = () => {
        card.removeEventListener('pointermove', onMove)
        card.removeEventListener('pointerup', onUp)
        card.removeEventListener('pointercancel', onUp)
        try {
          card.releasePointerCapture(e.pointerId)
        } catch {
          /* already released — fine */
        }
        if (dragging) {
          card.classList.remove('dragging')
          this.markDirty()
        } else {
          this.selectNode(node.id)
          this.openNodeEditor(node)
        }
      }
      card.addEventListener('pointermove', onMove)
      card.addEventListener('pointerup', onUp)
      card.addEventListener('pointercancel', onUp)
    })
  }

  // Pointerdown on an output port dot: draws a temp dashed path that follows
  // the pointer until release. The move/up/keydown listeners live on the
  // panel's ownerDocument (not this.element) because the pointer routinely
  // leaves the dot, the card, and even the viewport while drawing — but
  // that means they must be torn down explicitly, both when the drag ends
  // normally and in dispose() if the pane closes mid-drag.
  beginEdgeDrag(e, fromNodeId, fromOutput) {
    if (e.button !== 0) return
    e.stopPropagation() // keep this from also starting a card drag or canvas pan
    const doc = this.element.ownerDocument
    const fromDot = this.portDots.get(`out:${fromNodeId}:${fromOutput}`)
    if (!fromDot) return

    const temp = document.createElementNS(SVG_NS, 'path')
    temp.setAttribute('class', 'flow-edge-temp')
    this.svg.appendChild(temp)

    let hovered = null
    const setHovered = (dot) => {
      if (hovered === dot) return
      hovered?.classList.remove('flow-port-hover')
      hovered = dot
      hovered?.classList.add('flow-port-hover')
    }
    const contentPoint = (ev) => {
      const r = this.contentEl.getBoundingClientRect()
      return { x: ev.clientX - r.left, y: ev.clientY - r.top }
    }
    // elementFromPoint on the panel's own document — a popped-out pane has
    // its own document/coordinate space, and hit-testing the wrong one would
    // silently miss every port.
    const inputDotAt = (ev) => {
      const under = doc.elementFromPoint(ev.clientX, ev.clientY)
      const dot = under?.closest?.('.flow-port-dot')
      return dot && dot.dataset.dir === 'in' ? dot : null
    }
    const draw = (ev) => {
      const a = anchorOf(fromDot, this.contentEl.getBoundingClientRect())
      temp.setAttribute('d', edgePathD(a, contentPoint(ev)))
    }
    draw(e)

    const onMove = (ev) => {
      draw(ev)
      setHovered(inputDotAt(ev))
    }
    const finish = (ev) => {
      doc.removeEventListener('pointermove', onMove)
      doc.removeEventListener('pointerup', onUp)
      doc.removeEventListener('keydown', onKey)
      this.edgeDragCleanup = null
      setHovered(null)
      temp.remove()
      if (!ev) return // cancelled — Escape, or the pane closing mid-drag
      const dot = inputDotAt(ev)
      if (!dot) return // released over empty canvas — no edge
      const edge = { from: fromNodeId, fromOutput, to: dot.dataset.nodeId, toInput: dot.dataset.port, label: '' }
      const error = edgeError(this.flow, edge)
      if (error) {
        toast(error)
        return
      }
      addEdge(this.flow, edge)
      this.markDirty()
      this.recomputeEdges()
    }
    const onUp = (ev) => finish(ev)
    const onKey = (ev) => {
      if (ev.key !== 'Escape') return
      ev.preventDefault()
      finish(null)
    }
    doc.addEventListener('pointermove', onMove)
    doc.addEventListener('pointerup', onUp)
    doc.addEventListener('keydown', onKey)
    this.edgeDragCleanup = () => finish(null)
  }

  selectNode(id) {
    this.selectedNodeId = id
    this.selectedEdgeId = null
    this.applySelection()
  }

  selectEdge(id) {
    this.selectedEdgeId = id
    this.selectedNodeId = null
    this.applySelection()
  }

  clearSelection() {
    if (!this.selectedNodeId && !this.selectedEdgeId) return
    this.selectedNodeId = null
    this.selectedEdgeId = null
    this.applySelection()
  }

  applySelection() {
    for (const [id, card] of this.nodeCards) card.classList.toggle('selected', id === this.selectedNodeId)
    // recomputeEdges is cheap (it just redraws the SVG from this.flow.edges)
    // and is the only place that knows how to paint .selected onto an edge
    // path, so re-running it is simpler than hunting down the live element.
    this.recomputeEdges()
  }

  async deleteSelection() {
    if (this.selectedEdgeId) {
      const id = this.selectedEdgeId
      this.flow.edges = this.flow.edges.filter((edge) => edge.id !== id)
      this.selectedEdgeId = null
      this.markDirty()
      this.recomputeEdges()
      this.updateToolbarCounts()
      return
    }
    if (!this.selectedNodeId) return
    const nodeId = this.selectedNodeId
    const node = this.flow.nodes.find((n) => n.id === nodeId)
    if (!node) return
    const edgeCount = this.flow.edges.filter((edge) => edge.from === nodeId || edge.to === nodeId).length
    if (edgeCount > 0) {
      const ok = await confirmModal(
        'Delete node…',
        `“${node.name || node.id}” has ${edgeCount} edge${edgeCount === 1 ? '' : 's'} — deleting it also removes ${edgeCount === 1 ? 'that edge' : 'those edges'}.`,
        'Delete',
        this.element.ownerDocument
      )
      if (!ok) return
    }
    // The confirm is async — re-check the selection is still this node
    // before mutating (the user could have picked something else, or the
    // file could have reloaded, while the prompt was up).
    if (this.selectedNodeId !== nodeId) return
    this.flow.nodes = this.flow.nodes.filter((n) => n.id !== nodeId)
    this.flow.edges = this.flow.edges.filter((edge) => edge.from !== nodeId && edge.to !== nodeId)
    for (const key of this.portKeysByNode.get(nodeId) || []) this.portDots.delete(key)
    this.portKeysByNode.delete(nodeId)
    this.nodeCards.get(nodeId)?.remove()
    this.nodeCards.delete(nodeId)
    this.selectedNodeId = null
    this.markDirty()
    this.recomputeEdges()
    this.updateToolbarCounts()
  }

  addNodeAction() {
    if (!this.flow) return
    // Drop the new node centered on whatever's currently in view, in model
    // coordinates: invert applyPan's translate to find the content-local
    // point under the viewport's center, offset by half the card footprint
    // so the card is centered rather than top-left-anchored there, then
    // invert the same origin/PAD offset buildNodeCard uses to place a card
    // from node.x/y.
    const rect = this.viewportEl.getBoundingClientRect()
    const contentX = rect.width / 2 - this.pan.x - NODE_W / 2
    const contentY = rect.height / 2 - this.pan.y - NODE_H / 2
    const node = addNode(this.flow, {
      kind: 'claude',
      name: 'untitled',
      instructions: '',
      expects: '',
      produces: '',
      inputs: [],
      outputs: [],
      x: contentX + this.origin.x - PAD / 2,
      y: contentY + this.origin.y - PAD / 2,
    })
    this.contentEl.appendChild(this.buildNodeCard(node))
    this.markDirty()
    this.recomputeEdges()
    this.updateToolbarCounts()
    this.selectNode(node.id)
    // A fresh node named "untitled" with no instructions is useless until
    // edited — open the editor immediately instead of waiting for a second
    // click.
    this.openNodeEditor(node)
  }

  // name/description rows for one port column (Inputs or Outputs) inside the
  // node editor modal. Returns the wrapper element plus a rows() reader so
  // the modal's Save handler can pull the current values back out.
  buildPortEditor(label, singular, initialPorts) {
    const wrap = el('div', 'flow-port-editor')
    wrap.appendChild(el('div', 'flow-port-editor-head', label))
    const rowsEl = el('div', 'flow-port-editor-rows')
    wrap.appendChild(rowsEl)

    const addRow = (port) => {
      const row = el('div', 'flow-port-row')
      const nameInput = el('input')
      nameInput.type = 'text'
      nameInput.placeholder = 'name'
      nameInput.value = port?.name || ''
      const descInput = el('input')
      descInput.type = 'text'
      descInput.placeholder = 'description'
      descInput.value = port?.description || ''
      const removeBtn = el('button', 'flow-port-row-remove', '✕')
      removeBtn.type = 'button'
      removeBtn.title = `remove this ${singular}`
      removeBtn.addEventListener('click', () => row.remove())
      row.append(nameInput, descInput, removeBtn)
      rowsEl.appendChild(row)
    }
    for (const p of initialPorts) addRow(p)

    const addBtn = el('button', 'flow-port-editor-add', `+ ${singular}`)
    addBtn.type = 'button'
    addBtn.addEventListener('click', () => addRow())
    wrap.appendChild(addBtn)

    return {
      element: wrap,
      rows: () =>
        [...rowsEl.querySelectorAll('.flow-port-row')]
          .map((row) => {
            const [nameInput, descInput] = row.querySelectorAll('input')
            return { name: nameInput.value.trim(), description: descInput.value.trim() }
          })
          // An unnamed port can't be referenced by an edge (edgeError and the
          // portDots keys both key off the name) — drop it rather than
          // saving a row that could never be wired to anything.
          .filter((p) => p.name),
    }
  }

  openNodeEditor(node) {
    const doc = this.element.ownerDocument
    const m = modalShell('Edit node', undefined, doc)
    m.err.remove() // nothing here blocks on validation — no error line needed
    m.body.parentElement.classList.add('flow-node-modal')

    const field = (label, control) => {
      m.body.appendChild(el('label', 'flow-field-label', label))
      m.body.appendChild(control)
      return control
    }

    const nameInput = field('Name', el('input'))
    nameInput.type = 'text'
    nameInput.value = node.name || ''

    const kindOptions = [...AGENTS, 'terminal']
    // A hand-edited flow.json can carry a kind we don't offer (plan §2.2
    // warns rather than blocks on this) — keep it selectable so opening this
    // modal and saving without touching Kind can't silently "fix" it away.
    if (node.kind && !kindOptions.includes(node.kind)) kindOptions.unshift(node.kind)
    const kindSelect = field('Kind', el('select'))
    for (const k of kindOptions) {
      const opt = el('option', null, k)
      opt.value = k
      kindSelect.appendChild(opt)
    }
    kindSelect.value = node.kind || kindOptions[0]

    const instructionsInput = field('Instructions', el('textarea'))
    instructionsInput.value = node.instructions || ''
    const expectsInput = field('Expects', el('textarea'))
    expectsInput.value = node.expects || ''
    const producesInput = field('Produces', el('textarea'))
    producesInput.value = node.produces || ''

    const inputsEditor = this.buildPortEditor('Inputs', 'input', node.inputs || [])
    const outputsEditor = this.buildPortEditor('Outputs', 'output', node.outputs || [])
    const portsWrap = el('div', 'flow-modal-ports')
    portsWrap.append(inputsEditor.element, outputsEditor.element)
    m.body.appendChild(portsWrap)

    m.button('Save', () => {
      node.name = nameInput.value.trim() || node.id
      node.kind = kindSelect.value
      node.instructions = instructionsInput.value
      node.expects = expectsInput.value
      node.produces = producesInput.value
      // Renaming or removing a port that an existing edge references is
      // allowed — plan §2.2 treats a stale port name as a warning, not an
      // error, exactly like a hand-edited file with a dangling port name.
      // The edge just stops drawing (recomputeEdges can't resolve its
      // portDots key any more); nothing prunes it automatically, so the
      // wiring is recoverable if the port comes back under the same name.
      // Pruning automatically was the alternative and was rejected: a
      // rename is the common case (fixing a typo in a port name) and would
      // otherwise silently destroy an edge the user almost certainly meant
      // to keep.
      node.inputs = inputsEditor.rows()
      node.outputs = outputsEditor.rows()
      m.close()
      this.replaceNodeCard(node)
      this.markDirty()
      this.recomputeEdges()
    })
    m.button('Cancel', () => m.close(), 'ghost')
    setTimeout(() => nameInput.focus(), 0)
  }

  // Rebuilds one node's card in place after an edit (name/kind/instructions/
  // ports can all change what needs to be drawn). Drops the node's previous
  // portDots entries first — a renamed/removed port must not leave a stale
  // `dir:nodeId:name` key that recomputeEdges could still resolve into a
  // ghost edge endpoint.
  replaceNodeCard(node) {
    const old = this.nodeCards.get(node.id)
    for (const key of this.portKeysByNode.get(node.id) || []) this.portDots.delete(key)
    this.portKeysByNode.delete(node.id)
    const card = this.buildNodeCard(node)
    if (old) old.replaceWith(card)
    else this.contentEl.appendChild(card)
    if (this.selectedNodeId === node.id) card.classList.add('selected')
  }

  // Pointer capture on the viewport itself, not window/document: once
  // captured, this element keeps receiving move/up events for that pointer
  // even if the cursor leaves it (or the window) — no global listener needed,
  // and nothing to clean up in dispose() (popout safety, plan §5).
  wirePan() {
    const v = this.viewportEl
    v.addEventListener('pointerdown', (e) => {
      if (e.button !== 0) return
      if (e.target.closest('.flow-node')) return // node cards run their own drag/click gesture
      this.panDrag = { startX: e.clientX - this.pan.x, startY: e.clientY - this.pan.y }
      this.panStart = { x: e.clientX, y: e.clientY }
      v.setPointerCapture(e.pointerId)
      v.classList.add('panning')
    })
    v.addEventListener('pointermove', (e) => {
      if (!this.panDrag) return
      this.pan.x = e.clientX - this.panDrag.startX
      this.pan.y = e.clientY - this.panDrag.startY
      this.applyPan()
    })
    const endPan = (e) => {
      if (!this.panDrag) return
      const start = this.panStart
      this.panDrag = null
      v.classList.remove('panning')
      try {
        v.releasePointerCapture(e.pointerId)
      } catch {
        /* already released (e.g. right after pointercancel) — fine */
      }
      // A pointerdown/up on empty canvas that never moved past a few pixels
      // is a click, not a pan — treat it as "clicked the background" and
      // drop whatever node/edge was selected, the same as most graph editors.
      if (start && Math.abs(e.clientX - start.x) < 4 && Math.abs(e.clientY - start.y) < 4) this.clearSelection()
    }
    v.addEventListener('pointerup', endPan)
    v.addEventListener('pointercancel', endPan)
    // No zoom: a scale transform needs pointer-position correction so the
    // point under the cursor stays put while the content grows/shrinks under
    // it — easy to get subtly wrong (plan §2.4 calls this out by name). Pan
    // alone covers "look at a big flow" for v1; skip until it earns its keep.
  }

  applyPan() {
    this.contentEl.style.transform = `translate(${this.pan.x}px, ${this.pan.y}px)`
  }

  recomputeEdges() {
    if (!this.svg || !this.flow) return
    const wrapperRect = this.contentEl.getBoundingClientRect()
    // 0×0 means this panel hasn't had a real layout pass yet (restored into a
    // background tab, or just not painted this tick) — bail and wait for the
    // next onDidDimensionsChange rather than drawing anchors off a collapsed
    // rect.
    if (!wrapperRect.width || !wrapperRect.height) return

    while (this.svg.firstChild) this.svg.removeChild(this.svg.firstChild)

    for (const edge of this.flow.edges) {
      const fromDot = this.portDots.get(`out:${edge.from}:${edge.fromOutput}`)
      const toDot = this.portDots.get(`in:${edge.to}:${edge.toInput}`)
      // A port name that doesn't match any declared input/output is only a
      // *warning* in validateFlow (plan §2.2 — hand-edited files must still
      // open), so the nodes exist but there's no dot for this port. Skip
      // drawing this one path instead of crashing on a null anchor. The same
      // thing happens here right after a port is renamed/removed in the
      // editor modal — see replaceNodeCard.
      if (!fromDot || !toDot) continue
      const a = anchorOf(fromDot, wrapperRect)
      const b = anchorOf(toDot, wrapperRect)

      const path = buildEdgePath(a, b)
      path.classList.toggle('selected', edge.id === this.selectedEdgeId)
      this.svg.appendChild(path)

      // Classic SVG "hit area" trick: the visible line is only 1.5px wide,
      // far too thin to click reliably, so a second, transparent copy of the
      // same path is drawn on top with a much wider stroke and
      // pointer-events: stroke (see style.css) — it catches clicks across
      // the whole visual width of the edge without changing how it looks.
      const hit = buildEdgePath(a, b)
      hit.setAttribute('class', 'flow-edge-hit')
      hit.addEventListener('pointerdown', (e) => {
        if (e.button !== 0) return
        e.stopPropagation() // don't let this bubble into the viewport's pan handler
        this.selectEdge(edge.id)
      })
      this.svg.appendChild(hit)

      if (edge.label) this.svg.appendChild(buildEdgeLabel(a, b, edge.label))
    }
  }

  markDirty() {
    this.dirty = true
    this.api?.setTitle('● ' + this.name)
  }

  clearDirty() {
    this.dirty = false
    this.api?.setTitle(this.name)
  }

  // Read by panes.js's close guard: closing a dirty flow pane asks first.
  // The ● prefix markDirty sets on the title is what its confirm strips.
  isDirty() {
    return !!this.dirty
  }

  async save() {
    if (!this.flow) return
    const json = JSON.stringify(this.flow, null, 2) + '\n'
    try {
      await tome.fs.writeFile(this.path, json)
    } catch (err) {
      toast(`could not save ${this.name}: ${err.message}`)
      return
    }
    this.savedText = json
    this.diskConflict = false
    this.clearDirty()
    this.refreshWarningStrip()
  }

  // A file changed on disk. Our own save trips the watcher too, so compare
  // content rather than trying to time-window our writes — that is the only
  // check that cannot race (same reasoning as editor.js's onDiskChanged).
  async onDiskChanged() {
    if (!this.flow) return // still loading, or stuck in the unrecoverable error state
    let text
    try {
      text = await tome.fs.readFile(this.path)
    } catch {
      toast(`${this.name} is no longer readable on disk`)
      return
    }
    if (text === this.savedText) return // our own write, or a no-op touch

    const doc = this.element.ownerDocument
    // The node editor modal is a live edit-in-progress even though
    // this.dirty hasn't flipped yet (it only flips on Save) — reloading the
    // graph out from under it would orphan the node object it's holding.
    // modalShell always names its overlay 'ag-overlay' and keeps only one
    // open at a time, so this is a cheap, reliable "is something being
    // edited right now" check.
    if (!this.dirty && !doc.getElementById('ag-overlay')) {
      this.reload(text)
      return
    }
    // Dirty (or mid-edit): don't clobber it with a silent reload. Flag the
    // conflict instead — Save will overwrite the newer file, which the
    // warning strip now calls out, mirroring editor.js's "keep the buffer,
    // the next save overwrites" choice for a dirty pane.
    this.diskConflict = true
    this.refreshWarningStrip()
  }

  // Silent refresh for a clean pane whose file changed outside Tome: re-parse
  // and redraw the whole graph, same as a fresh open (plan §2.4/§5).
  reload(text) {
    let flow
    try {
      flow = JSON.parse(text)
      if (!flow || typeof flow !== 'object') throw new Error('not a JSON object')
      if (!Array.isArray(flow.nodes)) flow.nodes = []
      if (!Array.isArray(flow.edges)) flow.edges = []
    } catch (err) {
      toast(`${this.name}: reload failed — ${err.message}`)
      return
    }
    let errors, warnings
    try {
      ;({ errors, warnings } = validateFlow(flow))
    } catch (err) {
      toast(`${this.name}: reload failed — ${err.message}`)
      return
    }
    if (errors.length) {
      toast(`${this.name} changed on disk but now has structural problems — keeping the last good graph open`)
      return
    }
    this.savedText = text
    this.flow = flow
    this.diskConflict = false
    // Full DOM rebuild: node/edge counts and every port may have changed,
    // and this.origin (frozen per render — see renderGraph) is meant to be
    // re-derived exactly at moments like this, same as a fresh open.
    while (this.element.firstChild) this.element.removeChild(this.element.firstChild)
    this.renderGraph(warnings)
  }

  openAsText() {
    // `dock` comes from panes.js, which imports FlowPanel from this file to
    // register the 'flow' component — a cycle, but the same shape as the
    // existing panes.js <-> menus.js cycle already in this codebase, and safe
    // for the same reason: the binding is only read here, inside a click
    // handler, long after both modules finish their top-level evaluation.
    dock.addPanel({
      id: `text:${this.path}`,
      component: 'editor',
      title: `${this.name} (text)`,
      params: { path: this.path },
    })
    // After a restart this tab collapses back into the flow panel:
    // componentOf() classifies any params.path ending in .flow.json as
    // 'flow' regardless of which id it was saved under, and the restore path
    // calls openFile(), which always dedupes to `file:<path>`. So the
    // `text:` pane and the flow pane merge into one on next launch —
    // accepted v1 behavior (plan §5's restore note, taken one step further).
  }

  dispose() {
    const doc = this.element.ownerDocument
    if (this.onKeyDown) doc.removeEventListener('keydown', this.onKeyDown)
    // In case a pointer was mid-drag drawing an edge when this pane closed —
    // those listeners live on `doc`, not on this.element, so dockview tearing
    // down the DOM would otherwise leak them.
    this.edgeDragCleanup?.()
    if (this.watched) tome.fs.unwatch(this.path)
    flowPanels.delete(this)
  }
}

// Every live FlowPanel with a successfully-parsed graph, so the single
// fs:changed listener below can dispatch to whichever one (if any) has this
// path open — same shape as editor.js's module-level `editors` Set, and
// registered once here rather than per-instance for the same reason: main
// sends one 'fs:changed' event per change, not one per listener.
const flowPanels = new Set()
tome.fs.onChanged((p) => {
  for (const panel of flowPanels) if (panel.path === p) panel.onDiskChanged()
})
