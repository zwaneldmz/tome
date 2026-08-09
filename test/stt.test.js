// Pins the STT plumbing that can be pinned without a whisper binary: binary
// discovery order, the availability messages users act on, arg order to the
// sidecar, and the temp-file cleanup that must survive a failed run.
import { describe, it, expect } from 'vitest'
import { mkdtempSync, rmSync, readdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { whisperBin, modelPath, sttUnavailable, transcribe, NO_BIN } from '../src/main/lib/stt.js'

describe('whisperBin', () => {
  it('lets TOME_WHISPER_BIN override discovery', () => {
    expect(whisperBin({ TOME_WHISPER_BIN: '/x/y/whisper' })).toBe('/x/y/whisper')
  })
  it('falls back to a PATH lookup name at worst', () => {
    expect(whisperBin({})).toBeTruthy()
  })
})

describe('sttUnavailable', () => {
  it('names the install fix when the binary path is dead', () => {
    expect(sttUnavailable('/nope/whisper-cli', '/nope/model.bin')).toBe(NO_BIN)
  })
  it('gives the exact download command when only the model is missing', () => {
    const why = sttUnavailable('/bin/ls', '/nope/models/ggml-base.en.bin')
    expect(why).toMatch(/curl -L -o "\/nope\/models\/ggml-base\.en\.bin"/)
    expect(why).toMatch(/mkdir -p "\/nope\/models"/)
  })
  it('is satisfied by an existing binary and model file', () => {
    expect(sttUnavailable('/bin/ls', '/bin/ls')).toBeNull()
  })
  it('derives the model path under userData/models', () => {
    expect(modelPath('/ud')).toBe('/ud/models/ggml-base.en.bin')
  })
})

describe('transcribe', () => {
  it('spawns bin with -m model -f wav, returns collapsed stdout, cleans up', async () => {
    const tempDir = mkdtempSync(join(tmpdir(), 'tome-stt-'))
    try {
      // /bin/echo stands in for whisper-cli: its output IS its argv, which
      // pins the argument order without needing a model or a mic.
      const out = await transcribe({ wav: new ArrayBuffer(4), bin: '/bin/echo', model: '/m.bin', tempDir })
      expect(out).toMatch(/^-m \/m\.bin -f .*tome-stt-.*\.wav --no-timestamps$/)
      expect(readdirSync(tempDir)).toEqual([]) // temp wav removed
    } finally {
      rmSync(tempDir, { recursive: true, force: true })
    }
  })

  it('cleans up the temp wav even when the spawn fails', async () => {
    const tempDir = mkdtempSync(join(tmpdir(), 'tome-stt-'))
    try {
      await expect(
        transcribe({ wav: new ArrayBuffer(4), bin: '/nope/whisper-cli', model: '/m.bin', tempDir })
      ).rejects.toMatchObject({ code: 'ENOENT' })
      expect(readdirSync(tempDir)).toEqual([])
    } finally {
      rmSync(tempDir, { recursive: true, force: true })
    }
  })

  it('rejects on timeout instead of hanging', async () => {
    const tempDir = mkdtempSync(join(tmpdir(), 'tome-stt-'))
    const slow = join(tempDir, 'slow.sh')
    writeFileSync(slow, '#!/bin/sh\nsleep 5\n', { mode: 0o755 })
    try {
      await expect(
        transcribe({ wav: new ArrayBuffer(4), bin: slow, model: '/m.bin', tempDir, timeoutMs: 100 })
      ).rejects.toBeTruthy()
    } finally {
      rmSync(tempDir, { recursive: true, force: true })
    }
  }, 10_000)
})
