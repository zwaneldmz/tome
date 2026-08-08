// Tiny markdown renderer for assistant chat bubbles. Model output is
// untrusted, so nothing ever goes through innerHTML: every block is built
// from DOM nodes and textContent. Supports fenced code blocks (with a copy
// button), inline code, bold, italic, h1-h3, unordered/ordered lists, and
// paragraphs with line breaks.

// Parse a run of inline markdown into DOM nodes appended to `parent`.
// Matches `code`, **bold**, *italic*, in that precedence order.
const INLINE_RE = /(`[^`]*`)|(\*\*[^*]+\*\*)|(\*[^*\n]+\*)/g

function appendInline(parent, text) {
  let last = 0
  let m
  INLINE_RE.lastIndex = 0
  while ((m = INLINE_RE.exec(text))) {
    if (m.index > last) parent.appendChild(document.createTextNode(text.slice(last, m.index)))
    const raw = m[0]
    if (m[1]) {
      const code = document.createElement('code')
      code.textContent = raw.slice(1, -1)
      parent.appendChild(code)
    } else if (m[2]) {
      const strong = document.createElement('strong')
      strong.textContent = raw.slice(2, -2)
      parent.appendChild(strong)
    } else {
      const em = document.createElement('em')
      em.textContent = raw.slice(1, -1)
      parent.appendChild(em)
    }
    last = m.index + raw.length
  }
  if (last < text.length) parent.appendChild(document.createTextNode(text.slice(last)))
}

function codeBlock(lines) {
  const pre = document.createElement('pre')
  const code = document.createElement('code')
  code.textContent = lines.join('\n')
  pre.appendChild(code)
  const btn = document.createElement('button')
  btn.type = 'button'
  btn.className = 'code-copy'
  btn.textContent = 'copy'
  btn.title = 'Copy code to clipboard'
  btn.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(code.textContent)
      btn.textContent = 'copied'
      btn.classList.add('copied')
      setTimeout(() => {
        btn.textContent = 'copy'
        btn.classList.remove('copied')
      }, 1200)
    } catch {
      btn.textContent = 'failed'
      setTimeout(() => {
        btn.textContent = 'copy'
      }, 1200)
    }
  })
  pre.appendChild(btn)
  return pre
}

// Render markdown source into `container`, replacing its contents.
// Safe to call repeatedly while a reply streams in.
export function renderMarkdown(container, src) {
  container.textContent = ''
  const lines = String(src).split('\n')
  let i = 0
  let para = [] // accumulated paragraph lines
  let list = null // current <ul>/<ol>
  let listType = null

  const flushPara = () => {
    if (!para.length) return
    const p = document.createElement('p')
    para.forEach((line, n) => {
      if (n > 0) p.appendChild(document.createElement('br'))
      appendInline(p, line)
    })
    container.appendChild(p)
    para = []
  }
  const flushList = () => {
    if (!list) return
    container.appendChild(list)
    list = null
    listType = null
  }
  const openList = (type) => {
    if (listType === type) return
    flushList()
    list = document.createElement(type)
    listType = type
  }

  while (i < lines.length) {
    const line = lines[i]
    const fence = line.match(/^```/)
    if (fence) {
      flushPara()
      flushList()
      const body = []
      i++
      while (i < lines.length && !/^```/.test(lines[i])) {
        body.push(lines[i])
        i++
      }
      i++ // skip the closing fence (or run off the end mid-stream)
      container.appendChild(codeBlock(body))
      continue
    }
    const heading = line.match(/^(#{1,3})\s+(.+)$/)
    if (heading) {
      flushPara()
      flushList()
      const h = document.createElement('h' + heading[1].length)
      appendInline(h, heading[2])
      container.appendChild(h)
      i++
      continue
    }
    const ul = line.match(/^\s*[-*]\s+(.+)$/)
    const ol = line.match(/^\s*\d+[.)]\s+(.+)$/)
    if (ul || ol) {
      flushPara()
      openList(ul ? 'ul' : 'ol')
      const li = document.createElement('li')
      appendInline(li, (ul || ol)[1])
      list.appendChild(li)
      i++
      continue
    }
    if (line.trim() === '') {
      flushPara()
      flushList()
      i++
      continue
    }
    para.push(line)
    i++
  }
  flushPara()
  flushList()
}
