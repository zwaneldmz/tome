// Pins confineRealAbs / confineRealAbsSync (src/main/lib/flow-confine.js) —
// the realpath-confinement flow-runner.js and flow-tools.js apply to every
// managed path they build themselves (a run directory, a log file, a
// flow.json target), mirroring brain.js's confineReal contract: validate
// the REAL path stays inside root, but return the LEXICAL one, so a
// symlinked tmp dir (macOS's own /tmp -> /private/tmp) never rewrites a
// path a caller is about to compare byte for byte. Real temp directories
// and real symlinks throughout, no mocks — this is the first test in the
// repo to create an actual symlink and confirm confinement survives one,
// rather than only a string that merely *looks* like an escape
// (test/brain.test.js, confine.js's own lexical-only guard).
import { describe, it, expect, beforeEach, afterEach } from 'vitest'
import { mkdtempSync, rmSync, mkdirSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { confineRealAbs, confineRealAbsSync } from '../src/main/lib/flow-confine.js'
import { safeSegment } from '../src/shared/flow-model.js'

let root, outside
beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), 'tome-confine-root-'))
  outside = mkdtempSync(join(tmpdir(), 'tome-confine-outside-'))
})
afterEach(() => {
  rmSync(root, { recursive: true, force: true })
  rmSync(outside, { recursive: true, force: true })
})

describe('confineRealAbs (async — flow-runner.js)', () => {
  it('returns the lexical path unchanged for an ordinary path inside root', async () => {
    const full = join(root, 'a', 'b')
    mkdirSync(full, { recursive: true })
    expect(await confineRealAbs(root, full)).toBe(full)
  })

  it('mustExist:true follows a symlinked FILE outside root and rejects it', async () => {
    const target = join(outside, 'secret.flow.json')
    writeFileSync(target, '{}')
    const link = join(root, 'evil.flow.json')
    symlinkSync(target, link)
    expect(await confineRealAbs(root, link)).toBeNull()
  })

  it('mustExist:true rejects a path reached through a symlinked ANCESTOR directory', async () => {
    writeFileSync(join(outside, 'x.flow.json'), '{}')
    symlinkSync(outside, join(root, 'linked'))
    expect(await confineRealAbs(root, join(root, 'linked', 'x.flow.json'))).toBeNull()
  })

  it('mustExist:false walks up to the nearest existing ancestor and rejects a symlinked one', async () => {
    // .tome/flows/<name>/runs/<id> — nothing below "flows" exists yet, but
    // "flows" itself was replaced with a symlink out of root, exactly as an
    // earlier run (or a hand-edited workspace) could leave it.
    symlinkSync(outside, join(root, 'flows'))
    const notYetCreated = join(root, 'flows', 'myflow', 'runs', 'r1')
    expect(await confineRealAbs(root, notYetCreated, { mustExist: false })).toBeNull()
  })

  it('mustExist:false accepts a not-yet-created path whose nearest existing ancestor is real', async () => {
    const notYetCreated = join(root, 'flows', 'myflow', 'runs', 'r1')
    expect(await confineRealAbs(root, notYetCreated, { mustExist: false })).toBe(notYetCreated)
  })

  it('rejects a path that is not even lexically inside root, and root itself does not count', async () => {
    expect(await confineRealAbs(root, join(outside, 'x'))).toBeNull()
    expect(await confineRealAbs(root, root)).toBeNull() // same rule confine() applies — strictly inside only
  })

  it('rejects non-string/empty/missing input rather than throwing', async () => {
    expect(await confineRealAbs(root, null)).toBeNull()
    expect(await confineRealAbs(null, join(root, 'x'))).toBeNull()
    expect(await confineRealAbs(root, undefined)).toBeNull()
  })

  it('rejects a root that does not exist at all', async () => {
    const ghostRoot = join(root, 'never-created')
    expect(await confineRealAbs(ghostRoot, join(ghostRoot, 'x'), { mustExist: false })).toBeNull()
  })
})

describe('confineRealAbsSync — same contract, synchronous (flow-tools.js stays sync)', () => {
  it('returns the lexical path unchanged for an ordinary path inside root', () => {
    const full = join(root, 'a.flow.json')
    expect(confineRealAbsSync(root, full, { mustExist: false })).toBe(full)
  })

  it('follows a symlinked ancestor directory and rejects it', () => {
    symlinkSync(outside, join(root, 'flows'))
    const full = join(root, 'flows', 'x.flow.json')
    expect(confineRealAbsSync(root, full, { mustExist: false })).toBeNull()
  })

  it('follows a symlinked file and rejects it (mustExist:true)', () => {
    const target = join(outside, 'secret.flow.json')
    writeFileSync(target, '{}')
    const link = join(root, 'evil.flow.json')
    symlinkSync(target, link)
    expect(confineRealAbsSync(root, link)).toBeNull()
  })
})

// safeSegment lives in flow-model.js (validateFlow's own identifier guard),
// exercised here alongside the path helper it exists to back up — the two
// together are what stand between a hand-edited node id/output name and a
// handoff path (or a run's own directory tree) that escapes the workspace.
describe('safeSegment', () => {
  it('accepts ordinary identifiers', () => {
    expect(safeSegment('n1')).toBe(true)
    expect(safeSegment('stale-list')).toBe(true)
    expect(safeSegment('egress-report')).toBe(true)
  })

  it('rejects non-string and empty input', () => {
    expect(safeSegment(undefined)).toBe(false)
    expect(safeSegment(null)).toBe(false)
    expect(safeSegment(42)).toBe(false)
    expect(safeSegment('')).toBe(false)
  })

  it('rejects "." and ".." exactly', () => {
    expect(safeSegment('.')).toBe(false)
    expect(safeSegment('..')).toBe(false)
  })

  it('rejects a path separator anywhere in the string, not just as the whole value', () => {
    expect(safeSegment('a/b')).toBe(false)
    expect(safeSegment('a\\b')).toBe(false)
    // A traversal sequence is rejected as a whole, not merely for equalling "..".
    expect(safeSegment('../../../escaped')).toBe(false)
  })

  it('rejects a colon (drive prefix on one platform, inert elsewhere — refused either way)', () => {
    expect(safeSegment('a:b')).toBe(false)
  })

  it('rejects control characters', () => {
    expect(safeSegment('a\nb')).toBe(false)
    expect(safeSegment('a\0b')).toBe(false)
    expect(safeSegment('a\x7fb')).toBe(false)
  })

  it('rejects a leading "-"', () => {
    expect(safeSegment('-rf')).toBe(false)
  })

  it('allows a "-" that is not leading', () => {
    expect(safeSegment('n-1')).toBe(true)
  })
})
