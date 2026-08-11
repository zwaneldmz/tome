// Realpath-confinement for the ABSOLUTE managed paths flow-runner.js and
// flow-tools.js build themselves by joining a trusted root with segments
// that are already vetted (a generated run id, logName's sanitized string,
// the literal "run.json", a name badName/validateFlow already accepted) —
// as opposed to confine.js's confine(), which takes a root plus an
// UNTRUSTED renderer-supplied `rel` and has never seen the filesystem.
// Lexically joined paths are still not enough on their own: an ancestor
// directory anywhere in the existing part of the path (.tome, .tome/flows,
// a per-flow runs/ folder from an earlier run) can itself be a symlink, and
// every fs call that follows one silently operates outside `root`.
//
// Mirrors brain.js's confineReal exactly — "validate real, return lexical":
// mustExist:true realpath()s the target itself and checks containment;
// mustExist:false (a target that may not exist yet, e.g. a run directory
// about to be mkdir'd) walks up via dirname to the nearest EXISTING
// ancestor and checks that instead, which is the only part of the path a
// symlink could actually be. Either way the LEXICAL `full` comes back on
// success, never the realpath'd one — a symlinked tmp dir in a test (macOS's
// own /tmp -> /private/tmp) must not rewrite a path a caller compares byte
// for byte against a plain join(). Every failure, whatever the reason,
// returns null so a caller needs exactly one falsy check.
//
// Two copies of the same ~15 lines, not one generic core: flow-runner.js
// runs on fs/promises top to bottom, but flow-tools.js is deliberately
// synchronous (its own header comment — conductor.js calls it un-awaited),
// and threading a sync/async split through one function would cost more
// than the duplication does.
import { realpathSync, promises as fsp } from 'node:fs'
import { dirname, sep } from 'node:path'

// `full` must already be lexically and STRICTLY inside `root` (root itself
// does not count, same rule confine() applies) before any fs call is worth
// making — a caller that hands in something not even lexically confined is
// a bug, not a symlink, and gets the same null either way.
function lexicallyInside(root, full) {
  return typeof root === 'string' && !!root && typeof full === 'string' && full.startsWith(root + sep)
}

export async function confineRealAbs(root, full, { mustExist = true } = {}) {
  if (!lexicallyInside(root, full)) return null
  try {
    const realRoot = await fsp.realpath(root)
    if (mustExist) {
      const real = await fsp.realpath(full)
      return real.startsWith(realRoot + sep) ? full : null
    }
    let dir = dirname(full)
    for (;;) {
      try {
        const realDir = await fsp.realpath(dir)
        return realDir === realRoot || realDir.startsWith(realRoot + sep) ? full : null
      } catch {
        const parent = dirname(dir)
        if (parent === dir) return null // reached the filesystem root without finding one that exists
        dir = parent
      }
    }
  } catch {
    return null
  }
}

// Same contract, synchronous — for flow-tools.js only (see file header).
export function confineRealAbsSync(root, full, { mustExist = true } = {}) {
  if (!lexicallyInside(root, full)) return null
  try {
    const realRoot = realpathSync(root)
    if (mustExist) {
      const real = realpathSync(full)
      return real.startsWith(realRoot + sep) ? full : null
    }
    let dir = dirname(full)
    for (;;) {
      try {
        const realDir = realpathSync(dir)
        return realDir === realRoot || realDir.startsWith(realRoot + sep) ? full : null
      } catch {
        const parent = dirname(dir)
        if (parent === dir) return null
        dir = parent
      }
    }
  } catch {
    return null
  }
}
