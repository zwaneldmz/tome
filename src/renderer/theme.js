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
  background: '#0b0b11',
  foreground: '#c9d4e3',
  cursor: '#ff2ea6',
  cursorAccent: '#0b0b11',
  selectionBackground: 'rgba(0,229,255,0.22)',
  black: '#11131c',
  red: '#ff3b5c',
  green: '#3dff9e',
  yellow: '#ffd23e',
  blue: '#00a6ff',
  magenta: '#ff2ea6',
  cyan: '#00e5ff',
  white: '#c9d4e3',
  brightBlack: '#566179',
  brightRed: '#ff6b84',
  brightGreen: '#7dffbe',
  brightYellow: '#ffe37e',
  brightBlue: '#57c4ff',
  brightMagenta: '#ff7ec9',
  brightCyan: '#7ef2ff',
  brightWhite: '#eef4fb',
}

const XTERM_LIGHT = {
  background: '#ffffff',
  foreground: '#2b2b31',
  cursor: '#d70a63',
  cursorAccent: '#ffffff',
  selectionBackground: 'rgba(0,113,227,0.18)',
  black: '#2b2b31',
  red: '#c9214a',
  green: '#127a4b',
  yellow: '#8a6100',
  blue: '#0058c4',
  magenta: '#b31378',
  cyan: '#00707f',
  white: '#d8d8dd',
  brightBlack: '#6e6e78',
  brightRed: '#e0335f',
  brightGreen: '#1a9c62',
  brightYellow: '#a97a00',
  brightBlue: '#0071e3',
  brightMagenta: '#d21a90',
  brightCyan: '#008ea1',
  brightWhite: '#101014',
}

export const xtermTheme = (mode = themeState.mode) => (mode === 'dark' ? XTERM_DARK : XTERM_LIGHT)
