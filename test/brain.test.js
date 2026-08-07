// Pins brain.confine() — the vault traversal guard. Verified correct today
// (pi review §2); a regression here is a renderer-reachable file escape.
import { describe, it, expect } from 'vitest'
import { join, sep } from 'node:path'
import { confine } from '../src/main/lib/confine.js'

const ROOT = join(sep, 'vaults', 'foo')

describe('confine()', () => {
  it('allows normal relative paths inside the vault', () => {
    expect(confine(ROOT, 'note.md', true)).toBe(join(ROOT, 'note.md'))
    expect(confine(ROOT, 'sub/dir/note.md', true)).toBe(join(ROOT, 'sub', 'dir', 'note.md'))
    expect(confine(ROOT, 'sub folder', false)).toBe(join(ROOT, 'sub folder'))
  })

  it('blocks .. traversal', () => {
    expect(confine(ROOT, '../outside.md', true)).toBeNull()
    expect(confine(ROOT, 'sub/../../outside.md', true)).toBeNull()
    expect(confine(ROOT, '..', false)).toBeNull()
    // backslash separators count too (win-style input on any platform)
    expect(confine(ROOT, '..\\outside.md', true)).toBeNull()
  })

  it('blocks absolute paths', () => {
    expect(confine(ROOT, '/etc/passwd.md', true)).toBeNull()
    expect(confine(ROOT, '/vaults/foo/note.md', true)).toBeNull()
  })

  it('blocks sibling-prefix escapes (vault "foo" must not accept "../foobar/x")', () => {
    expect(confine(ROOT, '../foobar/x.md', true)).toBeNull()
    // and the resolved-path check alone must not pass a sibling prefix
    expect(confine(join(sep, 'vaults', 'foo'), 'foo2.md', true)).toBe(join(ROOT, 'foo2.md'))
  })

  it('rejects non-string input', () => {
    expect(confine(ROOT, null, true)).toBeNull()
    expect(confine(ROOT, undefined, true)).toBeNull()
    expect(confine(ROOT, 42, true)).toBeNull()
    expect(confine(ROOT, ['note.md'], true)).toBeNull()
  })

  it('requireMd demands a .md extension', () => {
    expect(confine(ROOT, 'note.txt', true)).toBeNull()
    expect(confine(ROOT, 'note', true)).toBeNull()
    expect(confine(ROOT, 'note.txt', false)).toBe(join(ROOT, 'note.txt'))
    expect(confine(ROOT, 'folder', false)).toBe(join(ROOT, 'folder'))
  })

  it('rejects the vault root itself (must resolve strictly inside)', () => {
    expect(confine(ROOT, '.', false)).toBeNull()
    expect(confine(ROOT, '', false)).toBeNull()
  })
})
