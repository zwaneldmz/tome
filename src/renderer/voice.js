// Ambient voice chat: a topbar mic button toggles a hands-free session —
// the user talks, the assistant answers aloud, and NO pane opens. The
// transcript is the assistant chat pane's transcript: a chat pane opened
// with the canonical chatId 'chat-voice' shows the same conversation and
// continues it by text.
//
// Turn loop (state machine, `state` below):
//   idle → listening → transcribing → thinking → speaking → listening …
// Mic capture mirrors ChatPanel.startRec (getUserMedia, 16 kHz AudioContext,
// ScriptProcessor collecting Float32 chunks, encodeWav, tome.stt.transcribe);
// endpointing comes from the pure VAD in shared/vad.js. Replies stream back
// over tome.chat — voice.js registers its OWN delta/done/tool listeners
// (renderer.js fans those events to chats.get(id) only, and multiple
// ipcRenderer listeners are fine), filtered to id 'chat-voice' and to an
// active session.
import { tome, toast } from './util.js'
import { chats } from './regs.js'
import { addChat } from './panes.js'
import { floatingMenu, menuItem, menuLabel, menuRule } from './menus.js'
import { preferencesModal } from './preferences.js'
import { micIcon } from './icons.js'
import { loadHistory, persistHistory, flushHistory } from './chat-history.js'
import { encodeWav } from '../shared/wav.js'
import { makeVad } from '../shared/vad.js'

// Canonical id, per the WS-A contract: the voice session and a chat pane
// opened with this id share history via the store key 'chat-log-chat-voice'.
export const VOICE_CHAT_ID = 'chat-voice'

let btn = null
let active = false
let state = 'idle' // idle | listening | transcribing | thinking | speaking
let rec = null // { stream, ctx, proc, chunks, utter }
let vad = null
let history = [] // the session's copy — see the note in pane()
let autoSend = true
let speakRate = 1
let bargeIn = true
let reply = '' // accumulated deltas of the current assistant turn
let spokenUpTo = 0 // reply prefix already handed to speechSynthesis
let speakingNow = false // an utterance is currently playing

// State changes are not toasts (they'd spam the notification log), but
// screen readers still need them — same #sr-live region toast() uses,
// cleared then re-filled on the next tick so identical repeats announce.
function announce(msg) {
  const live = document.getElementById('sr-live')
  if (!live) return
  live.textContent = ''
  setTimeout(() => (live.textContent = msg), 50)
}

const STATE_UI = {
  idle: { cls: '', title: 'Voice chat (⌘⇧V)', label: 'Voice chat', say: null },
  listening: {
    cls: 'listening',
    title: 'Listening… click to stop',
    label: 'Voice chat — listening',
    say: 'Voice chat listening',
  },
  transcribing: {
    cls: 'thinking',
    title: 'Transcribing…',
    label: 'Voice chat — transcribing',
    say: 'Transcribing',
  },
  thinking: {
    cls: 'thinking',
    title: 'Assistant is thinking… click to stop',
    label: 'Voice chat — waiting for the assistant',
    say: 'Assistant is thinking',
  },
  speaking: {
    cls: 'speaking',
    title: 'Speaking… click to stop',
    label: 'Voice chat — speaking',
    say: 'Assistant is speaking',
  },
}

function setState(next) {
  state = next
  const ui = STATE_UI[next] || STATE_UI.idle
  if (!btn) return
  for (const cls of ['listening', 'thinking', 'speaking']) btn.classList.remove(cls)
  if (ui.cls) btn.classList.add(ui.cls)
  btn.title = ui.title
  btn.setAttribute('aria-label', ui.label)
  btn.setAttribute('aria-pressed', String(active))
  if (ui.say) announce(ui.say)
}

