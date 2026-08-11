// Pins isAppUrl() (TOME-006): the allowlist behind main's will-navigate and
// will-redirect handlers, which used to not exist at all — a renderer-driven
// top-level navigation or a server redirect had no application-owned policy,
// just whatever Electron defaults to. The cases below are the acceptance
// test from the council report verbatim: deny https, foreign file, foreign
// tome, and redirect targets; allow only the packaged renderer and popout.
import { describe, it, expect } from 'vitest'
import { join, sep } from 'node:path'
import { pathToFileURL } from 'node:url'
import { isAppUrl } from '../src/main/lib/app-url.js'

const RENDERER_DIR = join(sep, 'app', 'out', 'renderer')
const INDEX_URL = pathToFileURL(join(RENDERER_DIR, 'index.html')).href
const POPOUT_URL = pathToFileURL(join(RENDERER_DIR, 'popout.html')).href

describe('isAppUrl() — packaged (no devOrigin)', () => {
  it('allows the packaged main renderer file', () => {
    expect(isAppUrl(INDEX_URL, { rendererDir: RENDERER_DIR })).toBe(true)
  })

  it('allows the packaged, validated popout file', () => {
    expect(isAppUrl(POPOUT_URL, { rendererDir: RENDERER_DIR })).toBe(true)
  })

  it('denies an https target', () => {
    expect(isAppUrl('https://evil.example.com/index.html', { rendererDir: RENDERER_DIR })).toBe(false)
  })

  it('denies a foreign file: target, including a same-named file elsewhere on disk', () => {
    // Same basename, wrong directory — a basename-only check would wrongly
    // allow this. The comparison must be the full resolved path.
    const foreign = pathToFileURL(join(sep, 'tmp', 'evil', 'popout.html')).href
    expect(isAppUrl(foreign, { rendererDir: RENDERER_DIR })).toBe(false)
    expect(isAppUrl(pathToFileURL(join(sep, 'etc', 'passwd')).href, { rendererDir: RENDERER_DIR })).toBe(
      false,
    )
  })

  it('denies a foreign tome: target', () => {
    // tome: is registered for embedding sub-resources (img/iframe), never a
    // valid top-level navigation target.
    expect(isAppUrl('tome://x?p=%2Fetc%2Fpasswd', { rendererDir: RENDERER_DIR })).toBe(false)
  })

  it('denies a redirect-style target the same way (will-redirect reuses this check)', () => {
    expect(isAppUrl('https://attacker.example.com/collect', { rendererDir: RENDERER_DIR })).toBe(false)
  })

  it('denies a malformed URL without throwing', () => {
    expect(isAppUrl('not a url', { rendererDir: RENDERER_DIR })).toBe(false)
    expect(isAppUrl('', { rendererDir: RENDERER_DIR })).toBe(false)
  })
})

describe('isAppUrl() — development (devOrigin set)', () => {
  const devOrigin = 'http://localhost:5173'

  it('allows any path on the dev server origin', () => {
    expect(isAppUrl('http://localhost:5173/popout.html', { devOrigin, rendererDir: RENDERER_DIR })).toBe(
      true,
    )
    expect(isAppUrl('http://localhost:5173/', { devOrigin, rendererDir: RENDERER_DIR })).toBe(true)
  })

  it('denies a different host or port', () => {
    expect(isAppUrl('http://localhost:9999/popout.html', { devOrigin, rendererDir: RENDERER_DIR })).toBe(
      false,
    )
    expect(isAppUrl('http://evil.example.com:5173/', { devOrigin, rendererDir: RENDERER_DIR })).toBe(false)
  })

  it('denies a protocol mismatch on the same host', () => {
    expect(isAppUrl('https://localhost:5173/', { devOrigin, rendererDir: RENDERER_DIR })).toBe(false)
  })

  it('ignores the packaged file check entirely once a devOrigin is set', () => {
    expect(isAppUrl(INDEX_URL, { devOrigin, rendererDir: RENDERER_DIR })).toBe(false)
  })
})
