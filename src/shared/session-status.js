// Per-pane activity → a status the sidebar can show at a glance.
// Pure, so the timing rules are pinned by vitest without a pty.
//
//   working   output arrived within the last QUIET_MS
//   attention a real burst of work finished (or the pane rang the bell)
//             while you were looking elsewhere — it probably wants you
//   idle      quiet, and you have seen whatever it last did
//   exited    the process is gone
//
// ponytail: output-timing heuristic, not a protocol. An agent that pauses
// mid-task for >QUIET_MS reads as "attention" early; upgrade path is agent
// hooks (e.g. Claude Code's Notification hook) reporting real state.
export const QUIET_MS = 2500
export const MIN_BURST_MS = 1500

export const newActivity = () => ({ burstStart: 0, lastOut: -Infinity, bell: false, seen: true, exited: false })

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

// Looking at a pane acknowledges whatever it last did.
export function markSeen(a) {
  a.seen = true
  a.bell = false
}

export function sessionStatus(a, now) {
  if (a.exited) return 'exited'
  if (now - a.lastOut < QUIET_MS && !a.bell) return 'working'
  if (!a.seen && (a.bell || a.lastOut - a.burstStart >= MIN_BURST_MS)) return 'attention'
  return 'idle'
}
