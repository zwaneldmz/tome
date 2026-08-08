// Text editor pane: CodeMirror with language detection and Mod-s save.
import { basicSetup } from 'codemirror'
import { EditorView, keymap } from '@codemirror/view'
import { LanguageDescription } from '@codemirror/language'
import { languages } from '@codemirror/language-data'
import { tome } from '../util.js'
import { cmTheme } from '../theme.js'

export class EditorPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-editor'
  }
  async init({ params, api }) {
    const path = params.path
    const name = path.split('/').pop()
    let text = ''
    try {
      text = await tome.fs.readFile(path)
    } catch (err) {
      this.element.textContent = `Could not read ${path}: ${err.message}`
      return
    }
    this.savedText = text
    const lang = LanguageDescription.matchFilename(languages, name)
    const langExt = lang ? await lang.load() : []
    const save = (view) => {
      const doc = view.state.doc.toString()
      tome.fs.writeFile(path, doc).then(() => {
        this.savedText = doc
        api.setTitle(name)
      })
      return true
    }
    const theme = cmTheme()
    this.view = new EditorView({
      doc: text,
      parent: this.element,
      extensions: [
        basicSetup,
        theme.ext(),
        langExt,
        keymap.of([{ key: 'Mod-s', run: save }]),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) {
            this.dirty = this.view.state.doc.toString() !== this.savedText
            api.setTitle(this.dirty ? '● ' + name : name)
          }
        }),
      ],
    })
    this.untheme = theme.attach(this.view)
  }
  // Read by panes.js's close guard: closing a dirty editor asks first.
  isDirty() {
    return !!this.dirty
  }
  dispose() {
    this.untheme?.()
    this.view?.destroy()
  }
}
