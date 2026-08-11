// Air-gap pane strips (per-terminal banner showing providers-only / open
// internet state) and the modals behind them: unlock, first-run setup, TOTP
// enrollment.
import { tome, toast } from './util.js'
import { agState } from './state.js'
import { strips } from './regs.js'
import { modalShell } from './modals.js'
import { renderStatusbar } from './statusbar.js'

const blockedThrottle = new Map()
const blockedCounts = new Map() // paneId -> blocked attempts since last click/unlock

export function stripRender(paneId) {
  const strip = strips.get(paneId)
  if (!strip) return
  const st = agState.panes[paneId]
  const label = strip.querySelector('.ag-label')
  if (!st || st.mode === 'providers') {
    strip.classList.remove('open')
    label.textContent = '⛨ model APIs only — click to allow internet'
  } else {
    strip.classList.add('open')
    const left = Math.max(0, st.expiresAt - Date.now())
    const m = Math.floor(left / 60000)
    const s = String(Math.floor((left % 60000) / 1000)).padStart(2, '0')
    label.textContent = `⛉ internet open · relocks in ${m}:${s} — click to relock`
  }
  // the tally only lives on the strip while the pane is providers-only
  if (st?.mode === 'open') setBlockedCount(paneId, 0)
  else renderBlockedCount(paneId)
}

function renderBlockedCount(paneId) {
  const countEl = strips.get(paneId)?.querySelector('.ag-count')
  if (!countEl) return
  const n = blockedCounts.get(paneId) || 0
  countEl.textContent = n > 0 ? `· ${n} blocked` : ''
}

function setBlockedCount(paneId, n) {
  blockedCounts.set(paneId, n)
  renderBlockedCount(paneId)
}
setInterval(() => {
  for (const id of strips.keys()) {
    if (agState.panes[id]?.mode === 'open') stripRender(id)
  }
}, 1000)

tome.airgap.onState((s) => {
  Object.assign(agState, s)
  for (const id of strips.keys()) stripRender(id)
  renderStatusbar()
})
tome.airgap.onBlocked(({ paneId, host }) => {
  // the tally counts every attempt; the flash/toast stay throttled per host
  setBlockedCount(paneId, (blockedCounts.get(paneId) || 0) + 1)
  const key = paneId + host
  if (Date.now() - (blockedThrottle.get(key) || 0) < 5000) return
  blockedThrottle.set(key, Date.now())
  const strip = strips.get(paneId)
  if (strip) {
    const f = strip.querySelector('.ag-flash')
    f.textContent = `✕ ${host}`
    f.classList.remove('show')
    void f.offsetWidth
    f.classList.add('show')
  }
  toast(`airgap blocked: ${host}`, 'err')
})

export async function airgapModal(paneId) {
  const state = await tome.airgap.state()
  Object.assign(agState, state)
  if (!state.auth.configured) return setupModal(paneId)
  const st = state.panes[paneId]

  // opening the modal acknowledges the tally — clear it on strip click
  setBlockedCount(paneId, 0)

  if (st?.mode === 'open') {
    const m = modalShell('⛉ pane is on the open internet')
    m.note('Relock now to return this pane to model-APIs-only mode.')
    m.button('Relock now', async () => {
      await tome.airgap.relock(paneId)
      m.close()
      toast('Internet closed — back to model APIs only', 'ok')
    })
    return
  }

  const m = modalShell('⛨ allow internet for this pane')
  m.note(`Grants this pane open internet for a limited time, then relocks itself.`)
  // app login already proved the passphrase — opening a pane wants the second
  // factor: the authenticator code when enrolled, the passphrase otherwise
  let pass = null
  let code = null
  if (state.auth.totp) code = m.input('2FA code (6 digits)', 'text')
  else pass = m.input('passphrase')
  const mins = document.createElement('select')
  for (const v of [15, 30, 60]) {
    const o = document.createElement('option')
    o.value = v
    o.textContent = `${v} minutes`
    mins.appendChild(o)
  }
  m.body.appendChild(mins)
  if (!state.auth.totp) {
    m.note('Tip: enroll an authenticator app for 2FA below.')
    m.button(
      'Enable 2FA…',
      async () => {
        m.close()
        totpModal()
      },
      'ghost'
    )
  }
  const go = async () => {
    const r = await tome.airgap.unlock({
      paneId,
      passphrase: pass?.value,
      code: code?.value,
      minutes: +mins.value,
    })
    if (r.ok) {
      m.close()
      toast(`Internet allowed for ${mins.value} min`, 'ok')
    } else {
      m.err.textContent = r.error
    }
  }
  m.button('Allow internet', go)
  const field = code || pass
  field.addEventListener('keydown', (e) => e.key === 'Enter' && go())
  setTimeout(() => field.focus(), 0)
}

// Second-factor re-auth before main will spawn an UNSANDBOXED (ungapped) pane
// (TOME-001). Main answers pty:create with { reauth: true } and spawns nothing
// until it gets back a verified passphrase/TOTP. Resolves to the credentials to
// retry with, or null if the user dismisses (Escape/scrim/Cancel) — in which
// case the pane is simply not started. errMsg carries main's last refusal
// (wrong code, throttled) so a retry shows why the previous attempt failed.
export async function reauthPrompt(errMsg) {
  const state = await tome.airgap.state()
  return new Promise((resolve) => {
    let settled = false
    const done = (v) => {
      if (settled) return
      settled = true
      resolve(v)
    }
    // onClose fires on Escape/scrim/Cancel — treat every dismissal as "no".
    const m = modalShell('⛨ unlock to launch an unsandboxed shell', () => done(null))
    m.note(
      'This pane runs with your full privileges and open network access. Re-enter your credentials to allow it — every unsandboxed pane asks again.'
    )
    if (errMsg) m.err.textContent = errMsg
    let pass = null
    let code = null
    if (state.auth.totp) code = m.input('2FA code (6 digits)', 'text')
    else pass = m.input('passphrase')
    const go = () => {
      done(state.auth.totp ? { code: code.value } : { passphrase: pass.value })
      m.close()
    }
    m.button('Unlock', go)
    m.button('Cancel', () => m.close(), 'ghost')
    const field = code || pass
    field.addEventListener('keydown', (e) => e.key === 'Enter' && go())
    setTimeout(() => field.focus(), 0)
  })
}

function setupModal(paneId) {
  const m = modalShell('⛨ set up air-gap unlock')
  m.note('Choose the passphrase that allows internet on air-gapped panes. Stored as a salted hash.')
  const p1 = m.input('passphrase')
  const p2 = m.input('repeat passphrase')
  m.button('Set passphrase', async () => {
    if (p1.value.length < 8) return (m.err.textContent = 'Too short — 8 characters minimum.')
    if (p1.value !== p2.value) return (m.err.textContent = 'Passphrases differ.')
    const r = await tome.airgap.setup(p1.value)
    if (!r.ok) return (m.err.textContent = r.error)
    m.close()
    toast('Passphrase set', 'ok')
    if (paneId) airgapModal(paneId)
  })
}

export function totpModal() {
  const m = modalShell('⛉ enroll authenticator (TOTP)')
  m.note('Add this secret to your authenticator app, then confirm a code.')
  tome.airgap.enrollTotp().then(({ secret, uri }) => {
    const s = m.note(secret)
    s.classList.add('ag-secret')
    m.note(uri)
    const code = m.input('code from the app', 'text')
    m.button('Confirm', async () => {
      if (await tome.airgap.confirmTotp(code.value)) {
        m.close()
        toast('2FA enabled', 'ok')
      } else {
        m.err.textContent = 'Code did not match — try the next one.'
      }
    })
  })
}
