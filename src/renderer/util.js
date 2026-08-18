// Shared renderer helpers. `tome` is the preload allowlist bridge
// (contextBridge, sandboxed renderer) — every module uses the same handle.

export const tome = window.tome

export function el(tag, cls, text) {
  const n = document.createElement(tag)
  if (cls) n.className = cls
  if (text != null) n.textContent = text
  return n
}

const toasts = document.getElementById('toasts')
// Toasts vanish after ~5 s, so every one is also logged (session-only, capped)
// behind the bell in the top bar — a missed "egress blocked" stays retrievable.
export const notifLog = []
const NOTIF_CAP = 100
export function toast(msg, kind = 'err') {
  notifLog.push({ ts: Date.now(), kind, msg: String(msg) })
  if (notifLog.length > NOTIF_CAP) notifLog.splice(0, notifLog.length - NOTIF_CAP)
  document.getElementById('btn-notifs')?.classList.add('unseen')
  const t = el('div', 'toast ' + kind, msg)
  toasts.appendChild(t)
  setTimeout(() => t.classList.add('out'), 4200)
  setTimeout(() => t.remove(), 4800)
  // Screen readers only announce live-region *changes* — clear then re-fill on
  // the next tick so back-to-back identical toasts each get spoken.
  const live = document.getElementById('sr-live')
  if (live) {
    live.textContent = ''
    setTimeout(() => (live.textContent = String(msg)), 50)
  }
}
