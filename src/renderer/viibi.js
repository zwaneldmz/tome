// Viibi — the mascot. A small bookmark-sprite in the status bar that mirrors
// what the app is doing: resting when idle, reading while flows / chat /
// voice are busy, gapped when the air gap is holding, blocked on a refused
// request, and error on a failure. Pure presentation — it reads shared state
// and the event bus, and never mutates app state.
//
// The SVG and animation CSS are lifted from docs/viibi.html (the design pass):
// one rounded-bookmark body, five faces, a falling-snow tail, recoloured
// entirely through a `--viibi` variable so it themes and re-states for free.
import { tome } from './util.js'
import { agState } from './state.js'
import { runningCount, RUN_PANE_PREFIX } from '../shared/flow-run-plan.js'
import { voiceActive } from './voice.js'
import { toggleViibiPopover } from './viibi-popover.js'
import { uq } from './mentor.js'

// The one component stylesheet. Injected as a <style> rather than living in
// style.css so the mascot stays a single, deletable unit. CSP allows inline
// styles (style-src 'unsafe-inline'), same as every status-bar glyph.
const CSS = `
.viibi { --viibi: var(--accent); --viibi-fg: var(--accent-fg); --viibi-mist: #ffffff; display: block; }
.viibi { --viibi-glow: 0 0 8px rgba(0, 113, 227, 0.28); }
:root[data-theme='dark'] .viibi { --viibi-glow: 0 0 12px rgba(0, 229, 255, 0.45); }
.viibi .body { fill: var(--viibi); filter: drop-shadow(var(--viibi-glow)); }
.viibi .eye { fill: var(--viibi-fg); }
.viibi .mouth { fill: none; stroke: var(--viibi-fg); stroke-width: 2.2; stroke-linecap: round; }
.viibi .eye-arc { fill: none; stroke: var(--viibi-fg); stroke-width: 2.6; stroke-linecap: round; }
.viibi .mouth-open { fill: var(--viibi-fg); stroke: none; }
.viibi .mouth-o { fill: var(--viibi-fg); stroke: none; }
.viibi .mouth-frown { fill: none; stroke: var(--viibi-fg); stroke-width: 2.2; stroke-linecap: round; }
.viibi .shield { fill: var(--viibi); stroke: var(--viibi); filter: drop-shadow(var(--viibi-glow)); }
.viibi .shieldcross { stroke: var(--viibi-fg); stroke-width: 3.5; fill: none; stroke-linecap: round; }
.viibi .eyesx path { stroke: var(--viibi-fg); stroke-width: 2.4; fill: none; stroke-linecap: round; }
.viibi .halo { animation: v-breathe 3.6s ease-in-out infinite; }
.viibi .bodygroup { transform-box: fill-box; transform-origin: 50% 35%; animation: v-bob 3s ease-in-out infinite; }
.viibi .eyes { transform-box: fill-box; transform-origin: 50% 50%; animation: v-blink 4.6s infinite; }
.viibi .wisp { fill: var(--viibi-mist); filter: drop-shadow(0 0 3px var(--viibi-mist)); transform-box: fill-box; transform-origin: center; animation: v-wisp 1.8s linear infinite; animation-delay: var(--d, 0s); opacity: 0; }
.viibi .spark { opacity: 0; }
.viibi .eyesx { display: none; }
.viibi .eyes-happy, .viibi .eyes-wide, .viibi .mouth-open, .viibi .mouth-o, .viibi .mouth-frown { display: none; }
.viibi .shield { display: none; }
.viibi .cap { display: none; fill: var(--viibi); filter: drop-shadow(var(--viibi-glow)); }
.viibi.s-mentor .cap { display: block; }

.viibi.s-reading .bodygroup { animation: v-tilt 0.9s ease-in-out infinite; }
.viibi.s-reading .wisp { animation-duration: 0.9s; }
.viibi.s-reading .spark { animation: v-spark 1.6s infinite; }
.viibi.s-reading .spark.s2 { animation-delay: 0.45s; }
.viibi.s-reading .spark.s3 { animation-delay: 0.9s; }
.viibi.s-reading .eyes { display: none; }
.viibi.s-reading .eyes-happy { display: block; }
.viibi.s-reading .mouth { display: none; }
.viibi.s-reading .mouth-open { display: block; }

.viibi.s-gapped .wisps { display: none; }
.viibi.s-gapped .shield { display: block; }
.viibi.s-gapped .bodygroup { animation: v-breathe 2.2s ease-in-out infinite; }

.viibi.s-blocked { --viibi: var(--no); --viibi-fg: #fff; --viibi-mist: #ffd9de; --viibi-glow: 0 0 12px rgba(255, 59, 92, 0.5); }
.viibi.s-blocked .wisps { display: none; }
.viibi.s-blocked .bodygroup { animation: v-jolt 0.5s ease-in-out infinite; }
.viibi.s-blocked .halo { animation: v-pulse 0.5s ease-in-out infinite; }
.viibi.s-blocked .eyes { display: none; }
.viibi.s-blocked .eyes-wide { display: block; }
.viibi.s-blocked .mouth { display: none; }
.viibi.s-blocked .mouth-o { display: block; }

.viibi.s-error { --viibi: var(--faint); --viibi-fg: var(--text); --viibi-mist: #c3cad6; }
.viibi.s-error .eyes { display: none; }
.viibi.s-error .eyesx { display: block; }
.viibi.s-error .mouth { display: none; }
.viibi.s-error .mouth-frown { display: block; }
.viibi.s-error .wisps { opacity: 0.4; }
.viibi.s-error .bodygroup { animation: v-ember 2.4s ease-in-out infinite; }

@keyframes v-bob { 0%, 100% { transform: translateY(0); } 50% { transform: translateY(-5px); } }
@keyframes v-breathe { 0%, 100% { opacity: 0.4; } 50% { opacity: 0.85; } }
@keyframes v-pulse { 0%, 100% { opacity: 0.35; } 50% { opacity: 1; } }
@keyframes v-blink { 0%, 91%, 100% { transform: scaleY(1); } 94% { transform: scaleY(0.12); } }
@keyframes v-tilt { 0%, 100% { transform: rotate(-5deg); } 50% { transform: rotate(5deg); } }
@keyframes v-spark { 0%, 100% { opacity: 0; transform: translateY(0); } 50% { opacity: 1; transform: translateY(-6px); } }
@keyframes v-jolt { 0%, 100% { transform: translateX(0); } 20%, 60% { transform: translateX(2.5px); } 40%, 80% { transform: translateX(-2.5px); } }
@keyframes v-ember { 0%, 100% { opacity: 0.35; transform: rotate(6deg) translateY(8px); } 50% { opacity: 0.6; transform: rotate(5deg) translateY(8px); } }
@keyframes v-wisp { 0% { opacity: 0; transform: translate(0, 0) scale(1); } 15% { opacity: 0.9; } 100% { opacity: 0; transform: translate(var(--tx, 0px), var(--ty, 46px)) scale(0.3); } }

#sb-viibi { display: inline-flex; align-items: center; height: 100%; cursor: pointer; }
#sb-viibi:hover { opacity: 0.85; }
#sb-viibi .viibi { width: 24px; height: 36px; }
@media (prefers-reduced-motion: reduce) { .viibi * { animation: none !important; } }
`

