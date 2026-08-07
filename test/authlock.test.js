// Pins the authlock crypto against RFC 4226/6238 — verified correct today
// (pi review §2); these tests exist so the next edit owns any regression.
import { describe, it, expect } from 'vitest'
import { hotp, b32encode, b32decode, totp } from '../src/main/lib/totp.js'
import { randomBytes } from 'node:crypto'

// RFC 4226 Appendix D — secret is the ASCII string "12345678901234567890"
const RFC_SECRET = Buffer.from('12345678901234567890', 'ascii')
const RFC_VECTORS = [
  [0, '755224'],
  [1, '287082'],
  [2, '359152'],
  [3, '969429'],
  [4, '338314'],
  [5, '254676'],
  [6, '287922'],
  [7, '162583'],
  [8, '399871'],
  [9, '520489'],
]

describe('hotp (RFC 4226)', () => {
  it.each(RFC_VECTORS)('counter %i -> %s', (counter, expected) => {
    expect(hotp(RFC_SECRET, counter)).toBe(expected)
  })
})

describe('totp (RFC 6238)', () => {
  it('round-trips: generate then verify at the same instant', () => {
    const secret = randomBytes(20)
    const now = Date.now()
    const code = totp(secret, now)
    expect(code).toMatch(/^\d{6}$/)
    // same 30s step must reproduce the same code
    expect(totp(secret, now)).toBe(code)
    // a different step should (overwhelmingly) produce a different code
    expect(totp(secret, now + 60_000)).not.toBe(code)
  })

  it('matches hotp at the 30s time step', () => {
    const secret = randomBytes(20)
    const t = 1_700_000_000_000
    expect(totp(secret, t)).toBe(hotp(secret, Math.floor(t / 30_000)))
  })
})

describe('base32', () => {
  it('encode/decode round-trips random secrets', () => {
    for (let i = 0; i < 20; i++) {
      const buf = randomBytes(1 + i) // exercise every padding length
      expect(b32decode(b32encode(buf))).toEqual(buf)
    }
  })

  it('decodes lowercase input identically', () => {
    const enc = b32encode(randomBytes(20))
    expect(b32decode(enc.toLowerCase())).toEqual(b32decode(enc))
  })

  it('ignores trailing padding', () => {
    const buf = randomBytes(20)
    const enc = b32encode(buf)
    expect(b32decode(enc + '===')).toEqual(buf)
  })
})
