// spawnPolicy — the ＋ menu's spawn rules as a pure predicate (no jsdom in
// this repo, so the DOM-building half of menus.js stays out of vitest).
import { describe, it, expect } from 'vitest'
import { spawnPolicy } from '../src/renderer/spawn-policy.js'

const defaults = { egressDefault: true, containmentOnly: false }

describe('spawnPolicy', () => {
  it('gaps agents by default and keeps the terminal + toggle when nothing is on', () => {
    expect(spawnPolicy(defaults)).toEqual({
      containmentOnly: false,
      agentsGapped: true,
      showEgressDefaultToggle: true,
      showUnsandboxedTerminal: true,
    })
  })

  it('treats a missing or off containment-only pref as off', () => {
    expect(spawnPolicy({ egressDefault: false }).containmentOnly).toBe(false)
    expect(spawnPolicy({ ...defaults, containmentOnly: false }).containmentOnly).toBe(false)
  })

  it('containment-only removes the unsandboxed Terminal row — the one spawn that can never be gapped', () => {
    const p = spawnPolicy({ ...defaults, containmentOnly: true })
    expect(p.containmentOnly).toBe(true)
    expect(p.showUnsandboxedTerminal).toBe(false)
  })

  it('containment-only hides the egress-default toggle: it is a default, not a ceiling, and the ceiling has overridden it', () => {
    const p = spawnPolicy({ egressDefault: false, containmentOnly: true })
    expect(p.showEgressDefaultToggle).toBe(false)
  })

  it('containment-only forces agent panes gapped even when egress-default is off', () => {
    // egress-default off alone means "next agent spawns ungapped"; the
    // ceiling must win over the default.
    expect(spawnPolicy({ egressDefault: false, containmentOnly: true }).agentsGapped).toBe(true)
  })

  it('with the ceiling off, agents still follow the egress-default DEFAULT', () => {
    expect(spawnPolicy({ egressDefault: true, containmentOnly: false }).agentsGapped).toBe(true)
    expect(spawnPolicy({ egressDefault: false, containmentOnly: false }).agentsGapped).toBe(false)
    // And the unsandboxed Terminal row stays available either way.
    expect(spawnPolicy({ egressDefault: false }).showUnsandboxedTerminal).toBe(true)
  })
})