// The pane that shares our transcript, when it's open. Voice turns render
// live through it; when it's absent voice.js keeps history itself — the two
// never write concurrently, because the pane's own send() only fires while
// the voice session is idle (a pane busy on a voice turn has its own busy
// flag unset only via finish, which voice.js drives).
const pane = () => chats.get(VOICE_CHAT_ID)

function openTranscript() {
  addChat(undefined, { chatId: VOICE_CHAT_ID, title: 'assistant — voice' })
}

// ---------- TTS ----------
// Complete sentences are spoken as they stream in (long replies start
// talking fast instead of waiting for chat:done); the remainder flushes on
// done. Each queued chunk re-reads `speakRate` at speak time, so changing
// voice-rate mid-reply takes effect on the next sentence.
function speak(text) {
  if (!text.trim()) return
  speakingNow = true
  const u = new SpeechSynthesisUtterance(text)
  u.rate = speakRate
  u.onend = u.onerror = () => {
    speakingNow = false
    // Nothing left queued and the reply is over → back to the mic.
    if (active && state === 'speaking' && !speechSynthesis.pending) startListening()
  }
  speechSynthesis.speak(u)
}

// Longest complete sentence in the not-yet-spoken tail of the reply.
function takeSentences() {
  const tail = reply.slice(spokenUpTo)
  const m = tail.match(/^[\s\S]*[.!?](?=\s|$)/)
  if (!m) return ''
  spokenUpTo += m[0].length
  return m[0]
}

function stopSpeaking() {
  speechSynthesis.cancel()
  speakingNow = false
}

// ---------- mic capture ----------
// Same capture approach as ChatPanel.startRec: raw Float32 + our own WAV
// encode, because MediaRecorder only emits webm/opus, which whisper.cpp
// can't read. The VAD watches the same chunks and ends the utterance on
// ~900 ms of trailing silence (or the 60 s hard cap).
async function startListening() {
  if (!active || rec) return
  let stream
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: true })
  } catch {
    toast('microphone unavailable or permission denied')
    stopVoice()
    return
  }
  // 16 kHz is what whisper.cpp expects; Chromium resamples the mic for us.
  const ctx = new AudioContext({ sampleRate: 16000 })
  const chunks = [] // full session audio (barge-in included)
  let utter = [] // current utterance, reset on each VAD speech start
  vad = makeVad({
    onSpeechStart: () => {
      utter = []
      // Barge-in: talking over the assistant cancels TTS and becomes the
      // next user turn. The mic is live during 'speaking' precisely so this
      // can fire.
      if (state === 'speaking' && bargeIn) {
        stopSpeaking()
        setState('listening')
      }
    },
    onSpeechEnd: () => {
      if (state === 'listening') endUtterance()
    },
  })
  // ponytail: ScriptProcessor is deprecated but needs no worklet file; swap
  // for an AudioWorklet when Chromium actually removes it.
  const proc = ctx.createScriptProcessor(4096, 1, 1)
  proc.onaudioprocess = (ev) => {
    const c = new Float32Array(ev.inputBuffer.getChannelData(0))
    chunks.push(c)
    if (state === 'listening') {
      utter.push(c)
      vad.push(c)
    } else if (state === 'speaking' && bargeIn) {
      vad.push(c) // only listening for the barge-in onset
    }
  }
  ctx.createMediaStreamSource(stream).connect(proc)
  proc.connect(ctx.destination)
  rec = { stream, ctx, proc, chunks, utter: () => utter }
  setState('listening')
}

function stopMic() {
  const r = rec
  rec = null
  vad = null
  if (!r) return
  r.proc.disconnect()
  for (const t of r.stream.getTracks()) t.stop()
  r.ctx.close().catch(() => {})
}

