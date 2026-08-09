// First-run onboarding wizard: six short steps on top of modalShell, shown
// once after the first successful unlock/setup (store key 'onboarded-v1')
// and re-openable anytime from Preferences → "Replay setup wizard…" or the
// app menu's "Setup wizard…". Every step is skippable and every control
// writes the exact store keys Preferences uses, so the wizard and ⌘, never
// drift apart.
//
// Cross-workstream guards: tome.chat.providers (WS-B) and
// tome.agents.customs (WS-D) are optional-chained, so the wizard works the
// same before and after those bridges land.
import { tome, el } from './util.js'
import { prefs } from './state.js'
import { modalShell } from './modals.js'
import { totpModal } from './airgap-ui.js'
import { encodeWav } from '../shared/wav.js'

const ONBOARDED_KEY = 'onboarded-v1'

// One-line install hints per CLI — the README documents no agent install
// commands, so these are the upstream defaults.
const INSTALL_HINT = {
  claude: 'npm i -g @anthropic-ai/claude-code',
  opencode: 'curl -fsSL https://opencode.ai/install | bash',
  pi: 'npm i -g @earendil-works/pi-coding-agent',
}

const STEPS = ['Welcome', 'Agents', 'Assistant', 'Voice', 'Security', 'Done']

// First run only: the boot sequence calls this after bootAuth + bootChrome.
export async function maybeShowOnboarding() {
  if (tome.shotMode) return
  if (await tome.store.get(ONBOARDED_KEY)) return
  showOnboarding()
}

