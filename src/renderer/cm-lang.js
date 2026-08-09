// Shared lazy loader for CodeMirror language support. Importing
// @codemirror/language-data statically pulled its ~600 kB language table
// into the entry chunk of every launch, while an editor pane is only opened
// for some sessions. Here the table loads once on first use (the individual
// language implementations stay lazy behind it, as before). Brain notes are
// always markdown, so brain.js skips the table entirely and loads the
// markdown chunk directly.
import { LanguageDescription } from '@codemirror/language'

let languagesPromise = null
const loadLanguages = () => (languagesPromise ??= import('@codemirror/language-data'))

// Resolved language extension for a filename, or [] when nothing matches.
// Cached per filename: language-data's matchFilename is a linear scan over
// ~150 descriptions, and an editor opens per file, not per keystroke.
const extCache = new Map()
export async function langForFilename(name) {
  if (!extCache.has(name)) {
    const { languages } = await loadLanguages()
    const lang = LanguageDescription.matchFilename(languages, name)
    extCache.set(
      name,
      lang ? lang.load().catch(() => []) : Promise.resolve([])
    )
  }
  return extCache.get(name)
}

// Markdown without the table — the brain pane's only language.
let mdPromise = null
export function markdownLangExt() {
  return (mdPromise ??= import('@codemirror/lang-markdown')
    .then((m) => m.markdown())
    .catch(() => []))
}
