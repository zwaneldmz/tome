import { describe, it, expect } from 'vitest'
import { newActivity, noteOutput, markSeen, sessionStatus, ringsBell, QUIET_MS } from '../src/shared/session-status.js'

describe('session status', () => {
  it('walks working → attention → idle for a real burst you did not watch', () => {
    const a = newActivity()
    for (let t = 0; t <= 3000; t += 200) noteOutput(a, t, 'x')
    expect(sessionStatus(a, 3100)).toBe('working')
    expect(sessionStatus(a, 3000 + QUIET_MS + 1)).toBe('attention')
    markSeen(a)
    expect(sessionStatus(a, 9000)).toBe('idle')
  })

  it('a short blip (echo, prompt redraw) never asks for attention', () => {
    const a = newActivity()
    noteOutput(a, 0, 'x')
    expect(sessionStatus(a, QUIET_MS + 10)).toBe('idle')
  })

  it('a bell asks for attention at once; OSC terminators are not bells', () => {
    expect(ringsBell('\x1b]0;title\x07')).toBe(false)
    expect(ringsBell('done\x07')).toBe(true)
    const a = newActivity()
    noteOutput(a, 0, '\x07')
    expect(sessionStatus(a, 1)).toBe('attention')
  })

  it('exited wins over everything', () => {
    const a = newActivity()
    noteOutput(a, 0, 'x')
    a.exited = true
    expect(sessionStatus(a, 1)).toBe('exited')
  })
})
