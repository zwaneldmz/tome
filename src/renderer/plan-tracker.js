// Plan tracker HUD — the assistant's execution, live on glass.
//
// When a turn starts executing (the first tool call fires), a HUD docks at
// the bottom-right of the window and tracks the plan step by step: one row
// per tool call (icon, name, hint, wall-clock ms), the active step pulsing
// with a sweeping shimmer, completed steps popping green ✓, failures red ✕,
// and headless agent runs (run_agent) as live sub-rows ("⚙ claude —
// running"). The header ticks elapsed time; a progress bar fills with
// completed steps. On a clean finish the HUD stamps "✨ plan complete",
// fires a particle burst across it, and fades away a beat later. Failures
// stay up longer — red, to be read.
//
// The whole thing is a projection of events the backend already emits
// (`chat:tool`, the new `chat:tool-done`, `conductor:agent`, `chat:done`) —
// nothing here polls the backend or reads pane state. It works whether or
// not the chat pane is open: the events are app-wide.
//
// `prefers-reduced-motion` disables the pulse/shimmer/burst (DOM state
// still updates — the tracker is information first, motion second).

import { tome, el } from './util.js'
import { dock } from './panes.js'

const TOOL_ICONS = {
  list_panes: '▤',
  read_terminal: '≫',
  type_in_terminal: '⌨',
  open_pane: '▣',
  open_file: '▤',
  read_flow: '⛓',
  draft_flow: '⛓',
  write_file: '✎',
  read_file: '⛁',
  run_command: '❯',
  list_skills: '⚒',
  read_skill: '⚒',
  graph_query: '◈',
  graph_path: '◈',
  graph_explain: '◈',
  run_agent: '⚙',
  gate_question: '✋',
}

const AGENT_STATE = {
  started: 'running',
  done: 'done',
  failed: 'failed',
}

const HIDE_OK_MS = 2200 // flourish, then fade
const HIDE_FAIL_MS = 8000 // failures stay readable

const reduceMotion = () =>
  typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches

// ---- per-chat execution runs ----
const runs = new Map() // chatId -> { id, steps, startedAt, outcome, dismissed }
let activeId = null
let hud = null
let hideTimer = null
let tickTimer = null

function current() {
  return activeId ? runs.get(activeId) : null
}

function stepGlyph(tool) {
  return TOOL_ICONS[tool] || '•'
}

function fmtMs(ms) {
  const s = ms / 1000
  if (s < 10) return `${s.toFixed(1)}s`
  if (s < 60) return `${Math.round(s)}s`
  const m = Math.floor(s / 60)
  return `${m}m${Math.round(s % 60)}s`
}

function fmtElapsed(since) {
  const s = Math.max(0, Math.round((Date.now() - since) / 1000))
  const m = Math.floor(s / 60)
  return m ? `${m}:${String(s % 60).padStart(2, '0')}` : `0:${String(s).padStart(2, '0')}`
}

// ---- DOM ----

function buildHud() {
  const h = el('div', 'plan-hud')
  h.setAttribute('role', 'status')

  const head = el('div', 'plan-head')
  const glyph = el('span', 'plan-glyph', '⚡')
  const title = el('span', 'plan-title', 'executing plan')
  const meta = el('span', 'plan-meta', '')
  const close = el('button', 'plan-close', '✕')
  close.type = 'button'
  close.setAttribute('aria-label', 'Dismiss plan tracker')
  close.addEventListener('click', () => {
    const run = current()
    if (run) run.dismissed = true
    hideNow()
  })
  head.append(glyph, title, meta, close)
  // clicking the header (not the ✕) focuses the chat driving this run
  head.addEventListener('click', (e) => {
    if (e.target === close) return
    const run = current()
    if (run) dock.getPanel(run.id)?.api?.setActive()
  })

  const steps = el('div', 'plan-steps')
  steps.setAttribute('aria-live', 'polite')

  const bar = el('div', 'plan-bar')
  const fill = el('div', 'plan-fill')
  bar.appendChild(fill)

  const canvas = document.createElement('canvas')
  canvas.className = 'plan-burst'
  canvas.setAttribute('aria-hidden', 'true')

  h.append(head, steps, bar, canvas)

  // ---- drag (header only; clamped to the viewport; remembered) ----
  let dragging = null
  head.addEventListener('pointerdown', (e) => {
    if (e.target === close) return
    const rect = h.getBoundingClientRect()
    dragging = { dx: e.clientX - rect.left, dy: e.clientY - rect.top }
    head.setPointerCapture(e.pointerId)
    h.classList.add('dragging')
  })
  head.addEventListener('pointermove', (e) => {
    if (!dragging) return
    const pad = 12
    const x = Math.min(window.innerWidth - pad, Math.max(pad, e.clientX - dragging.dx))
    const y = Math.min(window.innerHeight - pad, Math.max(pad, e.clientY - dragging.dy))
    h.style.left = `${x}px`
    h.style.top = `${y}px`
    h.style.right = 'auto'
    h.style.bottom = 'auto'
  })
  head.addEventListener('pointerup', (e) => {
    if (!dragging) return
    dragging = null
    h.classList.remove('dragging')
    head.releasePointerCapture?.(e.pointerId)
    tome.store.set('plan-tracker-pos', { left: h.style.left, top: h.style.top }).catch(() => {})
  })

  document.body.appendChild(h)
  tome.store
    .get('plan-tracker-pos')
    .then((pos) => {
      if (pos && typeof pos.left === 'string') {
        h.style.left = pos.left
        h.style.top = pos.top
        h.style.right = 'auto'
        h.style.bottom = 'auto'
      }
    })
    .catch(() => {})
  return { h, glyph, title, meta, steps, bar, fill, canvas }
}

