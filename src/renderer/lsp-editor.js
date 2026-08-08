// CodeMirror side of the language-server bridge: diagnostics in the gutter,
// hover, and go-to-definition. Main owns the server processes; this file only
// translates between LSP's line/character coordinates and CodeMirror offsets.
import { setDiagnostics } from '@codemirror/lint'
import { hoverTooltip, keymap, EditorView } from '@codemirror/view'
import { tome, toast, el } from './util.js'

const SEVERITY = { 1: 'error', 2: 'warning', 3: 'info', 4: 'info' }

// LSP positions are (line, character), zero-based; CodeMirror wants a document
// offset. A stale position past the end of the doc clamps rather than throws —
// diagnostics routinely arrive a keystroke behind the buffer.
export function posOf(state, { line, character }) {
  const n = Math.min(Math.max(line + 1, 1), state.doc.lines)
  const l = state.doc.line(n)
  return Math.min(l.from + character, l.to)
}

function toCmDiagnostics(state, diagnostics) {
  return diagnostics
    .map((d) => {
      const from = posOf(state, d.range.start)
      const to = Math.max(from, posOf(state, d.range.end))
      return {
        from,
        // a zero-width diagnostic renders as nothing; widen it to one char
        to: to === from ? Math.min(from + 1, state.doc.length) : to,
        severity: SEVERITY[d.severity] || 'info',
        message: d.source ? `${d.source}: ${d.message}` : d.message,
      }
    })
    .sort((a, b) => a.from - b.from)
}

// path -> Set of EditorViews showing it. Diagnostics are pushed by path, and
// the same file can be open in more than one pane.
const views = new Map()

export function registerEditor(path, view) {
  if (!views.has(path)) views.set(path, new Set())
  views.get(path).add(view)
}
export function unregisterEditor(path, view) {
  const set = views.get(path)
  if (!set) return
  set.delete(view)
  if (!set.size) views.delete(path)
}

tome.lsp.onDiagnostics(({ path, diagnostics }) => {
  for (const view of views.get(path) || [])
    view.dispatch(setDiagnostics(view.state, toCmDiagnostics(view.state, diagnostics)))
})

// Said once per server, not per keystroke — a missing optional tool is not an
// error, it just means that language has no diagnostics.
tome.lsp.onMissing(({ cmd, langId }) =>
  toast(`no ${langId} language server — install ${cmd} for diagnostics`, 'ok')
)

// ---- hover ----
const hoverExt = (path) =>
  hoverTooltip(async (view, pos) => {
    const line = view.state.doc.lineAt(pos)
    const text = await tome.lsp.hover(path, line.number - 1, pos - line.from)
    if (!text) return null
    return {
      pos,
      create: () => {
        const dom = el('div', 'cm-lsp-hover')
        // servers send markdown; the fences and backticks add noise at this
        // size, so show the text plainly rather than half-rendering it
        dom.textContent = text.replace(/```[a-z]*\n?/gi, '').trim()
        return { dom }
      },
    }
  })

// ---- go to definition ----
// Registered by panes.js rather than imported: panes.js already imports the
// editor panel, and importing back the other way would close a module cycle.
let openFileFn = null
export const setOpenFile = (fn) => {
  openFileFn = fn
}

async function gotoDefinition(view, path) {
  const pos = view.state.selection.main.head
  const line = view.state.doc.lineAt(pos)
  const target = await tome.lsp.definition(path, line.number - 1, pos - line.from)
  if (!target) return toast('no definition found')
  openFileFn?.(target.path, undefined, undefined, {
    line: target.line,
    character: target.character,
  })
}

export function lspExtensions(path) {
  return [
    hoverExt(path),
    keymap.of([
      { key: 'F12', run: (view) => (gotoDefinition(view, path), true) },
    ]),
    EditorView.updateListener.of((u) => {
      if (u.docChanged) tome.lsp.didChange(path, u.state.doc.toString())
    }),
  ]
}
