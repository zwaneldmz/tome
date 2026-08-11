// Pure lifecycle predicate for ChatPanel.dispose() (TOME-015): should
// closing this pane abort its in-flight turn? DOM-free — no `document`,
// `window`, or other browser globals — so it is unit-testable without
// jsdom, which this repo does not have.
//
// VOICE_CHAT_ID is the ambient voice session's canonical chat id (WS-A
// contract, voice.js): a pane opened with this id shares its turn with
// voice.js — routed through its sendTurn() when the session is active, or
// driven by the pane's own composer when it is not. Only the FIRST case must
// never be cut off by closing its tab: voice.js, not this pane, owns that
// turn and is still listening for it elsewhere. When voice is not active,
// chat-voice behaves like any other pane — driven by its own composer, so
// disposing it while busy must abort exactly like an ordinary pane would, or
// the provider/tool loop in main is left running headless with nowhere to
// deliver its events (voiceOwns() in renderer.js routes chat:delta/tool/done
// to chats.get('chat-voice') whenever voiceActive() is false, and dispose()
// has already deleted that entry by the time they'd arrive). Symmetrically,
// an ordinary pane's own busy turn is never voice.js's, even if a voice
// session happens to be active on its own unrelated chatId — so voiceActive
// alone must never suppress an ordinary pane's abort either.
export const VOICE_CHAT_ID = 'chat-voice'

export function shouldAbortOnDispose(busy, chatId, voiceActive) {
  return !!busy && !(chatId === VOICE_CHAT_ID && voiceActive)
}
