// Inline SVG icon set — small, stroke-based, inherits currentColor so every
// icon tracks the design tokens across light/dark. Replaces the raw Unicode
// glyphs that rendered inconsistently across fonts/platforms. The ▚ sigil and
// ⛨/⛉ air-gap shields stay as text (brand), but chrome actions live here.
//
// Each icon is a function returning an <svg> element (16×16 viewBox, 1.6px
// stroke, round caps — the Apple-ish line weight at these sizes).

const SVG_NS = 'http://www.w3.org/2000/svg'

function svg(paths, { filled = false } = {}) {
  const s = document.createElementNS(SVG_NS, 'svg')
  s.setAttribute('viewBox', '0 0 16 16')
  s.setAttribute('width', '16')
  s.setAttribute('height', '16')
  s.setAttribute('aria-hidden', 'true')
  s.classList.add('icon')
  for (const d of [].concat(paths)) {
    const p = document.createElementNS(SVG_NS, 'path')
    p.setAttribute('d', d)
    if (filled) {
      p.setAttribute('fill', 'currentColor')
    } else {
      p.setAttribute('fill', 'none')
      p.setAttribute('stroke', 'currentColor')
      p.setAttribute('stroke-width', '1.6')
      p.setAttribute('stroke-linecap', 'round')
      p.setAttribute('stroke-linejoin', 'round')
    }
    s.appendChild(p)
  }
  return s
}

// ---- files & folders ----
export const folderIcon = (open = false) =>
  svg(
    open
      ? 'M2 4.5 A1.5 1.5 0 0 1 3.5 3 H6 l1.2 1.5 H12.5 A1.5 1.5 0 0 1 14 6 V7 H3.2 L2 11.5 Z M3.2 7 H14 L12.4 12 A1.5 1.5 0 0 1 11 13 H4 A1.5 1.5 0 0 1 2.6 12 Z'
      : 'M2 4.5 A1.5 1.5 0 0 1 3.5 3 H6 l1.2 1.5 H12.5 A1.5 1.5 0 0 1 14 6 V11.5 A1.5 1.5 0 0 1 12.5 13 H3.5 A1.5 1.5 0 0 1 2 11.5 Z'
  )

export const fileIcon = () =>
  svg('M4 1.8 H9 L12 4.8 V13.7 A1.3 1.3 0 0 1 10.7 15 H4 A1.3 1.3 0 0 1 2.7 13.7 V3 A1.3 1.3 0 0 1 4 1.8 Z M8.7 2 V5 H11.7')

// file/folder glyphs shrunk into the top-left so a "+" badge fits bottom-right
// without the strokes crossing at 16px
export const newFileIcon = () =>
  svg([
    'M3.2 1.2 H6.6 L8.8 3.4 V9.2 A1 1 0 0 1 7.8 10.2 H3.2 A1 1 0 0 1 2.2 9.2 V2.2 A1 1 0 0 1 3.2 1.2 Z',
    'M6.4 1.4 V3.6 H8.6',
    'M12 9.6 V14.4 M9.6 12 H14.4',
  ])
export const newFolderIcon = () =>
  svg([
    'M1.6 4.6 A1 1 0 0 1 2.6 3.6 H4.6 l0.9 1 H8.4 A1 1 0 0 1 9.2 5.4 V8.6 A1 1 0 0 1 8.4 9.4 H2.6 A1 1 0 0 1 1.6 8.6 Z',
    'M12 9.6 V14.4 M9.6 12 H14.4',
  ])

// ---- topbar chrome ----
export const bellIcon = () =>
  svg([
    'M8 2.2 A3.6 3.6 0 0 0 4.4 5.8 C4.4 8.5 3.4 9.8 2.6 10.6 H13.4 C12.6 9.8 11.6 8.5 11.6 5.8 A3.6 3.6 0 0 0 8 2.2 Z',
    'M6.6 12.6 A1.5 1.5 0 0 0 9.4 12.6',
  ])

