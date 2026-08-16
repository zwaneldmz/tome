// Review report pane — a read-only, LLM-generated usage report over local
// signals (event-log tallies, chat-transcript count, git status across the
// open repos, background flow runs, the skills catalog). Nothing phones home
// on its own: main builds a counts-only summary and makes one one-shot call
// to the user's configured chat provider, then returns the markdown for this
// pane to render. The only actions are refresh and "promote to brain", which
// writes the report text into the active workspace's vault.

import { el, tome, toast } from '../util.js'
import { renderMarkdown } from '../markdown.js'
import { activeWorkspace } from '../workspaces.js'

export class ReportPanel {
  constructor() {
    this.element = el('div', 'panel-report')
    this.element.innerHTML = `
      <div class="report-bar">
        <span class="report-title">review report</span>
        <button class="report-refresh" title="Refresh">⟳</button>
        <button class="report-promote" title="Save to this workspace's brain">↑ promote to brain</button>
      </div>
      <div class="report-body"></div>`
    this.reportText = ''
  }
  init() {
    this.bodyEl = this.element.querySelector('.report-body')
    this.element.querySelector('.report-refresh').addEventListener('click', () => this.load())
    this.element.querySelector('.report-promote').addEventListener('click', () => this.promote())
    this.load()
  }
  async load() {
    this.bodyEl.textContent = 'Generating review…'
    this.bodyEl.setAttribute('role', 'status')
    let res
    try {
      res = await tome.review.generate()
    } catch (err) {
      this.bodyEl.textContent = ''
      this.bodyEl.appendChild(el('div', 'report-err', err.message))
      return
    }
    this.reportText = res.report || ''
    const d = el('div', 'md')
    renderMarkdown(d, this.reportText)
    this.bodyEl.replaceChildren(d)
  }
  async promote() {
    const w = activeWorkspace()
    if (!w) return toast('no workspace for brain')
    const rel = 'review-' + new Date().toISOString().slice(0, 10) + '.md'
    try {
      await tome.brain.write(w.name, rel, this.reportText)
    } catch (err) {
      toast(`brain: ${err.message}`)
      return
    }
    toast('report promoted to brain', 'ok')
  }
  dispose() {}
}
