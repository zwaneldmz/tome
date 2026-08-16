// Preferences (⌘,): one modal that surfaces the settings that were scattered
// across menus and key chords. Every control reads and writes the same
// persisted keys the original surfaces use, so nothing drifts out of sync.
import { tome, el, toast } from './util.js'
import { prefs } from './state.js'
import { modalShell } from './modals.js'
import { setTheme, themeState, THEME_ORDER, THEME_GLYPH } from './theme.js'
import { TERM_FONT, setTermFontSize } from './panels/terminal.js'
import { editorPrefs, setEditorPrefs } from './panels/editor.js'
import { totpModal } from './airgap-ui.js'
import { showOnboarding } from './onboarding.js'
import { activeWorkspace } from './workspaces.js'
import { mentorState, saveMentorSettings, setUq, uq } from './mentor.js'

const THEME_LABEL = { system: 'Match system', light: 'Light', dark: 'Dark' }
const SIDEBAR_DEFAULT = 236

// ---- custom agents ----
// A renderer-side MIRROR of the vet rules in src/main/lib/custom-agents.js,
// duplicated here so the form can reject inline instead of round-tripping a
// bad entry into the store. The mirror is convenience, never authority:
// main re-vets every entry on every read of 'custom-agents', so a stale or
// bypassed copy of these regexes degrades to "entry silently missing from
// the ＋ menu", never to a command line.
const AGENT_ID_RE = /^[a-z0-9][a-z0-9-]{0,31}$/
const AGENT_BIN_RE = /^[a-z0-9][a-z0-9._-]{0,63}$/i
const AGENT_MODELFLAG_RE = /^--[a-z-]{2,20}$/
const AGENT_ARG_BAD_RE = /[^\x20-\x7e]|[;&|`$<>"'\\\s]/
const AGENT_RESERVED = new Set([
  'claude',
  'opencode',
  'pi',
  'terminal',
  'chat',
  'brain',
  'flow',
  'runs',
  'doc',
  'editor',
  'events',
])

// Inline vet of one form entry → error string or null. Kept in lockstep
// with vetCustomAgent's messages so a main-side refusal (visible as a
// silently dropped row) is debuggable against what the form said.
function vetAgentDraft({ id, label, bin, args, modelFlag }) {
  if (!AGENT_ID_RE.test(id)) return 'id: 1–32 chars of [a-z0-9-], starting with a letter or digit'
  if (AGENT_RESERVED.has(id)) return `id "${id}" is a built-in pane kind`
  if (!label || label.length > 40 || !/^[\x20-\x7e]+$/.test(label)) return 'label: 1–40 chars of printable ASCII'
  if (!AGENT_BIN_RE.test(bin)) return 'bin: a bare command name (no path separators)'
  if (args.length > 8) return 'args: at most 8 tokens'
  for (const a of args)
    if (!a || a.length > 64 || AGENT_ARG_BAD_RE.test(a))
      return 'args: single inert tokens (≤64 chars, no spaces or shell metacharacters)'
  if (modelFlag && !AGENT_MODELFLAG_RE.test(modelFlag)) return 'model flag: like --model (/--[a-z-]{2,20}/)'
  return null
}

// The "Agents" Preferences section: the user's custom CLIs as rows (label,
// bin, args, availability dot) plus an add-form. Persists through the same
// 'custom-agents' store key main reads, and nudges main afterwards so the
// conductor's kind descriptions refresh in-session. Exported because WS-E
// onboarding mounts the same section on its own surface.
export async function buildAgentsSection() {
  const section = el('section', 'prefs-section')
  section.append(el('h4', '', 'Agents'))
  // The store is the single writer/reader contract: load once, keep one
  // list, write it back whole on every mutation. main re-vets on read, so
  // persisting exactly what the form vetted is belt, not the suspenders.
  let customs = (await tome.agents.customs()) || []
  const listed = await tome.agents.list()
  const available = new Map(listed.filter((a) => a.custom).map((a) => [a.name, a.available]))

  // Enabled/disabled picker — the same 'agents-disabled' key the onboarding
  // wizard's Agents step writes and the ＋ menu reads. Built-ins first, then
  // customs; an unavailable CLI can be toggled here (it will simply offer
  // itself greyed-out when it lands back in the menu).
  const disabled = new Set((await tome.store.get('agents-disabled')) || [])
  if (listed.length) {
    for (const a of listed) {
      const sw = el('button', 'prefs-switch')
      sw.type = 'button'
      sw.setAttribute('role', 'switch')
      sw.append(el('span', 'prefs-knob'))
      const paint = () => {
        sw.classList.toggle('on', !disabled.has(a.name))
        sw.setAttribute('aria-checked', String(!disabled.has(a.name)))
      }
      sw.addEventListener('click', () => {
        disabled.has(a.name) ? disabled.delete(a.name) : disabled.add(a.name)
        tome.store.set('agents-disabled', [...disabled])
        paint()
      })
      const r = row(section, a.label || a.name, sw, a.custom ? `${a.name} · custom` : a.available ? null : 'not installed')
      const dot = el('span', 'prefs-agent-dot' + (a.available ? ' on' : ''))
      dot.title = a.available ? `${a.bin || a.name} found on PATH` : 'not found on PATH'
      r.querySelector('.prefs-label').prepend(dot)
      paint()
    }
  }

  const persist = async () => {
    await tome.store.set('custom-agents', customs)
    tome.agents.changed() // conductor + agents:list re-read on next use
  }

  const list = el('div', 'prefs-agents')
  const renderRows = () => {
    list.innerHTML = ''
    if (!customs.length) {
      const empty = el('div', 'prefs-hint', 'No custom agents yet — add one below.')
      empty.style.padding = '4px 0'
      list.appendChild(empty)
    }
    for (const a of customs) {
      const r = el('div', 'prefs-row')
      const text = el('div', 'prefs-text')
      const head = el('span', 'prefs-label')
      const dot = el('span', 'prefs-agent-dot' + (available.get(a.id) ? ' on' : ''))
      dot.title = available.get(a.id) ? `${a.bin} found on PATH` : `${a.bin} not found on PATH`
      head.append(dot, a.label)
      text.append(head)
      text.append(
        el(
          'span',
          'prefs-hint',
          `${a.id} · ${[a.bin, ...(a.args || [])].join(' ')}${a.modelFlag ? ` · ${a.modelFlag}` : ''}`
        )
      )
      const remove = el('button', 'ag-btn ghost', 'Remove')
      remove.type = 'button'
      remove.addEventListener('click', async () => {
        customs = customs.filter((c) => c.id !== a.id)
        await persist()
        renderRows()
        toast(`removed ${a.label}`, 'ok')
      })
      r.append(text, remove)
      list.appendChild(r)
    }
  }
  renderRows()
  section.appendChild(list)

  // add-form: four small inputs + Add, inline errors under it
  const form = el('div', 'prefs-agent-form')
  const mk = (placeholder) => {
    const i = el('input')
    i.type = 'text'
    i.placeholder = placeholder
    i.setAttribute('aria-label', placeholder)
    i.spellcheck = false
    return i
  }
  const idIn = mk('id — e.g. aider')
  const labelIn = mk('label — e.g. Aider')
  const binIn = mk('bin — e.g. aider')
  const argsIn = mk('args · model flag')
  const err = el('div', 'prefs-hint')
  err.style.color = 'var(--danger, #e5534b)'
  const add = el('button', 'ag-btn ghost', 'Add')
  add.type = 'button'
  add.addEventListener('click', async () => {
    // The last field carries the two optional values space-separated:
    // arg tokens first, a trailing --flag as the model flag. Splitting here
    // is exactly the join the spawn builder performs, so what you type is
    // what the command line gets.
    const extras = argsIn.value.trim().split(/\s+/).filter(Boolean)
    const modelFlag = extras.find((t) => t.startsWith('--')) || ''
    const draft = {
      id: idIn.value.trim(),
      label: labelIn.value.trim(),
      bin: binIn.value.trim(),
      args: extras.filter((t) => t !== modelFlag),
      ...(modelFlag ? { modelFlag } : {}),
    }
    err.textContent = ''
    const bad = vetAgentDraft(draft)
    if (bad) {
      err.textContent = bad
      return
    }
    if (customs.some((c) => c.id === draft.id)) {
      err.textContent = `id "${draft.id}" is already in the list`
      return
    }
    customs = [...customs, draft]
    await persist()
    renderRows()
    idIn.value = labelIn.value = binIn.value = argsIn.value = ''
    toast(`added ${draft.label} — reopen the ＋ menu to spawn it`, 'ok')
  })
  form.append(idIn, labelIn, binIn, argsIn, add)
  section.appendChild(form)
  section.appendChild(err)
  return section
}

// Label on the left, control on the right.
function row(parent, label, ctrl, hint) {
  const r = el('div', 'prefs-row')
  const text = el('div', 'prefs-text')
  text.append(el('span', 'prefs-label', label))
  if (hint) text.append(el('span', 'prefs-hint', hint))
  r.append(text, ctrl)
  parent.appendChild(r)
  return r
}

// Single-line text input that persists a store key on change; clearing the
// field writes null (no store:delete — null reads as unset).
function textRow(parent, label, hint, key, initial) {
  const input = el('input', 'prefs-input')
  input.type = 'text'
  input.spellcheck = false
  input.value = initial || ''
  input.addEventListener('change', () => {
    const v = input.value.trim()
    tome.store.set(key, v || null)
  })
  row(parent, label, input, hint)
  return input
}

// Switch-style toggle: a button with role=switch whose .on state mirrors the
// menu item's .active state. Reads/writes the exact store keys and prefs
// mutations from menus.js populateAddMenu.
function toggleRow(parent, label, hint, get, set) {
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
    paint()
  })
  row(parent, label, sw, hint)
  paint()
}

export async function preferencesModal() {
  const m = modalShell('Settings')
  m.err.remove() // no error line — prefs report via toasts
  m.body.parentElement.classList.add('prefs-box')
  let voiceWarmup = !!(await tome.store.get('voice-warmup'))

  // ---------- appearance ----------
  const appearance = el('section', 'prefs-section')
  appearance.append(el('h4', '', 'Appearance'))
  const seg = el('div', 'prefs-seg')
  seg.setAttribute('role', 'radiogroup')
  seg.setAttribute('aria-label', 'Theme')
  for (const pref of THEME_ORDER) {
    const b = el('button', '', `${THEME_GLYPH[pref]} ${THEME_LABEL[pref]}`)
    b.type = 'button'
    b.setAttribute('role', 'radio')
    b.setAttribute('aria-checked', String(themeState.pref === pref))
    b.classList.toggle('on', themeState.pref === pref)
    b.addEventListener('click', () => {
      setTheme(pref) // persists 'theme' and re-skins live
      for (const s of seg.children) {
        const on = s === b
        s.classList.toggle('on', on)
        s.setAttribute('aria-checked', String(on))
      }
    })
    seg.appendChild(b)
  }
  row(appearance, 'Theme', seg)
  m.body.appendChild(appearance)

  // ---------- terminal ----------
  const terminal = el('section', 'prefs-section')
  terminal.append(el('h4', '', 'Terminal'))
  const size = await tome.store.get('term-font-size')
  let fontSize =
    typeof size === 'number' && size >= TERM_FONT.min && size <= TERM_FONT.max ? size : TERM_FONT.default
  const stepper = el('div', 'prefs-stepper')
  const value = el('span', 'prefs-value', String(fontSize))
  const apply = (next) => {
    fontSize = Math.min(TERM_FONT.max, Math.max(TERM_FONT.min, next))
    // setTermFontSize applies to every live terminal and persists
    // 'term-font-size', keeping Preferences and ⌘=/⌘-/⌘0 in sync
    setTermFontSize(fontSize)
    value.textContent = String(fontSize)
    minus.disabled = fontSize <= TERM_FONT.min
    plus.disabled = fontSize >= TERM_FONT.max
  }
  const minus = el('button', '', '−')
  minus.type = 'button'
  minus.setAttribute('aria-label', 'Decrease terminal font size')
  minus.addEventListener('click', () => apply(fontSize - 1))
  const plus = el('button', '', '+')
  plus.type = 'button'
  plus.setAttribute('aria-label', 'Increase terminal font size')
  plus.addEventListener('click', () => apply(fontSize + 1))
  stepper.append(minus, value, plus)
  row(terminal, 'Font size', stepper, `${TERM_FONT.min}–${TERM_FONT.max} · ⌘= / ⌘- / ⌘0`)
  minus.disabled = fontSize <= TERM_FONT.min
  plus.disabled = fontSize >= TERM_FONT.max
  m.body.appendChild(terminal)

  // ---------- editor ----------
  const editor = el('section', 'prefs-section')
  editor.append(el('h4', '', 'Editor'))
  const tabStep = el('div', 'prefs-stepper')
  const tabValue = el('span', 'prefs-value', String(editorPrefs.tabSize))
  const setTab = (n) => {
    const size = Math.min(8, Math.max(1, n))
    setEditorPrefs({ tabSize: size })
    tabValue.textContent = String(size)
    tabMinus.disabled = size <= 1
    tabPlus.disabled = size >= 8
  }
  const tabMinus = el('button', '', '−')
  tabMinus.type = 'button'
  tabMinus.setAttribute('aria-label', 'Decrease indent size')
  tabMinus.addEventListener('click', () => setTab(editorPrefs.tabSize - 1))
  const tabPlus = el('button', '', '+')
  tabPlus.type = 'button'
  tabPlus.setAttribute('aria-label', 'Increase indent size')
  tabPlus.addEventListener('click', () => setTab(editorPrefs.tabSize + 1))
  tabStep.append(tabMinus, tabValue, tabPlus)
  row(editor, 'Indent size', tabStep, 'spaces per Tab · 1–8')
  tabMinus.disabled = editorPrefs.tabSize <= 1
  tabPlus.disabled = editorPrefs.tabSize >= 8
  toggleRow(
    editor,
    'Wrap long lines',
    'soft-wrap instead of scrolling sideways',
    () => editorPrefs.wrap,
    (v) => setEditorPrefs({ wrap: v })
  )
  toggleRow(
    editor,
    'Trim trailing whitespace on save',
    'applied to the buffer, so the pane stays clean',
    () => editorPrefs.trimOnSave,
    (v) => setEditorPrefs({ trimOnSave: v })
  )
  toggleRow(
    editor,
    'Format on save',
    'Prettier, using the project’s own config',
    () => editorPrefs.formatOnSave,
    (v) => setEditorPrefs({ formatOnSave: v })
  )
  toggleRow(
    editor,
    'Autosave',
    'save a moment after you stop typing',
    () => editorPrefs.autosave,
    (v) => setEditorPrefs({ autosave: v })
  )
  m.body.appendChild(editor)

  // ---------- assistant ----------
  // Provider choice + model override for the assistant pane. Keys are NOT
  // stored: they come from the login shell (main's ensureLoginEnv), so this
  // section only shows whether each key was found — never the key itself.
  const assistant = el('section', 'prefs-section')
  assistant.append(el('h4', '', 'Assistant'))
  const chatInfo = await tome.chat.providers().catch(() => null)
  if (chatInfo) {
    const pseg = el('div', 'prefs-seg')
    pseg.setAttribute('role', 'radiogroup')
    pseg.setAttribute('aria-label', 'Assistant provider')
    const modelRow = { input: null } // filled below; provider switch repopulates it
    for (const p of chatInfo.providers) {
      // ● key found in the login shell · ○ missing — set it and restart
      const b = el('button', '', `${p.keySet ? '●' : '○'} ${p.label}`)
      b.type = 'button'
      b.setAttribute('role', 'radio')
      b.title = p.keySet ? `${p.keyEnv} found in your login shell` : `${p.keyEnv} not found in your login shell`
      b.setAttribute('aria-checked', String(chatInfo.active === p.id))
      b.classList.toggle('on', chatInfo.active === p.id)
      b.addEventListener('click', () => {
        tome.store.set('chat-provider', p.id)
        for (const s of pseg.children) {
          const on = s === b
          s.classList.toggle('on', on)
          s.setAttribute('aria-checked', String(on))
        }
        // Repopulate the model field with the newly picked provider's
        // default; the stored override only makes sense per provider.
        if (modelRow.input) {
          modelRow.input.value = p.model
          tome.store.set('chat-model', null)
        }
      })
      pseg.appendChild(b)
    }
    row(assistant, 'Provider', pseg, '● key found in your login shell · ○ missing')
    const activeEntry = chatInfo.providers.find((p) => p.id === chatInfo.active)
    const storedModel = await tome.store.get('chat-model')
    modelRow.input = textRow(
      assistant,
      'Model',
      'blank = provider default',
      'chat-model',
      typeof storedModel === 'string' && storedModel ? storedModel : activeEntry?.model
    )
    const keysHint = el(
      'div',
      'prefs-hint',
      'keys come from your login shell — set MOONSHOT_API_KEY / ZHIPU_API_KEY / ANTHROPIC_API_KEY and restart'
    )
    assistant.appendChild(keysHint)
  } else {
    assistant.appendChild(el('div', 'prefs-hint', 'Provider list unavailable.'))
  }
  m.body.appendChild(assistant)

  // ---------- security ----------
  const security = el('section', 'prefs-section')
  security.append(el('h4', '', 'Security'))
  toggleRow(
    security,
    'Spawn agents air-gapped',
    null,
    () => prefs.airgapDefault,
    (v) => {
      prefs.airgapDefault = v
      tome.store.set('airgap-default', v)
    }
  )
  toggleRow(
    security,
    'Assistant may run commands',
    null,
    () => prefs.conductorRun,
    (v) => {
      prefs.conductorRun = v
      tome.store.set('conductor-run', v)
      tome.conductor.allowRun(v)
    }
  )
  const enroll = el('button', 'ag-btn ghost', 'Enroll authenticator (2FA)…')
  enroll.type = 'button'
  enroll.addEventListener('click', () => {
    m.close()
    totpModal()
  })
  row(security, 'Two-factor authentication', enroll, 'required to open an air-gapped pane')
  m.body.appendChild(security)

  // ---------- voice ----------
  // Whisper availability + the launch warm-up opt-in the onboarding wizard's
  // Voice step writes — both surfaces use the same store key and the same
  // stt:status probe, so they can never disagree.
  const voice = el('section', 'prefs-section')
  voice.append(el('h4', '', 'Voice'))
  const sttStatus = el('div', 'prefs-hint', 'Checking local whisper…')
  voice.appendChild(sttStatus)
  tome.stt
    .status()
    .then((s) => {
      sttStatus.textContent = s.ready
        ? 'Local whisper transcription is ready.'
        : !s.bin
          ? 'whisper-cli not found — install it (brew install whisper-cpp) and restart.'
          : 'Speech model missing — the push-to-talk error message carries the one-time download command.'
    })
    .catch(() => (sttStatus.textContent = 'Whisper status unavailable.'))
  toggleRow(
    voice,
    'Warm up whisper at launch',
    'loads the speech model in the background so the first dictation is instant',
    () => voiceWarmup,
    (v) => {
      voiceWarmup = v
      tome.store.set('voice-warmup', v)
    }
  )
  m.body.appendChild(voice)

  // ---------- agents ----------
  m.body.appendChild(await buildAgentsSection())

  // ---------- sidebar ----------
  const sidebar = el('section', 'prefs-section')
  sidebar.append(el('h4', '', 'Sidebar'))
  const tree = document.getElementById('tree')
  const reset = el('button', 'ag-btn ghost', 'Reset width')
  reset.type = 'button'
  const width = el('span', 'prefs-value')
  const paintWidth = () => {
    // Read on demand (click + open), not during render — a layout read in
    // the build path forces sync reflow of the whole app under the modal.
    const current = Math.round(tree.getBoundingClientRect().width) || SIDEBAR_DEFAULT
    width.textContent = `${current} px`
  }
  reset.addEventListener('click', () => {
    tree.style.width = ''
    tome.store.set('sidebar-width', null) // no store:delete — null reads as unset
    paintWidth()
    toast('Sidebar width reset', 'ok')
  })
  const widthBox = el('div', 'prefs-inline')
  widthBox.append(width, reset)
  row(sidebar, 'Width', widthBox, `drag the divider · default ${SIDEBAR_DEFAULT} px`)
  paintWidth()
  m.body.appendChild(sidebar)

  // ---------- mentor ----------
  const mentor = el('section', 'prefs-section')
  mentor.append(el('h4', '', 'Mentor'))
  toggleRow(
    mentor,
    'Verbose guide (default)',
    'new workspaces teach rather than just do',
    () => mentorState.verboseDefault,
    (v) => saveMentorSettings({ verboseDefault: v })
  )
  toggleRow(
    mentor,
    'Test before implementing',
    'the mentor writes a failing test and checks understanding first',
    () => mentorState.gate,
    (v) => saveMentorSettings({ gate: v })
  )
  toggleRow(
    mentor,
    'Gate before commit',
    null,
    () => mentorState.gatePoints.commit,
    (v) => saveMentorSettings({ gatePoints: { ...mentorState.gatePoints, commit: v } })
  )
  toggleRow(
    mentor,
    'Gate before push',
    null,
    () => mentorState.gatePoints.push,
    (v) => saveMentorSettings({ gatePoints: { ...mentorState.gatePoints, push: v } })
  )
  const thrInput = el('input', 'prefs-input')
  thrInput.type = 'number'
  thrInput.min = '0'
  thrInput.max = '100'
  thrInput.value = String(mentorState.threshold)
  thrInput.addEventListener('change', () => {
    const n = Math.max(0, Math.min(100, Number(thrInput.value) || 0))
    saveMentorSettings({ threshold: n })
    thrInput.value = String(n)
  })
  row(mentor, 'Pass threshold', thrInput, 'understanding score needed to pass a gate · 0–100')
  const mix = el('div', 'prefs-mix')
  const MIX = [
    ['multiple_choice', 'Multiple choice'],
    ['true_false', 'True / false'],
    ['short_answer', 'Short answer'],
    ['code', 'Code'],
  ]
  for (const [key, label] of MIX) {
    const item = el('span', 'prefs-mix-item')
    const sw = el('button', 'prefs-switch')
    sw.type = 'button'
    sw.setAttribute('role', 'switch')
    sw.append(el('span', 'prefs-knob'))
    const paint = () => {
      const on = mentorState.questionTypes.includes(key)
      sw.classList.toggle('on', on)
      sw.setAttribute('aria-checked', String(on))
    }
    sw.addEventListener('click', () => {
      const next = mentorState.questionTypes.includes(key)
        ? mentorState.questionTypes.filter((t) => t !== key)
        : [...mentorState.questionTypes, key]
      saveMentorSettings({ questionTypes: next })
      paint()
    })
    item.append(el('span', 'prefs-mix-label', label), sw)
    mix.appendChild(item)
    paint()
  }
  row(mentor, 'Question mix', mix, 'which kinds of question the gate may ask')
  const resetUq = el('button', 'ag-btn ghost', 'Reset understanding score')
  resetUq.type = 'button'
  resetUq.addEventListener('click', () => {
    if (!activeWorkspace()) return toast('no active workspace to reset')
    setUq(0)
    toast('understanding score reset', 'ok')
  })
  row(mentor, 'Understanding score', resetUq, `per workspace · currently ${uq()}`)
  m.body.appendChild(mentor)

  // ---------- onboarding ----------
  const onboarding = el('section', 'prefs-section')
  onboarding.append(el('h4', '', 'Onboarding'))
  const replay = el('button', 'ag-btn ghost', 'Replay setup wizard…')
  replay.type = 'button'
  replay.addEventListener('click', () => {
    m.close()
    showOnboarding()
  })
  row(onboarding, 'Setup wizard', replay, 'the first-run tour — agents, assistant, voice, security')
  m.body.appendChild(onboarding)
}
