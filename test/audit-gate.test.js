// Pins scripts/audit-gate.mjs (TOME-010/TOME-017): the sole pass/fail
// authority for both CI dependency-audit steps (.github/workflows/build.yml)
// — npm audit's own exit code has no concept of a reviewed exception. Before
// this, nothing in the suite exercised severity ranking, exception-expiry
// parsing, or the blocked/excepted decision offline; a regression such as
// flipping isLive()'s expiry comparison, transposing SEVERITY_RANK, or
// inverting the rank/threshold skip condition would have silently let an
// unexcepted high/critical advisory pass CI green. The CLI entry point
// (main(), stdin/exit-code side effects) is guarded behind
// `if (import.meta.main)`, so importing this module for its exported pure
// functions never touches stdin or process.exitCode.
import { describe, it, expect } from 'vitest'
import { SEVERITY_RANK, isLive, advisoryId, evaluateAudit, bunToNpm, normalizeAudit } from '../scripts/audit-gate.mjs'

describe('SEVERITY_RANK', () => {
  it('ranks strictly info < low < moderate < high < critical', () => {
    // Pins the exact values, not just relative order — a transposition
    // between two adjacent levels would still "look" ordered against
    // neighbors but rank the wrong things at/above a given threshold.
    expect(SEVERITY_RANK).toEqual({ info: 0, low: 1, moderate: 2, high: 3, critical: 4 })
  })
})

describe('isLive()', () => {
  const future = new Date(Date.now() + 86_400_000).toISOString() // +1 day
  const past = new Date(Date.now() - 86_400_000).toISOString() // -1 day

  it('is true for a reasoned exception with a future expiry', () => {
    expect(isLive({ reason: 'vendor fix pending', expires: future })).toBe(true)
  })

  it('is false once expires has passed — the comparison this pins', () => {
    // A flipped `Date.now() < expiresAt` (for example to <=, or the operands
    // swapped) would accept an expired exception; both directions are wrong.
    expect(isLive({ reason: 'vendor fix pending', expires: past })).toBe(false)
  })

  it('is false with no reason, an empty reason, or a whitespace-only reason', () => {
    expect(isLive({ expires: future })).toBe(false)
    expect(isLive({ reason: '', expires: future })).toBe(false)
    expect(isLive({ reason: '   ', expires: future })).toBe(false)
  })

  it('is false with a missing or non-string expires', () => {
    expect(isLive({ reason: 'ok' })).toBe(false)
    expect(isLive({ reason: 'ok', expires: null })).toBe(false)
    expect(isLive({ reason: 'ok', expires: 12345 })).toBe(false)
  })

  it('is false with an unparseable expires string', () => {
    expect(isLive({ reason: 'ok', expires: 'not-a-date' })).toBe(false)
  })

  it('is false for a missing entry entirely', () => {
    expect(isLive(undefined)).toBe(false)
    expect(isLive(null)).toBe(false)
  })
})

describe('advisoryId()', () => {
  it('prefers the GHSA slug parsed from the advisory URL', () => {
    expect(advisoryId({ url: 'https://github.com/advisories/GHSA-abcd-1234-efgh', source: 999 })).toBe(
      'GHSA-abcd-1234-efgh',
    )
  })

  it('falls back to the numeric source id when the URL is absent or unmatched', () => {
    expect(advisoryId({ source: 12345 })).toBe('12345')
    expect(advisoryId({ url: 'https://example.com/not-an-advisory', source: 12345 })).toBe('12345')
  })

  it('treats source 0 as a valid id — an `!== undefined` check, not truthiness', () => {
    expect(advisoryId({ source: 0 })).toBe('0')
  })

  it('returns null when neither a matching URL nor a source id is present', () => {
    expect(advisoryId({})).toBeNull()
    expect(advisoryId({ url: 'https://example.com/nothing-here' })).toBeNull()
  })
})

