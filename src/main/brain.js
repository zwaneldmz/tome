// Per-workspace note vault ("brain"): Obsidian-compatible markdown with
// [[wikilinks]] and YAML-ish frontmatter. Lives outside Electron userData
// (~/Tome/Brains/<ws>) so air-gapped agent panes — whose seatbelt profile
// denies writes under userData — get full read/write with zero sandbox
// changes. Module shape mirrors airgap.js: pure functions + module state.
import { readFile, writeFile, mkdir, readdir, stat, unlink, copyFile, realpath } from 'node:fs/promises'
import { watch } from 'node:fs'
import { join, resolve, relative, dirname, basename, sep } from 'node:path'
import { homedir } from 'node:os'

export const BRAINS_ROOT = join(homedir(), 'Tome', 'Brains')
const REINDEX_DEBOUNCE_MS = 300

const AGENTS_MD = (ws) => `# AGENTS.md

This folder is the ${ws} workspace vault ($TOME_BRAIN) — a note vault, not project source. One idea per note; the H1 heading matches the filename.

## Conventions

- Frontmatter on every note:

  ---
  tags: [tag-one, tag-two]
  created: YYYY-MM-DD
  status: draft
  ---

- Lifecycle: ideas run draft → exploring → ready → promoted; tasks run active → done.
- Link related notes by wrapping a note's filename in double square brackets — matched by basename, case-insensitive.

## Cross-workspace facts

Facts that matter beyond this workspace belong in the core vault, not here. If $TOME_CORE_VAULT is set, copy the note there yourself; otherwise flag it in the Brain pane for promotion. Once a note has been copied to core, mark the local copy status: promoted.
`

