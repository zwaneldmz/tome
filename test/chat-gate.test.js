// Pure predicates for the no-default consent gate (launch hardening P3.1):
// what the chat header shows for each provider state, and the one state —
// never-picked — where a send must not happen at all. DOM-free, like
// chat-lifecycle.test.js; no jsdom is set up in this repo.
import { describe, it, expect } from 'vitest'
import { providerLineText, needsProviderPick } from '../src/renderer/chat-gate.js'

const ready = {
  providers: [],
  active: 'kimi',
  effective: {
    id: 'kimi',
    label: 'Kimi (Moonshot)',
    model: 'kimi-k3',
    host: 'https://api.moonshot.ai/v1',
    keyOrigin: { kind: 'file' },
  },
  reason: null,
  none: false,
}

describe('needsProviderPick', () => {
  it('is true exactly for the never-picked initial state', () => {
    expect(needsProviderPick({ providers: [], active: null, effective: null, reason: 'No provider — pick one.', none: true })).toBe(true)
  })

  it('is false when a pick exists — keyless rows are a key problem, not a pick problem', () => {
    expect(needsProviderPick({ ...ready, effective: null, reason: 'GLM (Z.ai) needs a key — paste one in ⌘, → Assistant.', none: false })).toBe(false)
  })

  it('is false when the provider is ready', () => {
    expect(needsProviderPick(ready)).toBe(false)
  })

  it('is false when the providers read failed (null) — the backend still refuses, the gate is UX', () => {
    expect(needsProviderPick(null)).toBe(false)
    expect(needsProviderPick(undefined)).toBe(false)
  })
})

describe('providerLineText', () => {
  it('shows label · model · host for a ready provider', () => {
    expect(providerLineText(ready)).toBe('Kimi (Moonshot) · kimi-k3 · https://api.moonshot.ai/v1')
  })

  it('names the none state with the P3.1 wording, not a reason string', () => {
    expect(providerLineText({ providers: [], active: null, effective: null, reason: 'whatever the banner says', none: true })).toBe('No provider — pick one')
  })

  it('falls back to the backend reason for a picked-but-keyless row', () => {
    expect(providerLineText({ ...ready, effective: null, reason: 'GLM (Z.ai) needs a key — paste one in ⌘, → Assistant.', none: false })).toBe('GLM (Z.ai) needs a key — paste one in ⌘, → Assistant.')
  })

  it('falls back to the picker hint when the read failed entirely', () => {
    expect(providerLineText(null)).toBe('No provider — pick one in ⌘,')
  })
})
