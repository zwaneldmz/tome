// Modal shell shared by the air-gap modals and the brain pane's dialogs.
// One overlay at a time: opening a modal removes any existing one.
import { el } from './util.js'

export function modalShell(title) {
  document.getElementById('ag-overlay')?.remove()
  const overlay = el('div')
  overlay.id = 'ag-overlay'
  const box = el('div', 'ag-box')
  const h = el('h3', '', title)
  const body = el('div', 'ag-body')
  const err = el('div', 'ag-err')
  box.append(h, body, err)
  overlay.appendChild(box)
  overlay.addEventListener('click', (e) => e.target === overlay && overlay.remove())
  document.body.appendChild(overlay)
  return {
    body,
    err,
    close: () => overlay.remove(),
    input(placeholder, type = 'password') {
      const i = el('input')
      i.type = type
      i.placeholder = placeholder
      body.appendChild(i)
      return i
    },
    button(label, onClick, cls = 'primary') {
      const b = el('button', 'ag-btn ' + cls, label)
      b.addEventListener('click', onClick)
      body.appendChild(b)
      return b
    },
    note(text) {
      const p = el('p', 'ag-note', text)
      body.appendChild(p)
      return p
    },
  }
}
