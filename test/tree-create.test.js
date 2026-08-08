// Pins validateRelPath()'s rules for the tree's new-file/new-folder prompt.
// These are UX guardrails, not the security boundary (see the header comment
// in tree-create.js) — but loosening them means the prompt silently joins an
// absolute path or a '..' segment onto the workspace root instead of
// toasting the user first, so the exact rules are worth pinning.
import { describe, it, expect } from 'vitest'
import { validateRelPath } from '../src/renderer/tree-create.js'

describe('validateRelPath', () => {
  it('accepts a simple name', () => {
    expect(validateRelPath('file.txt')).toEqual({ ok: true, rel: 'file.txt' })
  })

  it('accepts a nested path unchanged', () => {
    expect(validateRelPath('src/util.js')).toEqual({ ok: true, rel: 'src/util.js' })
  })

  it('normalizes trailing slashes', () => {
    expect(validateRelPath('folder/')).toEqual({ ok: true, rel: 'folder' })
    expect(validateRelPath('src/sub///')).toEqual({ ok: true, rel: 'src/sub' })
  })

  it('trims surrounding whitespace', () => {
    expect(validateRelPath('  name.txt  ')).toEqual({ ok: true, rel: 'name.txt' })
  })

  it('rejects empty input', () => {
    expect(validateRelPath('')).toEqual({ ok: false, reason: 'enter a name' })
    expect(validateRelPath('   ')).toEqual({ ok: false, reason: 'enter a name' })
  })

  it('rejects absolute paths', () => {
    expect(validateRelPath('/abs')).toEqual({ ok: false, reason: 'path cannot be absolute' })
  })

  it('rejects any ".." segment', () => {
    expect(validateRelPath('a/../b')).toEqual({ ok: false, reason: 'path may not contain ".."' })
    expect(validateRelPath('..')).toEqual({ ok: false, reason: 'path may not contain ".."' })
  })

  it('rejects empty segments from doubled slashes', () => {
    expect(validateRelPath('a//b')).toEqual({ ok: false, reason: '"a//b" is not a valid path' })
  })

  it('rejects "." segments', () => {
    expect(validateRelPath('./x')).toEqual({ ok: false, reason: '"./x" is not a valid path' })
  })

  it('rejects backslashes', () => {
    expect(validateRelPath('back\\slash')).toEqual({
      ok: false,
      reason: 'backslashes are not allowed — use "/" to separate folders',
    })
  })
})
