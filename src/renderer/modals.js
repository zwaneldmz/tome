// Modal shell shared by the air-gap modals and the brain pane's dialogs.
// One overlay at a time: opening a modal removes any existing one.
import { el } from './util.js'

export function modalShell(title) {
  document.getElementById('ag-overlay')?.remove()
  const prevFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null
  const overlay = el('div')
  overlay.id = 'ag-overlay'
  overlay.setAttribute('role', 'dialog')
  overlay.setAttribute('aria-modal', 'true')
  overlay.setAttribute('aria-label', title)
  const box = el('div', 'ag-box')
  const h = el('h3', '', title)
  const body = el('div', 'ag-body')
  const err = el('div', 'ag-err')
  box.append(h, body, err)
  overlay.appendChild(box)
  document.body.appendChild(overlay)

  // Focus trap: Tab cycles within the box, Escape closes like the overlay
  // click does, and closing hands focus back to whatever had it before.
  const FOCUSABLE = 'button:not(:disabled), input, select, textarea, [tabindex]'
  let closed = false
  const close = () => {
    if (closed) return
    closed = true
    overlay.remove()
    prevFocus?.focus()
  }
  overlay.addEventListener('click', (e) => e.target === overlay && close())
  overlay.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      e.preventDefault()
      close()
      return
    }
    if (e.key === 'Tab') {
      const f = [...overlay.querySelectorAll(FOCUSABLE)].filter((n) => !n.closest('.hidden'))
      if (!f.length) return
      const first = f[0]
      const last = f[f.length - 1]
      if (e.shiftKey && (document.activeElement === first || !overlay.contains(document.activeElement))) {
        e.preventDefault()
        last.focus()
      } else if (!e.shiftKey && (document.activeElement === last || !overlay.contains(document.activeElement))) {
        e.preventDefault()
        first.focus()
      }
    }
  })
  setTimeout(() => overlay.querySelector(FOCUSABLE)?.focus(), 0)
  return {
    body,
    err,
    close,
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

// Single-field prompt (rename a workspace, …). Resolves the entered string,
// or null when cancelled. Enter submits, Escape cancels.
export function promptModal(title, placeholder, initial = '', submitLabel = 'Save') {
  return new Promise((resolve) => {
    const m = modalShell(title)
    const input = m.input(placeholder, 'text')
    input.value = initial
    const done = (v) => {
      m.close()
      resolve(v)
    }
    input.addEventListener('keydown', (e) => e.key === 'Enter' && done(input.value))
    m.button(submitLabel, () => done(input.value))
    m.button('Cancel', () => done(null), 'ghost')
    setTimeout(() => input.select(), 0)
  })
}

// Small yes/no gate for destructive or lossy actions (close a dirty editor,
// delete a workspace, …). Resolves true only when the user confirms.
export function confirmModal(title, note, confirmLabel = 'Confirm') {
  return new Promise((resolve) => {
    const m = modalShell(title)
    if (note) m.note(note)
    const done = (v) => {
      m.close()
      resolve(v)
    }
    m.button(confirmLabel, () => done(true), 'danger')
    m.button('Cancel', () => done(false), 'ghost')
  })
}
