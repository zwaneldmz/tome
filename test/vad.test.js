// Pure-logic tests for the voice activity detector — no DOM, no audio
// hardware: chunks are synthesized Float32Arrays of a given RMS.
import { describe, it, expect } from 'vitest'
import { makeVad, VAD_DEFAULTS } from '../src/shared/vad.js'

const SR = 16000
const chunk = (ms, rms) => new Float32Array((ms / 1000) * SR).fill(rms)

function recorder(opts) {
  const events = []
  const vad = makeVad({
    onSpeechStart: () => events.push('start'),
    onSpeechEnd: () => events.push('end'),
    ...opts,
  })
  return { vad, events }
}

describe('makeVad', () => {
  it('stays silent under quiet chunks', () => {
    const { vad, events } = recorder()
    for (let i = 0; i < 20; i++) vad.push(chunk(100, 0.001))
    expect(events).toEqual([])
    expect(vad.speaking).toBe(false)
  })

  it('fires start then end around a loud stretch followed by silence', () => {
    const { vad, events } = recorder()
    for (let i = 0; i < 10; i++) vad.push(chunk(100, 0.05)) // 1 s of speech
    expect(events).toEqual(['start'])
    expect(vad.speaking).toBe(true)
    for (let i = 0; i < 9; i++) vad.push(chunk(100, 0.001)) // 900 ms silence
    expect(events).toEqual(['start', 'end'])
    expect(vad.speaking).toBe(false)
  })

  it('does not endpoint before the silence budget elapses', () => {
    const { vad, events } = recorder()
    vad.push(chunk(500, 0.05))
    for (let i = 0; i < 8; i++) vad.push(chunk(100, 0.001)) // 800 ms — under 900
    expect(events).toEqual(['start'])
    expect(vad.speaking).toBe(true)
    vad.push(chunk(100, 0.001)) // 900 ms — budget hit
    expect(events).toEqual(['start', 'end'])
  })

  it('resets the silence budget when speech resumes mid-pause', () => {
    const { vad, events } = recorder()
    vad.push(chunk(500, 0.05))
    for (let i = 0; i < 8; i++) vad.push(chunk(100, 0.001))
    vad.push(chunk(500, 0.05)) // speech resumes — budget restarts
    for (let i = 0; i < 8; i++) vad.push(chunk(100, 0.001))
    expect(events).toEqual(['start'])
    vad.push(chunk(100, 0.001))
    expect(events).toEqual(['start', 'end'])
  })

  it('ignores utterances shorter than minSpeechMs', () => {
    const { vad, events } = recorder()
    vad.push(chunk(200, 0.05)) // 200 ms — a click, under the 250 ms floor
    expect(events).toEqual(['start'])
    for (let i = 0; i < 9; i++) vad.push(chunk(100, 0.001))
    expect(events).toEqual(['start']) // end was suppressed
  })

  it('debounces onset: a single loud frame below the hangover is not speech', () => {
    const { vad, events } = recorder()
    vad.push(chunk(50, 0.05)) // 50 ms — under the 120 ms hangover
    vad.push(chunk(200, 0.001))
    expect(events).toEqual([])
    expect(vad.speaking).toBe(false)
  })

  it('hard-caps a runaway utterance at maxMs', () => {
    const { vad, events } = recorder({ maxMs: 2000 })
    for (let i = 0; i < 20; i++) vad.push(chunk(100, 0.05)) // 2 s of nonstop noise
    expect(events).toEqual(['start', 'end'])
    expect(vad.speaking).toBe(false)
  })

  it('re-arms for the next utterance after an endpoint', () => {
    const { vad, events } = recorder()
    vad.push(chunk(500, 0.05))
    for (let i = 0; i < 9; i++) vad.push(chunk(100, 0.001))
    vad.push(chunk(500, 0.05))
    expect(events).toEqual(['start', 'end', 'start'])
  })

  it('reset() drops all state mid-utterance', () => {
    const { vad, events } = recorder()
    vad.push(chunk(500, 0.05))
    expect(vad.speaking).toBe(true)
    vad.reset()
    expect(vad.speaking).toBe(false)
    for (let i = 0; i < 20; i++) vad.push(chunk(100, 0.001))
    expect(events).toEqual(['start']) // no end for the abandoned utterance
  })

  it('honours custom thresholds', () => {
    const { vad, events } = recorder({ threshold: 0.2 })
    vad.push(chunk(500, 0.05)) // loud by default, quiet under 0.2
    expect(events).toEqual([])
    vad.push(chunk(500, 0.3))
    expect(events).toEqual(['start'])
  })

  it('exposes sane defaults', () => {
    expect(VAD_DEFAULTS.silenceMs).toBe(900)
    expect(VAD_DEFAULTS.maxMs).toBe(60_000)
  })
})
