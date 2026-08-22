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
import { tome, el, toast } from './util.js'
import { prefs } from './state.js'
import { modalShell } from './modals.js'
import { totpModal } from './egress-ui.js'
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
// Except while locked: the lock overlay rides at z-index 3000, a full 1000
// above the wizard — a wizard opened under it looks present but every
// control is dead to clicks, which reads exactly like "the toggles don't
// work". bootAuth resolves before the auto-run path, so only the replay /
// menu entries can ever hit this.
export async function showOnboarding() {
  const st = await tome.auth.status().catch(() => null)
  if (st && !st.unlocked) return toast('Unlock the workspace first — then reopen the setup wizard.')
  // Choices collected along the way, replayed on the Done step.
  const state = {
    step: 0,
    provider: null,
    mic: null, // 'ok' | 'fail'
    dirty: false, // anything entered/chosen → Esc asks before skipping
  }
  // Escape decisions register BEFORE modalShell's own document-level
  // Esc-close (registered inside modalShell() below): capture-phase
  // keydown listeners on the document run in registration order, so this
  // handler — registered first — can veto the close for the two cases
  // where Esc means something else here (un-confirm, or confirm the skip).
  // The assignment targets below (footer/render/skip) don't exist yet; they
  // are filled in as the wizard builds, before any key can reach here.
  const stateRef = { confirming: false }
  let escTargets = null
  const onEsc = (e) => {
    if (e.key !== 'Escape') return
    if (stateRef.confirming) {
      e.stopImmediatePropagation()
      stateRef.confirming = false
      escTargets?.footer.classList.remove('hidden')
      escTargets?.render()
      return
    }
    if (state.dirty) {
      e.stopImmediatePropagation()
      escTargets?.skip()
    }
  }
  document.addEventListener('keydown', onEsc, true)
  const m = modalShell(`Set up Tome — ${STEPS[0]}`, () =>
    document.removeEventListener('keydown', onEsc, true)
  )
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
    if (stateRef.confirming) return
    if (state.step === STEPS.length - 1) return finish()
    state.step++
    render()
  }
  const back = () => {
    if (stateRef.confirming || state.step === 0) return
    state.step--
    render()
  }
  // The skip confirm renders inline: confirmModal would build its own shell,
  // and modalShell keeps one overlay at a time — the wizard would be gone
  // even if the user changed their mind.
  const skip = () => {
    if (stateRef.confirming || state.step === 0 || state.step === STEPS.length - 1) return
    if (!state.dirty) return finish()
    stateRef.confirming = true
    m.body.textContent = ''
    footer.classList.add('hidden')
    note('Skip setup? Your choices so far will be lost.')
    const yes = el('button', 'ag-btn danger', 'Skip setup')
    yes.type = 'button'
    yes.addEventListener('click', finish)
    const no = el('button', 'ag-btn ghost', 'Keep going')
    no.type = 'button'
    no.addEventListener('click', () => {
      stateRef.confirming = false
      footer.classList.remove('hidden')
      render()
    })
    m.body.append(yes, no)
    setTimeout(() => no.focus(), 0)
  }
  nextBtn.addEventListener('click', next)
  backBtn.addEventListener('click', back)
  skipBtn.addEventListener('click', skip)
  // Arrow/Enter navigation stays on the overlay (no conflict with
  // modalShell's Esc handling — that lives on the document now); the
  // Escape branches moved up to onEsc, registered before modalShell.
  escTargets = { footer, render, skip }
  overlay.addEventListener(
    'keydown',
    (e) => {
      if (stateRef.confirming) {
        return
      }
      if (e.key === 'ArrowRight' || (e.key === 'Enter' && e.target.tagName !== 'BUTTON')) {
        e.preventDefault()
        next()
      } else if (e.key === 'ArrowLeft') {
        e.preventDefault()
        back()
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
      'Tome runs your coding agents behind an egress, in one workspace — every pane sandboxed, every network request yours to allow.'
    )
    note('This takes about 30 seconds, and you can skip any step.')
  }

  function stepAgents() {
    note('Pick the agent CLIs Tome may spawn. Unchecked agents stay installed but leave the ＋ menu.')
    const list = el('div', 'ob-agents')
    m.body.appendChild(list)
    const spinner = el('p', 'ag-note', 'Checking your PATH…')
    spinner.setAttribute('role', 'status')
    list.appendChild(spinner)
    note('More CLIs can be added later in Settings (⌘,) → Agents.')
    const paint = async (agents) => {
      spinner.remove()
      const disabled = new Set((await tome.store.get('agents-disabled')) || [])
      const row = (id, label, available, hintText) => {
        const r = el('div', 'ob-agent ob-agent-pick')
        const cb = el('input')
        cb.type = 'checkbox'
        cb.checked = !disabled.has(id)
        cb.disabled = !available // not installed — nothing to offer
        cb.setAttribute('aria-label', `Enable ${label}`)
        cb.addEventListener('change', async () => {
          cb.checked ? disabled.delete(id) : disabled.add(id)
          state.dirty = true
          await tome.store.set('agents-disabled', [...disabled])
        })
        const name = el('span', 'ob-agent-name', label)
        r.append(
          cb,
          el('span', 'ob-dot-avail ' + (available ? 'ok' : 'off'), available ? '●' : '○'),
          name
        )
        if (!available) r.appendChild(el('span', 'ob-agent-hint', hintText || 'not on your PATH'))
        list.appendChild(r)
      }
      for (const a of agents) row(a.name, a.label || a.name, a.available, INSTALL_HINT[a.name])
    }
    // Inline spinner, never blocking Next: a slow login shell just leaves the
    // list empty until the probe resolves. If the user has moved on by then,
    // the result is dropped — m.body now belongs to another step.
    const myStep = state.step
    const stale = () => state.step !== myStep
    Promise.resolve(tome.agents.list())
      .then((agents) => !stale() && paint(agents))
      .catch(
        () => !stale() && (spinner.textContent = 'Could not check for agents — find them later in Settings (⌘,).')
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
      .then(async (info) => {
        // chat:providers returns { providers, active, effective } — main's
        // active is the stored pick validated against the registry, so it
        // is the correct initial selection (and what a skipped step keeps).
        const providers = info?.providers || []
        if (!providers.length) {
          if (!stale()) note('No assistant providers configured — the assistant keeps its shell default.')
          return
        }
        if (stale()) return
        note('Pick the model the assistant pane talks to. ● means a key is known — paste one inline if it is not.')
        const group = el('div', 'ob-providers')
        group.setAttribute('role', 'radiogroup')
        group.setAttribute('aria-label', 'Assistant provider')

        // Inline key field on the SELECTED row: pasted keys are the fix
        // for Finder-launched apps that never see the login shell, so the
        // wizard must not send the user back to export-vars-and-restart.
        const keyRow = el('div', 'ob-keyrow')
        const keyIn = el('input', 'ob-keyin')
        keyIn.type = 'password'
        keyIn.placeholder = 'paste API key for the selected provider'
        keyIn.setAttribute('aria-label', 'API key')
        const keySave = el('button', 'ag-btn ghost', 'Save key')
        keySave.type = 'button'
        keyRow.append(keyIn, keySave)

        const paint = (selectedId) => {
          for (const s of group.children) {
            const sel = s.dataset.pid === selectedId
            s.classList.toggle('on', sel)
            s.setAttribute('aria-checked', String(sel))
          }
        }
        for (const p of providers) {
          const b = el('button', 'ob-provider')
          b.type = 'button'
          b.dataset.pid = p.id
          b.setAttribute('role', 'radio')
          const hasKey = !!p.keyOrigin
          b.title = hasKey
            ? `key from ${p.keyOrigin.kind === 'shell' || p.keyOrigin.kind === 'env' ? `${p.keyOrigin.kind}: ${p.keyOrigin.name}` : p.keyOrigin.kind}`
            : 'no key yet — paste one below'
          b.append(
            el('span', 'ob-dot-avail ' + (hasKey ? 'ok' : 'off'), hasKey ? '●' : '○'),
            el('span', 'ob-agent-name', p.label || p.id),
            el('span', 'ob-provider-model', p.model)
          )
          b.addEventListener('click', () => {
            state.provider = p.id
            state.dirty = true
            tome.store.set('chat-provider', p.id)
            keySave.dataset.pid = p.id
            paint(p.id)
          })
          group.appendChild(b)
        }
        keySave.addEventListener('click', async () => {
          const id = keySave.dataset.pid || state.provider || info.active
          if (!id) return toast('pick a provider first')
          try {
            await tome.chat.keySet(id, keyIn.value.trim())
            keyIn.value = ''
            toast('key saved', 'ok')
            // Refresh dots so ● reflects the paste immediately.
            const again = await tome.chat.providers()
            const row = again?.providers?.find((p) => p.id === id)
            const dot = group.querySelector(`[data-pid="${id}"] .ob-dot-avail`)
            if (dot && row?.keyOrigin) {
              dot.textContent = '●'
              dot.classList.remove('off')
              dot.classList.add('ok')
            }
          } catch (err) {
            toast(err.message)
          }
        })
        m.body.appendChild(group)
        m.body.appendChild(keyRow)
        paint(state.provider || info.active)
        keySave.dataset.pid = state.provider || info.active || providers[0].id
      })
      .catch(
        () => !stale() && note('Could not load providers — the assistant keeps its current settings.')
      )
  }

  function stepVoice() {
    note('One second of audio is transcribed on-device — nothing leaves the machine.')
    const btn = el('button', 'ag-btn ghost', 'Test microphone')
    btn.type = 'button'
    const out = el('p', 'ag-note')
    out.setAttribute('role', 'status')
    m.body.append(btn, out)
    // The whisper binary and the model file are separate one-time installs —
    // say which side is missing instead of a bare "not ready". sttUnavailable
    // is the same availability check main runs before every transcription, so
    // the wizard can never claim ready when the mic test would fail.
    const sttRow = el('div', 'ob-agent')
    const sttDot = el('span', 'ob-dot-avail off', '…')
    const sttName = el('span', 'ob-agent-name', 'Local whisper transcription')
    const sttHint = el('span', 'ob-agent-hint', 'checking…')
    sttRow.append(sttDot, sttName, sttHint)
    m.body.appendChild(sttRow)
    // Task 5: one-click model download, shown only when whisper is the engine
    // and the model (not the binary) is the missing half — the binary still
    // needs `brew install whisper-cpp`, which no in-app button can do.
    const downloadBtn = el('button', 'ag-btn ghost', 'Download model')
    downloadBtn.type = 'button'
    downloadBtn.hidden = true
    m.body.appendChild(downloadBtn)
    const myStep = state.step
    const paintStt = (s) => {
      if (state.step !== myStep) return // navigated away — body belongs to another step
      const apple = s.engine === 'apple'
      sttName.textContent = apple ? 'Apple on-device dictation' : 'local whisper transcription'
      downloadBtn.hidden = true
      if (apple) {
        sttDot.className = 'ob-dot-avail ' + (s.ready ? 'ok' : 'off')
        sttDot.textContent = s.ready ? '●' : '○'
        sttHint.textContent = s.ready
          ? 'Voice is ready — Apple on-device dictation, no setup needed.'
          : s.why
      } else if (s.ready) {
        sttDot.className = 'ob-dot-avail ok'
        sttDot.textContent = '●'
        sttHint.textContent = 'Local whisper transcription is ready.'
      } else if (!s.bin) {
        sttDot.className = 'ob-dot-avail off'
        sttDot.textContent = '○'
        sttHint.textContent = 'Install whisper-cli (`brew install whisper-cpp`) and restart.'
      } else {
        sttDot.className = 'ob-dot-avail off'
        sttDot.textContent = '○'
        sttHint.textContent = 'speech model not downloaded'
        downloadBtn.hidden = false
      }
    }
    tome.stt
      .status()
      .then(paintStt)
      .catch(() => {
        if (state.step === myStep) sttHint.textContent = 'status unavailable'
      })
    downloadBtn.addEventListener('click', async () => {
      if (downloadBtn.disabled) return
      downloadBtn.disabled = true
      downloadBtn.textContent = 'Downloading…'
      try {
        const res = await tome.stt.downloadModel()
        if (res?.error) {
          sttHint.textContent = res.error
        } else {
          sttHint.textContent = 'Speech model downloaded.'
          const s = await tome.stt.status().catch(() => null)
          if (s) paintStt(s)
        }
      } catch (err) {
        sttHint.textContent = err?.message || 'Download failed.'
      } finally {
        downloadBtn.disabled = false
        downloadBtn.textContent = 'Download model'
      }
    })
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
    // The wizard's other toggles write the exact keys Preferences uses — the
    // model warm-up opt-in lives here too (Preferences → Voice mirrors it).
    const warmupSw = el('button', 'prefs-switch')
    warmupSw.type = 'button'
    warmupSw.setAttribute('role', 'switch')
    warmupSw.append(el('span', 'prefs-knob'))
    const paintWarmup = () => {
      warmupSw.classList.toggle('on', !!state.warmup)
      warmupSw.setAttribute('aria-checked', String(!!state.warmup))
    }
    warmupSw.addEventListener('click', () => {
      state.warmup = !state.warmup
      state.dirty = true
      tome.store.set('voice-warmup', state.warmup)
      paintWarmup()
    })
    const warmupRow = el('div', 'ob-row')
    const warmupText = el('div', 'ob-row-text')
    warmupText.append(
      el('span', 'prefs-label', 'Warm up whisper at launch'),
      el('span', 'prefs-hint', 'loads the speech model in the background so the first dictation is instant')
    )
    warmupRow.append(warmupText, warmupSw)
    m.body.appendChild(warmupRow)
    tome.store.get('voice-warmup').then((v) => {
      state.warmup = !!v
      if (state.step === myStep) paintWarmup()
    })
    paintWarmup()
    // Navigating away mid-test must not leave the mic indicator on.
    state.cleanup = release
  }

  function stepSecurity() {
    toggle(
      'Spawn agents contained',
      'new agent panes start with no network until you allow it',
      () => prefs.egressDefault,
      (v) => {
        prefs.egressDefault = v
        tome.store.set('egress-default', v)
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
    item('Contained agents', prefs.egressDefault ? 'on' : 'off')
    item('Assistant commands', prefs.conductorRun ? 'on' : 'off')
    m.body.appendChild(summary)
    note('Change any of this anytime in Settings (⌘,).')
  }

  const RENDERERS = [stepWelcome, stepAgents, stepAssistant, stepVoice, stepSecurity, stepDone]
  render()
}