async function endUtterance() {
  const r = rec
  if (!r) return
  const parts = r.utter()
  const total = parts.reduce((n, c) => n + c.length, 0)
  if (!total) return
  const samples = new Float32Array(total)
  let at = 0
  for (const c of parts) {
    samples.set(c, at)
    at += c.length
  }
  // encode at the rate we actually got — a device that refused 16 kHz still
  // produces a valid WAV, and whisper's own error then says what's wrong
  const wav = encodeWav(samples, r.ctx.sampleRate)
  setState('transcribing')
  let res
  try {
    res = await tome.stt.transcribe(wav)
  } catch (err) {
    toast('transcription failed: ' + (err?.message || err))
    if (active) setState('listening') // mic is still open — keep the session
    return
  }
  if (!active) return // stopped while whisper was running
  if (res?.error) {
    // The stt:transcribe error payload already carries user-facing install
    // advice — surface it verbatim and end the session gracefully.
    toast(res.error)
    stopVoice()
    return
  }
  const text = res?.text?.trim()
  if (!text) {
    setState('listening')
    return
  }
  if (!autoSend) {
    // Push-to-talk mode: dictated text lands in the transcript pane's
    // composer and is NEVER auto-sent — the user reads what was heard,
    // edits, and presses Enter (same posture as ChatPanel's mic button).
    // The pane's ChatPanel instance lands in the chats registry on its init
    // callback, which dockview runs synchronously inside addPanel — so the
    // registry lookup right after addChat already sees it.
    if (!pane()) openTranscript()
    const input = pane()?.input
    if (input) {
      input.value = input.value ? input.value.trimEnd() + ' ' + text : text
      input.focus()
    } else {
      toast('open the voice transcript to review dictated text')
    }
    setState('listening')
    return
  }
  sendTurn(text)
}

// ---------- the turn ----------
function sendTurn(text) {
  history.push({ role: 'user', content: text })
  const p = pane()
  if (p) {
    // The pane is open: route the turn through it so its log, history array
    // and the session's copy stay one conversation (never two arrays over
    // the same store key).
    p.pushExternal('user', text)
    p.busy = true
    p.stopBtn.classList.remove('hidden')
    p.aiBubble()
    p.startWait()
    p.currentText = ''
    p.segText = ''
  } else {
    persistHistory(VOICE_CHAT_ID, history)
  }
  reply = ''
  spokenUpTo = 0
  setState('thinking')
  // Brain injection is OFF for voice (no brainWs): an ambient session has no
  // workspace context picked, and surprise vault reads are worse than none.
  tome.chat.send(VOICE_CHAT_ID, history).catch((err) => {
    onDone({ id: VOICE_CHAT_ID, error: err?.message || String(err) })
  })
}

function onDelta({ id, text }) {
  if (!active || id !== VOICE_CHAT_ID) return
  reply += text
  pane()?.appendDelta(text)
  if (state === 'thinking') setState('speaking')
  if (state === 'speaking') {
    const s = takeSentences()
    if (s) speak(s)
  }
}

function onTool({ id, tool, hint }) {
  if (!active || id !== VOICE_CHAT_ID) return
  pane()?.toolNote(tool, hint)
  // A tool ran between text segments: the not-yet-spoken tail belongs to the
  // old segment — flush it so it isn't lost when the reply continues.
  if (state === 'speaking') {
    const s = reply.slice(spokenUpTo)
    spokenUpTo = reply.length
    if (s.trim()) speak(s)
  }
}

function onDone({ id, error, aborted }) {
  if (!active || id !== VOICE_CHAT_ID) return
  const p = pane()
  if (error) {
    p?.finish(error, aborted) // rolls the failed user turn out + persists
    if (!p) {
      // Roll the failed user message out of history, same as ChatPanel — a
      // voice session has no composer to hand it back to, so say so.
      const last = history.pop()
      if (!aborted && last?.role === 'user') toast('voice turn failed — not sent')
      persistHistory(VOICE_CHAT_ID, history)
    } else if (!aborted) {
      toast('voice turn failed — it is back in the transcript composer')
    }
    toast(String(error))
    setState('listening')
    return
  }
  if (p) {
    // The pane pushes its own copy of the assistant text in finish() and
    // persists — its currentText accumulated the same deltas, and the
    // session shares its array, so nothing more to write.
    p.finish()
  } else {
    history.push({ role: 'assistant', content: reply })
    persistHistory(VOICE_CHAT_ID, history)
  }
  const rest = reply.slice(spokenUpTo)
  spokenUpTo = reply.length
  setState('speaking')
  if (rest.trim()) speak(rest)
  // A short reply may already be fully spoken; if nothing is queued or
  // playing, go straight back to the mic.
  if (!speakingNow && !speechSynthesis.pending) startListening()
}

