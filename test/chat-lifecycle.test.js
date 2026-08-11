// Pure predicate for ChatPanel.dispose() (TOME-015): a pane closed mid-reply
// must abort its in-flight turn, unless voice.js owns it. DOM-free — no
// jsdom needed, none is set up in this repo.
import { describe, it, expect } from 'vitest'
import { shouldAbortOnDispose, VOICE_CHAT_ID } from '../src/renderer/chat-lifecycle.js'

describe('shouldAbortOnDispose', () => {
  it('aborts a busy, ordinary pane', () => {
    expect(shouldAbortOnDispose(true, 'chat-abc123', false)).toBe(true)
  })

  it('does nothing for an idle pane', () => {
    expect(shouldAbortOnDispose(false, 'chat-abc123', false)).toBe(false)
  })

  it('never aborts the canonical voice chat id while voice.js actually owns its turn', () => {
    expect(shouldAbortOnDispose(true, VOICE_CHAT_ID, true)).toBe(false)
    expect(shouldAbortOnDispose(false, VOICE_CHAT_ID, true)).toBe(false)
  })

  it('aborts the canonical voice chat id when voice is not active — it is just an ordinary busy pane then', () => {
    // Regression guard: a chatId !== VOICE_CHAT_ID check alone (no voiceActive
    // condition) used to suppress this abort unconditionally, orphaning the
    // provider/tool loop exactly like TOME-015 describes — voiceOwns() in
    // renderer.js only intercepts events for VOICE_CHAT_ID while voice is
    // actually active, so with voice inactive nothing else would ever abort it.
    expect(shouldAbortOnDispose(true, VOICE_CHAT_ID, false)).toBe(true)
  })

  it('still aborts a busy ordinary pane even while a voice session is active elsewhere', () => {
    // Regression guard: a bare `!voiceActive` term used to suppress this
    // abort too, even though voice.js only ever drives VOICE_CHAT_ID — an
    // unrelated pane's own turn is never at risk from voice being on.
    expect(shouldAbortOnDispose(true, 'chat-abc123', true)).toBe(true)
  })

  it('coerces a truthy-but-non-boolean busy flag', () => {
    expect(shouldAbortOnDispose(1, 'chat-abc123', false)).toBe(true)
    expect(shouldAbortOnDispose(0, 'chat-abc123', false)).toBe(false)
  })
})
