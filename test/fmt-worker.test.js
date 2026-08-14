// Pins fmt-worker.js's PARSER_BY_EXT extension table and the
// self.onmessage request/response contract tome-ipc.js's formatInWorker()
// depends on. Drives handleFormatRequest directly rather than a real
// MessageEvent/Worker — see fmt-worker.js's own guard around
// `self.onmessage` for why the module is importable at all under vitest's
// plain-Node module resolution (no `self` global exists there).
import { describe, it, expect } from 'vitest'
import babel from 'prettier/plugins/babel'
import estree from 'prettier/plugins/estree'
import typescript from 'prettier/plugins/typescript'
import postcss from 'prettier/plugins/postcss'
import markdown from 'prettier/plugins/markdown'
import yaml from 'prettier/plugins/yaml'
import html from 'prettier/plugins/html'
import { PARSER_BY_EXT, inferParser, handleFormatRequest } from '../src/renderer/fmt-worker.js'

describe('PARSER_BY_EXT / inferParser', () => {
  const cases = [
    ['a.js', 'babel', [babel, estree]],
    ['a.jsx', 'babel', [babel, estree]],
    ['a.mjs', 'babel', [babel, estree]],
    ['a.cjs', 'babel', [babel, estree]],
    ['a.ts', 'typescript', [typescript, estree]],
    ['a.tsx', 'typescript', [typescript, estree]],
    ['a.mts', 'typescript', [typescript, estree]],
    ['a.cts', 'typescript', [typescript, estree]],
    ['a.json', 'json', [babel, estree]],
    ['a.jsonc', 'jsonc', [babel, estree]],
    ['a.json5', 'json5', [babel, estree]],
    ['a.css', 'css', [postcss]],
    ['a.scss', 'scss', [postcss]],
    ['a.less', 'less', [postcss]],
    ['a.md', 'markdown', [markdown]],
    ['a.markdown', 'markdown', [markdown]],
    ['a.yml', 'yaml', [yaml]],
    ['a.yaml', 'yaml', [yaml]],
    ['a.html', 'html', [html, postcss, babel, estree]],
    ['a.htm', 'html', [html, postcss, babel, estree]],
  ]

  it.each(cases)('%s -> parser %s with the expected plugin set', (path, parser, plugins) => {
    const info = inferParser(path)
    expect(info.parser).toBe(parser)
    expect(info.plugins).toEqual(plugins)
  })

  it('is case-insensitive on the extension', () => {
    expect(inferParser('Component.TSX').parser).toBe('typescript')
  })

  it("matches on the path's basename, ignoring dots in directory segments", () => {
    expect(inferParser('a.b.dir/file.ts').parser).toBe('typescript')
  })

  it('returns null for an unknown extension', () => {
    expect(inferParser('a.xyz')).toBeNull()
  })

  it('returns null for a path with no extension at all', () => {
    expect(inferParser('Makefile')).toBeNull()
  })
})

describe('handleFormatRequest', () => {
  it('formats real content and echoes the request id untouched', async () => {
    const res = await handleFormatRequest({ id: 7, path: 'a.js', content: 'const x=1' })
    expect(res).toEqual({ id: 7, value: 'const x = 1;\n' })
  })

  it('returns { value: null } for a file type with no registered parser, without throwing', async () => {
    const res = await handleFormatRequest({ id: 'x', path: 'a.bin', content: 'whatever' })
    expect(res).toEqual({ id: 'x', value: null })
  })

  it('returns a single-line { value: { error } } for a syntax error, never throwing', async () => {
    const res = await handleFormatRequest({ id: 3, path: 'a.ts', content: 'const x: = ;;;' })
    expect(res.id).toBe(3)
    expect(typeof res.value.error).toBe('string')
    expect(res.value.error.length).toBeGreaterThan(0)
    expect(res.value.error).not.toContain('\n')
  })

  it('formats css through the postcss plugin', async () => {
    const res = await handleFormatRequest({ id: 1, path: 'a.css', content: 'a{color:red}' })
    expect(res).toEqual({ id: 1, value: 'a {\n  color: red;\n}\n' })
  })

  it('formats yaml through the yaml plugin', async () => {
    const res = await handleFormatRequest({ id: 2, path: 'a.yaml', content: 'a:   1' })
    expect(res).toEqual({ id: 2, value: 'a: 1\n' })
  })

  it('formats markdown through the markdown plugin', async () => {
    const res = await handleFormatRequest({ id: 4, path: 'a.md', content: '#   Title' })
    expect(res).toEqual({ id: 4, value: '# Title\n' })
  })
})
