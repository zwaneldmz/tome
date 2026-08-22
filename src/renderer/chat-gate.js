// The no-default consent gate (launch hardening P3.1): pure functions over
// the `chat:providers` payload deciding what a chat pane does with its
// next send. DOM-free on purpose — this repo runs vitest with no jsdom
// (see test/chat-lifecycle.test.js), so the decision logic lives here and
// panels/chat.js stays thin.
//
// The two non-ready states the header must tell apart, from the backend's
// own fields: `none` is the explicit never-picked flag, `effective` the
// resolved provider. "No provider — pick one" is the initial state only —
// a picked-but-keyless row instead shows the backend's `reason` (a send
// then reaches the backend and fails there with the same words, which is
// correct: the user already chose, they need a key, not a picker).

export function providerLineText(info) {
  if (info?.effective) {
    const e = info.effective
    return `${e.label} · ${e.model} · ${e.host}`
  }
  if (info?.none) return 'No provider — pick one'
  return info?.reason || 'No provider — pick one in ⌘,'
}

// True exactly for the never-picked initial state — the one state where a
// send must not happen at all: the pane routes to the provider picker
// instead of calling chat.send. A failed providers() read (null) returns
// false and the send proceeds — the backend still refuses with the same
// reason, so the gate is UX, never the security boundary.
export function needsProviderPick(info) {
  return !!info && info.none === true && !info.effective
}