// Unconditional — the Preferences replay row and the app menu both land here.
export function showOnboarding() {
  // Choices collected along the way, replayed on the Done step.
  const state = {
    step: 0,
    provider: null,
    mic: null, // 'ok' | 'fail'
    dirty: false, // anything entered/chosen → Esc asks before skipping
  }
  const m = modalShell(`Set up Tome — ${STEPS[0]}`)
  m.err.remove() // steps report inline, never through the error line
  const box = m.body.parentElement
  box.classList.add('ob-box')
  const overlay = box.parentElement

  // Header: step dots under the title, filled up to the current step.
  const dots = el('div', 'ob-dots')
  dots.setAttribute('aria-hidden', 'true')
  for (const name of STEPS) {
    const d = el('span', 'ob-dot')
    d.title = name
    dots.appendChild(d)
  }
  box.insertBefore(dots, m.body)

  // Footer owns navigation; steps render into the body above it.
  const footer = el('div', 'ob-footer')
  const skipBtn = el('button', 'ag-btn ghost', 'Skip')
  skipBtn.type = 'button'
  const backBtn = el('button', 'ag-btn ghost', 'Back')
  backBtn.type = 'button'
  const nextBtn = el('button', 'ag-btn primary', 'Next')
  nextBtn.type = 'button'
  footer.append(skipBtn, el('span', 'ob-footer-gap'), backBtn, nextBtn)
  box.appendChild(footer)

  // Screen readers only announce live-region *changes* — clear, then fill on
  // the next tick (same pattern as toast()).
  const announce = (text) => {
    const live = document.getElementById('sr-live')
    if (!live) return
    live.textContent = ''
    setTimeout(() => (live.textContent = text), 50)
  }

  const paintDots = () => {
    ;[...dots.children].forEach((d, i) => d.classList.toggle('on', i <= state.step))
  }

  function render() {
    state.cleanup?.()
    state.cleanup = null
    m.body.textContent = ''
    overlay.setAttribute('aria-label', `Set up Tome — ${STEPS[state.step]}`)
    box.querySelector('h3').textContent = `Set up Tome — ${STEPS[state.step]}`
    paintDots()
    RENDERERS[state.step]()
    const last = state.step === STEPS.length - 1
    backBtn.classList.toggle('hidden', state.step === 0)
    skipBtn.classList.toggle('hidden', state.step === 0 || last)
    nextBtn.textContent = state.step === 0 ? 'Set up Tome' : last ? 'Start working' : 'Next'
    announce(`Step ${state.step + 1} of ${STEPS.length}: ${STEPS[state.step]}`)
    setTimeout(() => nextBtn.focus(), 0)
  }

  const finish = () => {
    state.cleanup?.()
    tome.store.set(ONBOARDED_KEY, true)
    m.close()
  }
  const next = () => {
    if (state.confirming) return
    if (state.step === STEPS.length - 1) return finish()
    state.step++
    render()
  }
  const back = () => {
    if (state.confirming || state.step === 0) return
    state.step--
    render()
  }
  // The skip confirm renders inline: confirmModal would build its own shell,
  // and modalShell keeps one overlay at a time — the wizard would be gone
  // even if the user changed their mind.
  const skip = () => {
    if (state.confirming || state.step === 0 || state.step === STEPS.length - 1) return
    if (!state.dirty) return finish()
    state.confirming = true
    m.body.textContent = ''
    footer.classList.add('hidden')
    note('Skip setup? Your choices so far will be lost.')
    const yes = el('button', 'ag-btn danger', 'Skip setup')
    yes.type = 'button'
    yes.addEventListener('click', finish)
    const no = el('button', 'ag-btn ghost', 'Keep going')
    no.type = 'button'
    no.addEventListener('click', () => {
      state.confirming = false
      footer.classList.remove('hidden')
      render()
    })
    m.body.append(yes, no)
    setTimeout(() => no.focus(), 0)
  }
  nextBtn.addEventListener('click', next)
  backBtn.addEventListener('click', back)
  skipBtn.addEventListener('click', skip)
  // →/Enter = next, ← = back; Escape stays on modalShell (closes like the
  // scrim) unless something was entered, where it becomes the skip confirm.
  // Capture phase: this must see Escape before modalShell's close handler.
  overlay.addEventListener(
    'keydown',
    (e) => {
      if (state.confirming) {
        if (e.key === 'Escape') {
          e.preventDefault()
          e.stopPropagation()
          state.confirming = false
          footer.classList.remove('hidden')
          render()
        }
        return
      }
      if (e.key === 'ArrowRight' || (e.key === 'Enter' && e.target.tagName !== 'BUTTON')) {
        e.preventDefault()
        next()
      } else if (e.key === 'ArrowLeft') {
        e.preventDefault()
        back()
      } else if (e.key === 'Escape' && state.dirty) {
        e.preventDefault()
        e.stopPropagation()
        skip()
      }
    },
    true
  )

  // ---------- steps ----------

  const note = (text) => m.body.appendChild(el('p', 'ag-note', text))

  // A switch identical to Preferences' toggleRow, writing the same keys.
  function toggle(label, hint, get, set) {
    const r = el('div', 'ob-row')
    const text = el('div', 'ob-row-text')
    text.append(el('span', 'prefs-label', label))
    if (hint) text.append(el('span', 'prefs-hint', hint))
    const sw = el('button', 'prefs-switch')
    sw.type = 'button'
    sw.setAttribute('role', 'switch')
    const paint = () => {
      sw.classList.toggle('on', get())
      sw.setAttribute('aria-checked', String(get()))
    }
    sw.append(el('span', 'prefs-knob'))
    sw.addEventListener('click', () => {
      set(!get())
      state.dirty = true
      paint()
    })
    r.append(text, sw)
    m.body.appendChild(r)
    paint()
  }

  function stepWelcome() {
    note(
      'Tome runs your coding agents behind an air gap, in one workspace — every pane sandboxed, every network request yours to allow.'
    )
    note('This takes about 30 seconds, and you can skip any step.')
  }

  function stepAgents() {
    const list = el('div', 'ob-agents')
    m.body.appendChild(list)
    const spinner = el('p', 'ag-note', 'Checking your PATH…')
    spinner.setAttribute('role', 'status')
    list.appendChild(spinner)
    note('More CLIs can be added later in ⌘, → Agents.')
    const paint = (agents, customs) => {
      spinner.remove()
      for (const a of agents) {
        const r = el('div', 'ob-agent')
        r.append(
          el('span', 'ob-dot-avail ' + (a.available ? 'ok' : 'off'), a.available ? '●' : '○'),
          el('span', 'ob-agent-name', a.name)
        )
        if (!a.available) {
          const hint = el('span', 'ob-agent-hint', INSTALL_HINT[a.name] || 'not on your PATH')
          r.appendChild(hint)
        }
        list.appendChild(r)
      }
      for (const c of customs || []) {
        const r = el('div', 'ob-agent')
        r.append(
          el('span', 'ob-dot-avail ok', '●'),
          el('span', 'ob-agent-name', c.name || c),
          el('span', 'prefs-hint', 'custom')
        )
        list.appendChild(r)
      }
    }
    // Inline spinner, never blocking Next: a slow login shell just leaves the
    // list empty until the probe resolves. If the user has moved on by then,
    // the result is dropped — m.body now belongs to another step.
    const myStep = state.step
    const stale = () => state.step !== myStep
    Promise.all([tome.agents.list(), Promise.resolve(tome.agents.customs?.() ?? null)])
      .then(([agents, customs]) => !stale() && paint(agents, customs))
      .catch(
        () => !stale() && (spinner.textContent = 'Could not check for agents — find them later in ⌘,.')
      )
  }

  function stepAssistant() {
    const providersP = tome.chat.providers?.()
    if (!providersP) {
      note('The assistant uses ANTHROPIC_API_KEY from your shell — set it and restart Tome.')
      return
    }
    const myStep = state.step
    const stale = () => state.step !== myStep
    Promise.resolve(providersP)
      .then(async (providers) => {
        const saved = await tome.store.get('chat-provider')
        if (stale()) return
        const group = el('div', 'ob-providers')
        group.setAttribute('role', 'radiogroup')
        group.setAttribute('aria-label', 'Assistant provider')
        for (const p of providers) {
          const b = el('button', 'ob-provider')
          b.type = 'button'
          b.setAttribute('role', 'radio')
          const on = (saved ?? state.provider) === p.id
          b.setAttribute('aria-checked', String(on))
          b.classList.toggle('on', on)
          b.append(
            el('span', 'ob-dot-avail ' + (p.hasKey ? 'ok' : 'off'), p.hasKey ? '●' : '○'),
            el('span', 'ob-agent-name', p.label || p.id)
          )
          b.addEventListener('click', () => {
            state.provider = p.id
            state.dirty = true
            tome.store.set('chat-provider', p.id)
            for (const s of group.children) {
              const sel = s === b
              s.classList.toggle('on', sel)
              s.setAttribute('aria-checked', String(sel))
            }
          })
          group.appendChild(b)
        }
        m.body.appendChild(group)
      })
      .catch(
        () => !stale() && note('Could not load providers — the assistant keeps its current settings.')
      )
  }

  function stepVoice() {
    note('One second of audio goes to the local whisper model — nothing leaves the machine.')
    const btn = el('button', 'ag-btn ghost', 'Test microphone')
    btn.type = 'button'
    const out = el('p', 'ag-note')
    out.setAttribute('role', 'status')
    m.body.append(btn, out)
    let ctx = null
    let stream = null
    const release = () => {
      for (const t of stream?.getTracks() || []) t.stop()
      stream = null
      ctx?.close().catch(() => {})
      ctx = null
    }
    btn.addEventListener('click', async () => {
      if (btn.disabled) return
      btn.disabled = true
      out.textContent = 'Listening for 1 second…'
      try {
        stream = await navigator.mediaDevices.getUserMedia({ audio: true })
      } catch {
        state.mic = 'fail'
        out.textContent = 'Microphone unavailable or permission denied.'
        btn.disabled = false
        return
      }
      try {
        // Same capture path as the chat pane's push-to-talk: raw Float32 at
        // 16 kHz, our own WAV encode (MediaRecorder's webm is unreadable by
        // whisper.cpp).
        ctx = new AudioContext({ sampleRate: 16000 })
        const chunks = []
        const proc = ctx.createScriptProcessor(4096, 1, 1)
        proc.onaudioprocess = (ev) => chunks.push(new Float32Array(ev.inputBuffer.getChannelData(0)))
        ctx.createMediaStreamSource(stream).connect(proc)
        proc.connect(ctx.destination)
        await new Promise((r) => setTimeout(r, 1000))
        proc.disconnect()
        if (!ctx) return // navigated away mid-record — release() already ran
        const rate = ctx.sampleRate
        release()
        const total = chunks.reduce((n, c) => n + c.length, 0)
        const samples = new Float32Array(total)
        let at = 0
        for (const c of chunks) {
          samples.set(c, at)
          at += c.length
        }
        out.textContent = 'Transcribing…'
        const res = await tome.stt.transcribe(encodeWav(samples, rate))
        if (res?.error) {
          state.mic = 'fail'
          out.textContent = res.error // advice verbatim — it carries install steps
        } else {
          state.mic = 'ok'
          out.textContent = res?.text ? `mic works ✓ — whisper heard: “${res.text}”` : 'mic works ✓'
        }
      } catch (err) {
        if (ctx) {
          state.mic = 'fail'
          release()
          out.textContent = 'Transcription failed: ' + (err?.message || err)
        }
      } finally {
        btn.disabled = false
      }
    })
    // Navigating away mid-test must not leave the mic indicator on.
    state.cleanup = release
  }

  function stepSecurity() {
    toggle(
      'Spawn agents air-gapped',
      'new agent panes start with no network until you allow it',
      () => prefs.airgapDefault,
      (v) => {
        prefs.airgapDefault = v
        tome.store.set('airgap-default', v)
      }
    )
    toggle(
      'Assistant may run commands',
      'the assistant chat can execute shell commands you approve',
      () => prefs.conductorRun,
      (v) => {
        prefs.conductorRun = v
        tome.store.set('conductor-run', v)
        tome.conductor.allowRun(v)
      }
    )
    const enroll = el('button', 'ag-btn ghost ob-link', 'Enroll authenticator (2FA)…')
    enroll.type = 'button'
    enroll.addEventListener('click', () => {
      tome.store.set(ONBOARDED_KEY, true) // the wizard isn't coming back after this
      m.close()
      totpModal()
    })
    m.body.appendChild(enroll)
  }

  function stepDone() {
    const summary = el('div', 'ob-summary')
    const item = (label, value) => {
      const r = el('div', 'ob-summary-row')
      r.append(el('span', 'prefs-label', label), el('span', 'prefs-value', value))
      summary.appendChild(r)
    }
    item('Assistant provider', state.provider || 'shell default')
    item('Microphone', state.mic === 'ok' ? 'works ✓' : state.mic === 'fail' ? 'needs setup' : 'not tested')
    item('Air-gapped agents', prefs.airgapDefault ? 'on' : 'off')
    item('Assistant commands', prefs.conductorRun ? 'on' : 'off')
    m.body.appendChild(summary)
    note('Change any of this anytime in ⌘,.')
  }

  const RENDERERS = [stepWelcome, stepAgents, stepAssistant, stepVoice, stepSecurity, stepDone]
  render()
}