function render() {
  const run = current()
  if (!hud || !run) return
  const h = hud

  const done = run.steps.filter((s) => s.status === 'ok').length
  const failed = run.steps.some((s) => s.status === 'fail')
  const running = run.steps.some((s) => s.status === 'active')

  if (run.outcome == null) {
    h.glyph.textContent = '⚡'
    h.glyph.classList.remove('done', 'fail')
    h.title.textContent = 'executing plan'
    h.h.classList.toggle('plan-hud-fail', false)
    h.meta.textContent = `${fmtElapsed(run.startedAt)} · ${run.steps.length} step${run.steps.length === 1 ? '' : 's'}`
  } else if (run.outcome === 'ok') {
    h.glyph.textContent = '✨'
    h.glyph.classList.add('done')
    h.glyph.classList.remove('fail')
    h.title.textContent = `plan complete · ${fmtElapsed(run.startedAt)}`
    h.meta.textContent = `${run.steps.length} step${run.steps.length === 1 ? '' : 's'} · ${done} ✓`
  } else {
    h.glyph.textContent = '✕'
    h.glyph.classList.remove('done')
    h.glyph.classList.add('fail')
    h.title.textContent = run.outcome === 'aborted' ? 'plan stopped' : 'plan failed'
    h.h.classList.toggle('plan-hud-fail', true)
    h.meta.textContent = failed
      ? `${failed} failed · ${fmtElapsed(run.startedAt)}`
      : `${fmtElapsed(run.startedAt)} · ${run.steps.length} steps`
  }

  // steps
  h.steps.replaceChildren()
  for (const s of run.steps) {
    const row = el('div', 'plan-step ' + s.status)
    const dot = el('span', 'plan-dot')
    dot.textContent = s.status === 'ok' ? '✓' : s.status === 'fail' ? '✕' : ''
    const body = el('div', 'plan-body')
    const tool = el('div', 'plan-tool')
    tool.append(
      el('span', 'plan-tool-glyph', stepGlyph(s.tool)),
      document.createTextNode(s.tool)
    )
    if (s.hint) tool.append(el('span', 'plan-hint', `· ${s.hint}`))
    body.appendChild(tool)
    if (s.agent) {
      body.append(
        el(
          'div',
          'plan-agent ' + (AGENT_STATE[s.agent.status] || s.agent.status),
          `⚙ ${s.agent.kind} — ${AGENT_STATE[s.agent.status] || s.agent.status}`
        )
      )
    }
    const ms = el('span', 'plan-ms', s.ms != null ? fmtMs(s.ms) : '')
    row.append(dot, body, ms)
    h.steps.appendChild(row)
  }
  if (!run.outcome && !running && !run.steps.length) {
    h.steps.appendChild(el('div', 'plan-wait', 'waiting for the first step…'))
  }

  // progress: completed vs known steps (steps reveal as they execute)
  const total = Math.max(1, run.steps.length)
  const pct = run.outcome === 'ok' ? 100 : Math.round((done / total) * 100)
  h.fill.style.width = `${pct}%`
  h.fill.classList.toggle('plan-fill-fail', !!failed)
}

function showHud() {
  if (!hud) hud = buildHud()
  hud.h.classList.remove('plan-hud-hidden')
  if (hideTimer) {
    clearTimeout(hideTimer)
    hideTimer = null
  }
  if (!tickTimer) {
    tickTimer = setInterval(() => render(), 250)
  }
  render()
}

