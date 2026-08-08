// Text editor pane: CodeMirror with language detection and Mod-s save.
import { basicSetup } from 'codemirror'
import { EditorView, keymap } from '@codemirror/view'
import { EditorState, Compartment } from '@codemirror/state'
import { indentWithTab } from '@codemirror/commands'
import { LanguageDescription, indentUnit } from '@codemirror/language'
import { languages } from '@codemirror/language-data'
import { tome, toast } from '../util.js'
import { confirmModal } from '../modals.js'
import { cmTheme } from '../theme.js'
import { renderStatusbar } from '../statusbar.js'
import { fileIcon } from '../icons.js'

// Every open editor, so a preference change re-skins the live ones instead of
// only applying to panes opened afterwards.
const editors = new Set()

export const EDITOR_DEFAULTS = {
  tabSize: 2,
  wrap: false,
  trimOnSave: true,
  autosave: false,
  formatOnSave: false,
}
export const AUTOSAVE_MS = 800
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
    tome.fs.watch(path)
  }

  markDirty() {
    this.dirty = this.view.state.doc.toString() !== this.savedText
    this.api?.setTitle(this.dirty ? '● ' + this.name : this.name)
    clearTimeout(this.autosaveTimer)
    // debounced: save once typing pauses, never mid-keystroke
    if (this.dirty && editorPrefs.autosave)
      this.autosaveTimer = setTimeout(() => this.save(), AUTOSAVE_MS)
  }

  async save() {
    if (!this.view) return
    if (editorPrefs.formatOnSave) await this.format()
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
    clearTimeout(this.autosaveTimer)
    if (this.path) tome.fs.unwatch(this.path)
    this.untheme?.()
    this.view?.destroy()
  }

  // A file changed on disk. Our own save trips the watcher too, so compare
  // content rather than trying to time-window our writes — that is the only
  // check that cannot race.
  async onDiskChanged() {
    if (!this.view || this.reloading) return
    let text
    try {
      text = await tome.fs.readFile(this.path)
    } catch {
      toast(`${this.name} is no longer readable on disk`)
      return
    }
    if (text === this.savedText) return // our own write
    if (!this.dirty) return this.replaceDoc(text) // clean: just follow the file
    const ok = await confirmModal(
      'File changed on disk',
      `“${this.name}” changed outside Tome, and this pane has unsaved changes. Reloading discards them.`,
      'Reload from disk'
    )
    if (!ok) return // keep the buffer; the next save overwrites the file
    // re-read: the file may have moved on again while the prompt was up
    try {
      this.replaceDoc(await tome.fs.readFile(this.path))
    } catch {
      toast(`could not reload ${this.name}`)
    }
  }

  // Formatting lands as a document edit, so the buffer and the file agree and
  // undo still reaches the pre-format text.
  async format() {
    const before = this.view.state.doc.toString()
    let out
    try {
      out = await tome.fs.format(this.path, before)
    } catch {
      return
    }
    if (out == null) return // no parser for this file type
    if (out.error) return toast(`${this.name}: ${out.error}`)
    if (out === before) return
    const sel = this.view.state.selection.main.head
    this.view.dispatch({
      changes: { from: 0, to: this.view.state.doc.length, insert: out },
      selection: { anchor: Math.min(sel, out.length) },
    })
  }

  replaceDoc(text) {
    this.reloading = true
    const sel = this.view.state.selection.main.head
    this.view.dispatch({
      changes: { from: 0, to: this.view.state.doc.length, insert: text },
      selection: { anchor: Math.min(sel, text.length) },
    })
    this.savedText = text
    this.reloading = false
    this.markDirty()
  }
}

// One listener for every editor: main sends the path, each pane checks if it
// is the one that changed.
tome.fs.onChanged((p) => {
  for (const ed of editors) if (ed.path === p) ed.onDiskChanged()
})

// Save every open editor with unsaved changes (⌘⌥S).
export async function saveAllEditors() {
  const dirty = [...editors].filter((e) => e.isDirty())
  for (const ed of dirty) await ed.save()
  return dirty.length
}
