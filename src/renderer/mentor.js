// Mentor mode — renderer half of the teaching loop. Owns the persisted
// `mentor` settings (single store key), the per-workspace verbose/uq flags,
// and the one gate subscription that opens the comprehension overlay when the
// model calls gate_question. The backend contract is already landed in
// src-tauri: `chat_send` takes `verbose`, the gate emits `mentor:check`, and
// `mentor_answer` completes it — this file only consumes that surface.
import { tome } from './util.js'
import { activeWorkspace, saveWs } from './workspaces.js'
import { showMentorOverlay } from './mentor-overlay.js'

const DEFAULTS = {
  verboseDefault: false,
  gate: true,
  gatePoints: { implement: true, commit: true, push: true },
  questionTypes: ['multiple_choice', 'true_false', 'short_answer'],
  threshold: 60,
}

export const mentorState = {
  ...DEFAULTS,
  gatePoints: { ...DEFAULTS.gatePoints },
  questionTypes: [...DEFAULTS.questionTypes],
}

export function saveMentorSettings(partial) {
  Object.assign(mentorState, partial)
  tome.store.set('mentor', mentorState)
}

// Verbose (teaching) persona — a workspace's own flag wins over the default.
export function isVerbose() {
  return activeWorkspace()?.verbose ?? mentorState.verboseDefault
}

// Understanding score, stored per workspace (0..100; a gate pass adds to it).
export function uq() {
  return activeWorkspace()?.uq ?? 0
}

export function setUq(n) {
  const w = activeWorkspace()
  if (!w) return
  w.uq = n
  saveWs()
  // statusbar.js listens for this instead of importing renderUq back here —
  // statusbar.js imports this module, so the reverse import would cycle.
  window.dispatchEvent(new window.CustomEvent('mentor:uq-changed'))
}

// The model called gate_question: open the gate overlay. Subscribed once, at
// module load. showMentorOverlay is only *called* inside this callback (never
// at module-evaluation time), so the mentor.js <-> mentor-overlay.js import
// cycle stays safe — by the time the event fires both modules are settled.
tome.mentor.onCheck((payload) => showMentorOverlay(payload))

// Merge persisted settings over the defaults. A missing/partial/malformed
// store value degrades to the defaults rather than throwing.
tome.store.get('mentor').then((saved) => {
  if (!saved || typeof saved !== 'object') return
  if (saved.gatePoints && typeof saved.gatePoints === 'object')
    Object.assign(mentorState.gatePoints, saved.gatePoints)
  if (Array.isArray(saved.questionTypes)) mentorState.questionTypes = saved.questionTypes
  for (const key of ['verboseDefault', 'gate', 'threshold']) {
    if (saved[key] !== undefined) mentorState[key] = saved[key]
  }
})