// Falling-snow particles: [cx, cy, r, drift-x, drift-y, delay]
const WISPS = [
  [46, 95, 2.0, 5, 58, 0], [63, 97, 1.7, -4, 62, 0.18], [52, 99, 1.5, 7, 60, 0.36], [70, 101, 2.0, -5, 64, 0.54],
  [44, 103, 1.8, 8, 62, 0.72], [58, 105, 1.6, -2, 66, 0.09], [67, 108, 1.7, -6, 68, 0.27], [49, 110, 2.1, 6, 64, 0.45],
  [61, 113, 1.5, -3, 68, 0.63], [72, 116, 1.4, -7, 66, 0.81], [54, 118, 1.9, 7, 70, 0.15], [64, 122, 1.4, -5, 70, 0.33],
  [47, 124, 1.7, 8, 72, 0.51], [57, 128, 1.6, -2, 70, 0.69], [68, 131, 1.3, -6, 72, 0.87], [50, 134, 1.5, 7, 72, 0.21],
  [60, 138, 1.4, -1, 70, 0.39], [55, 142, 1.3, 5, 72, 0.57], [65, 146, 1.2, -5, 70, 0.75]
]

function wispMarkup() {
  return WISPS.map(
    (w) =>
      `<circle class="wisp" cx="${w[0]}" cy="${w[1]}" r="${w[2]}" style="--tx:${w[3]}px; --ty:${w[4]}px; --d:${w[5]}s"/>`
  ).join('')
}

