// Validates the string typed into the tree's "new file"/"new folder" prompt
// before it's joined onto a workspace root (`${activeRoot}/${input}`). This
// is UX validation only, not a security boundary — fs:mkdir/fs:createFile
// are user-driven by design (see the confinement comment in
// src/main/index.js) and take whatever path they're handed; catching '..'
// and friends here just keeps the prompt from producing a confusing
// ENOENT/EEXIST instead of a useful toast. Pure on purpose: no imports, so
// vitest can exercise it directly and tree.js can call it from the DOM.

export function validateRelPath(input) {
  if (typeof input !== 'string') return { ok: false, reason: 'enter a name' }
  const trimmed = input.trim().replace(/\/+$/, '')
  if (!trimmed) return { ok: false, reason: 'enter a name' }
  if (trimmed.startsWith('/')) return { ok: false, reason: 'path cannot be absolute' }
  if (trimmed.includes('\\')) {
    return { ok: false, reason: 'backslashes are not allowed — use "/" to separate folders' }
  }
  const segments = trimmed.split('/')
  for (const seg of segments) {
    if (seg === '..') return { ok: false, reason: 'path may not contain ".."' }
    if (seg === '' || seg === '.') return { ok: false, reason: `"${trimmed}" is not a valid path` }
  }
  return { ok: true, rel: segments.join('/') }
}
