// Unlock authentication: scrypt-hashed passphrase + optional TOTP (RFC 6238).
// Stored in userData/airgap-auth.json (0600) — the seatbelt profile denies
// air-gapped panes read access to this file.
import { randomBytes, scryptSync, createHmac, timingSafeEqual } from 'node:crypto'
import { readFile, writeFile, chmod } from 'node:fs/promises'
import { join } from 'node:path'

let file = null
let auth = null // { salt, hash, totp?: { secret, active } }
let unlocked = false // app-level login state (session-scoped, main process only)

export const isUnlocked = () => unlocked
export const markUnlocked = () => {
  unlocked = true
}

export async function initAuth(userData) {
  file = join(userData, 'airgap-auth.json')
  try {
    auth = JSON.parse(await readFile(file, 'utf8'))
  } catch {
    auth = null
  }
}

async function save() {
  await writeFile(file, JSON.stringify(auth))
  await chmod(file, 0o600)
}

export function authStatus() {
  return { configured: !!auth?.hash, totp: !!auth?.totp?.active }
}

export async function setPassphrase(pass) {
  const salt = randomBytes(16).toString('hex')
  const hash = scryptSync(pass, salt, 32).toString('hex')
  auth = { ...(auth || {}), salt, hash }
  await save()
}

export function verifyPassphrase(pass) {
  if (!auth?.hash) return false
  const h = scryptSync(pass, auth.salt, 32)
  return timingSafeEqual(h, Buffer.from(auth.hash, 'hex'))
}

// ---- TOTP (RFC 6238, SHA-1, 6 digits, 30s steps) ----
const B32 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'

function b32encode(buf) {
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

function b32decode(s) {
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

export async function enrollTotp() {
  const secret = b32encode(randomBytes(20))
  auth = { ...(auth || {}), totp: { secret, active: false } }
  await save()
  return { secret, uri: `otpauth://totp/tome?secret=${secret}&issuer=tome` }
}

export function verifyTotp(code) {
  if (!auth?.totp) return false
  const secret = b32decode(auth.totp.secret)
  const t = Math.floor(Date.now() / 30_000)
  return [t - 1, t, t + 1].some((c) => hotp(secret, c) === String(code))
}

export async function confirmTotp(code) {
  if (!verifyTotp(code)) return false
  auth.totp.active = true
  await save()
  return true
}

export function totpActive() {
  return !!auth?.totp?.active
}
