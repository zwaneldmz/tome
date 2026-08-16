// Viibi's companion popover — the little control surface the status-bar
// mascot opens when clicked: the understanding score, a verbose toggle, and
// shortcuts to the review report / score reset. Pure presentation; every
// action mutates shared mentor/workspace state through the same APIs the
// other surfaces use. One popover at a time, dismissed on outside click.
import { el, toast } from './util.js'
import { isVerbose, uq, setUq } from './mentor.js'
import { activeWorkspace, saveWs } from './workspaces.js'
import { addReport } from './panes.js'

let pop = null

function switchRow(label, hint, get, set) {
  const r = el('div', 'viibi-pop-row')
  const text = el('div', 'viibi-pop-rowtext')
  text.append(el('span', 'viibi-pop-label', label))
  if (hint) text.append(el('span', 'viibi-pop-hint', hint))
  const sw = el('button', 'prefs-switch')
  sw.type = 'button'
  sw.setAttribute('role', 'switch')
  sw.append(el('span', 'prefs-knob'))
  const paint = () => {
    sw.classList.toggle('on', get())
    sw.setAttribute('aria-checked', String(get()))
  }
  sw.addEventListener('click', () => {
    set(!get())
    paint()
  })
  r.append(text, sw)
  paint()
  return r
}

export function toggleViibiPopover(anchor) {
  if (pop) {
    pop.remove()
    pop = null
    return
  }
  pop = el('div', 'viibi-pop')
  pop.setAttribute('role', 'dialog')
  pop.setAttribute('aria-label', 'Viibi — mentor controls')

  pop.appendChild(el('div', 'viibi-pop-title', 'Viibi'))
  const score = el('div', 'viibi-pop-score', `UQ ${uq()}`)
  score.setAttribute('aria-live', 'polite')
  pop.appendChild(score)

  pop.appendChild(
    switchRow('Verbose guide', 'teaching persona', isVerbose, (v) => {
      const w = activeWorkspace()
      if (!w) return toast('no workspace to remember that in')
      w.verbose = v
      saveWs()
    })
  )

  const report = el('button', 'ag-btn ghost viibi-pop-btn', 'Review report…')
  report.addEventListener('click', () => {
    pop?.remove()
    pop = null
    addReport()
  })
  const reset = el('button', 'ag-btn ghost viibi-pop-btn', 'Reset understanding')
  reset.addEventListener('click', () => {
    setUq(0)
    score.textContent = 'UQ 0'
    toast('understanding score reset', 'ok')
  })
  pop.append(report, reset)

  document.body.appendChild(pop)
  const r = anchor.getBoundingClientRect()
  pop.style.bottom = `${Math.round(window.innerHeight - r.top) + 8}px`
  pop.style.right = `${Math.max(8, Math.round(window.innerWidth - r.right))}px`

  const off = (e) => {
    if (!pop || pop.contains(e.target)) return
    pop.remove()
    pop = null
    document.removeEventListener('click', off)
  }
  setTimeout(() => document.addEventListener('click', off), 0)
}
