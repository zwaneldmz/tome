// Pins the two policy decisions createPty (src/main/index.js) delegates here
// (TOME-001): whether a pane is actually gapped, and what directory it
// starts in. Both used to come straight from the renderer with no main-side
// check at all — a renderer requesting gapped:false always won, even with
// the 'airgap-default' preference set to gap everything, and `cwd` reached
// pty.spawn unchanged, unlike every other renderer-supplied path in this app
// (isConfinedPath/confinedRealPath, confineToRoot, confine). The cases below
// are the acceptance test: a renderer that asks for LESS isolation than
// policy wants must be overridden, and a cwd outside the open workspace
// roots (and outside home) must fall back to home rather than reach
// pty.spawn unchanged.
import { describe, it, expect, beforeEach, afterEach } from 'vitest'
import { mkdtempSync, rmSync, mkdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { resolveGapping, resolveSpawnCwd } from '../src/main/lib/pty-authority.js'

describe('resolveGapping()', () => {
  it('renderer requests no gap, but policy defaults to gapped -> gapped wins', () => {
    expect(resolveGapping(false, true)).toBe(true)
  })

  it('renderer requests a gap even though the policy default is off -> still gapped', () => {
    expect(resolveGapping(true, false)).toBe(true)
  })

  it('both sides agree there is no gap -> ungapped', () => {
    expect(resolveGapping(false, false)).toBe(false)
  })

  it('both sides agree there is a gap -> gapped', () => {
    expect(resolveGapping(true, true)).toBe(true)
  })

  it('the renderer can only ever ADD isolation, never remove what policy wants (TOME-001 case)', () => {
    // A compromised renderer sending gapped:false in any falsy shape must
    // not escape a "gap by default" policy.
    for (const rendererGapped of [false, undefined, null, 0, '']) {
      expect(resolveGapping(rendererGapped, true)).toBe(true)
    }
  })

  it('treats missing/falsy inputs on both sides as no gap requested', () => {
    expect(resolveGapping(undefined, undefined)).toBe(false)
    expect(resolveGapping(null, null)).toBe(false)
  })
})

describe('resolveSpawnCwd()', () => {
  let root, sibling, home
  beforeEach(() => {
    root = mkdtempSync(join(tmpdir(), 'tome-pty-root-'))
    sibling = mkdtempSync(join(tmpdir(), 'tome-pty-sibling-'))
    home = mkdtempSync(join(tmpdir(), 'tome-pty-home-'))
  })
  afterEach(() => {
    rmSync(root, { recursive: true, force: true })
    rmSync(sibling, { recursive: true, force: true })
    rmSync(home, { recursive: true, force: true })
  })

  it('accepts a directory inside an open workspace root', () => {
    const dir = join(root, 'sub')
    mkdirSync(dir)
    expect(resolveSpawnCwd(dir, [root], home)).toBe(dir)
  })

  it('accepts the root itself', () => {
    expect(resolveSpawnCwd(root, [root], home)).toBe(root)
  })

  it('accepts a directory inside the home subtree even when no root matches', () => {
    const dir = join(home, 'projects', 'x')
    mkdirSync(dir, { recursive: true })
    expect(resolveSpawnCwd(dir, [root], home)).toBe(dir)
  })

  it('accepts home itself', () => {
    expect(resolveSpawnCwd(home, [root], home)).toBe(home)
  })

  it('falls back to home for a directory outside every root and outside home', () => {
    expect(resolveSpawnCwd(sibling, [root], home)).toBe(home)
  })

  it('falls back to home for a same-prefix sibling of a root (not actually nested under it)', () => {
    // "<root>-evil" must not pass for open root "<root>" — string-prefix
    // matching without the separator would wrongly allow this, same trap
    // confineToRoot/isConfinedPath already guard against.
    const evil = `${root}-evil`
    mkdirSync(evil)
    expect(resolveSpawnCwd(evil, [root], home)).toBe(home)
  })

  it('falls back to home for a path that does not exist', () => {
    expect(resolveSpawnCwd(join(root, 'never-created'), [root], home)).toBe(home)
  })

  it('falls back to home for an existing FILE, not a directory', () => {
    const file = join(root, 'a.txt')
    writeFileSync(file, 'x')
    expect(resolveSpawnCwd(file, [root], home)).toBe(home)
  })

  it('falls back to home for non-string or empty cwd, without throwing', () => {
    expect(resolveSpawnCwd(undefined, [root], home)).toBe(home)
    expect(resolveSpawnCwd(null, [root], home)).toBe(home)
    expect(resolveSpawnCwd(42, [root], home)).toBe(home)
    expect(resolveSpawnCwd('', [root], home)).toBe(home)
  })

  it('falls back to home when no workspace roots are open yet (matches isConfinedPath pre-ws:sync)', () => {
    const dir = join(root, 'sub2')
    mkdirSync(dir)
    expect(resolveSpawnCwd(dir, [], home)).toBe(home)
    expect(resolveSpawnCwd(dir, undefined, home)).toBe(home)
  })

  it('picks whichever open root actually contains the path when several are open', () => {
    const other = mkdtempSync(join(tmpdir(), 'tome-pty-other-'))
    const dir = join(other, 'sub')
    mkdirSync(dir)
    expect(resolveSpawnCwd(dir, [root, other], home)).toBe(dir)
    rmSync(other, { recursive: true, force: true })
  })
})
