// Sentence chunking for streamed TTS. speechSynthesis re-attacks on every
// queued utterance, so a lone "Yes." spoken on its own costs an audible
// gap. nextSpeakChunk returns the shortest sentence that clears `minChars`,
// letting a short opener ride along with the sentence after it. Pure on
// purpose (string in, string out) so vitest exercises it without a DOM or
// speechSynthesis behind it — same posture as vad.js.

export const MIN_SPEAK_CHARS = 24

export function nextSpeakChunk(tail, minChars = MIN_SPEAK_CHARS) {
  // Sentence boundaries in text order: '.', '!', or '?' followed by
  // whitespace or end-of-string. The FIRST boundary at or past `minChars`
  // is the shortest prefix that clears the minimum — a short first sentence
  // is skipped, so the next complete sentence extends the chunk instead.
  for (const m of tail.matchAll(/[.!?](?=\s|$)/g)) {
    if (m.index + 1 >= minChars) return tail.slice(0, m.index + 1)
  }
  // No boundary reached the minimum: the tail had no terminator at all, or
  // even its full run of short sentences stayed under `minChars`.
  return ''
}
