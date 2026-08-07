// Unlock authentication: scrypt-hashed passphrase + optional TOTP (RFC 6238).
// Stored in userData/airgap-auth.json (0600) — the seatbelt profile denies
// air-gapped panes read access to this file.
export const MIN_PASSPHRASE_LEN = 8
import { randomBytes, scryptSync, timingSafeEqual } from 'node:crypto'
import { readFile, writeFile, chmod } from 'node:fs/promises'
import { join } from 'node:path'
import { safeStorage } from 'electron'
import { b32encode, b32decode, hotp } from './lib/totp.js'

export { hotp }

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
  if (typeof pass !== 'string' || pass.length < MIN_PASSPHRASE_LEN)
    throw new Error(`Passphrase must be at least ${MIN_PASSPHRASE_LEN} characters.`)
  const salt = randomBytes(16).toString('hex')
  const hash = scryptSync(pass, salt, 32).toString('hex')
  auth = { ...(auth || {}), salt, hash }
  migrateTotpSecret() // re-wrap any legacy plaintext TOTP secret on this write
  await save()
}

export function verifyPassphrase(pass) {
  if (!auth?.hash) return false
  const h = scryptSync(pass, auth.salt, 32)
  return timingSafeEqual(h, Buffer.from(auth.hash, 'hex'))
}

// ---- TOTP (RFC 6238) — crypto lives in ./lib/totp.js (pinned by tests) ----

export async function enrollTotp() {
  const secret = b32encode(randomBytes(20))
  auth = { ...(auth || {}), totp: { secret: protectSecret(secret), active: false } }
  await save()
  return { secret, uri: `otpauth://totp/tome?secret=${secret}&issuer=tome` }
}

export function verifyTotp(code) {
  if (!auth?.totp) return false
  let stored
  try {
    stored = unprotectSecret(auth.totp.secret)
  } catch {
    return false // encrypted secret but no keychain to unwrap it
  }
  const secret = b32decode(stored)
  const t = Math.floor(Date.now() / 30_000)
  return [t - 1, t, t + 1].some((c) => hotp(secret, c) === String(code))
}

export async function confirmTotp(code) {
  if (!verifyTotp(code)) return false
  auth.totp.active = true
  migrateTotpSecret()
  await save()
  return true
}

export function totpActive() {
  return !!auth?.totp?.active
}

// ---- TOTP secret at rest ----
// base32 in a 0600 file is reversible by anything that reads the disk, so
// when the OS keychain is available the secret is wrapped with electron's
// safeStorage (Keychain on macOS) and stored as enc:v1:<base64>. Without a
// keychain (e.g. Linux headless) we keep the legacy plaintext base32.
const TOTP_ENC_PREFIX = 'enc:v1:'
const canEncrypt = () => {
  try {
    return safeStorage.isEncryptionAvailable()
  } catch {
    return false
  }
}
const protectSecret = (b32) =>
  canEncrypt() ? TOTP_ENC_PREFIX + safeStorage.encryptString(b32).toString('base64') : b32
const unprotectSecret = (stored) =>
  stored.startsWith(TOTP_ENC_PREFIX) && canEncrypt()
    ? safeStorage.decryptString(Buffer.from(stored.slice(TOTP_ENC_PREFIX.length), 'base64'))
    : stored // legacy plaintext, or keychain gone (secret unrecoverable -> verify fails)

// One-way upgrade: a legacy plaintext secret on disk is re-wrapped on the
// next save (enroll/confirm/passphrase change). Decrypt-only fallback: if
// the keychain disappears we can still *read* an encrypted secret but new
// writes stay plaintext rather than pretending to be encrypted.
function migrateTotpSecret() {
  if (auth?.totp?.secret && !auth.totp.secret.startsWith(TOTP_ENC_PREFIX) && canEncrypt())
    auth.totp.secret = protectSecret(auth.totp.secret)
}

// ---- login throttling ----
// scrypt at default cost is ~30 ms — a speed bump, not a wall, against IPC-
// speed brute force. Back off exponentially after repeated failures
// (5 → 30 s, doubling, capped at 30 min), per purpose so the lock screen and
// pane unlock throttle independently. Success resets the counter.
const BACKOFF_AFTER = 5
const BACKOFF_BASE_MS = 30_000
const BACKOFF_MAX_MS = 30 * 60_000
const attempts = new Map() // purpose -> { fails, nextAt }

export function throttleRetryIn(purpose) {
  const a = attempts.get(purpose)
  return Math.max(0, (a?.nextAt || 0) - Date.now())
}

export function recordFailure(purpose) {
  const a = attempts.get(purpose) || { fails: 0, nextAt: 0 }
  a.fails++
  if (a.fails >= BACKOFF_AFTER)
    a.nextAt =
      Date.now() +
      Math.min(BACKOFF_BASE_MS * 2 ** (a.fails - BACKOFF_AFTER), BACKOFF_MAX_MS)
  attempts.set(purpose, a)
}

export function recordSuccess(purpose) {
  attempts.delete(purpose)
}
