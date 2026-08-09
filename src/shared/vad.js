// Energy-based voice activity detector for the ambient voice session. Pure
// on purpose (Float32 chunks in, callbacks out) so vitest exercises it
// without a DOM or an AudioContext behind it — same posture as wav.js.
//
// The detector has two states. In silence it watches the RMS of each chunk
// and arms once energy crosses the speech threshold (a short hangover keeps
// plosive onsets from arming and immediately releasing). While armed it
// counts trailing quiet time; crossing the silence budget ends the utterance.
// A hard cap ends runaway utterances (a fan, a TV) so the turn loop always
// gets its audio back.

export const VAD_DEFAULTS = {
  sampleRate: 16000,
  threshold: 0.015, // RMS of int16-ish mic noise sits ~0.005; speech ~0.03+
  silenceMs: 900, // endpoint budget — long enough for mid-sentence pauses
  minSpeechMs: 250, // below this an "utterance" was a click/cough — ignore
  maxMs: 60_000, // hard cap per utterance
  hangoverMs: 120, // onset debounce — one loud frame is not speech
}

export function makeVad({ onSpeechStart, onSpeechEnd, ...opts } = {}) {
  const o = { ...VAD_DEFAULTS, ...opts }
  // All budgets are tracked in SAMPLES, not wall time, so tests feed chunks
  // synchronously and a stalled audio thread can't skew endpointing either.
  const silenceN = (o.silenceMs / 1000) * o.sampleRate
  const minSpeechN = (o.minSpeechMs / 1000) * o.sampleRate
  const maxN = (o.maxMs / 1000) * o.sampleRate
  const hangoverN = (o.hangoverMs / 1000) * o.sampleRate

  let speaking = false
  let onsetN = 0 // consecutive loud samples seen while silent
  let quietN = 0 // consecutive quiet samples seen while speaking
  let speechN = 0 // total samples in the current utterance (cap + min-length)

  return {
    push(chunk) {
      // RMS over the whole chunk: ScriptProcessor hands us 4096-sample
      // blocks (~256 ms at 16 kHz), so per-chunk energy is already smoothed.
      let sum = 0
      for (let i = 0; i < chunk.length; i++) sum += chunk[i] * chunk[i]
      const rms = Math.sqrt(sum / (chunk.length || 1))
      const loud = rms >= o.threshold

      if (!speaking) {
        onsetN = loud ? onsetN + chunk.length : 0
        if (onsetN >= hangoverN) {
          speaking = true
          quietN = 0
          speechN = onsetN
          onsetN = 0
          onSpeechStart?.()
        }
        return
      }

      speechN += chunk.length
      quietN = loud ? 0 : quietN + chunk.length
      const capped = speechN >= maxN
      if (quietN >= silenceN || capped) {
        speaking = false
        onsetN = 0
        // Speech length EXCLUDES the trailing silence that ended it —
        // otherwise a 200 ms click followed by 900 ms of quiet would count
        // as 1.1 s of "speech" and survive the min-utterance floor.
        const n = speechN - quietN
        quietN = 0
        speechN = 0
        // Too short to have been a real utterance — drop it silently rather
        // than spending a whisper round-trip on a keyboard click. The hard
        // cap always ends the utterance: 60 s of audio was clearly speech
        // (or noise the user needs transcribed either way).
        if (capped || n >= minSpeechN) onSpeechEnd?.()
      }
    },
    // True between speech onset and endpoint — voice.js polls this for the
    // barge-in check while TTS is playing.
    get speaking() {
      return speaking
    },
    reset() {
      speaking = false
      onsetN = 0
      quietN = 0
      speechN = 0
    },
  }
}
