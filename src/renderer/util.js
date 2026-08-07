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
export function toast(msg, kind = 'err') {
  const t = el('div', 'toast ' + kind, msg)
  toasts.appendChild(t)
  setTimeout(() => t.classList.add('out'), 4200)
  setTimeout(() => t.remove(), 4800)
}