describe('evaluateAudit()', () => {
  const ghsa = 'GHSA-test-0000-0001'
  const vulnAt = (severity, { via = [] } = {}) => ({
    name: 'evil-pkg',
    severity,
    via: via.length ? via : [{ source: 1, title: 'A bad thing', url: `https://github.com/advisories/${ghsa}` }],
  })

  it('blocks an unexcepted advisory at or above the threshold', () => {
    const { blocked, reviewed, excepted } = evaluateAudit({ p: vulnAt('high') }, {}, SEVERITY_RANK.high)
    expect(blocked).toEqual([
      { name: 'evil-pkg', severity: 'high', unexcepted: [{ id: ghsa, title: 'A bad thing', url: expect.any(String) }] },
    ])
    expect(reviewed).toBe(1)
    expect(excepted).toBe(0)
  })

  it('does not block a live-excepted advisory', () => {
    const future = new Date(Date.now() + 86_400_000).toISOString()
    const exceptions = { [ghsa]: { reason: 'accepted risk, patch queued', expires: future } }
    const { blocked, excepted, reviewed } = evaluateAudit({ p: vulnAt('critical') }, exceptions, SEVERITY_RANK.critical)
    expect(blocked).toEqual([])
    expect(excepted).toBe(1)
    expect(reviewed).toBe(1)
  })

  it('treats an EXPIRED exception as unexcepted — still blocks', () => {
    const past = new Date(Date.now() - 86_400_000).toISOString()
    const exceptions = { [ghsa]: { reason: 'accepted risk, patch queued', expires: past } }
    const { blocked } = evaluateAudit({ p: vulnAt('high') }, exceptions, SEVERITY_RANK.high)
    expect(blocked).toHaveLength(1)
  })

  it('skips advisories below the threshold entirely — not reviewed, not blocked', () => {
    const { blocked, reviewed } = evaluateAudit({ p: vulnAt('moderate') }, {}, SEVERITY_RANK.high)
    expect(blocked).toEqual([])
    expect(reviewed).toBe(0)
  })

  it('does not skip an advisory exactly AT the threshold — the rank < threshold boundary', () => {
    // Regression guard: an inverted or off-by-one skip condition (for example
    // `rank <= threshold`) would wrongly drop the exact-threshold case.
    const { blocked, reviewed } = evaluateAudit({ p: vulnAt('high') }, {}, SEVERITY_RANK.high)
    expect(reviewed).toBe(1)
    expect(blocked).toHaveLength(1)
  })

  it('fails safe on an unrecognized severity string — ranked as critical, never skipped', () => {
    const { blocked, reviewed } = evaluateAudit({ p: vulnAt('made-up-severity') }, {}, SEVERITY_RANK.critical)
    expect(reviewed).toBe(1)
    expect(blocked).toHaveLength(1)
  })

  it('blocks a package with a mix of excepted and unexcepted advisories, counting both', () => {
    const otherGhsa = 'GHSA-test-0000-0002'
    const future = new Date(Date.now() + 86_400_000).toISOString()
    const exceptions = { [ghsa]: { reason: 'reviewed', expires: future } }
    const vuln = vulnAt('high', {
      via: [
        { source: 1, title: 'Covered', url: `https://github.com/advisories/${ghsa}` },
        { source: 2, title: 'Not covered', url: `https://github.com/advisories/${otherGhsa}` },
      ],
    })
    const { blocked, excepted } = evaluateAudit({ p: vuln }, exceptions, SEVERITY_RANK.high)
    expect(excepted).toBe(1)
    expect(blocked).toEqual([
      { name: 'evil-pkg', severity: 'high', unexcepted: [{ id: otherGhsa, title: 'Not covered', url: expect.any(String) }] },
    ])
  })

  it('ignores string entries in `via` (a transitive dependency name, not an advisory object)', () => {
    const vuln = vulnAt('high', { via: ['some-transitive-dep'] })
    const { blocked, reviewed } = evaluateAudit({ p: vuln }, {}, SEVERITY_RANK.high)
    expect(reviewed).toBe(1)
    expect(blocked).toEqual([]) // no object-shaped advisory to be unexcepted about
  })

  it('is a pure function of its inputs — no I/O, safe to call repeatedly', () => {
    const report = { p: vulnAt('critical') }
    const first = evaluateAudit(report, {}, SEVERITY_RANK.critical)
    const second = evaluateAudit(report, {}, SEVERITY_RANK.critical)
    expect(second).toEqual(first)
  })
})

describe('bunToNpm()', () => {
  it('folds bun audit JSON into the npm shape evaluateAudit() consumes', () => {
    const bun = {
      nanoid: [
        { id: 1139427, url: 'https://github.com/advisories/GHSA-2v37-7h3g-55p8', title: 'nanoid: bad thing', severity: 'high' },
        { id: 123, url: 'https://github.com/advisories/GHSA-aaaa-bbbb-cccc', title: 'nanoid: worse thing', severity: 'critical' },
      ],
      'some-pkg': [{ id: 999, url: 'https://github.com/advisories/GHSA-zzzz', title: 'low', severity: 'low' }],
    }
    const npm = bunToNpm(bun)
    expect(npm.vulnerabilities.nanoid).toEqual({
      name: 'nanoid',
      severity: 'critical', // the max of high + critical, like npm reports
      via: [
        { title: 'nanoid: bad thing', url: 'https://github.com/advisories/GHSA-2v37-7h3g-55p8', source: 1139427 },
        { title: 'nanoid: worse thing', url: 'https://github.com/advisories/GHSA-aaaa-bbbb-cccc', source: 123 },
      ],
    })
    expect(npm.vulnerabilities['some-pkg'].severity).toBe('low')
  })

  it('ranks an unrecognized bun severity as critical — fails safe, never skipped', () => {
    const bun = { pkg: [{ id: 1, title: 'x', url: 'https://github.com/advisories/GHSA-zz', severity: 'made-up' }] }
    expect(bunToNpm(bun).vulnerabilities.pkg.severity).toBe('critical')
  })

  it('treats an empty report as an empty vulnerabilities object, not a failure', () => {
    expect(bunToNpm({})).toEqual({ vulnerabilities: {} })
  })
})

describe('normalizeAudit()', () => {
  it('passes npm-shaped reports through untouched', () => {
    const npm = { vulnerabilities: { pkg: { name: 'pkg', severity: 'high', via: [] } } }
    expect(normalizeAudit(npm)).toBe(npm)
  })

  it('transforms bun-shaped reports (no top-level vulnerabilities key)', () => {
    const bun = { pkg: [{ id: 1, url: 'https://github.com/advisories/GHSA-zz', title: 'x', severity: 'high' }] }
    const normalized = normalizeAudit(bun)
    expect(normalized.vulnerabilities.pkg.severity).toBe('high')
  })
})
