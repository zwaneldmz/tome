// Event log pane — read-only tail of main's persistent userData/events.jsonl:
// conductor tool calls, air-gap unlocks/relocks, blocked egress. Newest first;
// live records prepend via the events:appended push. No actions here except
// refresh — the log is an audit trail, not a control surface.

import { el } from '../util.js'
import { summary, stamp } from '../event-summary.js'

const tome = () => window.tome

export class EventsPanel {
  constructor() {
    this.element = el('div', 'panel-events')
    this.element.innerHTML = `
      <div class="events-bar">
        <span class="events-title">security event log</span>
        <button class="events-refresh" title="Refresh">⟳</button>
      </div>
      <div class="events-list"></div>`
  }
  init() {
    this.listEl = this.element.querySelector('.events-list')
    this.element.querySelector('.events-refresh').addEventListener('click', () => this.load())
    this.offAppend = tome().events.onAppended((rec) => this.prepend(rec))
    this.load()
  }
  async load() {
    this.listEl.textContent = 'loading…'
    let events
    try {
      events = await tome().events.list()
    } catch (err) {
      this.listEl.textContent = ''
      this.listEl.appendChild(el('div', 'events-err', err.message))
      return
    }
    // readEvents returns oldest-first; the pane shows newest on top.
    this.render([...events].reverse())
  }
  render(rows) {
    this.listEl.textContent = ''
    if (!rows.length) {
      this.listEl.appendChild(el('div', 'events-empty', 'no events yet'))
      return
    }
    for (const rec of rows) this.listEl.appendChild(this.row(rec))
  }
  prepend(rec) {
    this.listEl.querySelector('.events-empty')?.remove()
    this.listEl.prepend(this.row(rec))
  }
  row(rec) {
    const failed = rec.kind === 'conductor:tool' && rec.ok === false
    const row = el('div', 'events-row')
    row.append(
      el('span', 'events-kind' + (failed ? ' failed' : ''), rec.kind + (failed ? ' ✕' : '')),
      el('span', 'events-summary', summary(rec)),
      el('span', 'events-ts', stamp(rec.ts))
    )
    return row
  }
  dispose() {
    // Drop the renderer-side listener so a closed pane stops receiving pushes.
    this.offAppend?.()
  }
}
