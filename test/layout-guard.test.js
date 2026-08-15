import { describe, it, expect } from 'vitest'
import { isValidSavedLayout } from '../src/shared/layout.js'

// Regression guard: restoreLayout used to check `Array.isArray(saved.panels)`,
// but dockview.toJSON() serializes panels as an object — so restore silently
// no-op'd on every boot and panes never came back after a restart.
describe('isValidSavedLayout', () => {
  it('accepts the dockview shape: panels as a non-empty object', () => {
    expect(isValidSavedLayout({ panels: { 'pty-1': { id: 'pty-1' } } })).toBe(true)
  })

  it('rejects a panels array (the old buggy assumption)', () => {
    expect(isValidSavedLayout({ panels: [{ id: 'pty-1' }] })).toBe(false)
  })

  it('rejects an empty panels object', () => {
    expect(isValidSavedLayout({ panels: {} })).toBe(false)
  })

  it('rejects null / undefined / non-objects', () => {
    expect(isValidSavedLayout(null)).toBe(false)
    expect(isValidSavedLayout(undefined)).toBe(false)
    expect(isValidSavedLayout({})).toBe(false)
    expect(isValidSavedLayout('x')).toBe(false)
  })
})
