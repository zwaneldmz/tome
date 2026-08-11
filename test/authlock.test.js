// Pins the authlock crypto against RFC 4226/6238 — verified correct today
// (pi review §2); these tests exist so the next edit owns any regression.
import { describe, it, expect, afterAll } from 'vitest'
import { hotp, b32encode, b32decode, totp } from '../src/main/lib/totp.js'
import { randomBytes } from 'node:crypto'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { initAuth, enrollTotp, confirmTotp } from '../src/main/authlock.js'

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

// TOME-005: enrollTotp() used to unconditionally overwrite auth.totp — active
// factor or not — and confirmTotp() re-activates whatever secret is current.
// airgap:enrollTotp/airgap:confirmTotp both sat on OPEN_CHANNELS (reachable
// pre-auth), so an unauthenticated caller could roll a secret only they know
// over the owner's live one and confirm it out from under them. Guarded now:
// enrollTotp() refuses once a factor is active; first enrollment still works.
//
// authlock.js imports 'electron' at module scope but loads fine under
// vitest: outside a real Electron process the 'electron' devDependency
// resolves to a path string, not an API object, so `safeStorage` destructures
// to undefined — every call site already wraps it in try/catch (canEncrypt),
// which is what keeps this file testable without a fake electron module.
const authDirs = []
async function freshAuth() {
  const dir = await mkdtemp(join(tmpdir(), 'tome-authlock-'))
  authDirs.push(dir)
  await initAuth(dir) // fresh dir -> no file on disk -> auth resets to null
}
afterAll(async () => {
  for (const dir of authDirs.splice(0)) await rm(dir, { recursive: true, force: true }).catch(() => {})
})

describe('enrollTotp() active-factor guard', () => {
  it('first-time enrollment succeeds (no active factor yet)', async () => {
    await freshAuth()
    const { secret, uri } = await enrollTotp()
    expect(secret).toMatch(/^[A-Z2-7]+$/)
    expect(uri).toBe(`otpauth://totp/tome?secret=${secret}&issuer=tome`)
  })

  it('re-enrolling before confirmation still succeeds (factor not active yet)', async () => {
    await freshAuth()
    const first = await enrollTotp()
    const second = await enrollTotp()
    expect(second.secret).toMatch(/^[A-Z2-7]+$/)
    expect(second.secret).not.toBe(first.secret)
  })

  it('refuses to overwrite an already-active factor', async () => {
    await freshAuth()
    const { secret } = await enrollTotp()
    expect(await confirmTotp(totp(b32decode(secret)))).toBe(true)
    await expect(enrollTotp()).rejects.toThrow('Active second factor present.')
  })
})
