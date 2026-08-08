// App lock screen. Shown before the workspace when login is configured:
// Touch ID (macOS) or passphrase + TOTP. First run offers setup (skippable).
// The real enforcement lives in the main process — every sensitive IPC channel
// refuses until auth:login / auth:touchid succeeds — this overlay is just the door.
// NOTE: this overlay() is intentionally separate from modals.js's modalShell —
// the lock screen is a full-viewport gate (#lock-overlay, sigil, no dismiss),
// not a dismissible dialog.
import { el } from './util.js'

function overlay() {
  const o = el('div')
  o.id = 'lock-overlay'
  const box = el('div', 'ag-box lock-box')
  const sigil = el('div', 'lock-sigil', '▚')
  const h = el('h3', '', 'Tome is locked')
  const body = el('div', 'ag-body')
  const err = el('div', 'ag-err')
  box.append(sigil, h, body, err)
  o.appendChild(box)
  document.body.appendChild(o)
  return { o, h, body, err }
}

// resolves once unlocked (or setup skipped); the overlay removes itself
export function bootAuth(tome, toast) {
  return new Promise((resolve) => {
    ;(async () => {
      if (tome.shotMode) return resolve()
      const st = await tome.auth.status()
      if (st.unlocked) return resolve()
      if (!st.configured) return setupScreen(tome, toast, resolve)
      lockScreen(tome, toast, st, resolve)
    })()
  })
}

function lockScreen(tome, toast, st, resolve) {
  const { body, err } = overlay()
  const done = (o) => {
    o.remove()
    toast('Workspace unlocked', 'ok')
    resolve()
  }
  const root = document.getElementById('lock-overlay')

  const pass = el('input')
  pass.type = 'password'
  pass.placeholder = 'passphrase'
  let code = null
  if (st.totp) {
    code = el('input')
    code.type = 'text'
    code.placeholder = '2FA code (6 digits)'
    code.autocomplete = 'one-time-code'
  }
  const unlock = el('button', 'ag-btn primary', 'Unlock')
  const login = async () => {
    const r = await tome.auth.login({ passphrase: pass.value, code: code?.value })
    if (r.ok) done(root)
    else err.textContent = r.error
  }
  unlock.addEventListener('click', login)

  if (st.touchId) {
    const tid = el('button', 'ag-btn primary', '⌘ Unlock with Touch ID')
    tid.addEventListener('click', async () => {
      const r = await tome.auth.touchid()
      if (r.ok) done(root)
      else err.textContent = r.error
    })
    body.appendChild(tid)
    body.appendChild(el('p', 'ag-note', `or use your passphrase${st.totp ? ' + 2FA' : ''}:`))
    tid.click() // prompt immediately; falling back to the form is one Esc away
  }
  body.appendChild(pass)
  if (code) body.appendChild(code)
  body.appendChild(unlock)
  const last = code || pass
  for (const i of [pass, code].filter(Boolean))
    i.addEventListener('keydown', (e) => e.key === 'Enter' && (i === last ? login() : (code || unlock).focus()))
  if (!st.touchId) setTimeout(() => pass.focus(), 0)
}

function setupScreen(tome, toast, resolve) {
  const { h, body, err } = overlay()
  h.textContent = 'Protect this workspace'
  const root = document.getElementById('lock-overlay')
  body.appendChild(
    el(
      'p',
      'ag-note',
      'Set a passphrase to lock Tome at launch. It also arms the air-gap unlock. You can enroll an authenticator app (2FA) right after — the air gap will then ask for a code instead of the passphrase.'
    )
  )
  const p1 = el('input')
  p1.type = 'password'
  p1.placeholder = 'passphrase'
  const p2 = el('input')
  p2.type = 'password'
  p2.placeholder = 'repeat passphrase'
  const set = el('button', 'ag-btn primary', 'Set passphrase')
  const skip = el('button', 'ag-btn ghost', 'Skip for now')
  const dismiss = () => {
    root.remove()
    resolve()
  }
  // Escape is the skip path — only on setup. The lock screen itself has no
  // Escape handler: it stays until unlocked.
  root.addEventListener('keydown', (e) => e.key === 'Escape' && (e.preventDefault(), dismiss()))
  set.addEventListener('click', async () => {
    if (p1.value.length < 8) return (err.textContent = 'Too short — 8 characters minimum.')
    if (p1.value !== p2.value) return (err.textContent = 'Passphrases differ.')
    const r = await tome.airgap.setup(p1.value)
    if (!r.ok) return (err.textContent = r.error)
    toast('Passphrase set — enable 2FA from any air-gap strip', 'ok')
    root.remove()
    resolve()
  })
  skip.addEventListener('click', dismiss)
  body.append(p1, p2, set, skip)
  setTimeout(() => p1.focus(), 0)
}
