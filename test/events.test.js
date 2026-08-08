// Pins the event log's pure core (src/main/lib/eventlog.js): the append cap,
// the record shape with injectable ts, and crash-tolerant JSONL parsing — a
// truncated final line from a crash mid-append must never break reads.
import { describe, it, expect } from 'vitest'
import { CAP, TAIL, makeEvent, appendEvent, parseEvents, tailEvents } from '../src/main/lib/eventlog.js'

describe('makeEvent', () => {
  it('spreads fields over { ts, kind }', () => {
    expect(makeEvent('airgap:unlock', { paneId: 'pty-3', minutes: 15 }, '2026-08-09T10:00:00.000Z')).toEqual({
      ts: '2026-08-09T10:00:00.000Z',
      kind: 'airgap:unlock',
      paneId: 'pty-3',
      minutes: 15,
    })
  })

  it('defaults ts to an ISO string when not injected', () => {
    const rec = makeEvent('airgap:relock', { paneId: 'pty-1' })
    expect(rec.kind).toBe('airgap:relock')
    expect(new Date(rec.ts).toISOString()).toBe(rec.ts)
  })
})

describe('appendEvent', () => {
  it('appends one JSON line and returns a new array', () => {
    const before = []
    const after = appendEvent(before, makeEvent('airgap:blocked', { paneId: 'pty-2', host: 'evil.com' }, 't1'))
    expect(after).not.toBe(before)
    expect(after).toEqual(['{"ts":"t1","kind":"airgap:blocked","paneId":"pty-2","host":"evil.com"}'])
  })

  it('caps at 5000 lines, dropping the oldest', () => {
    let lines = []
    for (let i = 0; i < CAP; i++) lines = appendEvent(lines, { ts: `t${i}`, kind: 'k', i })
    expect(lines).toHaveLength(CAP)
    lines = appendEvent(lines, { ts: 't-new', kind: 'k', i: CAP })
    expect(lines).toHaveLength(CAP)
    expect(JSON.parse(lines[0]).i).toBe(1) // t0 dropped
    expect(JSON.parse(lines[CAP - 1]).ts).toBe('t-new')
  })
})

describe('parseEvents', () => {
  it('round-trips appended lines', () => {
    let lines = []
    lines = appendEvent(lines, makeEvent('conductor:tool', { tool: 'open_pane', chatId: 'chat-1', ok: true, hint: 'terminal' }, 't1'))
    lines = appendEvent(lines, makeEvent('airgap:relock', { paneId: 'pty-1' }, 't2'))
    expect(parseEvents(lines.join('\n') + '\n')).toEqual([
      { ts: 't1', kind: 'conductor:tool', tool: 'open_pane', chatId: 'chat-1', ok: true, hint: 'terminal' },
      { ts: 't2', kind: 'airgap:relock', paneId: 'pty-1' },
    ])
  })

  it('skips a truncated final line (crash mid-append)', () => {
    const text = '{"ts":"t1","kind":"airgap:unlock","paneId":"pty-3"}\n{"ts":"t2","kind":"airgap:unl'
    expect(parseEvents(text)).toEqual([{ ts: 't1', kind: 'airgap:unlock', paneId: 'pty-3' }])
  })

  it('skips blank lines and non-object JSON', () => {
    expect(parseEvents('\n  \n42\n"x"\nnull\n{}\n')).toEqual([{}])
  })

  it('parses an empty/missing file to []', () => {
    expect(parseEvents('')).toEqual([])
  })

  it('tolerates CRLF line endings (no \\r leaks into fields)', () => {
    const rec = '{"ts":"t1","kind":"airgap:relock","paneId":"pty-1"}'
    const out = parseEvents(rec + '\r\n' + rec)
    expect(out).toHaveLength(2)
    expect(out[0]).toEqual({ ts: 't1', kind: 'airgap:relock', paneId: 'pty-1' })
    for (const v of Object.values(out[0])) expect(String(v)).not.toContain('\r')
  })
})

describe('tailEvents', () => {
  it('returns the whole array when under TAIL', () => {
    const events = [{ kind: 'a' }, { kind: 'b' }]
    expect(tailEvents(events)).toBe(events)
  })

  it('at exactly TAIL the input is returned by identity (no copy)', () => {
    // Pins current behavior: the >-vs->= boundary means "exactly at the
    // limit" is the same object back, not a slice.
    const events = Array.from({ length: 200 }, (_, i) => ({ kind: 'k', i }))
    expect(tailEvents(events)).toBe(events)
  })

  it('honors an explicit n', () => {
    const events = Array.from({ length: 10 }, (_, i) => ({ kind: 'k', i }))
    const tail = tailEvents(events, 3)
    expect(tail).toHaveLength(3)
    expect(tail[0].i).toBe(7)
    expect(tailEvents(events, 10)).toBe(events) // exactly n: identity too
  })

  it('keeps the most recent TAIL (200), oldest-first', () => {
    expect(TAIL).toBe(200)
    const events = Array.from({ length: 250 }, (_, i) => ({ kind: 'k', i }))
    const tail = tailEvents(events)
    expect(tail).toHaveLength(200)
    expect(tail[0].i).toBe(50)
    expect(tail[199].i).toBe(249)
  })
})
