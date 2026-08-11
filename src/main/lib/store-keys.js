// The store:get/store:set authorization decision (TOME-004), extracted so it
// is testable without an Electron main process — index.js is the only
// caller. store:get/store:set stay open pre-login for the lock screen (see
// index.js's OPEN_CHANNELS), which used to mean ANY well-shaped key was
// readable/writable before login: chat transcripts (chat-log-*), policy
// toggles (chat-provider, custom-agents, ...), even 'airgap-repo-consents' —
// the SAME userData directory main uses for its own egress-consent file, so
// an unauthenticated store:set on that key forged consent for main to load
// on next boot. Two things are enforced here now: which userData filenames
// are main's own and may never be named by a store key at all, and — while
// locked — which of the remaining keys the lock screen is actually allowed
// to touch.

// Every userData filename main itself writes outside the JSON store: the
// egress allowlist (airgap.json), the auth file (airgap-auth.json), the
// repo-consent file (airgap-repo-consents.json, airgap.js saveRepoConsents),
// and the persistent event log (events.jsonl, events.js). None may ever be
// named by a store key, at any lock state — a store:set on one of these
// would let the renderer overwrite a file main treats as its own.
export const RESERVED_KEYS = new Set(['airgap', 'airgap-auth', 'airgap-repo-consents', 'events'])

// The only store key any pre-auth UI actually reads: renderer.js runs
// theme.js's bootTheme() before lock.js's bootAuth() so the lock overlay
// paints in the right palette (see renderer.js's boot sequence). lock.js
// itself reads nothing from the store. Keep this to exactly what's
// empirically read before login — widening it re-opens whatever key gets
// added next (another transcript, another policy toggle).
export const LOCKSCREEN_STORE_KEYS = new Set(['theme'])

const KEY_SHAPE = /^[a-z0-9][a-z0-9-]*$/

export function isReservedKey(key) {
  return typeof key === 'string' && RESERVED_KEYS.has(key)
}

// Shape + reservation, independent of lock state: plain slugs only (no
// traversal), never one of main's own files. Mirrors index.js's former
// inline vetKey() check.
export function isValidStoreKey(key) {
  return typeof key === 'string' && KEY_SHAPE.test(key) && !RESERVED_KEYS.has(key)
}

// The full decision store:get/store:set apply: a key must be shape-valid and
// unreserved always, and — while locked — must also be one of the
// lock-screen keys above. `locked` mirrors index.js's isLockedNow() at the
// call site.
export function isStoreKeyAllowed(key, { locked } = {}) {
  if (!isValidStoreKey(key)) return false
  return !locked || LOCKSCREEN_STORE_KEYS.has(key)
}
