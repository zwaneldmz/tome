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
import { encodeWav, floatToPcm16 } from '../shared/wav.js'
import { makeVad } from '../shared/vad.js'
import { nextSpeakChunk } from '../shared/tts.js'
import { VOICE_CHAT_ID } from './chat-lifecycle.js'

// Canonical id, per the WS-A contract: the voice session and a chat pane
// opened with this id share history via the store key 'chat-log-chat-voice'.
// Defined in chat-lifecycle.js (a DOM-free module) so ChatPanel.dispose's
// abort predicate can read the same constant without importing this file's
// speechSynthesis/getUserMedia side effects.
export { VOICE_CHAT_ID }

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
let useApple = false // stream via Apple's on-device recognizer (set in initVoice)
let streamReady = false // this utterance's streaming session is live (per-utterance)
let streamedAny = false // at least one chunk actually reached the recognizer this utterance
let heardSoFar = '' // latest streaming partial, for push-to-talk live-write
let composerPrefix = '' // composer value before the current utterance began

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

// The pane that shares the voice transcript, when it's open. Voice turns render
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
// done. Choppiness guard: speechSynthesis re-attacks on EVERY queued
// utterance, so one-word sentences ("Yes.", "2.") each cost an audible gap.
// The queue below coalesces: while an utterance is playing, new text
// accumulates and ships as ONE follow-up utterance — at most two speech
// segments in flight at any time, whatever the token rate.
let speakBuf = '' // text arrived while an utterance was playing
let voiceName = null // persisted 'voice-name'; null = system default

// The default voice is whatever the OS hands Chromium first — often a
// robotic compact voice even on macOS, which reads as "terrible" no matter
// how smooth the queueing is. Prefer, in order: the user's persisted pick,
// a premium/natural en voice, any local en voice, the OS default.
function pickVoice() {
  const voices = speechSynthesis.getVoices()
  if (!voices.length) return null
  if (voiceName) {
    const chosen = voices.find((v) => v.name === voiceName)
    if (chosen) return chosen
  }
  const en = voices.filter((v) => v.lang?.startsWith('en'))
  const pool = en.length ? en : voices
  return (
    pool.find((v) => /premium|enhanced|natural|neural/i.test(v.name)) ||
    pool.find((v) => /samantha|ava|zoe|allison|susan/i.test(v.name)) ||
    pool.find((v) => v.localService) ||
    pool[0]
  )
}
// getVoices() is empty until the OS list loads; re-resolve when it arrives.
speechSynthesis.onvoiceschanged = () => pickVoice()

