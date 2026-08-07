// Pure HOTP/TOTP + base32 (RFC 4226/6238, SHA-1, 6 digits, 30s steps).
// Extracted from authlock.js so the crypto is testable without module state.
import { createHmac } from 'node:crypto'

const B32 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'

export function b32encode(buf) {
  let bits = 0
  let val = 0
  let out = ''
  for (const b of buf) {
    val = (val << 8) | b
    bits += 8
    while (bits >= 5) {
      out += B32[(val >>> (bits - 5)) & 31]
      bits -= 5
    }
  }
  if (bits) out += B32[(val << (5 - bits)) & 31]
  return out
}

export function b32decode(s) {
  let bits = 0
  let val = 0
  const out = []
  for (const c of s.replace(/=+$/, '').toUpperCase()) {
    const i = B32.indexOf(c)
    if (i < 0) continue
    val = (val << 5) | i
    bits += 5
    if (bits >= 8) {
      out.push((val >>> (bits - 8)) & 255)
      bits -= 8
    }
  }
  return Buffer.from(out)
}

export function hotp(secret, counter) {
  const buf = Buffer.alloc(8)
  buf.writeBigUInt64BE(BigInt(counter))
  const h = createHmac('sha1', secret).update(buf).digest()
  const o = h[19] & 0xf
  return String((h.readUInt32BE(o) & 0x7fffffff) % 1e6).padStart(6, '0')
}

// TOTP code for a given timestamp (ms); defaults to now.
export function totp(secret, at = Date.now()) {
  return hotp(secret, Math.floor(at / 30_000))
}
