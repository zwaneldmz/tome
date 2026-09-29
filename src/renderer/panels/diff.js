// Working-tree diff for one file — the review surface the Changes sidebar
// opens. One pane per repo, re-pointed at whichever file you click, so
// reviewing an agent's work is a walk down the list, not a pile of tabs.
import { tome, el } from '../util.js'
import { renderDiff } from '../history.js'
import { changesIcon } from '../icons.js'

export class DiffPanel {
  constructor() {
    this.element = el('div', 'panel-diff')
    this.head = el('div', 'diff-head')
    this.body = el('div', 'diff-body hist-diff')
    this.element.append(this.head, this.body)
  }
  init({ params, api }) {
    this.api = api
    this.show(params.dir, params.file)
  }
  async show(dir, file) {
    this.dir = dir
    this.file = file
    this.api?.updateParameters?.({ dir, file, diff: true })
    this.api?.setTitle?.(`Δ ${file.split('/').pop()}`)
    const at = file.lastIndexOf('/')
    this.head.replaceChildren(
      el('span', 'diff-dir', at > 0 ? file.slice(0, at + 1) : ''),
      el('span', 'diff-file', file.slice(at + 1))
    )
    this.body.replaceChildren(el('div', 'hist-err', 'loading…'))
    let text
    try {
      text = await tome.git.diff(dir, '', file)
    } catch (err) {
      this.body.replaceChildren(el('div', 'hist-err', err.message))
      return
    }
    if (this.file !== file) return // stale: another file was picked meanwhile
    this.body.replaceChildren(
      text?.trim() ? renderDiff(text) : el('div', 'hist-err', 'No textual diff — a new, binary, or unchanged file. Open it to see its contents.')
    )
  }
  statusMeta() {
    return this.file ? { icon: changesIcon, text: this.file, title: this.dir } : null
  }
}