// Spoken text is not rendered text: code fences read as punctuation soup,
// and markdown markers get spelled out ("asterisk asterisk bold"). Strip
// down to speakable prose before anything reaches speechSynthesis.
function speakable(text) {
  return text
    .replace(/```[\s\S]*?(```|$)/g, ' code snippet. ')
    .replace(/`([^`]*)`/g, '$1')
    .replace(/^#{1,6}\s+/gm, '')
    .replace(/\*\*([^*]*)\*\*/g, '$1')
    .replace(/\*([^*]*)\*/g, '$1')
    .replace(/__([^_]*)__/g, '$1')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/^\s*[-*+]\s+/gm, '')
    .replace(/^\s*\d+\.\s+/gm, '')
    .replace(/[ \t]+/g, ' ')
    .trim()
}

function speak(text) {
  const prose = speakable(text)
  if (!prose) return
  if (speakingNow) {
    speakBuf += ' ' + prose
    return
  }
  speakingNow = true
  const u = new SpeechSynthesisUtterance(prose)
  const v = pickVoice()
  if (v) u.voice = v
  u.rate = speakRate // read per utterance, so ⌘ voice-rate applies next chunk
  u.onend = u.onerror = () => {
    speakingNow = false
    const next = speakBuf
    speakBuf = ''
    if (next.trim()) return speak(next)
    // Nothing left queued and the reply is over → back to the mic.
    if (active && state === 'speaking' && !speechSynthesis.pending) startListening()
  }
  speechSynthesis.speak(u)
}

// Complete sentences in the not-yet-spoken tail of the reply, sized by
// nextSpeakChunk (shared/tts.js) so a lone "Yes." never ships alone.
function takeSentences() {
  const chunk = nextSpeakChunk(reply.slice(spokenUpTo))
  if (!chunk) return ''
  spokenUpTo += chunk.length
  return chunk
}

function stopSpeaking() {
  speechSynthesis.cancel()
  speakingNow = false
  speakBuf = ''
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
      heardSoFar = ''
      streamReady = false
      streamedAny = false
      // The composer contents at the moment this utterance began — streaming
      // partials and the final text both rewrite `prefix + ' ' + <heard>`
      // over it, so a user's already-typed text is never clobbered.
      composerPrefix = pane()?.input?.value ?? ''
      // Stream when Apple's recognizer is the engine: open a fresh session at
      // the mic's actual rate (every utterance = one session). `begin` reports
      // failure as a RESOLVED `{ error }` value, not a rejection, so the
      // result shape is checked here — not just `.catch` — before `streamReady`
      // flips. A failed/unavailable begin degrades to the batch path.
      if (useApple) {
        tome.stt
          .begin(rec.ctx.sampleRate)
          .then((r) => {
            // The session stopped while begin was still in flight: never arm a
            // dead session, and sweep up the late-born worker (stopVoice's own
            // cancel was a no-op — no session existed yet when it ran).
            if (!active) {
              if (!r?.error) tome.stt.cancel().catch(() => {})
              return
            }
            if (r?.error) {
              useApple = false
              streamReady = false
            } else {
              streamReady = true
            }
          })
          .catch(() => {
            if (!active) return
            useApple = false
            streamReady = false
          })
      }
      // Barge-in: talking over the assistant cancels TTS and becomes the
      // next user turn. The mic is live during 'speaking' precisely so this
      // can fire.
      if (state === 'speaking' && bargeIn) {
        stopSpeaking()
        setState('listening')
      }
      // Interrupt-while-thinking: talking while the assistant is still
      // composing cancels the in-flight turn and returns to the mic. The
      // abort surfaces through onDone (aborted: true) — see below.
      if (state === 'thinking') {
        tome.chat.abort(VOICE_CHAT_ID)
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
      // Stream the live chunk to the Apple recognizer as 16-bit PCM; a
      // failed append is not worth surfacing (the utterance is already being
      // collected in `utter`, so the batch fallback still has the audio).
      // Only once the session is actually live — a begin that hasn't settled
      // (or resolved `{ error }`) must not feed a recognizer that isn't there.
      if (useApple && streamReady) {
        tome.stt.append(floatToPcm16(c)).catch(() => {})
        streamedAny = true
      }
    } else if (state === 'speaking' && bargeIn) {
      vad.push(c) // only listening for the barge-in onset
    } else if (state === 'thinking' && bargeIn) {
      vad.push(c) // also listening for the barge-in onset while composing
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
  setState('transcribing')
  let res
  if (useApple && streamReady && streamedAny) {
    // Streaming: chunks actually reached main via stt.append, so end the
    // session and take its final transcription. Decided synchronously — never
    // await beginPromise — so a begin that settles only after this endpoint
    // (e.g. the first-use TCC prompt) has streamed nothing and falls through
    // to the batch path below, instead of blocking finish() on an empty
    // request for up to 60s and dropping the text.
    try {
      res = await tome.stt.finish()
    } catch (err) {
      toast('transcription failed: ' + (err?.message || err))
      if (active) setState('listening') // mic is still open — keep the session
      return
    }
  } else {
    const samples = new Float32Array(total)
    let at = 0
    for (const c of parts) {
      samples.set(c, at)
      at += c.length
    }
    // encode at the rate actually received — a device that refused 16 kHz still
    // produces a valid WAV, and whisper's own error then says what's wrong
    const wav = encodeWav(samples, r.ctx.sampleRate)
    try {
      res = await tome.stt.transcribe(wav)
    } catch (err) {
      toast('transcription failed: ' + (err?.message || err))
      if (active) setState('listening') // mic is still open — keep the session
      return
    }
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
      // Rewrite `prefix + ' ' + <heard>` with the final text — this
      // overwrites the live partial (if any) and equals the batch path's
      // append when no partial was ever written, so one expression serves
      // both engines.
      const prefix = composerPrefix.trimEnd()
      input.value = prefix ? prefix + ' ' + text : text
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
  tome.chat.send(VOICE_CHAT_ID, history, undefined, false, undefined, true).catch((err) => {
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
    // An abort during a thinking interrupt surfaces as error "Stopped." —
    // that is a normal return to the mic, not a failure worth a toast.
    if (!aborted) toast(String(error))
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
  // A live streaming session is abandoned mid-utterance — cancel it so the
  // recognizer stops consuming audio; the result (if any) is dropped.
  if (useApple) tome.stt.cancel().catch(() => {})
  streamReady = false
  streamedAny = false
  // The pane (if open) keeps whatever partial reply it rendered — it
  // finalizes it when its own chat:done lands. Our copy flushes directly.
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
    // Voice picker: the same en voices pickVoice() ranks, the user's pick
    // persisted to 'voice-name'. System default = the automatic ranking.
    const voices = speechSynthesis.getVoices().filter((v) => v.lang?.startsWith('en'))
    if (voices.length) {
      menuLabel(menu, 'Voice')
      menuItem(menu, {
        label: 'System default',
        active: !voiceName,
        onClick: () => {
          voiceName = null
          tome.store.set('voice-name', null)
        },
      })
      for (const v of voices.slice(0, 8)) {
        menuItem(menu, {
          label: v.name,
          hint: v.localService ? '' : 'network',
          active: voiceName === v.name,
          onClick: () => {
            voiceName = v.name
            tome.store.set('voice-name', v.name)
            if (active && state === 'speaking') return // applies next utterance
            // idle: audition the pick right away
            const u = new SpeechSynthesisUtterance(`Hi — this is ${v.name}.`)
            u.voice = v
            u.rate = speakRate
            speechSynthesis.speak(u)
          },
        })
      }
      menuRule(menu)
    }
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
  // Streaming partials: arrive once per recognizer result while Apple
  // streaming is active. autoSend ignores them (the final text is what gets
  // sent); push-to-talk live-writes them into the transcript composer so the
  // user watches the dictation land. Never auto-sent here.
  tome.stt.onPartial(({ text }) => {
    if (!active) return
    heardSoFar = text || ''
    if (autoSend) return
    const input = pane()?.input
    if (!input) return
    const prefix = composerPrefix.trimEnd()
    input.value = prefix ? prefix + ' ' + heardSoFar : heardSoFar
  })
  autoSend = (await tome.store.get('voice-auto-send')) !== false // default true
  bargeIn = (await tome.store.get('voice-bargein')) !== false // default true
  const rate = await tome.store.get('voice-rate')
  if (typeof rate === 'number' && rate > 0) speakRate = rate
  voiceName = await tome.store.get('voice-name') // null = automatic ranking
  // Stream through Apple's on-device recognizer when it is the resolved
  // engine; a status failure (or a non-apple engine) just leaves the batch
  // path in charge.
  try {
    const status = await tome.stt.status()
    useApple = status?.engine === 'apple'
  } catch {
    useApple = false
  }
  setState('idle')
}
