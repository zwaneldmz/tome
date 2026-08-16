// Assistant chat pane: streamed replies, conductor tool chips, optional
// brain context and spoken replies.
import { tome, toast } from '../util.js'
import { renderMarkdown } from '../markdown.js'
import { chats } from '../regs.js'
import { activeWorkspace } from '../workspaces.js'
import { encodeWav } from '../../shared/wav.js'
import { loadHistory, persistHistory, flushHistory } from '../chat-history.js'
import { shouldAbortOnDispose } from '../chat-lifecycle.js'
import { voiceActive } from '../voice.js'
import { isVerbose } from '../mentor.js'

export class ChatPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-chat'
    this.element.innerHTML = `
      <div class="chat-log"></div>
      <form class="chat-form">
        <button type="button" class="chat-brain-toggle" title="Inject workspace brain context" aria-label="Inject workspace brain context" aria-pressed="false">◈ brain</button>
        <textarea rows="2" aria-label="Message the assistant" placeholder="Ask the assistant… (Enter to send · Shift+Enter newline · dictate with the 🎤 key)"></textarea>
        <button type="button" class="chat-mic" title="Push to talk — local whisper transcription (click to start/stop · Esc cancels)" aria-label="Push to talk">🎙</button>
        <button type="button" class="chat-speak" title="Speak replies aloud" aria-label="Speak replies aloud" aria-pressed="false">🔊</button>
        <button type="button" class="chat-stop hidden" title="Stop the reply (aborts the assistant's current answer)" aria-label="Stop the reply">■</button>
        <button type="submit">Send</button>
      </form>`
  }
  init({ params }) {
    this.chatId = params.chatId
    this.history = []
    this.busy = false
    this.brainOn = false
    chats.set(this.chatId, this)
    this.log = this.element.querySelector('.chat-log')
    this.input = this.element.querySelector('textarea')
    this.brainToggle = this.element.querySelector('.chat-brain-toggle')
    this.brainToggle.addEventListener('click', () => {
      this.brainOn = !this.brainOn
      this.brainToggle.classList.toggle('active', this.brainOn)
      this.brainToggle.setAttribute('aria-pressed', String(this.brainOn))
    })
    this.speak = false
    this.speakBtn = this.element.querySelector('.chat-speak')
    this.speakBtn.addEventListener('click', () => {
      this.speak = !this.speak
      this.speakBtn.classList.toggle('active', this.speak)
      this.speakBtn.setAttribute('aria-pressed', String(this.speak))
      if (!this.speak) speechSynthesis.cancel()
    })
    this.stopBtn = this.element.querySelector('.chat-stop')
    this.stopBtn.addEventListener('click', () => tome.chat.abort(this.chatId))
    this.micBtn = this.element.querySelector('.chat-mic')
    this.micBtn.addEventListener('click', () => (this.rec ? this.stopRec() : this.startRec()))
    this.element.addEventListener('keydown', (e) => {
      if (e.key === 'Escape' && this.rec) {
        e.preventDefault()
        this.stopRec(true)
      }
    })
    this.element.querySelector('form').addEventListener('submit', (e) => {
      e.preventDefault()
      this.send()
    })
    this.input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault()
        this.send()
      }
    })
    // The layout system restores a chat with the SAME chatId — a stored
    // transcript under that id is this pane's prior conversation, so replay
    // it. A brand-new chat has no log and starts empty.
    if (typeof this.chatId === 'string') this.loadHistory()
  }
  // Render the persisted transcript through the same safe paths as live
  // traffic (textContent for the user, the markdown renderer for the
  // assistant — never innerHTML).
  async loadHistory() {
    if (this.chatId == null || this.history.length) return // not a fresh panel
    const msgs = await loadHistory(this.chatId)
    if (!msgs.length) return
    this.history = msgs
    for (const m of msgs) {
      if (m.role === 'user') {
        this.bubble('me', m.content)
      } else {
        const div = this.bubble('ai', '')
        const body = document.createElement('div')
        body.className = 'md'
        div.appendChild(body)
        renderMarkdown(body, m.content)
      }
    }
  }
  persistHistory() {
    persistHistory(this.chatId, this.history)
  }
  // A turn that originated OUTSIDE this pane (the ambient voice session):
  // push it into the shared history and render it, so the pane and voice.js
  // never fork into two arrays over the same chat-log-store key. Returns
  // without rendering when the pane is mid-reply — voice turns only arrive
  // while the pane is idle (voice.js owns the busy turn), so no interleave.
  pushExternal(role, content) {
    this.history.push({ role, content })
    this.persistHistory()
    if (role === 'user') {
      this.bubble('me', content)
    } else {
      const div = this.bubble('ai', '')
      const body = document.createElement('div')
      body.className = 'md'
      div.appendChild(body)
      renderMarkdown(body, content)
    }
  }
  bubble(cls, text) {
    const div = document.createElement('div')
    div.className = 'msg ' + cls
    div.textContent = text
    this.log.appendChild(div)
    this.log.scrollTop = this.log.scrollHeight
    return div
  }
  // assistant bubbles get a markdown body div so deltas can re-render as DOM
  aiBubble() {
    this.current = this.bubble('ai', '')
    this.currentBody = document.createElement('div')
    this.currentBody.className = 'md'
    this.current.appendChild(this.currentBody)
  }
  // typing dots until the first delta + a muted elapsed-seconds readout
  // that ticks once a second until the reply finishes
  startWait() {
    this.wait = document.createElement('span')
    this.wait.className = 'chat-wait'
    this.waitDots = document.createElement('span')
    this.waitDots.className = 'chat-dots'
    for (let i = 0; i < 3; i++) this.waitDots.appendChild(document.createElement('span'))
    this.waitElapsed = document.createElement('span')
    this.waitElapsed.className = 'chat-elapsed'
    this.wait.append(this.waitDots, this.waitElapsed)
    this.waitStart = Date.now()
    this.tickWait()
    this.waitTimer = setInterval(() => this.tickWait(), 1000)
    this.current.appendChild(this.wait)
  }
  tickWait() {
    if (!this.waitElapsed) return
    const s = Math.floor((Date.now() - this.waitStart) / 1000)
    this.waitElapsed.textContent = `\u2026 ${s}s`
  }
  stopWait() {
    clearInterval(this.waitTimer)
    this.waitTimer = null
    this.wait?.remove()
    this.wait = null
    this.waitDots = null
    this.waitElapsed = null
  }
  // Push-to-talk. Raw Float32 capture + our own WAV encode, no MediaRecorder:
  // it only emits webm/opus, which whisper.cpp can't read. The transcript
  // lands in the composer and is NEVER auto-sent — the user reads what was
  // heard, edits, and presses Enter (same posture as the auto-run guard).
  async startRec() {
    if (this.rec || this.transcribing) return
    let stream
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true })
    } catch {
      toast('microphone unavailable or permission denied')
      return
    }
    // 16 kHz is what whisper.cpp expects; Chromium resamples the mic for us.
    const ctx = new AudioContext({ sampleRate: 16000 })
    const chunks = []
    // ponytail: ScriptProcessor is deprecated but needs no worklet file; swap
    // for an AudioWorklet when Chromium actually removes it.
    const proc = ctx.createScriptProcessor(4096, 1, 1)
    proc.onaudioprocess = (ev) => chunks.push(new Float32Array(ev.inputBuffer.getChannelData(0)))
    ctx.createMediaStreamSource(stream).connect(proc)
    proc.connect(ctx.destination)
    this.rec = { stream, ctx, proc, chunks }
    this.micBtn.classList.add('rec')
    // runaway guard: a forgotten open mic stops (and transcribes) itself
    this.recTimer = setTimeout(() => this.stopRec(), 120_000)
  }
  async stopRec(cancel) {
    const rec = this.rec
    if (!rec) return
    this.rec = null
    clearTimeout(this.recTimer)
    this.micBtn.classList.remove('rec')
    rec.proc.disconnect()
    for (const t of rec.stream.getTracks()) t.stop()
    // encode at the rate we actually got — a device that refused 16 kHz still
    // produces a valid WAV, and whisper's own error then says what's wrong
    const rate = rec.ctx.sampleRate
    rec.ctx.close().catch(() => {})
    if (cancel) return
    const total = rec.chunks.reduce((n, c) => n + c.length, 0)
    if (!total) return
    const samples = new Float32Array(total)
    let at = 0
    for (const c of rec.chunks) {
      samples.set(c, at)
      at += c.length
    }
    this.transcribing = true
    this.micBtn.classList.add('busy')
    try {
      const res = await tome.stt.transcribe(encodeWav(samples, rate))
      if (res?.error) toast(res.error)
      else if (!res?.text) toast('heard nothing')
      else {
        this.input.value = this.input.value ? this.input.value.trimEnd() + ' ' + res.text : res.text
        this.input.focus()
      }
    } catch (err) {
      toast('transcription failed: ' + (err?.message || err))
    } finally {
      this.transcribing = false
      this.micBtn.classList.remove('busy')
    }
  }
  send() {
    const text = this.input.value.trim()
    // `busy` also covers a voice-session turn in flight (voice.js sets it):
    // typed messages only join the shared history while voice is idle.
    if (!text || this.busy) return
    this.busy = true
    this.stopBtn.classList.remove('hidden')
    this.input.value = ''
    this.bubble('me', text)
    this.history.push({ role: 'user', content: text })
    this.persistHistory()
    this.aiBubble()
    this.startWait()
    this.currentText = ''
    this.segText = ''
    let brainWs
    if (this.brainOn) {
      const w = activeWorkspace()
      if (w) brainWs = w.name
      else toast('no workspace for brain context')
    }
    // main catches and re-signals over chat:done; this catch is the backstop
    // so a rejected invoke never dies as an unhandled rejection
    tome.chat.send(this.chatId, this.history, brainWs, isVerbose()).catch((err) => {
      this.finish(err?.message || String(err))
    })
  }
  appendDelta(text) {
    this.currentText += text
    this.segText += text
    if (this.current) {
      // first delta: the reply has arrived, swap the dots out
      if (this.waitDots) {
        this.waitDots.remove()
        this.waitDots = null
      }
      // Deltas arrive per token (dozens/sec): re-rendering markdown on every
      // one janks the main thread — and with it the speechSynthesis queue of
      // the voice session sharing this pane. Paint at most one frame per
      // animation frame; finish() always renders the complete final text.
      if (!this.deltaRaf) {
        this.deltaRaf = requestAnimationFrame(() => {
          this.deltaRaf = null
          if (!this.current) return // finish() already ran
          renderMarkdown(this.currentBody, this.segText)
          this.log.scrollTop = this.log.scrollHeight
        })
      }
    }
  }
  // a conductor tool ran between text segments: chip it, start a fresh bubble
  toolNote(tool, hint) {
    if (this.current && !this.segText) this.current.remove()
    this.bubble('tool', `⚙ ${tool}${hint ? ' · ' + hint : ''}`)
    this.aiBubble()
    // keep the elapsed readout alive across the segment break
    if (this.wait) this.current.appendChild(this.wait)
    this.segText = ''
  }
  finish(error, aborted) {
    this.busy = false
    this.stopBtn.classList.add('hidden')
    this.stopWait()
    if (this.deltaRaf) {
      cancelAnimationFrame(this.deltaRaf)
      this.deltaRaf = null
    }
    if (this.current && !this.segText) this.current.remove()
    else if (this.current) renderMarkdown(this.currentBody, this.segText)
    if (error) {
      this.bubble('err', error)
      // Roll the failed user message out of history, but hand it back to the
      // input so it can be resent instead of vanishing. On a user-initiated
      // stop the message was answered (partially) — keep it in history.
      const last = this.history.pop()
      if (!aborted && last?.role === 'user' && !this.input.value.trim()) {
        this.input.value = last.content
        this.input.focus()
      }
    } else {
      this.history.push({ role: 'assistant', content: this.currentText })
      if (this.speak && this.currentText) {
        speechSynthesis.cancel()
        speechSynthesis.speak(new SpeechSynthesisUtterance(this.currentText.slice(0, 1500)))
      }
    }
    this.persistHistory()
    this.current = null
  }
  dispose() {
    // A pane closed mid-reply used to leave its provider/tool loop running
    // headless in main with nowhere to deliver chat:delta/chat:tool/chat:done
    // — abort it here, same as the stop button, unless voice.js owns this
    // turn (TOME-015).
    if (shouldAbortOnDispose(this.busy, this.chatId, voiceActive())) tome.chat.abort(this.chatId)
    this.stopRec(true)
    this.stopWait()
    if (this.deltaRaf) {
      cancelAnimationFrame(this.deltaRaf)
      this.deltaRaf = null
    }
    // flush a pending debounced write so a quick close doesn't drop the tail
    flushHistory(this.chatId, this.history)
    chats.delete(this.chatId)
  }
}
