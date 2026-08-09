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

const THEME_LABEL = { system: 'Match system', light: 'Light', dark: 'Dark' }
const SIDEBAR_DEFAULT = 236

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

  // ---------- onboarding ----------
  const onboarding = el('section', 'prefs-section')
  onboarding.append(el('h4', '', 'Onboarding'))
  const replay = el('button', 'ag-btn ghost', 'Replay setup wizard…')
  replay.type = 'button'
  replay.addEventListener('click', () => {
    m.close()
    showOnboarding()
  })
  onboarding.appendChild(replay)
  m.body.appendChild(onboarding)
}
