import { describe, it, expect } from 'vitest'
import { GROUPS, normalize, filterRows, sectionToGroup } from '../src/renderer/settings-nav.js'

// A tiny stand-in for the index preferencesModal builds at render: entries
// carry a group, a section, and the row/heading text the query matches on.
const ENTRIES = [
  { groupId: 'general', sectionId: 'appearance', text: 'Appearance — Theme Match system' },
  { groupId: 'general', sectionId: 'terminal', text: 'Terminal — Font size 10–28' },
  { groupId: 'assistant', sectionId: 'assistant', text: 'Assistant — Provider GLM (Z.ai)' },
  { groupId: 'assistant', sectionId: 'assistant', text: 'Assistant — Custom row — model id deepseek-v4-pro' },
  { groupId: 'security', sectionId: 'security', text: 'Security — Two-factor authentication (2FA)' },
]

describe('GROUPS', () => {
  it('is the seven settings groups in rail order', () => {
    expect(GROUPS.map((g) => g.id)).toEqual([
      'general',
      'assistant',
      'agents',
      'security',
      'integrations',
      'voice',
      'mentor',
    ])
  })

  it('gives every group a non-empty label and section list', () => {
    for (const g of GROUPS) {
      expect(typeof g.label).toBe('string')
      expect(g.label.length).toBeGreaterThan(0)
      expect(g.sections.length).toBeGreaterThan(0)
    }
  })
})

describe('normalize', () => {
  it('lowercases, collapses whitespace runs, and trims', () => {
    expect(normalize('  Two-Factor\n  Authentication ')).toBe('two-factor authentication')
  })
})

describe('filterRows', () => {
  it('matches everything on an empty query', () => {
    const r = filterRows('', ENTRIES)
    expect(r.count).toBe(4)
    expect(r.sections).toEqual(new Set(ENTRIES.map((e) => e.sectionId)))
    expect(r.groups).toEqual(new Set(['general', 'assistant', 'security']))
  })

  it('matches everything on a whitespace-only query', () => {
    const r = filterRows('   \t ', ENTRIES)
    expect(r.count).toBe(4)
    expect(r.sections.has('appearance')).toBe(true)
  })

  it('matches case-insensitively on substrings', () => {
    const r = filterRows('FONT SIZE', ENTRIES)
    expect(r.sections).toEqual(new Set(['terminal']))
    expect(r.groups).toEqual(new Set(['general']))
  })

  it('finds text that only differs by case and spacing', () => {
    expect(filterRows('AUTHENTICATION', ENTRIES).sections).toEqual(new Set(['security']))
    expect(filterRows('2fa', ENTRIES).sections).toEqual(new Set(['security']))
  })

  it('does not light sibling sections of the same group on a hit in one of them', () => {
    // The provider rows share the assistant section; only one of them
    // mentions GLM.
    const r = filterRows('glm', ENTRIES)
    expect(r.sections).toEqual(new Set(['assistant']))
    expect(r.groups).toEqual(new Set(['assistant']))
  })

  it('omits groups with zero matches from the groups set', () => {
    const r = filterRows('deepseek', ENTRIES)
    expect(r.groups).toEqual(new Set(['assistant']))
    expect(r.groups.has('general')).toBe(false)
    expect(r.groups.has('security')).toBe(false)
  })

  it('counts matching sections, not matching entries', () => {
    // Two entries for the same section plus one for another → count 2.
    const r = filterRows('a', [
      { groupId: 'general', sectionId: 'appearance', text: 'alpha' },
      { groupId: 'general', sectionId: 'appearance', text: 'beta-a' },
      { groupId: 'general', sectionId: 'terminal', text: 'nope' },
      { groupId: 'security', sectionId: 'security', text: 'gamma' },
    ])
    expect(r.sections).toEqual(new Set(['appearance', 'security']))
    expect(r.count).toBe(2)
  })

  it('returns empty sets for a query that matches nothing', () => {
    const r = filterRows('zzz-nothing', ENTRIES)
    expect(r.count).toBe(0)
    expect(r.sections.size).toBe(0)
    expect(r.groups.size).toBe(0)
  })
})

describe('sectionToGroup', () => {
  it('maps every known section to its group', () => {
    for (const g of GROUPS) for (const s of g.sections) expect(sectionToGroup(s)).toBe(g.id)
  })

  it('returns null for unknown ids without crashing', () => {
    expect(sectionToGroup('nope')).toBe(null)
    expect(sectionToGroup('')).toBe(null)
    expect(sectionToGroup(null)).toBe(null)
    expect(sectionToGroup(undefined)).toBe(null)
  })
})
