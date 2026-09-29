// Appearance: 'system' | 'light' | 'dark'.
//
// CSS carries both palettes — `:root` is light, `[data-theme='dark']` is the
// neon dark, and a `prefers-color-scheme` block covers the frames before this
// module has read the stored preference (CSP forbids an inline pre-paint
// script, so the media query is the anti-flash).
//
// Anything that can't be styled by CSS — xterm, CodeMirror, the doc iframes
// rendered in main — subscribes via onTheme() and re-skins live.
import { Compartment } from '@codemirror/state'
import { oneDark } from '@codemirror/theme-one-dark'
import { tome } from './util.js'

const mq = matchMedia('(prefers-color-scheme: dark)')
const subs = new Set()

export const themeState = { pref: 'system', mode: mq.matches ? 'dark' : 'light' }
export const isDark = () => themeState.mode === 'dark'
export const THEME_ORDER = ['system', 'light', 'dark']
export const THEME_GLYPH = { system: '◐', light: '☀', dark: '☾' }

const resolve = () => (themeState.pref === 'system' ? (mq.matches ? 'dark' : 'light') : themeState.pref)

function apply() {
  themeState.mode = resolve()
  document.documentElement.dataset.theme = themeState.mode
  // popout windows are separate documents — keep their <html> stamped too
  for (const w of popoutDocs) {
    try {
      w.documentElement.dataset.theme = themeState.mode
    } catch {}
  }
  tome.theme?.set(themeState.pref, themeState.mode)
  for (const cb of subs) {
    try {
      cb(themeState.mode)
    } catch (err) {
      console.warn('theme subscriber failed:', err)
    }
  }
}

// Documents belonging to popped-out pane windows (registered by panes.js).
const popoutDocs = new Set()
export function trackThemedDocument(doc) {
  popoutDocs.add(doc)
  doc.documentElement.dataset.theme = themeState.mode
  return () => popoutDocs.delete(doc)
}

/** Subscribe to theme changes. Fires immediately with the current mode. */
export function onTheme(cb) {
  subs.add(cb)
  cb(themeState.mode)
  return () => subs.delete(cb)
}

export function setTheme(pref) {
  themeState.pref = THEME_ORDER.includes(pref) ? pref : 'system'
  tome.store.set('theme', themeState.pref)
  apply()
}

export function cycleTheme() {
  setTheme(THEME_ORDER[(THEME_ORDER.indexOf(themeState.pref) + 1) % THEME_ORDER.length])
  return themeState.pref
}

export async function bootTheme() {
  try {
    const saved = await tome.store.get('theme')
    if (THEME_ORDER.includes(saved)) themeState.pref = saved
  } catch {}
  mq.addEventListener('change', () => themeState.pref === 'system' && apply())
  apply()
}

// ---------- CodeMirror ----------
// Dark gets one-dark; light gets CodeMirror's own default, which is already a
// clean white sheet — no extra theme dependency for the sake of one palette.
export function cmTheme() {
  const compartment = new Compartment()
  const value = () => (isDark() ? oneDark : [])
  return {
    /** a fresh binding for a new EditorState */
    ext: () => compartment.of(value()),
    /** keep a live view in sync; returns an unsubscribe */
    attach: (view) =>
      onTheme(() => view.dispatch({ effects: compartment.reconfigure(value()) })),
  }
}

// ---------- xterm palettes ----------
// xterm paints to a canvas, so it can't read CSS variables; these mirror the
// --term-* intent of each palette in style.css.
const XTERM_DARK = {
  background: '#15161a',
  foreground: '#c9cace',
  cursor: '#95a6ff',
  cursorAccent: '#15161a',
  selectionBackground: 'rgba(149,166,255,0.26)',
  black: '#1c1e23',
  red: '#f07078',
  green: '#6fcf9a',
  yellow: '#e2c275',
  blue: '#7aa2f7',
  magenta: '#c49cf2',
  cyan: '#6cc6d9',
  white: '#c9cace',
  brightBlack: '#5f6268',
  brightRed: '#ff8f96',
  brightGreen: '#8fe0b3',
  brightYellow: '#f0d494',
  brightBlue: '#9db8ff',
  brightMagenta: '#d8b8ff',
  brightCyan: '#90dcea',
  brightWhite: '#f1f1f3',
}

const XTERM_LIGHT = {
  background: '#ffffff',
  foreground: '#2b2c30',
  cursor: '#3b54d4',
  cursorAccent: '#ffffff',
  selectionBackground: 'rgba(59,84,212,0.18)',
  black: '#2b2c30',
  red: '#c4323f',
  green: '#1c7f55',
  yellow: '#8f6a00',
  blue: '#2f4fc4',
  magenta: '#8a3fb8',
  cyan: '#0e7285',
  white: '#d8d8dc',
  brightBlack: '#62656b',
  brightRed: '#dd4652',
  brightGreen: '#23996a',
  brightYellow: '#a87e00',
  brightBlue: '#3b54d4',
  brightMagenta: '#a452d4',
  brightCyan: '#128ba1',
  brightWhite: '#111214',
}

export const xtermTheme = (mode = themeState.mode) => (mode === 'dark' ? XTERM_DARK : XTERM_LIGHT)
