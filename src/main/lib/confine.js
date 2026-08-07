// Shared confinement for every note/folder path derived from renderer input:
// must stay a string resolving inside `root`, no leading slash, no `..`
// segment. `requireMd` additionally demands a .md extension (notes only —
// promote's core-vault folder argument is a directory, not a note).
// Extracted from brain.js so the guard is testable without module state.
import { resolve, sep } from 'node:path'

export function confine(root, rel, requireMd) {
  if (typeof rel !== 'string') return null
  if (requireMd && !rel.endsWith('.md')) return null
  if (rel.startsWith('/')) return null
  if (rel.split(/[\\/]/).includes('..')) return null
  const full = resolve(root, rel)
  if (!full.startsWith(root + sep)) return null
  return full
}
