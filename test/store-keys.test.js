// Pins the store:get/store:set authorization decision (TOME-004). Before
// this, store:get/store:set (open pre-login for the lock screen — see
// index.js's OPEN_CHANNELS) accepted any well-shaped key at any lock state:
// a compromised or pre-auth renderer could store:set 'airgap-repo-consents'
// — the SAME userData filename airgap.js's saveRepoConsents() owns — and
// forge egress consent for main to load on next boot, or read/write chat
// transcripts (chat-log-*) and policy toggles (chat-provider, custom-agents,
// airgap-default, ...) before a single credential was checked. The tests
// below are exactly those refusals: a main-owned filename must never be
// nameable as a store key, and while locked only the lock screen's own key
// ('theme' — see LOCKSCREEN_STORE_KEYS) may be read or written.
import { describe, it, expect } from 'vitest'
import {
  RESERVED_KEYS,
  LOCKSCREEN_STORE_KEYS,
  isReservedKey,
  isValidStoreKey,
  isStoreKeyAllowed,
} from '../src/main/lib/store-keys.js'

describe('isReservedKey()', () => {
  it('rejects every main-owned userData filename', () => {
    // airgap.json (egress allowlist), airgap-auth.json (credentials),
    // airgap-repo-consents.json (repo egress consent), events.jsonl (the
    // persistent event log) — none of these are store values.
    for (const key of ['airgap', 'airgap-auth', 'airgap-repo-consents', 'events'])
      expect(isReservedKey(key)).toBe(true)
  })

  it('does not reserve an ordinary key', () => {
    expect(isReservedKey('theme')).toBe(false)
    expect(isReservedKey('workspaces')).toBe(false)
    expect(isReservedKey('chat-log-abc123')).toBe(false)
  })

  it('rejects non-string input without throwing', () => {
    expect(isReservedKey(null)).toBe(false)
    expect(isReservedKey(undefined)).toBe(false)
    expect(isReservedKey(42)).toBe(false)
  })
})

describe('isValidStoreKey()', () => {
  it('accepts plain slugs', () => {
    expect(isValidStoreKey('theme')).toBe(true)
    expect(isValidStoreKey('chat-log-abc123')).toBe(true)
    expect(isValidStoreKey('a')).toBe(true)
    expect(isValidStoreKey('9lives')).toBe(true)
  })

  it('rejects reserved keys even though they are shape-valid', () => {
    for (const key of RESERVED_KEYS) expect(isValidStoreKey(key)).toBe(false)
  })

  it('rejects traversal and non-slug characters', () => {
    expect(isValidStoreKey('../airgap-auth')).toBe(false)
    expect(isValidStoreKey('a/b')).toBe(false)
    expect(isValidStoreKey('a.json')).toBe(false)
    expect(isValidStoreKey('UPPER')).toBe(false)
    expect(isValidStoreKey('-leading-dash')).toBe(false)
    expect(isValidStoreKey('')).toBe(false)
  })

  it('rejects non-string input without throwing', () => {
    expect(isValidStoreKey(null)).toBe(false)
    expect(isValidStoreKey(undefined)).toBe(false)
    expect(isValidStoreKey(123)).toBe(false)
  })
})

describe('isStoreKeyAllowed()', () => {
  it('rejects airgap-repo-consents at any lock state (the forgery this closes)', () => {
    // A pre-auth store:set on this exact key is what let an unauthenticated
    // renderer write the file airgap.js's loadRepoConsents() reads on next
    // boot — forged egress consent without ever logging in.
    expect(isStoreKeyAllowed('airgap-repo-consents', { locked: true })).toBe(false)
    expect(isStoreKeyAllowed('airgap-repo-consents', { locked: false })).toBe(false)
  })

  it('rejects airgap-auth at any lock state', () => {
    expect(isStoreKeyAllowed('airgap-auth', { locked: true })).toBe(false)
    expect(isStoreKeyAllowed('airgap-auth', { locked: false })).toBe(false)
  })

  it('denies a chat transcript key while locked', () => {
    expect(isStoreKeyAllowed('chat-log-x', { locked: true })).toBe(false)
  })

  it('allows a chat transcript key once unlocked', () => {
    expect(isStoreKeyAllowed('chat-log-x', { locked: false })).toBe(true)
  })

  it('denies policy keys while locked', () => {
    for (const key of [
      'airgap-default',
      'conductor-run',
      'chat-provider',
      'chat-model',
      'custom-agents',
      'core-vault',
      'onboarded-v1',
    ])
      expect(isStoreKeyAllowed(key, { locked: true })).toBe(false)
  })

  it("allows 'theme' while locked (the lock screen's own key)", () => {
    expect(isStoreKeyAllowed('theme', { locked: true })).toBe(true)
  })

  it('allows a normal key once unlocked', () => {
    expect(isStoreKeyAllowed('workspaces', { locked: false })).toBe(true)
  })

  it('LOCKSCREEN_STORE_KEYS stays minimal — every member must independently pass while locked', () => {
    for (const key of LOCKSCREEN_STORE_KEYS) expect(isStoreKeyAllowed(key, { locked: true })).toBe(true)
  })

  it('treats a missing options object as unlocked=false, not a throw', () => {
    expect(isStoreKeyAllowed('theme')).toBe(true)
  })
})
