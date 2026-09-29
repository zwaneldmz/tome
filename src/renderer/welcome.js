// What the grid shows when nothing is open: the few ways to start, with the
// keys that do the same thing next time. dockview mounts it as the watermark.
import { tome, el } from './util.js'
import { prefs } from './state.js'
import { addTerminal, addChat } from './panes.js'
import { activeWorkspace } from './workspaces.js'
import { openFolder } from './menus.js'
import { spawnPolicy } from './spawn-policy.js'

const isMac = navigator.platform.startsWith('Mac')
const MOD = isMac ? '⌘' : 'Ctrl+'

export class Welcome {
  constructor() {
    this.element = el('div', 'welcome')
    this.element.__welcome = this
    this.render()
  }
  init() {}
  async render() {
    const w = activeWorkspace()
    const policy = spawnPolicy(prefs)
    const box = el('div', 'welcome-box')
    const where = el('div', 'welcome-where', w ? w.name : 'No folder open')
    const lede = el(
      'p',
      'welcome-lede',
      !w
        ? 'Open a project folder to start agents in it.'
        : policy.agentsGapped
          ? 'Agents start sealed: sandboxed, and able to reach model APIs only.'
          : 'Agents start unsealed, with your full network. Change that from the shield above.'
    )
    box.append(where, lede)

    if (!w) {
      const open = el('button', 'btn-primary welcome-open', 'Open folder…')
      open.addEventListener('click', openFolder)
      box.appendChild(open)
    } else {
      const grid = el('div', 'welcome-grid')
      let agents = []
      try {
        const disabled = new Set((await tome.store.get('agents-disabled')) || [])
        agents = (await tome.agents.list()).filter((a) => a.available && !disabled.has(a.name))
      } catch {}
      for (const a of agents) grid.appendChild(card(a.label || a.name, policy.agentsGapped ? 'sealed agent' : 'agent', () => addTerminal(a.name), true))
      if (policy.showUnsandboxedTerminal) grid.appendChild(card('Terminal', 'host shell', () => addTerminal('terminal')))
      grid.appendChild(card('Assistant', 'chat', () => addChat()))
      box.appendChild(grid)
      if (!agents.length)
        box.appendChild(el('p', 'welcome-note', 'No agent CLIs found on your PATH. Install claude, opencode, or pi, or add your own in Settings → Agents.'))
    }

    const keys = el('div', 'welcome-keys')
    for (const [k, what] of [
      [MOD + 'K', 'find anything'],
      [MOD + 'T', 'open something new'],
      [MOD + 'J', 'next agent waiting for you'],
      [MOD + '⇧G', 'review changes'],
    ]) {
      const row = el('div', 'welcome-key')
      row.append(el('kbd', '', k), el('span', '', what))
      keys.appendChild(row)
    }
    box.appendChild(keys)
    this.element.replaceChildren(box)
  }
}

function card(name, sub, run, agent) {
  const b = el('button', 'welcome-card' + (agent ? ' is-agent' : ''))
  b.append(el('span', 'welcome-card-name', name), el('span', 'welcome-card-sub', sub))
  b.addEventListener('click', run)
  return b
}

// Re-render live watermarks when the workspace changes under them.
export const refreshWelcome = () => {
  for (const node of document.querySelectorAll('.welcome')) node.__welcome?.render()
}