// ---------- session lifecycle ----------
async function startVoice() {
  if (active) return
  active = true
  // Continue the transcript: whatever the pane (or a previous session)
  // persisted under chat-log-chat-voice is this session's context. A live
  // pane's in-memory array wins — it may hold turns not yet flushed.
  const p = pane()
  history = p ? p.history : await loadHistory(VOICE_CHAT_ID)
  if (!active) return // toggled off while the store read was in flight
  await startListening()
}

export function stopVoice() {
  if (!active) return
  active = false
  tome.chat.abort(VOICE_CHAT_ID)
  stopSpeaking()
  stopMic()
  // The pane (if open) keeps whatever partial reply it rendered — it will
  // finalize it when its own chat:done lands. Our copy flushes directly.
  if (!pane()) flushHistory(VOICE_CHAT_ID, history)
  setState('idle')
}

export function toggleVoice() {
  if (active) stopVoice()
  else startVoice()
}

export const voiceActive = () => active

// ---------- topbar button + context menu ----------
function openVoiceMenu() {
  floatingMenu(btn, (menu) => {
    menuLabel(menu, 'Voice chat')
    menuItem(menu, { label: 'Open transcript', onClick: openTranscript })
    menuItem(menu, {
      label: 'Push-to-talk mode',
      hint: autoSend ? 'off' : 'on',
      active: !autoSend,
      onClick: () => {
        // Same contract as ChatPanel's mic: dictated text is reviewed in the
        // composer, never sent unheard.
        autoSend = !autoSend
        tome.store.set('voice-auto-send', autoSend)
      },
    })
    menuItem(menu, {
      label: 'Barge-in',
      hint: bargeIn ? 'on' : 'off',
      active: bargeIn,
      onClick: () => {
        bargeIn = !bargeIn
        tome.store.set('voice-bargein', bargeIn)
      },
    })
    menuRule(menu)
    menuItem(menu, { label: 'Voice settings…', onClick: () => preferencesModal() })
  })
}

// Wired once from renderer boot. Listeners are registered unconditionally —
// each guards on `active` + the canonical id, so an idle session costs
// nothing and events for ordinary chat panes pass through untouched.
export async function initVoice() {
  btn = document.getElementById('btn-voice')
  btn.appendChild(micIcon())
  btn.addEventListener('click', (e) => {
    e.stopPropagation()
    toggleVoice()
  })
  btn.addEventListener('contextmenu', (e) => {
    e.preventDefault()
    e.stopPropagation()
    openVoiceMenu()
  })
  // Esc stops the session from anywhere in the window — but not while the
  // user is editing text (Esc has its own field-level meanings there, and a
  // chat pane's own Esc handler covers dictation cancel).
  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' || !active) return
    const t = e.target
    if (t instanceof Element && (t.tagName === 'TEXTAREA' || t.tagName === 'INPUT')) return
    e.preventDefault()
    stopVoice()
  })
  tome.chat.onDelta(onDelta)
  tome.chat.onDone(onDone)
  tome.chat.onTool(onTool)
  autoSend = (await tome.store.get('voice-auto-send')) !== false // default true
  bargeIn = (await tome.store.get('voice-bargein')) !== false // default true
  const rate = await tome.store.get('voice-rate')
  if (typeof rate === 'number' && rate > 0) speakRate = rate
  setState('idle')
}
