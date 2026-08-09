// Pins the WAV encoder whisper.cpp's input rides on: exact header bytes,
// little-endian int16 scaling, and the clamp that keeps an overshooting
// sample from wrapping into a full-scale click.
import { describe, it, expect } from 'vitest'
import { encodeWav } from '../src/shared/wav.js'

const bytes = (buf, off, len) => new Uint8Array(buf, off, len)
const tag = (buf, off) => String.fromCharCode(...bytes(buf, off, 4))

describe('encodeWav', () => {
  it('writes a canonical 44-byte PCM header', () => {
    const buf = encodeWav(new Float32Array(100), 16000)
    const v = new DataView(buf)
    expect(buf.byteLength).toBe(44 + 200)
    expect(tag(buf, 0)).toBe('RIFF')
    expect(v.getUint32(4, true)).toBe(36 + 200)
    expect(tag(buf, 8)).toBe('WAVE')
    expect(tag(buf, 12)).toBe('fmt ')
    expect(v.getUint16(20, true)).toBe(1) // PCM
    expect(v.getUint16(22, true)).toBe(1) // mono
    expect(v.getUint32(24, true)).toBe(16000)
    expect(v.getUint32(28, true)).toBe(32000) // byte rate
    expect(v.getUint16(32, true)).toBe(2) // block align
    expect(v.getUint16(34, true)).toBe(16) // bit depth
    expect(tag(buf, 36)).toBe('data')
    expect(v.getUint32(40, true)).toBe(200)
  })

  it('scales samples to little-endian int16 and clamps overshoot', () => {
    const buf = encodeWav(new Float32Array([0, 1, -1, 0.5, 1.5, -2]), 16000)
    const v = new DataView(buf)
    const sample = (i) => v.getInt16(44 + i * 2, true)
    expect(sample(0)).toBe(0)
    expect(sample(1)).toBe(0x7fff)
    expect(sample(2)).toBe(-0x8000)
    expect(sample(3)).toBe(16383) // 0.5 * 0x7fff truncated by setInt16
    expect(sample(4)).toBe(0x7fff) // clamped, not wrapped
    expect(sample(5)).toBe(-0x8000) // clamped, not wrapped
  })

  it('honours the sample rate argument', () => {
    const v = new DataView(encodeWav(new Float32Array(0), 44100))
    expect(v.getUint32(24, true)).toBe(44100)
    expect(v.getUint32(28, true)).toBe(88200)
  })
})
