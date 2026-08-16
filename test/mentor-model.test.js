// Pure scoring for the mentor-mode comprehension gate — no DOM, no IPC.
import { describe, it, expect } from 'vitest'
import { scoreGate, nextUq, AUTO_TYPES } from '../src/shared/mentor-model.js'

describe('scoreGate', () => {
  it('scores multiple-choice correct/incorrect against the canonical answer', () => {
    const qs = [
      { type: 'multiple_choice', answer: 'b' },
      { type: 'multiple_choice', answer: 'a' },
    ]
    expect(scoreGate(qs, { '0': 'b', '1': 'c' })).toEqual({ correct: 1, total: 2 })
  })

  it('scores true_false the same way', () => {
    const qs = [
      { type: 'true_false', answer: 'True' },
      { type: 'true_false', answer: 'False' },
    ]
    expect(scoreGate(qs, { '0': 'True', '1': 'True' })).toEqual({ correct: 1, total: 2 })
  })

  it('ignores non-auto-scorable types (short_answer, code) entirely', () => {
    const qs = [
      { type: 'multiple_choice', answer: 'x' },
      { type: 'short_answer', answer: 'anything' },
      { type: 'code', answer: 'return 42' },
    ]
    expect(scoreGate(qs, { '0': 'x', '1': 'wrong', '2': 'also wrong' })).toEqual({
      correct: 1,
      total: 1,
    })
  })

  it('accepts an array of answers as well as an object map', () => {
    const qs = [{ type: 'multiple_choice', answer: 'b' }]
    expect(scoreGate(qs, ['b'])).toEqual({ correct: 1, total: 1 })
  })

  it('treats a missing answer as incorrect, not an error', () => {
    const qs = [{ type: 'multiple_choice', answer: 'b' }]
    expect(scoreGate(qs, {})).toEqual({ correct: 0, total: 1 })
  })

  it('returns zero totals for an empty question list', () => {
    expect(scoreGate(undefined, {})).toEqual({ correct: 0, total: 0 })
    expect(scoreGate([], {})).toEqual({ correct: 0, total: 0 })
  })
})

describe('nextUq', () => {
  it('adds a proportional 0..20 step for a perfect gate', () => {
    expect(nextUq(0, { correct: 2, total: 2 })).toBe(20)
    expect(nextUq(40, { correct: 1, total: 1 })).toBe(60)
  })

  it('adds a partial step for a partial gate', () => {
    expect(nextUq(0, { correct: 1, total: 2 })).toBe(10)
  })

  it('clamps at 100', () => {
    expect(nextUq(95, { correct: 2, total: 2 })).toBe(100)
  })

  it('never goes below 0 (a poorly answered gate does not subtract)', () => {
    expect(nextUq(5, { correct: 0, total: 2 })).toBe(5)
  })

  it('leaves the score unchanged when there is nothing to auto-score', () => {
    expect(nextUq(42, { correct: 0, total: 0 })).toBe(42)
  })

  it('treats a missing previous score as 0', () => {
    expect(nextUq(undefined, { correct: 1, total: 1 })).toBe(20)
  })
})

describe('AUTO_TYPES', () => {
  it('lists exactly the auto-scorable kinds', () => {
    expect(AUTO_TYPES).toEqual(['multiple_choice', 'true_false'])
  })
})
