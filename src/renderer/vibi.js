// Vibi — the mascot. A small bookmark-sprite in the status bar that mirrors
// what the app is doing: resting when idle, reading while flows / chat /
// voice are busy, gapped when the air gap is holding, blocked on a refused
// request, and error on a failure. Pure presentation — it reads shared state
// and the event bus, and never mutates app state.
//
// The SVG and animation CSS are lifted from docs/vibi.html (the design pass):
// one rounded-bookmark body, five faces, a falling-snow tail, recoloured
// entirely through a `--vibi` variable so it themes and re-states for free.
import { tome } from './util.js'
import { agState } from './state.js'
import { runningCount, RUN_PANE_PREFIX } from '../shared/flow-run-plan.js'
import { voiceActive } from './voice.js'

// The one component stylesheet. Injected as a <style> rather than living in
// style.css so the mascot stays a single, deletable unit. CSP allows inline
// styles (style-src 'unsafe-inline'), same as every status-bar glyph.
const CSS = `
.vibi { --vibi: var(--accent); --vibi-fg: var(--accent-fg); --vibi-mist: #ffffff; display: block; }
.vibi { --vibi-glow: 0 0 8px rgba(0, 113, 227, 0.28); }
:root[data-theme='dark'] .vibi { --vibi-glow: 0 0 12px rgba(0, 229, 255, 0.45); }
.vibi .body { fill: var(--vibi); filter: drop-shadow(var(--vibi-glow)); }
.vibi .eye { fill: var(--vibi-fg); }
.vibi .mouth { fill: none; stroke: var(--vibi-fg); stroke-width: 2.2; stroke-linecap: round; }
.vibi .eye-arc { fill: none; stroke: var(--vibi-fg); stroke-width: 2.6; stroke-linecap: round; }
.vibi .mouth-open { fill: var(--vibi-fg); stroke: none; }
.vibi .mouth-o { fill: var(--vibi-fg); stroke: none; }
.vibi .mouth-frown { fill: none; stroke: var(--vibi-fg); stroke-width: 2.2; stroke-linecap: round; }
.vibi .shield { fill: var(--vibi); stroke: var(--vibi); filter: drop-shadow(var(--vibi-glow)); }
.vibi .shieldcross { stroke: var(--vibi-fg); stroke-width: 3.5; fill: none; stroke-linecap: round; }
.vibi .eyesx path { stroke: var(--vibi-fg); stroke-width: 2.4; fill: none; stroke-linecap: round; }
.vibi .halo { animation: v-breathe 3.6s ease-in-out infinite; }
.vibi .bodygroup { transform-box: fill-box; transform-origin: 50% 35%; animation: v-bob 3s ease-in-out infinite; }
.vibi .eyes { transform-box: fill-box; transform-origin: 50% 50%; animation: v-blink 4.6s infinite; }
.vibi .wisp { fill: var(--vibi-mist); filter: drop-shadow(0 0 3px var(--vibi-mist)); transform-box: fill-box; transform-origin: center; animation: v-wisp 1.8s linear infinite; animation-delay: var(--d, 0s); opacity: 0; }
.vibi .spark { opacity: 0; }
.vibi .eyesx { display: none; }
.vibi .eyes-happy, .vibi .eyes-wide, .vibi .mouth-open, .vibi .mouth-o, .vibi .mouth-frown { display: none; }
.vibi .shield { display: none; }

.vibi.s-reading .bodygroup { animation: v-tilt 0.9s ease-in-out infinite; }
.vibi.s-reading .wisp { animation-duration: 0.9s; }
.vibi.s-reading .spark { animation: v-spark 1.6s infinite; }
.vibi.s-reading .spark.s2 { animation-delay: 0.45s; }
.vibi.s-reading .spark.s3 { animation-delay: 0.9s; }
.vibi.s-reading .eyes { display: none; }
.vibi.s-reading .eyes-happy { display: block; }
.vibi.s-reading .mouth { display: none; }
.vibi.s-reading .mouth-open { display: block; }

.vibi.s-gapped .wisps { display: none; }
.vibi.s-gapped .shield { display: block; }
.vibi.s-gapped .bodygroup { animation: v-breathe 2.2s ease-in-out infinite; }

.vibi.s-blocked { --vibi: var(--no); --vibi-fg: #fff; --vibi-mist: #ffd9de; --vibi-glow: 0 0 12px rgba(255, 59, 92, 0.5); }
.vibi.s-blocked .wisps { display: none; }
.vibi.s-blocked .bodygroup { animation: v-jolt 0.5s ease-in-out infinite; }
.vibi.s-blocked .halo { animation: v-pulse 0.5s ease-in-out infinite; }
.vibi.s-blocked .eyes { display: none; }
.vibi.s-blocked .eyes-wide { display: block; }
.vibi.s-blocked .mouth { display: none; }
.vibi.s-blocked .mouth-o { display: block; }

.vibi.s-error { --vibi: var(--faint); --vibi-fg: var(--text); --vibi-mist: #c3cad6; }
.vibi.s-error .eyes { display: none; }
.vibi.s-error .eyesx { display: block; }
.vibi.s-error .mouth { display: none; }
.vibi.s-error .mouth-frown { display: block; }
.vibi.s-error .wisps { opacity: 0.4; }
.vibi.s-error .bodygroup { animation: v-ember 2.4s ease-in-out infinite; }

@keyframes v-bob { 0%, 100% { transform: translateY(0); } 50% { transform: translateY(-5px); } }
@keyframes v-breathe { 0%, 100% { opacity: 0.4; } 50% { opacity: 0.85; } }
@keyframes v-pulse { 0%, 100% { opacity: 0.35; } 50% { opacity: 1; } }
@keyframes v-blink { 0%, 91%, 100% { transform: scaleY(1); } 94% { transform: scaleY(0.12); } }
@keyframes v-tilt { 0%, 100% { transform: rotate(-5deg); } 50% { transform: rotate(5deg); } }
@keyframes v-spark { 0%, 100% { opacity: 0; transform: translateY(0); } 50% { opacity: 1; transform: translateY(-6px); } }
@keyframes v-jolt { 0%, 100% { transform: translateX(0); } 20%, 60% { transform: translateX(2.5px); } 40%, 80% { transform: translateX(-2.5px); } }
@keyframes v-ember { 0%, 100% { opacity: 0.35; transform: rotate(6deg) translateY(8px); } 50% { opacity: 0.6; transform: rotate(5deg) translateY(8px); } }
@keyframes v-wisp { 0% { opacity: 0; transform: translate(0, 0) scale(1); } 15% { opacity: 0.9; } 100% { opacity: 0; transform: translate(var(--tx, 0px), var(--ty, 46px)) scale(0.3); } }

#sb-vibi { display: inline-flex; align-items: center; height: 100%; }
#sb-vibi .vibi { width: 14px; height: 20px; }
@media (prefers-reduced-motion: reduce) { .vibi * { animation: none !important; } }
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
    `<svg class="vibi s-resting" viewBox="0 0 120 170" aria-hidden="true">` +
    `<defs><radialGradient id="vibi-halo"><stop offset="0" stop-color="var(--vibi)" stop-opacity="0.55"/><stop offset="1" stop-color="var(--vibi)" stop-opacity="0"/></radialGradient></defs>` +
    `<g class="halo"><circle cx="60" cy="50" r="50" fill="url(#vibi-halo)"/></g>` +
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
  return 'resting'
}

function apply() {
  if (!el) return
  const s = flashState || computeState()
  if (s === current) return
  current = s
  el.setAttribute('class', 'vibi s-' + s)
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
  const host = document.getElementById('sb-vibi')
  if (!host) return
  const style = document.createElement('style')
  style.textContent = CSS
  document.head.appendChild(style)
  host.innerHTML = svgMarkup()
  el = host.querySelector('.vibi')
}

export function initVibi() {
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
initVibi()
