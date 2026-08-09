// Chat transcript persistence, shared by ChatPanel and the ambient voice
// session (voice.js). Extracted from ChatPanel so both writers agree on the
// store key shape, the cap, and the debounce — the voice session and a chat
// pane opened with chatId 'chat-voice' read and write the SAME log.
import { tome } from './util.js'

// Transcripts persist to the main-process JSON store so a chat pane restored
// from a saved layout comes back with its conversation. Writes are debounced
// and capped — the log is a convenience, not an archive.
export const HISTORY_CAP = 100
export const SAVE_DEBOUNCE = 400

export const historyKey = (chatId) => 'chat-log-' + chatId

// The store is untrusted on reload: validate the shape before any of it
// becomes a message — anything that isn't a non-empty user/assistant string
// pair is dropped.
export async function loadHistory(chatId) {
  let saved
  try {
    saved = await tome.store.get(historyKey(chatId))
  } catch {
    return []
  }
  if (!Array.isArray(saved)) return []
  return saved
    .filter(
      (m) =>
        m &&
        typeof m.content === 'string' &&
        m.content &&
        (m.role === 'user' || m.role === 'assistant')
    )
    .slice(-HISTORY_CAP)
}

// Debounced per chatId: callers mutate freely and persist on every turn
// boundary; only the trailing write hits disk. The timer map is module-level
// so a ChatPanel and the voice session writing the same id coalesce into one
// write instead of racing each other.
const timers = new Map()

export function persistHistory(chatId, history) {
  if (typeof chatId !== 'string' || !chatId) return
  clearTimeout(timers.get(chatId))
  timers.set(
    chatId,
    setTimeout(() => {
      timers.delete(chatId)
      tome.store.set(historyKey(chatId), history.slice(-HISTORY_CAP)).catch(() => {})
    }, SAVE_DEBOUNCE)
  )
}

// Flush a pending debounced write NOW — ChatPanel.dispose calls this so a
// quick pane close doesn't drop the tail of the conversation.
export function flushHistory(chatId, history) {
  clearTimeout(timers.get(chatId))
  timers.delete(chatId)
  if (typeof chatId !== 'string' || !chatId || !history.length) return
  tome.store.set(historyKey(chatId), history.slice(-HISTORY_CAP)).catch(() => {})
}
