// Document pane: pdf (Chromium's viewer), images, docx/xlsx (converted in
// main), binary fallback.
import { tome } from '../util.js'

export class DocPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-doc'
  }
  async init({ params }) {
    const { mode, path } = params
    const url = 'tome://local/?p=' + encodeURIComponent(path)
    if (mode === 'pdf') {
      const f = document.createElement('iframe')
      f.className = 'doc-frame'
      f.src = url
      this.element.appendChild(f)
    } else if (mode === 'img') {
      const wrap = document.createElement('div')
      wrap.className = 'doc-img'
      const img = document.createElement('img')
      img.src = url
      wrap.appendChild(img)
      this.element.appendChild(wrap)
    } else if (mode === 'doc') {
      try {
        const { html } = await tome.doc.read(path)
        const f = document.createElement('iframe')
        f.className = 'doc-frame'
        f.sandbox = '' // converted content: no scripts, no navigation
        f.srcdoc = html
        this.element.appendChild(f)
      } catch (err) {
        this.fallback(path, err.message)
      }
    } else {
      this.fallback(path, 'No built-in viewer for this file type.')
    }
  }
  fallback(path, why) {
    const box = document.createElement('div')
    box.className = 'doc-fallback'
    const p = document.createElement('p')
    p.textContent = why
    const b = document.createElement('button')
    b.textContent = 'Open in default app'
    b.addEventListener('click', () => tome.openPath(path))
    box.append(p, b)
    this.element.appendChild(box)
  }
}
