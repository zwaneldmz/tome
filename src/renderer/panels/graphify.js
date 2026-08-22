// Graphify pane: the one-click workspace knowledge graph. A Build button
// runs the offline pipeline (tree-sitter AST extraction → Leiden
// clustering → GRAPH_REPORT.md + graph.html + graph.json), streamed into a
// console; a query bar runs read-only graph queries (query / path / explain
// / affected) against the built graph.json; the graph.html and report open
// straight from the header.
//
// Security notes, mirrored from the backend (src-tauri/src/graphify.rs):
// builds are pinned offline (--code-only + --no-label — no LLM, no
// network, no keys), and `add <url>` ingest is never exposed. Everything
// this pane starts reads or writes only inside the workspace's
// graphify-out/.
import { tome, toast, el } from '../util.js'
import { openFile } from '../panes.js'

const CONSOLE_CAP = 500 // kept DOM lines — a runaway build log must not balloon the pane

export class GraphifyPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-graphify'
    this.element.innerHTML = `
      <div class="graphify-head">
        <span class="graphify-title">code graph</span>
        <span class="graphify-status"></span>
        <span class="graphify-spacer"></span>
        <button class="graphify-open-graph hidden" title="Open graph.html in your browser">◉ graph</button>
        <button class="graphify-open-report hidden" title="Open GRAPH_REPORT.md in the editor">report</button>
        <button class="graphify-cancel hidden">cancel</button>
        <button class="graphify-build">◈ build graph</button>
      </div>
      <div class="graphify-console" aria-live="polite"></div>
      <div class="graphify-querybar">
        <select class="graphify-mode" aria-label="query mode">
          <option value="query">query</option>
          <option value="path">path A → B</option>
          <option value="explain">explain</option>
          <option value="affected">affected</option>
        </select>
        <input class="graphify-input" placeholder="a question, a symbol, or A → B" />
        <button class="graphify-run" disabled>run</button>
      </div>
      <div class="graphify-result"></div>`
  }

  async init({ params }) {
    this.ws = params.ws
    this.building = false

    this.statusEl = this.element.querySelector('.graphify-status')
    this.openGraphBtn = this.element.querySelector('.graphify-open-graph')
    this.openReportBtn = this.element.querySelector('.graphify-open-report')
    this.cancelBtn = this.element.querySelector('.graphify-cancel')
    this.buildBtn = this.element.querySelector('.graphify-build')
    this.consoleEl = this.element.querySelector('.graphify-console')
    this.modeSel = this.element.querySelector('.graphify-mode')
    this.inputEl = this.element.querySelector('.graphify-input')
    this.runBtn = this.element.querySelector('.graphify-run')
    this.resultEl = this.element.querySelector('.graphify-result')

    this.buildBtn.addEventListener('click', () => this.build())
    this.cancelBtn.addEventListener('click', () => this.cancel())
    this.openGraphBtn.addEventListener('click', () => {
      tome.openPath(this.status.graph_html).catch((e) => toast(e.message))
    })
    this.openReportBtn.addEventListener('click', () => {
      openFile(this.status.report)
    })
    this.runBtn.addEventListener('click', () => this.run())
    this.inputEl.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !this.runBtn.disabled) this.run()
    })

    await this.refreshStatus()
    // graphify's output layout means the graph is only usable once a build
    // has landed — until then the query bar stays off and the header says
    // what is missing, so the one thing that works (Build) is obvious.
  }

  // ---- status ----

  async refreshStatus() {
    try {
      this.status = await tome.graphify.status(this.ws)
    } catch (e) {
      toast(e.message)
      return
    }
    const s = this.status
    this.buildBtn.disabled = !s.available || this.building
    this.openGraphBtn.classList.toggle('hidden', !s.built)
    this.openReportBtn.classList.toggle('hidden', !s.built)
    this.runBtn.disabled = !s.built || !s.available
    if (s.available) {
      this.statusEl.textContent = s.version
        ? `${s.version}${s.built ? ' · graph built' : ''}`
        : 'installed'
    } else {
      this.statusEl.textContent = 'graphify not installed'
      this.buildBtn.title = s.reason || ''
      if (!s.built) {
        this.log(
          `graphify is not installed (${s.reason}). It runs fully offline — install it with:\n  pipx install graphifyy\n(requires Python 3.10+) and relaunch Tome.`
        )
      }
    }
  }

  // ---- build ----

  async build() {
    if (!this.status.available) {
      toast('graphify is not installed', 'err')
      return
    }
    this.building = true
    this.buildBtn.disabled = true
    this.cancelBtn.classList.remove('hidden')
    this.runBtn.disabled = true
    this.log(`building the workspace graph (offline, no LLM)\n`)
    try {
      const { summary } = await tome.graphify.build(this.ws, (line) => this.log(line))
      this.log(summary)
      toast(summary, 'ok')
    } catch (e) {
      this.log(`build failed: ${e.message}`)
      toast('graph build failed — see the console', 'err')
    } finally {
      this.building = false
      this.cancelBtn.classList.add('hidden')
      await this.refreshStatus()
    }
  }

  cancel() {
    tome.graphify.cancel().then(({ killed }) => {
      if (killed) this.log('cancelling…')
    })
  }

  // ---- queries ----

  async run() {
    const mode = this.modeSel.value
    const raw = this.inputEl.value.trim()
    if (!raw) return
    this.runBtn.disabled = true
    this.resultEl.textContent = '…'
    try {
      let out
      if (mode === 'query') {
        out = await tome.graphify.query(this.ws, raw)
      } else if (mode === 'path') {
        const parts = raw.split(/\s*(?:->|→)\s*/)
        if (parts.length !== 2) {
          this.resultEl.textContent = 'path mode needs two names: A → B'
          return
        }
        out = await tome.graphify.path(this.ws, parts[0], parts[1])
      } else if (mode === 'explain') {
        out = await tome.graphify.explain(this.ws, raw)
      } else {
        out = await tome.graphify.affected(this.ws, raw)
      }
      this.resultEl.textContent = out
    } catch (e) {
      this.resultEl.textContent = e.message
    } finally {
      this.runBtn.disabled = !this.status.built || this.building
      this.inputEl.focus()
    }
  }

  // ---- console ----

  log(line) {
    for (const part of String(line).split('\n')) {
      const row = el('div', 'graphify-line', part)
      this.consoleEl.appendChild(row)
    }
    while (this.consoleEl.children.length > CONSOLE_CAP) {
      this.consoleEl.removeChild(this.consoleEl.firstChild)
    }
    this.consoleEl.scrollTop = this.consoleEl.scrollHeight
  }
}
