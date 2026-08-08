// Persistent event log: main owns userData/events.jsonl, one JSON object per
// line (append-only, capped by the lib at write/parse time). The renderer
// reads the tail over the lock-gated events:list channel and gets live pushes
// via events:appended — it never touches the file itself.
import { appendFile, readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { CAP, makeEvent, parseEvents, tailEvents } from './lib/eventlog.js'

let file = null
let onEvent = () => {}
// Approximate line count, so the append-only file honors CAP on disk and
// not just at read time. Seeded lazily on the first append.
let lines = null
// The rewrite is a full-file read+write — amortized to at most once per 500
// appends so a chatty session isn't paying it on every record. The counter
// is approximate anyway, so the extra slack changes nothing semantically.
let sinceRewrite = 0

export function initEvents(userData) {
  file = join(userData, 'events.jsonl')
}

// Same sink shape as airgap.setEventSink — index.js wires it to
// win.webContents.send('events:' + type, payload).
export function setEventSink(fn) {
  onEvent = fn
}

// Fire-and-forget on purpose: logging must never break the thing being
// logged (a full disk must not wedge an air-gap unlock). The record is made
// before the await so the renderer push carries the same object that was
// written even if the write fails.
export function logEvent(kind, fields) {
  const record = makeEvent(kind, fields)
  if (file) {
    appendFile(file, JSON.stringify(record) + '\n', 'utf8').catch(() => {})
    countAndMaybeTrim()
  }
  onEvent('appended', record)
  return record
}

// Every step .catch()es and continues — logging must never break the thing
// being logged, least of all because the log's own housekeeping failed.
async function countAndMaybeTrim() {
  if (lines === null) {
    lines = await readFile(file, 'utf8')
      .then((t) => t.split('\n').filter((s) => s.trim()).length)
      .catch(() => 0)
  }
  lines++
  if (++sinceRewrite < 500) return
  sinceRewrite = 0
  if (lines <= CAP) return
  const text = await readFile(file, 'utf8').catch(() => null)
  if (text === null) return
  const tail = tailEvents(parseEvents(text), CAP)
  await writeFile(file, tail.map((r) => JSON.stringify(r)).join('\n') + '\n', 'utf8').catch(
    () => {}
  )
  lines = tail.length
}

// Missing file is a normal first-run state, not an error. Malformed lines
// (crash mid-append) are skipped by the lib, not surfaced.
export async function readEvents() {
  if (!file) return []
  let text
  try {
    text = await readFile(file, 'utf8')
  } catch {
    return []
  }
  return tailEvents(parseEvents(text))
}
