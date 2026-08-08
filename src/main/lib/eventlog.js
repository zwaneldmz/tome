// Pure core of the persistent event log (userData/events.jsonl): no electron,
// no fs — just the record shape, the JSONL parse, and the append cap, so the
// whole thing is testable without module state. src/main/events.js owns the
// actual file; the renderer only ever sees parsed records over IPC.
//
// The log records security-relevant ACTIONS (conductor tool calls, air-gap
// unlocks/relocks, blocked egress) — kinds + identifiers only, never tool
// inputs/outputs or typed text, which may carry secrets.

// Hard cap on retained entries: the file is append-only (no rotation), so
// without a bound it grows forever. Reads tail the most recent TAIL.
export const CAP = 5000
export const TAIL = 200

// `ts` is the caller's (injectable in tests) so a record is a pure value.
export function makeEvent(kind, fields, ts = new Date().toISOString()) {
  return { ts, kind, ...fields }
}

// Returns a NEW lines array (callers may reuse the input) with the record
// appended as one JSON line, oldest dropped when over CAP.
export function appendEvent(lines, event) {
  const next = lines.concat(JSON.stringify(event))
  return next.length > CAP ? next.slice(next.length - CAP) : next
}

// Parses JSONL back into records, skipping blank and malformed lines — a
// crash mid-append can leave a truncated final line, and that must not break
// every read that follows. Non-object lines are dropped too.
export function parseEvents(text) {
  const out = []
  for (const line of String(text).split('\n')) {
    const s = line.trim()
    if (!s) continue
    try {
      const rec = JSON.parse(s)
      if (rec && typeof rec === 'object') out.push(rec)
    } catch {
      // truncated/corrupt line — skip it, keep the rest
    }
  }
  return out
}

// Read-side helper: most recent TAIL, oldest-first (the pane reverses for
// newest-first display; live appends then simply prepend).
export function tailEvents(events, n = TAIL) {
  return events.length > n ? events.slice(events.length - n) : events
}
