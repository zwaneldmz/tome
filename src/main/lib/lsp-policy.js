// The two policy decisions lsp.js makes about renderer-supplied input,
// extracted so they are testable without an Electron main process (lsp.js
// itself can't be — see its own header). lsp.js imports both and is the only
// caller; this file owns no state of its own.
import { resolve, sep } from 'node:path'

// ---- root confinement ----
// Every LSP entry point (didOpen/didChange/didClose/hover/definition) is
// driven by a path the renderer names, and the root this resolves to becomes
// the spawned server's cwd (and, via resolveServerEnv below, used to steer
// what actually gets spawned). A prior version fell back to the file's own
// directory when it wasn't inside any open folder, so a compromised renderer
// could root a language server — and its PATH — wherever it liked. Out of
// root now means refused, not "root somewhere else": mirrors index.js's
// isConfinedPath (equals-or-nested-under one of the open folders), minus the
// brain-vault allowance, which has no LSP equivalent.
export function confineToRoot(path, folders) {
  if (typeof path !== 'string' || !path) return null
  const abs = resolve(path)
  const list = Array.isArray(folders) ? folders : []
  const hit = list
    .filter((f) => typeof f === 'string' && f && (abs === f || abs.startsWith(f + sep)))
    .sort((a, b) => b.length - a.length)[0]
  return hit || null
}

// ---- executable resolution ----
// Servers are launched by bare command name (SERVERS in lsp.js) and resolved
// by the OS via PATH lookup. This used to prepend `<root>/node_modules/.bin`
// so a project-pinned server won over a global install — but root can be a
// renderer-chosen (if confined, see confineToRoot) workspace folder, so that
// prefix let a compromised renderer plant its own binary at
// `<root>/node_modules/.bin/<cmd>` and have it run in place of the real
// language server. The trusted policy is simply the process's own PATH,
// never a per-workspace override — `root` is accepted (not just available
// as a closure) so the signature itself documents that it must NOT feed
// PATH, and a test can pin that varying it never changes the result.
export function resolveServerEnv(root, baseEnv = process.env) {
  return { ...baseEnv }
}
