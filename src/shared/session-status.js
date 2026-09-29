// Per-pane activity → a status the sidebar can show at a glance.
// Pure, so the rules are pinned by vitest without a pty.
//
//   working   the agent is doing something
//   attention it wants you — blocked on a prompt, or finished while you
//             were looking elsewhere
//   idle      nothing new since you last looked
//   exited    the process is gone
//
// Two sources, best first:
//  1. REPORTED — the agent says so itself (Claude Code's lifecycle hooks
//     write OSC 8663 ; working|waiting|done to the pane's terminal; see
//     CLAUDE_STATE_HOOKS_ARG in src-tauri/crates/tome-flow/src/agent_spawn.rs).
//     Once a pane has reported, it is trusted over the timing guess, so a
//     long silent "thinking" stretch no longer reads as finished.
//  2. INFERRED — for agents without hooks: output bursts followed by quiet.
//     ponytail: a timing heuristic, not a protocol — an agent that pauses
//     mid-task for >QUIET_MS reads as finished early.
export const QUIET_MS = 2500
export const MIN_BURST_MS = 1500
export const REPORTED_STATES = ['working', 'waiting', 'done']

export const newActivity = () => ({
  burstStart: 0,
  lastOut: -Infinity,
  bell: false,
  seen: true,
  exited: false,
  reported: null, // null until the agent reports; then one of REPORTED_STATES
})

// Strip OSC sequences first: they are terminated by BEL, which is not a ring.
const OSC = /\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g
export const ringsBell = (data) => typeof data === 'string' && data.replace(OSC, '').includes('\x07')

export function noteOutput(a, now, data) {
  if (now - a.lastOut > QUIET_MS) {
    a.burstStart = now
    a.seen = false
  }
  a.lastOut = now
  if (ringsBell(data)) {
    a.bell = true
    a.seen = false
  }
}

// An agent's own report. Anything outside the closed vocabulary is ignored —
// the sequence can be printed by any program in the pane.
export function noteReport(a, state) {
  if (!REPORTED_STATES.includes(state)) return false
  a.reported = state
  if (state !== 'working') a.seen = false // something new to acknowledge
  return true
}

// Keys the user typed into the pane. Esc / Ctrl-C interrupt a turn without
// any "done" report (Claude Code's Stop hook does not fire on interrupt), so
// they drop back to inference; any other answer to a waiting agent means it
// is moving again.
export function noteInput(a, data) {
  if (!a.reported) return
  if (data === '\x1b' || data === '\x03') a.reported = null
  else if (a.reported === 'waiting') a.reported = 'working'
}

// Looking at a pane acknowledges whatever it last did.
export function markSeen(a) {
  a.seen = true
  a.bell = false
}

// `active`: the pane is the one in front of you. A blocked agent keeps asking
// for you everywhere else even after you glanced at it — it is still stuck.
export function sessionStatus(a, now, active = false) {
  if (a.exited) return 'exited'
  if (a.reported === 'working') return 'working'
  if (a.reported === 'waiting') return active ? 'idle' : 'attention'
  if (a.reported === 'done') return a.seen ? 'idle' : 'attention'
  if (now - a.lastOut < QUIET_MS && !a.bell) return 'working'
  if (!a.seen && (a.bell || a.lastOut - a.burstStart >= MIN_BURST_MS)) return 'attention'
  return 'idle'
}

// Why an attention state is one — the sidebar's words for it.
export function attentionReason(a) {
  if (a.reported === 'waiting') return 'needs you'
  if (a.reported === 'done') return 'finished'
  return a.bell ? 'rang the bell' : 'went quiet'
}
