// Prettier formatting, off the main thread. Runs prettier/standalone (the
// browser-safe build — no Node fs/child_process, unlike the full `prettier`
// package the old Electron main process ran) inside a Web Worker, so a
// pathological or huge format never blocks the UI thread the way running it
// directly on the renderer's own thread would. One request/response message
// per format() call; see tome-ipc.js's fs.format for the client half of the
// wire contract and the worker's lifecycle (created lazily, on first use).
//
// Every plugin this worker can reach is a static top-level import rather
// than a dynamic one per extension: the worker itself is already only
// created lazily (see tome-ipc.js), so paying the combined plugin bundle's
// size once, off the main thread, the first time *anything* gets formatted
// is a simpler — and still cheap — tradeoff against per-language dynamic
// imports, which would also need Vite's `worker.format: 'es'` for the
// per-plugin chunks to actually code-split instead of inlining into one
// chunk anyway (Vite's default worker output format is 'iife', which has no
// module-loading mechanism of its own to split into).
import * as prettier from 'prettier/standalone'
import babel from 'prettier/plugins/babel'
import estree from 'prettier/plugins/estree'
import typescript from 'prettier/plugins/typescript'
import postcss from 'prettier/plugins/postcss'
import markdown from 'prettier/plugins/markdown'
import yaml from 'prettier/plugins/yaml'
import html from 'prettier/plugins/html'

// extension (no dot, lowercase) -> {parser, plugins}. Mirrors the
// extension-inference half of what Node's `prettier.getFileInfo` did for
// the old Electron main handler (`fmt:format`, src/main/index.js) — the
// OTHER half, `prettier.resolveConfig`'s upward filesystem walk for a
// project's own .prettierrc, has no equivalent here: `prettier/standalone`
// has no filesystem access at all (that's the whole point of the browser
// build), and hand-rolling a config-file walk over the fs bridge was cut
// from this pass's scope — noted in the phase 5a-docs task report as a
// deliberate regression against the Electron path, not an oversight.
export const JS_PLUGINS = [babel, estree]
export const TS_PLUGINS = [typescript, estree]
export const PARSER_BY_EXT = {
  js: { parser: 'babel', plugins: JS_PLUGINS },
  jsx: { parser: 'babel', plugins: JS_PLUGINS },
  mjs: { parser: 'babel', plugins: JS_PLUGINS },
  cjs: { parser: 'babel', plugins: JS_PLUGINS },
  ts: { parser: 'typescript', plugins: TS_PLUGINS },
  tsx: { parser: 'typescript', plugins: TS_PLUGINS },
  mts: { parser: 'typescript', plugins: TS_PLUGINS },
  cts: { parser: 'typescript', plugins: TS_PLUGINS },
  json: { parser: 'json', plugins: JS_PLUGINS },
  jsonc: { parser: 'jsonc', plugins: JS_PLUGINS },
  json5: { parser: 'json5', plugins: JS_PLUGINS },
  css: { parser: 'css', plugins: [postcss] },
  scss: { parser: 'scss', plugins: [postcss] },
  less: { parser: 'less', plugins: [postcss] },
  md: { parser: 'markdown', plugins: [markdown] },
  markdown: { parser: 'markdown', plugins: [markdown] },
  yml: { parser: 'yaml', plugins: [yaml] },
  yaml: { parser: 'yaml', plugins: [yaml] },
  html: { parser: 'html', plugins: [html, postcss, ...JS_PLUGINS] },
  htm: { parser: 'html', plugins: [html, postcss, ...JS_PLUGINS] },
}

// Same extraction doc.js/panes.js use: split on the last '/' then the last
// '.', lowercase, empty string when there's no extension at all.
export function inferParser(path) {
  const name = path.split('/').pop()
  const ext = (name.includes('.') ? name.split('.').pop() : '').toLowerCase()
  return PARSER_BY_EXT[ext] || null
}

// The request/response body, factored out of self.onmessage itself so it's
// callable (and testable) directly with a plain {id, path, content} object —
// no real MessageEvent, and no Worker global, required. Exported for
// test/fmt-worker.test.js; the worker wiring below is the only real caller.
export async function handleFormatRequest({ id, path, content }) {
  const info = inferParser(path)
  if (!info) {
    return { id, value: null } // no parser for this file type
  }
  try {
    const formatted = await prettier.format(content, { ...info, filepath: path })
    return { id, value: formatted }
  } catch (err) {
    // a syntax error mid-edit is normal — report it, never block the save
    return { id, value: { error: String(err.message || err).split('\n')[0] } }
  }
}

// `self` (the Web Worker global) does not exist under vitest's plain-Node
// module resolution — guarding this assignment is what lets this file be
// imported directly for its named exports above under `test/fmt-worker.test.
// js` without throwing a ReferenceError. `self` always exists in the real
// Worker context this file actually ships in (`new Worker(new URL(
// './fmt-worker.js', ...), { type: 'module' })` in tome-ipc.js), so this
// guard changes nothing about production behavior.
if (typeof self !== 'undefined') {
  self.onmessage = async (e) => {
    self.postMessage(await handleFormatRequest(e.data))
  }
}