function hideNow() {
  if (!hud) return
  hud.h.classList.add('plan-hud-hidden')
  if (hideTimer) {
    clearTimeout(hideTimer)
    hideTimer = null
  }
  if (tickTimer) {
    clearInterval(tickTimer)
    tickTimer = null
  }
}

function scheduleHide(ms) {
  if (hideTimer) clearTimeout(hideTimer)
  hideTimer = setTimeout(hideNow, ms)
}

// ---- event handlers ----

function onTool({ id, tool, hint }) {
  let run = runs.get(id)
  if (!run || run.outcome != null || run.dismissed) {
    // a fresh turn: the previous run (if any) is finished or was dismissed
    run = { id, steps: [], startedAt: Date.now(), outcome: null, dismissed: false }
    runs.set(id, run)
  }
  run.steps.push({ tool, hint: hint || '', status: 'active', startedAt: Date.now(), ms: null, agent: null })
  activeId = id
  showHud()
}

function onToolDone({ id, tool, ok, ms }) {
  const run = runs.get(id)
  if (!run) return
  // match the newest step still waiting (same tool name; a batch can carry
  // several of the same tool back-to-back, so match from the tail)
  for (let i = run.steps.length - 1; i >= 0; i--) {
    const s = run.steps[i]
    if (s.tool === tool && s.status === 'active') {
      s.status = ok ? 'ok' : 'fail'
      s.ms = ms ?? Date.now() - s.startedAt
      break
    }
  }
  if (activeId === id) render()
}

function onAgent({ chatId, kind, status }) {
  const run = runs.get(chatId)
  if (!run) return
  // the agent belongs to the newest run_agent step
  for (let i = run.steps.length - 1; i >= 0; i--) {
    const s = run.steps[i]
    if (s.tool === 'run_agent' && (!s.agent || s.agent.kind === kind)) {
      s.agent = { kind, status }
      break
    }
  }
  if (activeId === chatId) render()
}

function onDone({ id, error, aborted }) {
  const run = runs.get(id)
  if (!run) return
  run.outcome = error ? (aborted ? 'aborted' : 'fail') : 'ok'
  if (activeId === id) {
    render()
    if (run.outcome === 'ok') {
      if (!reduceMotion()) burst(hud.canvas, hud.h)
      scheduleHide(HIDE_OK_MS)
    } else {
      scheduleHide(HIDE_FAIL_MS)
    }
  }
}

// ---- the flourish: a short particle burst across the HUD ----

function burst(canvas, host) {
  const rect = host.getBoundingClientRect()
  const dpr = window.devicePixelRatio || 1
  canvas.width = Math.max(1, rect.width * dpr)
  canvas.height = Math.max(1, rect.height * dpr)
  canvas.style.width = `${rect.width}px`
  canvas.style.height = `${rect.height}px`
  const ctx = canvas.getContext('2d')
  if (!ctx) return
  ctx.scale(dpr, dpr)

  const s = getComputedStyle(host)
  const palette = ['--accent', '--g-add', '--bright'].map(
    (v) => s.getPropertyValue(v).trim() || '#3dff9e'
  )
  const particles = Array.from({ length: 42 }, () => ({
    x: rect.width * (0.25 + Math.random() * 0.5),
    y: rect.height * 0.3,
    vx: (Math.random() - 0.5) * 220,
    vy: -60 - Math.random() * 160,
    r: 1.5 + Math.random() * 2.5,
    c: palette[(Math.random() * palette.length) | 0],
    life: 1,
  }))
  const t0 = performance.now()
  const DURATION = 900
  const frame = (now) => {
    const t = (now - t0) / DURATION
    ctx.clearRect(0, 0, rect.width, rect.height)
    if (t >= 1) return
    for (const p of particles) {
      p.x += (p.vx * 16) / 1000
      p.y += (p.vy * 16) / 1000
      p.vy += 260 * (16 / 1000) // gravity
      p.life = 1 - t
      ctx.globalAlpha = Math.max(0, p.life)
      ctx.fillStyle = p.c
      ctx.beginPath()
      ctx.arc(p.x, p.y, p.r * p.life, 0, Math.PI * 2)
      ctx.fill()
    }
    ctx.globalAlpha = 1
    requestAnimationFrame(frame)
  }
  requestAnimationFrame(frame)
}

export function initPlanTracker() {
  tome.chat.onTool(onTool)
  tome.chat.onToolDone(onToolDone)
  tome.conductor.onAgent(onAgent)
  tome.chat.onDone(onDone)
}
