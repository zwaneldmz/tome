// Pure-logic tests for the TTS sentence chunker — no DOM, no
// speechSynthesis: strings in, strings out.
import { describe, it, expect } from 'vitest'
import { nextSpeakChunk, MIN_SPEAK_CHARS } from '../src/shared/tts.js'

describe('nextSpeakChunk', () => {
  it('returns a single long sentence in full', () => {
    const text = 'This is a single sentence that is definitely longer than twenty four characters.'
    expect(nextSpeakChunk(text)).toBe(text)
  })

  it('rides a short first sentence along with the following sentence', () => {
    const text = 'Yes. Here is a sentence that is long enough to clear the minimum.'
    expect(nextSpeakChunk(text)).toBe(text)
  })

  it('returns an empty string when no sentence terminator exists', () => {
    expect(nextSpeakChunk('no punctuation here just words')).toBe('')
  })

  it('returns only the first sentence of a multi-sentence tail (non-greedy)', () => {
    const first = 'This first sentence is long enough to stand alone.'
    const rest = ' Then a second sentence that should not be included yet.'
    expect(nextSpeakChunk(first + rest)).toBe(first)
  })

  it('returns an empty string when even two short sentences stay short', () => {
    expect(nextSpeakChunk('Yes. No.')).toBe('')
  })

  it('honours a custom minimum', () => {
    expect(nextSpeakChunk('Yes. No.', 100)).toBe('')
    expect(nextSpeakChunk('Yes. No.', 4)).toBe('Yes.')
    expect(nextSpeakChunk('Yes. No.', 5)).toBe('Yes. No.')
  })

  it('exposes the default minimum', () => {
    expect(MIN_SPEAK_CHARS).toBe(24)
  })
})
