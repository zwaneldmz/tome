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
  const m = modalShell('Preferences')
  m.err.remove() // no error line — prefs report via toasts
  m.body.parentElement.classList.add('prefs-box')

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
  security.appendChild(enroll)
  m.body.appendChild(security)

  // ---------- agents ----------
  m.body.appendChild(await buildAgentsSection())

  // ---------- sidebar ----------
  const sidebar = el('section', 'prefs-section')
  sidebar.append(el('h4', '', 'Sidebar'))
  const tree = document.getElementById('tree')
  const current = Math.round(tree.getBoundingClientRect().width) || SIDEBAR_DEFAULT
  const width = el('span', 'prefs-value', `${current} px`)
  row(sidebar, 'Current width', width, `default ${SIDEBAR_DEFAULT} px`)
  const reset = el('button', 'ag-btn ghost', 'Reset sidebar width')
  reset.type = 'button'
  reset.addEventListener('click', () => {
    tree.style.width = ''
    tome.store.set('sidebar-width', null) // no store:delete — null reads as unset
    width.textContent = `${SIDEBAR_DEFAULT} px`
    toast('Sidebar width reset', 'ok')
  })
  sidebar.appendChild(reset)
  m.body.appendChild(sidebar)
}
