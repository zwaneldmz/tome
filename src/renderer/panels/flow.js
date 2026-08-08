// Flow pane: read-only render of a `<name>.flow.json` graph (see
// flow-model.js and docs/FEATURE-PLAN-file-creation-and-flows.md §2.4).
// Nodes are absolutely-positioned cards; edges are a single SVG layer
// underneath them; panning translates a content wrapper. No editing this
// slice — no node drag, no edge drawing, no modal, no save, no run (those
// are later commits in the plan's build order, §3).
import { tome, el } from '../util.js'
import { validateFlow } from '../flow-model.js'
import { dock } from '../panes.js'

const SVG_NS = 'http://www.w3.org/2000/svg'

// Fixed card footprint. v1 has no auto-layout (plan §4 non-goal) and no
// drag, so a node keeps exactly the x/y the file gives it — PAD is just
// margin around the computed bounding box so cards near (0,0) aren't flush
// against the viewport edge.
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
    this.pan = { x: 0, y: 0 }
    // "dir:nodeId:portName" -> the port's dot element, filled in while
    // building node cards. Keyed rather than looked up via querySelector so
    // recomputeEdges never has to build a CSS attribute selector out of a
    // node/port name that came straight from a (possibly hand-edited, so
    // untrusted) flow.json file.
    this.portDots = new Map()

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

    // Errors mean the graph itself can't be trusted (dangling ids duplicate
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
    this.renderGraph(warnings)
    // Recompute anchors whenever this panel's own box changes — including
    // the very first real layout pass. A pane restored into a background tab
    // (or a fresh one not yet painted) measures 0×0 at init time, which would
    // otherwise bake in degenerate edge paths.
    api.onDidDimensionsChange(() => this.recomputeEdges())
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

  renderGraph(warnings) {
    this.element.appendChild(this.buildToolbar())
    if (warnings.length) this.element.appendChild(this.buildWarningStrip(warnings))

    this.viewportEl = el('div', 'flow-viewport')
    this.contentEl = el('div', 'flow-content')
    this.viewportEl.appendChild(this.contentEl)
    this.element.appendChild(this.viewportEl)

    this.svg = document.createElementNS(SVG_NS, 'svg')
    this.svg.setAttribute('class', 'flow-edges')
    // Edges are one SVG layer, appended before any node card, so nodes paint
    // on top of it in normal DOM stacking order (plan §2.4).
    this.contentEl.appendChild(this.svg)

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
    const n = this.flow.nodes.length
    const e = this.flow.edges.length
    bar.append(
      el('span', 'flow-name', this.flow.name || this.name),
      el('span', 'flow-meta', `${n} node${n === 1 ? '' : 's'} · ${e} edge${e === 1 ? '' : 's'}`)
    )
    const openText = el('button', 'flow-open-text', 'Open as text')
    openText.addEventListener('click', () => this.openAsText())
    bar.appendChild(openText)
    return bar
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
    // Session-only: the warnings describe the file on disk, not any state of
    // ours, so dismissing just hides the strip for this open pane — reopening
    // the file (or this panel) shows it again, which is the point.
    dismiss.addEventListener('click', () => strip.remove())
    strip.append(body, dismiss)
    return strip
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
    return card
  }

  buildPort(nodeId, name, dir) {
    const port = el('div', `flow-port flow-port-${dir}`)
    const dot = el('span', 'flow-port-dot')
    const label = el('span', 'flow-port-label', name || '')
    // Input ports read dot-then-label (dot flush on the card's left edge);
    // output ports read label-then-dot (dot flush on the right) — see the
    // .flow-ports-in/.flow-ports-out CSS, which relies on this DOM order
    // instead of flex-direction: row-reverse.
    if (dir === 'in') port.append(dot, label)
    else port.append(label, dot)
    if (name) this.portDots.set(`${dir}:${nodeId}:${name}`, dot)
    return port
  }

  // Pointer capture on the viewport itself, not window/document: once
  // captured, this element keeps receiving move/up events for that pointer
  // even if the cursor leaves it (or the window) — no global listener needed,
  // and nothing to clean up in dispose() (popout safety, plan §5).
  wirePan() {
    const v = this.viewportEl
    v.addEventListener('pointerdown', (e) => {
      if (e.button !== 0) return
      if (e.target.closest('.flow-node')) return // node drag is a later slice — don't pan out from under a future click there
      this.panDrag = { startX: e.clientX - this.pan.x, startY: e.clientY - this.pan.y }
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
      this.panDrag = null
      v.classList.remove('panning')
      try {
        v.releasePointerCapture(e.pointerId)
      } catch {
        /* already released (e.g. right after pointercancel) — fine */
      }
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
      // drawing this one path instead of crashing on a null anchor.
      if (!fromDot || !toDot) continue
      const a = anchorOf(fromDot, wrapperRect)
      const b = anchorOf(toDot, wrapperRect)
      this.svg.appendChild(buildEdgePath(a, b))
      if (edge.label) this.svg.appendChild(buildEdgeLabel(a, b, edge.label))
    }
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

  // No window/document-level listeners were registered — the pan handlers
  // above live on this.viewportEl (a descendant of this.element), so
  // dockview tears them down for free along with the element when this panel
  // is removed. Nothing to release here.
  dispose() {}
}