const FRONTMATTER_RE = /^---\r?\n([\s\S]*?)\r?\n---/
const TAGS_RE = /^tags:\s*\[(.*)\]\s*$/m
const STATUS_RE = /^status:\s*(.+?)\s*$/m
const CREATED_RE = /^created:\s*(.+?)\s*$/m
const WIKILINK_RE = /\[\[([^\]|#]+)(?:[#|][^\]]*)?\]\]/g

let onEvent = () => {}
const cache = new Map() // ws -> index
const watchers = new Map() // ws -> fs.FSWatcher
const debounceTimers = new Map() // ws -> Timeout

export function setEventSink(fn) {
  onEvent = fn
}

// Workspace names are free text from the UI, not vetted like pty:create's
// `kind` — sanitize before using in a path. Collisions (e.g. "a/b" and "a.b"
// both -> "a_b") share a vault; accepted.
function safe(ws) {
  return String(ws).replace(/[/\\:.]/g, '_') || 'workspace'
}

// If workspace rename is ever added, this needs to migrate the folder too —
// today the vault reattaches by name, so a rename would orphan it.
function brainRoot(ws) {
  return join(BRAINS_ROOT, safe(ws))
}

async function exists(path) {
  try {
    await stat(path)
    return true
  } catch {
    return false
  }
}

export async function ensureBrain(ws) {
  const root = brainRoot(ws)
  await mkdir(root, { recursive: true })
  const agentsPath = join(root, 'AGENTS.md')
  if (!(await exists(agentsPath))) await writeFile(agentsPath, AGENTS_MD(ws))
  return root
}

async function walk(dir, out) {
  let entries
  try {
    entries = await readdir(dir, { withFileTypes: true })
  } catch {
    return
  }
  for (const e of entries) {
    if (e.name.startsWith('.')) continue
    const full = join(dir, e.name)
    if (e.isDirectory()) await walk(full, out)
    else if (e.isFile() && e.name.endsWith('.md')) out.push(full)
  }
}

function parseFrontmatter(raw) {
  const fm = raw.match(FRONTMATTER_RE)
  if (!fm) return { tags: [], status: '', created: '', body: raw }
  const block = fm[1]
  const tagsM = block.match(TAGS_RE)
  const tags = tagsM
    ? tagsM[1]
        .split(',')
        .map((t) => t.trim())
        .filter(Boolean)
    : []
  const statusM = block.match(STATUS_RE)
  const createdM = block.match(CREATED_RE)
  return {
    tags,
    status: statusM ? statusM[1] : '',
    created: createdM ? createdM[1] : '',
    body: raw.slice(fm[0].length).replace(/^\n/, ''),
  }
}

export async function buildIndex(ws) {
  const root = brainRoot(ws)
  const files = []
  await walk(root, files)
  const notes = []
  for (const full of files) {
    let raw, st
    try {
      ;[raw, st] = await Promise.all([readFile(full, 'utf8'), stat(full)])
    } catch {
      continue // deleted between walk and read — drop it from this build
    }
    const rel = relative(root, full)
    const { tags, status, created, body } = parseFrontmatter(raw)
    const links = [...raw.matchAll(WIKILINK_RE)].map((m) => m[1].trim())
    notes.push({ rel, name: basename(rel, '.md'), tags, status, created, links, body, mtime: st.mtimeMs })
  }
  // Links resolve by basename, case-insensitive; a name shared by several
  // notes is left for the consumer (wikilink nav, graph) to resolve to the
  // shallowest rel — the backlinks map itself is keyed by name, not rel.
  const backlinks = {} // lowercased link-target name -> [linking rel, ...]
  for (const n of notes) {
    for (const link of n.links) {
      const key = link.toLowerCase()
      if (key === n.name.toLowerCase()) continue
      ;(backlinks[key] ??= []).push(n.rel)
    }
  }
  const index = { root, notes, backlinks }
  cache.set(ws, index)
  return index
}

export async function getIndex(ws) {
  const cached = cache.get(ws)
  if (cached) return cached
  const index = await buildIndex(ws)
  if (!watchers.has(ws)) startWatch(ws, index.root)
  return index
}

function scheduleReindex(ws) {
  clearTimeout(debounceTimers.get(ws))
  debounceTimers.set(
    ws,
    setTimeout(async () => {
      debounceTimers.delete(ws)
      const index = await buildIndex(ws)
      onEvent(ws, index)
    }, REINDEX_DEBOUNCE_MS)
  )
}

function stopWatch(ws) {
  watchers.get(ws)?.close()
  watchers.delete(ws)
  clearTimeout(debounceTimers.get(ws))
  debounceTimers.delete(ws)
}

function startWatch(ws, root) {
  stopWatch(ws)
  let watcher
  try {
    watcher = watch(root, { recursive: true }, () => scheduleReindex(ws))
  } catch {
    return // e.g. platform without recursive fs.watch support
  }
  watcher.on('error', () => stopWatch(ws))
  watcher.on('close', () => watchers.delete(ws))
  watchers.set(ws, watcher)
}

export async function open(ws) {
  const root = await ensureBrain(ws)
  const index = await buildIndex(ws)
  startWatch(ws, root)
  return { root, index }
}

export function close(ws) {
  stopWatch(ws)
  cache.delete(ws)
}

// Shared confinement for every note/folder path derived from renderer input:
// must stay a string resolving inside `root`, no leading slash, no `..`
// segment. `requireMd` additionally demands a .md extension (notes only —
// promote's core-vault folder argument is a directory, not a note).
function confine(root, rel, requireMd) {
  if (typeof rel !== 'string') return null
  if (requireMd && !rel.endsWith('.md')) return null
  if (rel.startsWith('/')) return null
  if (rel.split(/[\\/]/).includes('..')) return null
  const full = resolve(root, rel)
  if (!full.startsWith(root + sep)) return null
  return full
}

// Lexical confinement misses symlinks: a link inside the vault pointing
// outside is followed on read/write, and brain IPC runs unsandboxed in main.
// Resolve the real path and re-check containment. The realpath'd vault root
// is cached per call so a symlinked BRAINS_ROOT itself still works.
async function confineReal(root, rel, requireMd, { mustExist = true } = {}) {
  const full = confine(root, rel, requireMd)
  if (!full) return null
  try {
    const realRoot = await realpath(root)
    if (mustExist) {
      const real = await realpath(full)
      return real.startsWith(realRoot + sep) ? full : null
    }
    // write target may not exist yet: confine the nearest existing ancestor,
    // which catches a symlink anywhere in the existing part of the path
    let dir = dirname(full)
    for (;;) {
      try {
        const realDir = await realpath(dir)
        return realDir === realRoot || realDir.startsWith(realRoot + sep) ? full : null
      } catch {
        const parent = dirname(dir)
        if (parent === dir) return null
        dir = parent
      }
    }
  } catch {
    return null
  }
}

export async function readNote(ws, rel) {
  const root = brainRoot(ws)
  const full = await confineReal(root, rel, true)
  if (!full) throw new Error('brain: path escapes vault')
  return readFile(full, 'utf8')
}

export async function writeNote(ws, rel, content, exclusive) {
  const root = brainRoot(ws)
  const full = await confineReal(root, rel, true, { mustExist: false })
  if (!full) throw new Error('brain: path escapes vault')
  await mkdir(dirname(full), { recursive: true })
  try {
    await writeFile(full, content, exclusive ? { flag: 'wx' } : undefined)
    return { ok: true }
  } catch (err) {
    if (exclusive && err.code === 'EEXIST') return { exists: true }
    throw err
  }
}

export async function deleteNote(ws, rel) {
  const root = brainRoot(ws)
  const full = await confineReal(root, rel, true)
  if (!full) throw new Error('brain: path escapes vault')
  if (basename(full).toLowerCase() === 'agents.md') throw new Error('brain: AGENTS.md is protected')
  await unlink(full)
  return { ok: true }
}

// `root` is the already-resolved core-vault path (store key `core-vault`,
// read by the caller) — this module doesn't know the store's file convention.
export async function coreInfo(root) {
  if (typeof root !== 'string' || !root) return { configured: false, root: null, folders: [] }
  try {
    const entries = await readdir(root, { withFileTypes: true })
    const folders = entries
      .filter((e) => e.isDirectory() && !e.name.startsWith('.'))
      .map((e) => e.name)
      .sort()
    return { configured: true, root, folders }
  } catch {
    return { configured: false, root, folders: [] }
  }
}

export async function promote(coreRoot, ws, rel, folder, { overwrite, rename } = {}) {
  const info = await coreInfo(coreRoot)
  if (!info.configured) throw new Error('brain: core vault not configured')
  const srcFull = await confineReal(brainRoot(ws), rel, true)
  if (!srcFull) throw new Error('brain: path escapes vault')

  let destDir = info.root
  if (folder) {
    destDir = await confineReal(info.root, folder, false, { mustExist: false })
    if (!destDir) throw new Error('brain: folder escapes core vault')
  }
  await mkdir(destDir, { recursive: true })

  let name = basename(srcFull)
  let destFull = join(destDir, name)
  if (await exists(destFull)) {
    if (rename) {
      const stem = name.slice(0, -3)
      let n = 2
      do {
        name = `${stem} ${n}.md`
        destFull = join(destDir, name)
        n++
      } while (await exists(destFull))
    } else if (!overwrite) {
      return { collision: true }
    }
  }
  await copyFile(srcFull, destFull)
  return { ok: true, rel: relative(info.root, destFull) }
}

export async function contextFor(ws, query) {
  const index = await getIndex(ws)
  const terms = [...new Set((String(query).toLowerCase().match(/[a-z0-9]{4,}/g) || []))]
  if (!terms.length) return ''
  const scored = []
  for (const note of index.notes) {
    const body = note.body.toLowerCase()
    const name = note.name.toLowerCase()
    const tags = note.tags.map((t) => t.toLowerCase())
    let score = 0
    for (const term of terms) {
      score += body.split(term).length - 1
      if (name.includes(term)) score += 3
      if (tags.some((t) => t.includes(term))) score += 5
    }
    if (score > 0) scored.push({ note, score })
  }
  if (!scored.length) return ''
  scored.sort((a, b) => b.score - a.score)
  let out = '\n\nRelevant notes from the workspace brain:\n'
  for (const { note } of scored.slice(0, 3)) {
    const budget = 4000 - out.length
    if (budget <= 0) break
    const chunk = `\n### ${note.name}\n${note.body}`
    out += chunk.length > budget ? chunk.slice(0, budget) : chunk
  }
  return out
}
