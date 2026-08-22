// Modal shell shared by the egress modals and the brain pane's dialogs.
// One overlay at a time: opening a modal removes any existing one.
import { el } from './util.js'

// `onClose` fires however the modal goes away — a button, Escape, or a click
// on the scrim — so a caller awaiting a choice always gets an answer instead
// of a promise that never settles.
// `doc` targets another document — a popped-out pane lives in its own window,
// and a prompt about that window belongs in it, not back in the main one.
// dockview copies the app's stylesheets into every popout, so the modal is
// styled there; nodes made with the main document's createElement are adopted
// on append.
export function modalShell(title, onClose, doc = document) {
  doc.getElementById('ag-overlay')?.remove()
  const active = doc.activeElement
  const prevFocus = active && typeof active.focus === 'function' ? active : null
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
  doc.body.appendChild(overlay)

  // Focus trap: Tab cycles within the box, Escape closes like the overlay
  // click does, and closing hands focus back to whatever had it before.
  const FOCUSABLE = 'button:not(:disabled), input, select, textarea, [tabindex]'
  let closed = false
  const close = () => {
    if (closed) return
    closed = true
    doc.removeEventListener('keydown', esc, true)
    overlay.remove()
    prevFocus?.focus()
    onClose?.()
  }
  // Escape is handled on the DOCUMENT (capture phase), not the overlay:
  // modals that mount empty and fill their controls async (e.g. the TOTP
  // enroll dialog) leave focus on <body>, and a keydown from <body> never
  // bubbles through the overlay element — Escape would silently do
  // nothing. Capture-phase document handling sees it regardless of where
  // focus sits, which is what the "Escape closes" contract always meant.
  const esc = (e) => {
    if (e.key === 'Escape') {
      e.preventDefault()
      close()
    }
  }
  doc.addEventListener('keydown', esc, true)
  overlay.addEventListener('click', (e) => e.target === overlay && close())
  overlay.addEventListener('keydown', (e) => {
    if (e.key === 'Tab') {
      const f = [...overlay.querySelectorAll(FOCUSABLE)].filter((n) => !n.closest('.hidden'))
      if (!f.length) return
      const first = f[0]
      const last = f[f.length - 1]
      if (e.shiftKey && (doc.activeElement === first || !overlay.contains(doc.activeElement))) {
        e.preventDefault()
        last.focus()
      } else if (!e.shiftKey && (doc.activeElement === last || !overlay.contains(doc.activeElement))) {
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

// More than two ways forward (close a popout's panes, or move them here…).
// Resolves the chosen value, or null when dismissed — Escape and the scrim
// mean "do nothing", same as Cancel.
export function choiceModal(title, note, choices, doc) {
  return new Promise((resolve) => {
    const m = modalShell(title, () => resolve(null), doc)
    if (note) m.note(note)
    for (const c of choices)
      m.button(
        c.label,
        () => {
          resolve(c.value) // first resolve wins; the onClose null is a no-op
          m.close()
        },
        c.cls || 'primary'
      )
    m.button('Cancel', () => m.close(), 'ghost')
  })
}

// Small yes/no gate for destructive or lossy actions (close a dirty editor,
// delete a workspace, …). Resolves true only when the user confirms.
export function confirmModal(title, note, confirmLabel = 'Confirm', doc) {
  return new Promise((resolve) => {
    const m = modalShell(title, undefined, doc)
    if (note) m.note(note)
    const done = (v) => {
      m.close()
      resolve(v)
    }
    m.button(confirmLabel, () => done(true), 'danger')
    m.button('Cancel', () => done(false), 'ghost')
  })
}
