// Terminal/agent pane: xterm + the air-gap strip for gapped panes.
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import { tome } from '../util.js'
import { terms, strips } from '../regs.js'
import { onTheme, xtermTheme } from '../theme.js'
import { stripRender, airgapModal } from '../airgap-ui.js'

// Terminal font size is user-adjustable (⌘=/⌘-/⌘0, handled in keys.js) and
// persisted so every terminal — current and future — shares it.
export const TERM_FONT = { default: 12.5, min: 8, max: 28 }
let termFontSize = TERM_FONT.default

tome.store.get('term-font-size').then((v) => {
  if (typeof v === 'number' && v >= TERM_FONT.min && v <= TERM_FONT.max) {
    termFontSize = v
    for (const term of terms.values()) term.options.fontSize = v
  }
})

// delta: +1/-1 to step, 0 to reset. Every live terminal follows, and a
// resize event nudges each FitAddon to re-measure at the new cell size.
export function zoomTerminals(delta) {
  termFontSize =
    delta === 0
      ? TERM_FONT.default
      : Math.min(TERM_FONT.max, Math.max(TERM_FONT.min, termFontSize + delta))
  for (const term of terms.values()) term.options.fontSize = termFontSize
  window.dispatchEvent(new window.Event('resize'))
  tome.store.set('term-font-size', termFontSize)
}

export class TerminalPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-terminal'
  }
  init({ params, api }) {
    this.ptyId = params.ptyId
    let termHost = this.element
    if (params.airgap) {
      this.element.classList.add('airgapped')
      const strip = document.createElement('div')
      strip.className = 'airgap-strip'
      const label = document.createElement('span')
      label.className = 'ag-label'
      const flash = document.createElement('span')
      flash.className = 'ag-flash'
      const count = document.createElement('span')
      count.className = 'ag-count'
      strip.append(label, flash, count)
      strip.addEventListener('click', () => airgapModal(this.ptyId))
      termHost = document.createElement('div')
      termHost.className = 'termbox'
      this.element.append(strip, termHost)
      strips.set(this.ptyId, strip)
      stripRender(this.ptyId)
    }
    const term = new Terminal({
      fontSize: termFontSize,
      fontFamily: "'MesloLGS NF', 'JetBrainsMono Nerd Font', ui-monospace, Menlo, monospace",
      cursorBlink: true,
      theme: xtermTheme(),
    })
    // xterm paints to a canvas, so it can't inherit the CSS palette
    this.untheme = onTheme((mode) => {
      term.options.theme = xtermTheme(mode)
    })
    const fit = new FitAddon()
    term.loadAddon(fit)
    term.open(termHost)
    terms.set(this.ptyId, term)
    // main already re-signals failures as red text over pty:data; this catch
    // keeps a spawn error from surfacing as an unhandled rejection too
    tome.pty
      .create({
        id: this.ptyId,
        kind: params.kind,
        cwd: params.cwd,
        airgap: params.airgap,
        ws: params.ws,
      })
      .catch((err) => {
        console.error('pty:create failed:', err)
        term.write(`\r\n\x1b[31mpane failed to start: ${err?.message || err}\x1b[0m\r\n`)
      })
    term.onData((d) => tome.pty.write(this.ptyId, d))
    term.onResize(({ cols, rows }) => tome.pty.resize(this.ptyId, cols, rows))
    const refit = () => {
      try {
        fit.fit()
      } catch {}
    }
    api.onDidDimensionsChange(refit)
    api.onDidActiveChange(({ isActive }) => isActive && setTimeout(() => term.focus(), 0))
    requestAnimationFrame(refit)
    this.term = term
  }
  dispose() {
    tome.pty.kill(this.ptyId)
    terms.delete(this.ptyId)
    strips.delete(this.ptyId)
    this.untheme?.()
    this.term?.dispose()
  }
}
