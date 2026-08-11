// Pins the two policy decisions lsp.js delegates here (TOME-003): which
// workspace root a renderer-supplied path is allowed to resolve to, and what
// environment a language server gets launched with. Both used to be more
// permissive — rootFor() fell back to the opened file's own directory when
// it wasn't inside any open folder, and that same root's node_modules/.bin
// was prepended to the spawned server's PATH — so a compromised renderer (or
// a prompt-injected path from the conductor) could point an LSP process, and
// the binary that ran in its place, at a directory of its choosing. The
// tests that matter are exactly the refusals: an out-of-root path must never
// resolve to a root, and no root can ever make it into the spawn PATH.
import { describe, it, expect } from 'vitest'
import { join, sep } from 'node:path'
import { confineToRoot, resolveServerEnv } from '../src/main/lib/lsp-policy.js'

const WS = join(sep, 'workspace', 'proj')
const OTHER_WS = join(sep, 'workspace', 'other')

describe('confineToRoot()', () => {
  it('accepts a path inside the open folder, returning that folder as root', () => {
    expect(confineToRoot(join(WS, 'src', 'index.ts'), [WS])).toBe(WS)
    expect(confineToRoot(WS, [WS])).toBe(WS) // the folder itself
  })

  it('rejects a path outside every open folder', () => {
    expect(confineToRoot(join(sep, 'etc', 'passwd'), [WS])).toBeNull()
    expect(confineToRoot(join(sep, 'Users', 'evil', 'file.ts'), [WS])).toBeNull()
  })

  it('rejects when no folders are open at all', () => {
    expect(confineToRoot(join(WS, 'a.ts'), [])).toBeNull()
    expect(confineToRoot(join(WS, 'a.ts'), undefined)).toBeNull()
    expect(confineToRoot(join(WS, 'a.ts'), null)).toBeNull()
  })

  it("does not fall back to the opened file's own directory (the bug this closes)", () => {
    // The old rootFor() returned dirname(path) for anything unmatched, which
    // rooted a language server — and, via the old PATH prefix, ran a binary
    // — at a directory a compromised renderer chose outright. confineToRoot
    // must refuse instead of picking a substitute root.
    const outside = join(sep, 'tmp', 'evil-project', 'file.ts')
    expect(confineToRoot(outside, [WS])).toBeNull()
  })

  it('picks the most specific matching folder when open folders nest', () => {
    const nested = join(WS, 'packages', 'sub')
    const file = join(nested, 'index.ts')
    expect(confineToRoot(file, [WS, nested])).toBe(nested)
  })

  it('does not treat a same-prefix sibling folder as inside the workspace', () => {
    // "/workspace/proj-evil" must not pass for open folder "/workspace/proj".
    const sibling = `${WS}-evil`
    expect(confineToRoot(join(sibling, 'file.ts'), [WS])).toBeNull()
  })

  it('ignores folders that are not the one the path is actually under', () => {
    expect(confineToRoot(join(WS, 'a.ts'), [OTHER_WS])).toBeNull()
    expect(confineToRoot(join(WS, 'a.ts'), [OTHER_WS, WS])).toBe(WS)
  })

  it('rejects non-string, empty, or missing paths without throwing', () => {
    expect(confineToRoot(null, [WS])).toBeNull()
    expect(confineToRoot(undefined, [WS])).toBeNull()
    expect(confineToRoot('', [WS])).toBeNull()
    expect(confineToRoot(42, [WS])).toBeNull()
  })
})

describe('resolveServerEnv()', () => {
  const baseEnv = { PATH: '/usr/bin:/bin', HOME: '/Users/tester' }

  it('returns the base environment untouched', () => {
    expect(resolveServerEnv(WS, baseEnv)).toEqual(baseEnv)
    expect(resolveServerEnv(WS, baseEnv).PATH).toBe(baseEnv.PATH)
  })

  it("never prepends the workspace root's node_modules/.bin (the vulnerability this closes)", () => {
    // The removed behaviour built `${root}/node_modules/.bin:${PATH}` — a
    // compromised renderer could point `root` at a directory holding its own
    // "typescript-language-server" and have that run in the real server's
    // place. Pin that no root, however chosen, makes it back into PATH.
    for (const root of [WS, OTHER_WS, join(sep, 'tmp', 'evil-project')]) {
      const env = resolveServerEnv(root, baseEnv)
      expect(env.PATH).toBe(baseEnv.PATH)
      expect(env.PATH).not.toContain('node_modules')
      expect(env.PATH).not.toContain(root)
    }
  })

  it('defaults to process.env when no base environment is given', () => {
    expect(resolveServerEnv(WS).PATH).toBe(process.env.PATH)
  })

  it('returns a copy, not the same reference as the base environment', () => {
    expect(resolveServerEnv(WS, baseEnv)).not.toBe(baseEnv)
  })
})
