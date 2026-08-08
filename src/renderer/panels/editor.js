// Text editor pane: CodeMirror with language detection and Mod-s save.
import { basicSetup } from 'codemirror'
import { EditorView, keymap } from '@codemirror/view'
import { EditorState, Compartment } from '@codemirror/state'
import { indentWithTab } from '@codemirror/commands'
import { LanguageDescription, indentUnit } from '@codemirror/language'
import { languages } from '@codemirror/language-data'
import { tome, toast } from '../util.js'
import { cmTheme } from '../theme.js'
import { renderStatusbar } from '../statusbar.js'
import { fileIcon } from '../icons.js'

// Every open editor, so a preference change re-skins the live ones instead of
// only applying to panes opened afterwards.
const editors = new Set()

export const EDITOR_DEFAULTS = { tabSize: 2, wrap: false, trimOnSave: true }
export const editorPrefs = { ...EDITOR_DEFAULTS }

// Compartments are identity keys, so one pair covers every view.
const wrapComp = new Compartment()
const indentComp = new Compartment()

const wrapExt = () => (editorPrefs.wrap ? EditorView.lineWrapping : [])
const indentExt = () => [
  EditorState.tabSize.of(editorPrefs.tabSize),
  indentUnit.of(' '.repeat(editorPrefs.tabSize)),
]

export async function loadEditorPrefs() {
  const saved = await tome.store.get('editor')
  if (saved && typeof saved === 'object') Object.assign(editorPrefs, saved)
}

// Persist and push to every open editor at once.
export function setEditorPrefs(patch) {
  Object.assign(editorPrefs, patch)
  tome.store.set('editor', { ...editorPrefs })
  for (const ed of editors)
    ed.view?.dispatch({
      effects: [wrapComp.reconfigure(wrapExt()), indentComp.reconfigure(indentExt())],
    })
}

// Trailing whitespace goes as a document edit rather than a string fixup, so
// what lands on disk is what the buffer shows and the pane does not stay dirty.
function trimTrailing(view) {
  const changes = []
  for (let i = 1; i <= view.state.doc.lines; i++) {
    const line = view.state.doc.line(i)
    const keep = line.text.replace(/[ \t]+$/, '').length
    if (keep !== line.text.length) changes.push({ from: line.from + keep, to: line.to })
  }
  if (changes.length) view.dispatch({ changes })
}

export class EditorPanel {
  constructor() {
    this.element = document.createElement('div')
    this.element.className = 'panel-editor'
  }
  async init({ params, api }) {
    const path = params.path
    this.path = path
    const name = path.split('/').pop()
    this.name = name
    let text = ''
    try {
      text = await tome.fs.readFile(path)
    } catch (err) {
      this.element.textContent = `Could not read ${path}: ${err.message}`
      return
    }
    this.savedText = text
    this.api = api
    const lang = LanguageDescription.matchFilename(languages, name)
    const langExt = lang ? await lang.load() : []
    const theme = cmTheme()
    this.view = new EditorView({
      doc: text,
      parent: this.element,
      extensions: [
        basicSetup,
        theme.ext(),
        langExt,
        wrapComp.of(wrapExt()),
        indentComp.of(indentExt()),
        // Tab indents. CodeMirror leaves it unbound by default so keyboard
        // users can tab out of the editor; a code pane wants the indent.
        keymap.of([indentWithTab, { key: 'Mod-s', run: () => (this.save(), true) }]),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) this.markDirty()
          // cursor/selection moves and edits both refresh the status bar
          if (u.docChanged || u.selectionSet) renderStatusbar()
        }),
      ],
    })
    this.untheme = theme.attach(this.view)
    editors.add(this)
  }

  markDirty() {
    this.dirty = this.view.state.doc.toString() !== this.savedText
    this.api?.setTitle(this.dirty ? '● ' + this.name : this.name)
  }

  async save() {
    if (!this.view) return
    if (editorPrefs.trimOnSave) trimTrailing(this.view)
    const doc = this.view.state.doc.toString()
    try {
      await tome.fs.writeFile(this.path, doc)
    } catch (err) {
      toast(`could not save ${this.name}: ${err.message}`)
      return
    }
    this.savedText = doc
    this.markDirty()
  }

  // Status bar context: file name + cursor line:col.
  statusMeta() {
    if (!this.view) return null
    const head = this.view.state.selection.main.head
    const line = this.view.state.doc.lineAt(head)
    const col = head - line.from + 1
    return {
      icon: fileIcon,
      text: `${this.name} · Ln ${line.number}, Col ${col}`,
      title: this.path,
    }
  }
  // Read by panes.js's close guard: closing a dirty editor asks first.
  isDirty() {
    return !!this.dirty
  }
  dispose() {
    editors.delete(this)
    this.untheme?.()
    this.view?.destroy()
  }
}

// Save every open editor with unsaved changes (⌘⌥S).
export async function saveAllEditors() {
  const dirty = [...editors].filter((e) => e.isDirty())
  for (const ed of dirty) await ed.save()
  return dirty.length
}
