// Assistant chat pane: streamed replies, conductor tool chips, optional
// brain context and spoken replies.
import { tome, toast } from '../util.js'
import { renderMarkdown } from '../markdown.js'
import { chats } from '../regs.js'
import { activeWorkspace } from '../workspaces.js'

export class ChatPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-chat'
    this.element.innerHTML = `
      <div class="chat-log"></div>
      <form class="chat-form">
        <button type="button" class="chat-brain-toggle" title="Inject workspace brain context">◈ brain</button>
        <textarea rows="2" placeholder="Ask the assistant… (Enter to send · Shift+Enter newline · dictate with the 🎤 key)"></textarea>
        <button type="button" class="chat-speak" title="Speak replies aloud">🔊</button>
        <button type="button" class="chat-stop hidden" title="Stop the reply (aborts the assistant's current answer)">■</button>
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
    })
    this.speak = false
    this.speakBtn = this.element.querySelector('.chat-speak')
    this.speakBtn.addEventListener('click', () => {
      this.speak = !this.speak
      this.speakBtn.classList.toggle('active', this.speak)
      if (!this.speak) speechSynthesis.cancel()
    })
    this.stopBtn = this.element.querySelector('.chat-stop')
    this.stopBtn.addEventListener('click', () => tome.chat.abort(this.chatId))
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
  send() {
    const text = this.input.value.trim()
    if (!text || this.busy) return
    this.busy = true
    this.stopBtn.classList.remove('hidden')
    this.input.value = ''
    this.bubble('me', text)
    this.history.push({ role: 'user', content: text })
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
    tome.chat.send(this.chatId, this.history, brainWs).catch((err) => {
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
      renderMarkdown(this.currentBody, this.segText)
      this.log.scrollTop = this.log.scrollHeight
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
    this.current = null
  }
  dispose() {
    this.stopWait()
    chats.delete(this.chatId)
  }
}
