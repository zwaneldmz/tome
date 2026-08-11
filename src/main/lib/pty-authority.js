// The two policy decisions createPty (index.js) must own instead of trusting
// the renderer for (TOME-001): whether a pane is actually gapped, and what
// directory it starts in. Both used to come straight from the renderer with
// no main-side check at all — `airgap: gapped` was passed through verbatim
// even while the stored 'airgap-default' preference wanted every pane gapped,
// and `cwd` went to pty.spawn unchanged, unlike every other renderer-supplied
// path in this app (isConfinedPath/confinedRealPath, confineToRoot, confine).
// Extracted so both decisions are testable without an Electron main process;
// index.js is the only caller of either.
import { resolve, sep } from 'node:path'
import { statSync } from 'node:fs'

// ---- gapping ----
// The renderer may ask for MORE isolation than policy requires (a per-pane
// "run this one gapped" toggle) but can never ask for less: when policy
// wants panes gapped by default, a renderer request of gapped:false is
// overridden, not honored. `policyDefault` is the caller's already-resolved
// "policy wants gapping by default" boolean — index.js computes it as
// `(await readStore('airgap-default')) !== false` (absent means gapped,
// the same "on unless explicitly turned off" reading the renderer's own
// onboarding/preferences UI uses for the same key).
export function resolveGapping(rendererGapped, policyDefault) {
  return !!rendererGapped || !!policyDefault
}

// ---- spawn cwd ----
// A pane's STARTING directory only — a shell is free to cd anywhere the
// moment it's running, so unlike isConfinedPath/confinedRealPath this is not
// a filesystem confinement boundary. What it closes is a compromised (or
// merely buggy) renderer handing pty.spawn an arbitrary cwd outright: a path
// that doesn't exist (spawn fails outright), or one with no relationship to
// the workspace at all. Accepted only when `cwd` names an existing directory
// inside one of the open workspace `roots` or inside the user's `home`
// subtree; anything else — wrong type, outside both, or just not there —
// falls back to `home`, the same default createPty already used for a
// missing cwd.
function isInside(abs, base) {
  return typeof base === 'string' && !!base && (abs === base || abs.startsWith(base + sep))
}
export function resolveSpawnCwd(cwd, roots, home) {
  if (typeof cwd !== 'string' || !cwd) return home
  const abs = resolve(cwd)
  const list = Array.isArray(roots) ? roots : []
  if (!list.some((r) => isInside(abs, r)) && !isInside(abs, home)) return home
  try {
    return statSync(abs).isDirectory() ? abs : home
  } catch {
    return home // doesn't exist, or unreadable — never hand pty.spawn a dead cwd
  }
}
