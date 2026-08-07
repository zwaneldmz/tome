// Git history pane — IntelliJ-style: commit list on the left, commit detail
// (message, changed files) on the right, per-file diff below it.

const tome = () => window.tome

const STATUS = { A: ['A', 'g-add'], M: ['M', 'g-mod'], D: ['D', 'g-del'], R: ['R', 'g-mod'], C: ['C', 'g-add'], T: ['T', 'g-mod'] }

function el(tag, cls, text) {
  const n = document.createElement(tag)
  if (cls) n.className = cls
  if (text != null) n.textContent = text
  return n
}

export class HistoryPanel {
  constructor() {
    this.element = el('div', 'panel-history')
    this.element.innerHTML = `
      <div class="hist-left">
        <div class="hist-bar">
          <input class="hist-filter" placeholder="filter subject, author, hash…" />
          <button class="hist-refresh" title="Refresh">⟳</button>
        </div>
        <div class="hist-list"></div>
      </div>
      <div class="hist-right">
        <div class="hist-detail"></div>
        <div class="hist-diff"></div>
      </div>`
  }
  init({ params }) {
    this.dir = params.dir
    this.commits = []
    this.selected = null
    this.listEl = this.element.querySelector('.hist-list')
    this.detailEl = this.element.querySelector('.hist-detail')
    this.diffEl = this.element.querySelector('.hist-diff')
    this.filterEl = this.element.querySelector('.hist-filter')
    this.filterEl.addEventListener('input', () => this.renderList())
    this.element.querySelector('.hist-refresh').addEventListener('click', () => this.load())
    this.load()
  }
  async load() {
    this.listEl.textContent = 'loading…'
    try {
      this.commits = await tome().git.log(this.dir)
    } catch (err) {
      this.listEl.textContent = ''
      this.listEl.appendChild(el('div', 'hist-err', err.message))
      return
    }
    this.renderList()
    if (this.commits.length) this.select(this.commits[0])
  }
  renderList() {
    const q = this.filterEl.value.trim().toLowerCase()
    this.listEl.textContent = ''
    for (const c of this.commits) {
      if (q && !`${c.subject} ${c.author} ${c.short} ${c.hash}`.toLowerCase().includes(q)) continue
      const row = el('div', 'hist-row' + (c === this.selected ? ' active' : ''))
      const top = el('div', 'hist-row-top')
      top.appendChild(el('span', 'hist-subject', c.subject))
      for (const r of c.refs) top.appendChild(el('span', 'hist-ref' + (r.includes('HEAD') ? ' head' : ''), r))
      const sub = el('div', 'hist-row-sub')
      sub.append(el('span', 'hist-hash', c.short), el('span', '', c.author), el('span', 'hist-date', c.date))
      row.append(top, sub)
      row.addEventListener('click', () => this.select(c))
      this.listEl.appendChild(row)
    }
  }
  async select(c) {
    this.selected = c
    this.renderList()
    this.diffEl.textContent = ''
    this.detailEl.textContent = 'loading…'
    let detail
    try {
      detail = await tome().git.commit(this.dir, c.hash)
    } catch (err) {
      this.detailEl.textContent = ''
      this.detailEl.appendChild(el('div', 'hist-err', err.message))
      return
    }
    if (this.selected !== c) return // stale response
    this.detailEl.textContent = ''
    const head = el('div', 'hist-meta')
    head.append(el('span', 'hist-hash', c.short), el('span', '', `${c.author} · ${c.date}`))
    const msg = el('pre', 'hist-msg', detail.body)
    const files = el('div', 'hist-files')
    if (!detail.files.length) files.appendChild(el('div', 'hist-err', '(no file list — merge commit?)'))
    for (const f of detail.files) {
      const [sym, cls] = STATUS[f.status] || [f.status, '']
      const row = el('div', 'hist-file')
      row.append(el('span', 'hist-status ' + cls, sym), el('span', 'hist-path', f.path))
      row.addEventListener('click', () => {
        for (const r of files.children) r.classList.remove('active')
        row.classList.add('active')
        this.showDiff(c, f.path)
      })
      files.appendChild(row)
    }
    this.detailEl.append(head, msg, files)
  }
  async showDiff(c, file) {
    this.diffEl.textContent = 'loading…'
    let text
    try {
      text = await tome().git.diff(this.dir, c.hash, file)
    } catch (err) {
      this.diffEl.textContent = ''
      this.diffEl.appendChild(el('div', 'hist-err', err.message))
      return
    }
    if (this.selected !== c) return
    this.diffEl.textContent = ''
    if (!text.trim()) {
      this.diffEl.appendChild(el('div', 'hist-err', '(no textual diff — binary file or merge)'))
      return
    }
    const frag = document.createDocumentFragment()
    for (const line of text.split('\n')) {
      let cls = 'ctx'
      if (line.startsWith('+++') || line.startsWith('---') || line.startsWith('diff ') || line.startsWith('index ')) cls = 'head'
      else if (line.startsWith('@@')) cls = 'hunk'
      else if (line.startsWith('+')) cls = 'add'
      else if (line.startsWith('-')) cls = 'del'
      frag.appendChild(el('div', 'dl ' + cls, line || ' '))
    }
    this.diffEl.appendChild(frag)
  }
}
