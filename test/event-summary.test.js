// Pins the event-summary display helpers (src/renderer/event-summary.js):
// one branch per known event kind, plus the default branch for anything
// else — these strings are the audit UI, so formatting drift is a bug.
import { describe, it, expect } from 'vitest'
import { summary, stamp } from '../src/renderer/event-summary.js'

describe('summary', () => {
  it('conductor:tool — tool · hint, with "failed" when ok is false', () => {
    expect(summary({ kind: 'conductor:tool', tool: 'open_pane', hint: 'terminal', ok: true })).toBe(
      'open_pane · terminal'
    )
    expect(summary({ kind: 'conductor:tool', tool: 'open_pane', hint: 'terminal', ok: false })).toBe(
      'open_pane · terminal · failed'
    )
    expect(summary({ kind: 'conductor:tool', tool: 'read_terminal' })).toBe('read_terminal')
  })

  it('egress:unlock — paneId · minutes', () => {
    expect(summary({ kind: 'egress:unlock', paneId: 'pty-3', minutes: 15 })).toBe('pty-3 · 15m')
    expect(summary({ kind: 'egress:unlock', paneId: 'pty-3' })).toBe('pty-3')
  })

  it('egress:relock — paneId only', () => {
    expect(summary({ kind: 'egress:relock', paneId: 'pty-1' })).toBe('pty-1')
    expect(summary({ kind: 'egress:relock' })).toBe('')
  })

  it('egress:blocked — host · paneId, with × N when coalesced', () => {
    expect(summary({ kind: 'egress:blocked', host: 'evil.com', paneId: 'pty-2' })).toBe(
      'evil.com · pty-2'
    )
    expect(summary({ kind: 'egress:blocked', host: 'evil.com', paneId: 'pty-2', count: 7 })).toBe(
      'evil.com · pty-2 · × 7'
    )
  })

  it('flow-run — flow · node · status, with the exit code when a node ended', () => {
    // Background runs write one record per transition; the node-level ones
    // carry a node id and the run-level ones do not, so both read as one line.
    expect(
      summary({ kind: 'flow-run', event: 'run', run: 'm1h2k3', flow: 'release-notes', status: 'running', nodes: 3 })
    ).toBe('release-notes · running')
    expect(
      summary({ kind: 'flow-run', event: 'node', run: 'm1h2k3', flow: 'release-notes', node: 'n2', agent: 'claude', status: 'failed', exit: 1 })
    ).toBe('release-notes · n2 · failed · exit 1')
    // exit 0 is a value, not an absence — `!= null`, not truthiness.
    expect(
      summary({ kind: 'flow-run', event: 'node', flow: 'release-notes', node: 'n1', status: 'done', exit: 0 })
    ).toBe('release-notes · n1 · done · exit 0')
    // The cancel record has no status of its own; the verb is the event.
    expect(summary({ kind: 'flow-run', event: 'cancel', run: 'm1h2k3', flow: 'release-notes' })).toBe(
      'release-notes · cancel'
    )
  })

  it('default branch joins non-ts/kind field values', () => {
    expect(summary({ kind: 'something:new', ts: 't1', a: 'x', b: 'y' })).toBe('x · y')
  })

  it('default branch stringifies object values as [object Object]', () => {
    // PINNED AS-IS: String({}) is "[object Object]" — the log's own kinds
    // never carry object fields (identifiers only by design), so this only
    // shows for foreign records, where a raw string is acceptable. If the
    // log ever grows structured fields, fix the formatter, not this test.
    expect(summary({ kind: 'something:new', ts: 't1', payload: { x: 1 } })).toBe(
      '[object Object]'
    )
  })
})

describe('stamp', () => {
  it('returns an empty string for an unparseable ts', () => {
    expect(stamp('not a date')).toBe('')
  })

  it('shows a bare time for a timestamp today', () => {
    // Locale-dependent (12- vs 24-hour), so pin the shape: a time, no date.
    const s = stamp(new Date().toISOString())
    expect(s).toContain(':')
    expect(s).not.toMatch(/\w{3} /)
  })

  it('shows a weekday prefix for a timestamp this week', () => {
    const threeDaysAgo = new Date(Date.now() - 3 * 86_400_000)
    // If "3 days ago" crosses midnight-of-week boundaries the weekday branch
    // still applies (< 7 days); only the far past switches to month/day.
    expect(stamp(threeDaysAgo.toISOString())).toMatch(/^\w{3} .+:/)
  })

  it('shows month/day for a timestamp older than a week', () => {
    const old = new Date(Date.now() - 30 * 86_400_000)
    expect(stamp(old.toISOString())).toMatch(/^\w{3} \d{1,2}$/)
  })
})
