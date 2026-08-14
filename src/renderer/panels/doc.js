// Document pane: pdf (Chromium's viewer), images, docx/xlsx (converted
// renderer-side — see doc-convert.js), binary fallback.
import { tome } from '../util.js'
import { convertToHtml, base64ToBytes } from '../doc-convert.js'
import { isDark } from '../theme.js'

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
        // Same extraction panes.js's openFile used to pick `mode: 'doc'` in
        // the first place (CONV_EXT — docx/xlsx/xls only), so this is safe
        // by construction; doc_read_bytes enforces its own extension
        // allowlist too, so a routing bug here still can't pull bytes for
        // anything else.
        const name = path.split('/').pop()
        const ext = (name.includes('.') ? name.split('.').pop() : '').toLowerCase()
        const { base64 } = await tome.doc.readBytes(path)
        const html = await convertToHtml(ext, base64ToBytes(base64), isDark())
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
