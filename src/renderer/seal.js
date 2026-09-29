// The seal: one titlebar chip that answers "what can my agents reach right
// now?" and, when clicked, the one place to change it. Replaces the three
// security toggles that used to hide inside the ＋ menu.
//
//   Sealed · 3        every agent pane is model-APIs-only
//   1 open            some pane's internet door is open (amber, counts down)
//   Unsealed          new agents would spawn with your full network
import { tome, el, toast } from './util.js'
import { prefs, agState } from './state.js'
import { dock } from './panes.js'
import { wireMenu, closeMenus } from './menus.js'
import { spawnPolicy } from './spawn-policy.js'
import { egressModal } from './egress-ui.js'
import { shieldIcon, shieldOpenIcon } from './icons.js'
import { RUN_PANE_PREFIX } from '../shared/flow-run-plan.js'

const chip = document.getElementById('seal-chip')
const text = document.getElementById('seal-text')
const icon = chip.querySelector('.seal-icon')
let blocked = 0

tome.egress.onBlocked(() => {
  blocked++
  renderSeal()
})
tome.egress.onState(() => renderSeal())
dock.onDidAddPanel(() => renderSeal())
dock.onDidRemovePanel(() => renderSeal())

// Contained panes the user can see (flow runs have their own rows in Runs).
function sealedPanes() {
  return dock.panels.filter((p) => p.params?.ptyId && p.params.egress && !p.params.ptyId.startsWith(RUN_PANE_PREFIX))
}
const isOpen = (p) => agState.panes?.[p.params.ptyId]?.mode === 'open'

export function renderSeal() {
  const policy = spawnPolicy(prefs)
  const panes = sealedPanes()
  const open = panes.filter(isOpen).length
  chip.classList.remove('seal-open', 'seal-off')
  icon.replaceChildren(open || !policy.agentsGapped ? shieldOpenIcon() : shieldIcon())
  if (open) {
    chip.classList.add('seal-open')
    text.textContent = `${open} open`
    chip.title = `${open} of ${panes.length} contained pane${panes.length === 1 ? '' : 's'} can reach the internet`
  } else if (!policy.agentsGapped) {
    chip.classList.add('seal-off')
    text.textContent = 'Unsealed'
    chip.title = 'New agents start with your full network access'
  } else {
    text.textContent = panes.length ? `Sealed · ${panes.length}` : 'Sealed'
    chip.title = 'Agents reach model APIs only' + (blocked ? ` — ${blocked} request${blocked === 1 ? '' : 's'} blocked` : '')
  }
  chip.setAttribute('aria-label', `Containment: ${text.textContent}. ${chip.title}`)
}

// A labelled switch row. role=menuitemcheckbox keeps menu keyboard nav.
function switchRow(menu, { label, note, on, disabled, onToggle }) {
  const b = el('button', 'seal-switch')
  b.setAttribute('role', 'menuitemcheckbox')
  b.setAttribute('aria-checked', String(!!on))
  b.disabled = !!disabled
  const t = el('span', 'seal-switch-text')
  t.append(el('span', 'seal-switch-label', label), el('span', 'seal-switch-note', note))
  b.append(t, el('span', 'switch' + (on ? ' on' : '')))
  b.addEventListener('click', () => {
    onToggle()
    buildMenu(menu) // re-render in place; the popover stays open
  })
  menu.appendChild(b)
}

function buildMenu(menu) {
  menu.replaceChildren()
  const policy = spawnPolicy(prefs)
  const panes = sealedPanes()

  const head = el('div', 'seal-head')
  const open = panes.filter(isOpen).length
  head.append(
    el('div', 'seal-title', open ? 'Internet open on some panes' : policy.agentsGapped ? 'Agents are sealed' : 'Agents are unsealed'),
    el(
      'div',
      'seal-sub',
      policy.agentsGapped
        ? 'Sealed agents run in an OS sandbox and can reach model APIs only. ' + (blocked ? `${blocked} request${blocked === 1 ? '' : 's'} blocked this session.` : 'Nothing blocked yet.')
        : 'New agents start with your full network and file access.'
    )
  )
  menu.appendChild(head)

  menu.appendChild(el('div', 'menu-label', 'New agents'))
  if (policy.showEgressDefaultToggle) {
    switchRow(menu, {
      label: 'Start agents sealed',
      note: 'Sandbox plus model-APIs-only network',
      on: prefs.egressDefault,
      onToggle: () => {
        prefs.egressDefault = !prefs.egressDefault
        tome.store.set('egress-default', prefs.egressDefault)
        renderSeal()
      },
    })
  } else {
    menu.appendChild(el('div', 'seal-note', 'Containment-only mode is on: every agent starts sealed. Change it in Settings → Security.'))
  }
  switchRow(menu, {
    label: 'Sandboxed Docker',
    note: prefs.dockerGateway ? 'Sealed agents get a filtered Docker socket' : 'Turn on in Settings → Security first',
    on: prefs.dockerPanes,
    disabled: !prefs.dockerGateway,
    onToggle: () => {
      prefs.dockerPanes = !prefs.dockerPanes
    },
  })
  switchRow(menu, {
    label: 'Assistant may run commands',
    note: 'Otherwise it types into terminals and you press Enter',
    on: prefs.conductorRun,
    onToggle: () => {
      prefs.conductorRun = !prefs.conductorRun
      tome.store.set('conductor-run', prefs.conductorRun)
      tome.conductor.allowRun(prefs.conductorRun)
    },
  })

  if (panes.length) {
    menu.appendChild(el('div', 'menu-label', 'Sealed panes'))
    for (const p of panes) {
      const st = agState.panes?.[p.params.ptyId]
      const openNow = st?.mode === 'open'
      const row = el('button', 'seal-pane' + (openNow ? ' is-open' : ''))
      row.setAttribute('role', 'menuitem')
      const name = (p.title || '').replace(/^[^\p{L}\p{N}]+/u, '')
      let state = 'model APIs only'
      if (openNow) {
        const left = Math.max(0, (st.expiresAt || 0) - Date.now())
        state = `internet open · ${Math.ceil(left / 60000)} min left`
      }
      row.append(el('span', 'seal-pane-name', name), el('span', 'seal-pane-state', state))
      row.title = openNow ? 'Relock now' : 'Allow internet for a while…'
      row.addEventListener('click', async () => {
        closeMenus()
        if (openNow) {
          await tome.egress.relock(p.params.ptyId)
          toast(`Relocked ${name}`, 'ok')
        } else egressModal(p.params.ptyId)
      })
      menu.appendChild(row)
    }
  }

  const foot = el('div', 'seal-foot')
  const log = el('button', 'btn-quiet', 'Event log')
  log.addEventListener('click', () => {
    closeMenus()
    import('./panes.js').then((m) => m.addEvents())
  })
  const settings = el('button', 'btn-quiet', 'Security settings')
  settings.addEventListener('click', () => {
    closeMenus()
    import('./preferences.js').then((m) => m.preferencesModal({ section: 'security' }))
  })
  foot.append(log, settings)
  menu.appendChild(foot)
}

export function initSeal() {
  wireMenu('seal-chip', 'seal-menu', buildMenu)
  renderSeal()
}