function svgMarkup() {
  return (
    `<svg class="viibi s-resting" viewBox="0 0 120 170" aria-hidden="true">` +
    `<defs><radialGradient id="viibi-halo"><stop offset="0" stop-color="var(--viibi)" stop-opacity="0.55"/><stop offset="1" stop-color="var(--viibi)" stop-opacity="0"/></radialGradient></defs>` +
    `<g class="halo"><circle cx="60" cy="50" r="50" fill="url(#viibi-halo)"/></g>` +
    `<g class="wisps">${wispMarkup()}</g>` +
    `<g class="bodygroup">` +
    `<path class="body" d="M40 20 Q40 14 46 14 L74 14 Q80 14 80 20 L80 84 Q80 92 72 92 L48 92 Q40 92 40 84 Z"/>` +
    `<g class="eyes"><circle class="eye" cx="50" cy="40" r="3.4"/><circle class="eye" cx="70" cy="40" r="3.4"/></g>` +
    `<g class="eyes-happy"><path class="eye-arc" d="M45 40 Q50 35 55 40"/><path class="eye-arc" d="M65 40 Q70 35 75 40"/></g>` +
    `<g class="eyes-wide"><circle class="eye" cx="50" cy="40" r="5"/><circle class="eye" cx="70" cy="40" r="5"/></g>` +
    `<g class="eyesx"><path d="M46 36 L54 44 M54 36 L46 44"/><path d="M66 36 L74 44 M74 36 L66 44"/></g>` +
    `<path class="mouth" d="M54 52 Q60 58 66 52"/>` +
    `<path class="mouth-open" d="M54 52 Q60 64 66 52 Z"/>` +
    `<circle class="mouth-o" cx="60" cy="54" r="3.5"/>` +
    `<path class="mouth-frown" d="M54 58 Q60 52 66 58"/>` +
    `</g>` +
    `<g class="shield"><path d="M46 24 L74 24 L74 50 C74 66 60 74 60 80 C60 74 46 66 46 50 Z"/><path class="shieldcross" d="M60 30 V72 M50 51 H70"/></g>` +
    `<g class="sparkles"><circle class="spark s1" cx="93" cy="18" r="2.2"/><circle class="spark s2" cx="100" cy="42" r="1.6"/><circle class="spark s3" cx="22" cy="30" r="1.8"/></g>` +
    `<path class="cap" d="M60 0.5 L61.6 4.8 L66.2 5 L62.6 7.8 L63.8 12.3 L60 9.7 L56.2 12.3 L57.4 7.8 L53.8 5 L58.4 4.8 Z"/>` +
    `</svg>`
  )
}

let el = null
let current = 'resting'
let flashState = null
let flashTimer = null

// Busy signals. Each source is a boolean/set read at compute time; none of
// them writes app state.
let runsActive = false
const streaming = new Set() // chat ids with a reply currently streaming

function countGapped() {
  return Object.entries(agState.panes || {})
    .filter(([id]) => !id.startsWith(RUN_PANE_PREFIX))
    .filter(([, p]) => p?.mode !== 'open').length
}

function computeState() {
  if (runsActive || streaming.size > 0 || voiceActive()) return 'reading'
  if (countGapped() > 0) return 'gapped'
  // Mentor: a high understanding score (>= 80) turns Viibi into a little
  // graduation-cap star while idle. Deliberately below the busy states — a
  // streaming turn still reads as "reading", mentor only flavours idle.
  if (uq() >= 80) return 'mentor'
  return 'resting'
}

function apply() {
  if (!el) return
  const s = flashState || computeState()
  if (s === current) return
  current = s
  el.setAttribute('class', 'viibi s-' + s)
}

function flash(s, ms = 1600) {
  if (!el) return
  clearTimeout(flashTimer)
  flashState = s
  apply()
  flashTimer = setTimeout(() => {
    flashState = null
    apply()
  }, ms)
}

function mount() {
  const host = document.getElementById('sb-viibi')
  if (!host) return
  const style = document.createElement('style')
  style.textContent = CSS
  document.head.appendChild(style)
  host.innerHTML = svgMarkup()
  el = host.querySelector('.viibi')
  host.addEventListener('click', () => toggleViibiPopover(host))
}

export function initViibi() {
  mount()
  if (!el) return

  // Background flow runs — the one thing that keeps working with no pane.
  const onRuns = (list) => {
    runsActive = runningCount(list) > 0
    apply()
  }
  tome.runs.onChanged(onRuns)
  // Lock-gated while the lock screen is up: nothing to count yet anyway.
  tome.runs.list().then(onRuns, () => {})

  // Chat streaming — deltas in flight mark a turn as busy.
  tome.chat.onDelta(({ id }) => {
    streaming.add(id)
    apply()
  })
  tome.chat.onDone(({ id, error }) => {
    streaming.delete(id)
    if (error) flash('error')
    apply()
  })

  // Air gap: a refused request is a blocked flash; state changes re-derive
  // gapped-vs-resting.
  tome.airgap.onBlocked(() => flash('blocked'))
  tome.airgap.onState((s) => {
    Object.assign(agState, s)
    apply()
  })

  // Voice has no state event; its activity is read from voiceActive() and
  // synced on a slow tick (apply() no-ops when nothing changed, so this is a
  // cheap periodic re-derive, not real work).
  setInterval(() => apply(), 1000)

  apply()
}

// Self-initialize: this module is imported once (renderer.js) as a side
// effect, and the status bar lives for the life of the window. Matches
// statusbar.js's own module-level watchRuns().
initViibi()
