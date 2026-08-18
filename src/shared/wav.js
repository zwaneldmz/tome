// 16-bit PCM mono WAV encoder for the push-to-talk recorder. Exists because
// MediaRecorder emits webm/opus, which whisper.cpp can't read, and a 44-byte
// header is not worth an ffmpeg dependency. Pure on purpose (Float32 samples
// in, ArrayBuffer out) so vitest exercises it without a DOM or an
// AudioContext behind it.
export function encodeWav(samples, sampleRate = 16000) {
  const n = samples.length
  const buf = new ArrayBuffer(44 + n * 2)
  const v = new DataView(buf)
  const ascii = (off, s) => {
    for (let i = 0; i < s.length; i++) v.setUint8(off + i, s.charCodeAt(i))
  }
  ascii(0, 'RIFF')
  v.setUint32(4, 36 + n * 2, true)
  ascii(8, 'WAVE')
  ascii(12, 'fmt ')
  v.setUint32(16, 16, true) // fmt chunk size
  v.setUint16(20, 1, true) // PCM
  v.setUint16(22, 1, true) // mono
  v.setUint32(24, sampleRate, true)
  v.setUint32(28, sampleRate * 2, true) // byte rate: rate * channels * 2
  v.setUint16(32, 2, true) // block align
  v.setUint16(34, 16, true) // bits per sample
  ascii(36, 'data')
  v.setUint32(40, n * 2, true)
  for (let i = 0; i < n; i++) {
    // Clamp before scaling: getUserMedia can overshoot [-1, 1] slightly and
    // an unclamped 1.01 wraps to a full-scale negative click in int16.
    const s = Math.max(-1, Math.min(1, samples[i]))
    v.setInt16(44 + i * 2, s < 0 ? s * 0x8000 : s * 0x7fff, true)
  }
  return buf
}

// 16-bit PCM mono encoder for live streaming (voice-0.4 Task 3): the same
// little-endian int16 scaling as encodeWav's payload loop, but as a bare
// byte stream — no WAV header — because the Apple streaming path appends
// raw PCM chunks to a live SFSpeechAudioBufferRecognitionRequest rather than
// handing whisper.cpp a finished file. Pure on purpose (Float32 samples in,
// Uint8Array bytes out) so vitest exercises it without an AudioContext.
export function floatToPcm16(samples) {
  const out = new Uint8Array(samples.length * 2)
  const v = new DataView(out.buffer)
  for (let i = 0; i < samples.length; i++) {
    // Same clamp as encodeWav: an overshooting sample must not wrap.
    const s = Math.max(-1, Math.min(1, samples[i]))
    v.setInt16(i * 2, s < 0 ? s * 0x8000 : s * 0x7fff, true)
  }
  return out
}
