// Assistant chat pane: streamed replies, conductor tool chips, optional
// brain context and spoken replies.
import { tome, toast } from '../util.js'
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
  send() {
    const text = this.input.value.trim()
    if (!text || this.busy) return
    this.busy = true
    this.input.value = ''
    this.bubble('me', text)
    this.history.push({ role: 'user', content: text })
    this.current = this.bubble('ai', '')
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
      this.current.textContent = this.segText
      this.log.scrollTop = this.log.scrollHeight
    }
  }
  // a conductor tool ran between text segments: chip it, start a fresh bubble
  toolNote(tool, hint) {
    if (this.current && !this.segText) this.current.remove()
    this.bubble('tool', `⚙ ${tool}${hint ? ' · ' + hint : ''}`)
    this.current = this.bubble('ai', '')
    this.segText = ''
  }
  finish(error) {
    this.busy = false
    if (this.current && !this.segText) this.current.remove()
    if (error) {
      this.bubble('err', error)
      this.history.pop()
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
    chats.delete(this.chatId)
  }
}