// appearance — a circle half-lit (system), a sun (light), a moon (dark)
export const themeIcon = (pref) => {
  if (pref === 'light')
    return svg([
      'M8 5.4 A2.6 2.6 0 1 0 8 10.6 A2.6 2.6 0 1 0 8 5.4 Z',
      'M8 1.6 V3 M8 13 V14.4 M2.6 8 H1.2 M14.8 8 H13.4 M4 4 L3 3 M13 13 L12 12 M4 12 L3 13 M13 3 L12 4',
    ])
  if (pref === 'dark') return svg('M13.2 9.6 A5.4 5.4 0 1 1 6.4 2.8 A4.4 4.4 0 1 0 13.2 9.6 Z')
  // system: half-filled circle
  const s = svg('M8 2.6 A5.4 5.4 0 1 0 8 13.4 A5.4 5.4 0 1 0 8 2.6 Z')
  const half = document.createElementNS(SVG_NS, 'path')
  half.setAttribute('d', 'M8 2.6 A5.4 5.4 0 0 1 8 13.4 Z')
  half.setAttribute('fill', 'currentColor')
  s.appendChild(half)
  return s
}

// sidebar collapse — a pane with an arrow
export const sidebarIcon = () =>
  svg(['M2.4 2.8 H13.6 A1.2 1.2 0 0 1 14.8 4 V12 A1.2 1.2 0 0 1 13.6 13.2 H2.4 A1.2 1.2 0 0 1 1.2 12 V4 A1.2 1.2 0 0 1 2.4 2.8 Z', 'M6 2.8 V13.2'])

// git branch
export const branchIcon = () =>
  svg([
    'M4.5 2.2 A1.3 1.3 0 1 0 4.5 4.8 A1.3 1.3 0 1 0 4.5 2.2 Z',
    'M4.5 11.2 A1.3 1.3 0 1 0 4.5 13.8 A1.3 1.3 0 1 0 4.5 11.2 Z',
    'M11.5 5.2 A1.3 1.3 0 1 0 11.5 7.8 A1.3 1.3 0 1 0 11.5 5.2 Z',
    'M4.5 4.8 V11.2 M11.5 7.8 C11.5 10 9.5 10.2 4.5 10.2',
  ])

// tear-off / popout — a square with an arrow leaving top-right
export const popoutIcon = () =>
  svg(['M9.5 1.8 H14.2 V6.5 M14.2 1.8 L8.5 7.5', 'M12 9.5 V13.2 A1.3 1.3 0 0 1 10.7 14.5 H2.8 A1.3 1.3 0 0 1 1.5 13.2 V5.3 A1.3 1.3 0 0 1 2.8 4 H6.5'])

// plus (used at larger stroke for the add button)
export const plusIcon = () => svg('M8 2.5 V13.5 M2.5 8 H13.5')

// microphone — capsule on a cradle stand, for the topbar voice toggle
export const micIcon = () =>
  svg([
    'M8 1.8 A2.2 2.2 0 0 0 5.8 4 V7.6 A2.2 2.2 0 0 0 10.2 7.6 V4 A2.2 2.2 0 0 0 8 1.8 Z',
    'M4 7.2 A4 4 0 0 0 12 7.2 M8 11.2 V14.2 M5.6 14.2 H10.4',
  ])

// status-bar context: terminal prompt, chat bubble, note/brain, history clock
export const terminalIcon = () =>
  svg(['M2.6 3 H13.4 A1.2 1.2 0 0 1 14.6 4.2 V11.8 A1.2 1.2 0 0 1 13.4 13 H2.6 A1.2 1.2 0 0 1 1.4 11.8 V4.2 A1.2 1.2 0 0 1 2.6 3 Z', 'M4 6 L6.5 8 L4 10 M8 10.5 H12'])
export const chatIcon = () =>
  svg('M2.5 3 H13.5 A1.3 1.3 0 0 1 14.8 4.3 V10.5 A1.3 1.3 0 0 1 13.5 11.8 H6 L3 14.5 V11.8 H2.5 A1.3 1.3 0 0 1 1.2 10.5 V4.3 A1.3 1.3 0 0 1 2.5 3 Z')
export const brainIcon = () =>
  svg(['M12.5 8 A4.5 4.5 0 1 0 12.5 8.01 Z', 'M8 5.5 V10.5 M5.5 8 H10.5'])
export const historyIcon = () =>
  svg(['M8 2.4 A5.6 5.6 0 1 0 8 13.6 A5.6 5.6 0 1 0 8 2.4 Z', 'M8 5 V8.2 L10.3 9.8'])
export const docIcon = fileIcon
