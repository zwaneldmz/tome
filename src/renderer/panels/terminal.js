// Terminal/agent pane: xterm + the air-gap strip for gapped panes.
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import { tome } from '../util.js'
import { terms, strips } from '../regs.js'
import { stripRender, airgapModal } from '../airgap-ui.js'

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
      strip.append(label, flash)
      strip.addEventListener('click', () => airgapModal(this.ptyId))
      termHost = document.createElement('div')
      termHost.className = 'termbox'
      this.element.append(strip, termHost)
      strips.set(this.ptyId, strip)
      stripRender(this.ptyId)
    }
    const term = new Terminal({
      fontSize: 12.5,
      fontFamily: "'MesloLGS NF', 'JetBrainsMono Nerd Font', ui-monospace, Menlo, monospace",
      cursorBlink: true,
      theme: {
        background: '#060609',
        foreground: '#c9d4e3',
        cursor: '#ff2ea6',
        cursorAccent: '#060609',
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
      },
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
    this.term?.dispose()
  }
}
