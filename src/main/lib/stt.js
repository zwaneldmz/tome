// Local speech-to-text for push-to-talk: write the renderer's WAV to a temp
// file, run the whisper.cpp CLI over it, hand back stdout. Local by design
// (HANDOFF §5): audio never leaves the machine — no new egress, no new
// allowlist host, and the transcript is ordinary composer text. Electron-free
// so the availability messages and plumbing are testable; index.js injects
// the userData/temp paths.
import { execFile } from 'node:child_process'
import { existsSync } from 'node:fs'
import { writeFile, unlink } from 'node:fs/promises'
import { join, dirname } from 'node:path'

const MODEL = 'ggml-base.en.bin'
const MODEL_URL = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/' + MODEL

// GUI apps on macOS launch with a PATH that lacks the homebrew prefixes, so a
// bare execFile('whisper-cli') fails in the packaged app even when the user's
// terminal finds it fine. Probe the usual absolute homes first; the bare name
// stays as the last resort for PATH-correct launches (dev runs, linux).
const CANDIDATES = ['/opt/homebrew/bin/whisper-cli', '/usr/local/bin/whisper-cli', 'whisper-cli']

export function whisperBin(env = process.env) {
  if (env.TOME_WHISPER_BIN) return env.TOME_WHISPER_BIN
  return CANDIDATES.find((c) => !c.includes('/') || existsSync(c))
}

export const modelPath = (userData) => join(userData, 'models', MODEL)

// Existence probes for stt:status. A bare binary name (PATH-correct linux
// dev runs) can't be probed without a PATH walk — count it as present and
// let a real transcribe map ENOENT to NO_BIN, same as sttUnavailable does.
export const binExists = (bin) => !!bin && (!bin.includes('/') || existsSync(bin))
export const modelExists = (model) => existsSync(model)

// A user-facing reason STT can't run yet, or null when it can. The model file
// is a deliberate one-time manual download (plan §8): no downloader means no
// new egress path, and once the file exists the whole loop works air-gapped.
// A bare (non-absolute) bin can't be probed without a PATH walk — let
// execFile discover that and map ENOENT to the same message.
export function sttUnavailable(bin, model) {
  if (!bin || (bin.includes('/') && !existsSync(bin))) return NO_BIN
  if (!existsSync(model)) {
    return (
      'Speech model missing. Download it once:\n' +
      `  mkdir -p "${dirname(model)}" && curl -L -o "${model}" ${MODEL_URL}`
    )
  }
  return null
}

export const NO_BIN =
  'whisper-cli not found. Install it (brew install whisper-cpp) or point TOME_WHISPER_BIN at the binary.'

export async function transcribe({ wav, bin, model, tempDir, timeoutMs = 60_000 }) {
  // Main invents the temp path itself — the renderer only ever supplies bytes.
  const file = join(tempDir, `tome-stt-${process.pid}-${Date.now()}.wav`)
  await writeFile(file, Buffer.from(wav))
  try {
    const stdout = await new Promise((res, rej) => {
      execFile(
        bin,
        ['-m', model, '-f', file, '--no-timestamps'],
        { timeout: timeoutMs, killSignal: 'SIGKILL', maxBuffer: 4 * 1024 * 1024 },
        (err, out) => (err ? rej(err) : res(out))
      )
    })
    // whisper prints one padded line per segment; dictated text wants a
    // single run of prose.
    return String(stdout).replace(/\s+/g, ' ').trim()
  } finally {
    // Awaited so a resolved transcribe() means the temp file is really gone —
    // fire-and-forget here raced both the tests and any caller counting temps.
    await unlink(file).catch(() => {})
  }
}
