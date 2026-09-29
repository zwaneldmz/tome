import { describe, it, expect } from 'vitest'
import { newActivity, noteOutput, noteReport, noteInput, markSeen, sessionStatus, attentionReason, ringsBell, QUIET_MS } from '../src/shared/session-status.js'

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

describe('agent-reported state (Claude Code hooks)', () => {
  it('a report outranks the timing guess: long silent thinking stays working', () => {
    const a = newActivity()
    noteOutput(a, 0, 'x')
    expect(noteReport(a, 'working')).toBe(true)
    expect(sessionStatus(a, 60_000)).toBe('working')
  })

  it('done asks for you only until you look', () => {
    const a = newActivity()
    noteReport(a, 'done')
    expect(sessionStatus(a, 0)).toBe('attention')
    expect(attentionReason(a)).toBe('finished')
    markSeen(a)
    expect(sessionStatus(a, 0)).toBe('idle')
  })

  it('waiting (blocked on a prompt) keeps asking everywhere but the pane in front of you', () => {
    const a = newActivity()
    noteReport(a, 'waiting')
    markSeen(a)
    expect(sessionStatus(a, 0, true)).toBe('idle')
    expect(sessionStatus(a, 0, false)).toBe('attention')
    expect(attentionReason(a)).toBe('needs you')
  })

  it('answering a waiting agent moves it back to working; Esc / Ctrl-C drop to inference', () => {
    const a = newActivity()
    noteReport(a, 'waiting')
    noteInput(a, '1')
    expect(a.reported).toBe('working')
    noteInput(a, '\x1b')
    expect(a.reported).toBe(null)
    noteReport(a, 'working')
    noteInput(a, '\x03')
    expect(a.reported).toBe(null)
  })

  it('arrow keys are not an interrupt', () => {
    const a = newActivity()
    noteReport(a, 'working')
    noteInput(a, '\x1b[A')
    expect(a.reported).toBe('working')
  })

  it('anything outside the closed vocabulary is ignored', () => {
    const a = newActivity()
    for (const bad of ['', 'WORKING', 'unlock', 'working;x', 'done ']) expect(noteReport(a, bad)).toBe(false)
    expect(a.reported).toBe(null)
  })

  it('exited still wins over a report', () => {
    const a = newActivity()
    noteReport(a, 'working')
    a.exited = true
    expect(sessionStatus(a, 0)).toBe('exited')
  })
})
