// The conductor's flow hands: read_flow / draft_flow, extracted so the one
// invariant that matters — the model can only ever touch
// <workspaceRoot>/.tome/flows/<name>.flow.json, and only with content
// validateFlow accepts structurally — lives in a file vitest exercises with a
// real temp filesystem, not behind an Electron main process.
//
// Sync fs on purpose: flow documents are a few KB, these run once per tool
// call, and keeping runTool synchronous spares conductor.js an async rewrite.
import { readdirSync, readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs'
import { join, resolve, sep } from 'node:path'
import { validateFlow, unsafeFolderName } from '../../shared/flow-model.js'

const SUFFIX = '.flow.json'

const flowsDir = (root) => join(root, '.tome', 'flows')

// Which workspace root the flow lives under. An explicit root must be one of
// the open folders VERBATIM — compared, never resolved or prefix-matched, the
// same discipline as agent-spawn's allowlist: the path that reaches the
// filesystem is always our own copy of the string.
function pickRoot(roots, wanted) {
  if (!roots.length) return { error: 'No workspace folder is open yet — open a folder first.' }
  if (wanted == null) return { root: roots[0] }
  if (!roots.includes(wanted)) {
    return { error: `Unknown root. Open workspace folders: ${roots.join(', ')}` }
  }
  return { root: wanted }
}

// Reject before resolve — a name that carries a separator or ".." is an
// attack or a mistake either way, and sanitizing would silently write
// somewhere the user didn't name. The resolve check after it is the belt to
// that brace: even a name unsafeFolderName misjudges cannot escape the flows
// directory.
function badName(name) {
  if (typeof name !== 'string' || !name.trim()) return 'Flow name must be a non-empty string.'
  if (unsafeFolderName(name)) return `Flow name "${name}" can't contain "/", "\\", or be "..".`
  return null
}

function flowPath(root, name) {
  const dir = flowsDir(root)
  const abs = resolve(dir, name + SUFFIX)
  return abs.startsWith(resolve(dir) + sep) ? abs : null
}

// Nodes the model sent without coordinates get a left-to-right layout by
// dependency depth, so the pane shows a readable pipeline instead of a stack
// at 0,0. Depth via bounded edge relaxation — nodes.length passes — which
// simply stops improving on a cycle instead of looping forever.
// ponytail: no overlap avoidance with hand-placed nodes; drag fixes it.
function autoLayout(flow) {
  if (flow.nodes.every((n) => Number.isFinite(n.x) && Number.isFinite(n.y))) return
  const depth = new Map(flow.nodes.map((n) => [n.id, 0]))
  for (let pass = 0; pass < flow.nodes.length; pass++) {
    for (const e of flow.edges) {
      if (depth.has(e.from) && depth.has(e.to)) {
        depth.set(e.to, Math.max(depth.get(e.to), depth.get(e.from) + 1))
      }
    }
  }
  const rows = new Map() // depth -> rows already filled in that column
  for (const n of flow.nodes) {
    if (Number.isFinite(n.x) && Number.isFinite(n.y)) continue
    const d = depth.get(n.id) || 0
    const row = rows.get(d) || 0
    rows.set(d, row + 1)
    n.x = 40 + d * 300
    n.y = 40 + row * 170
  }
}

// List without a name, raw document text with one. Text rather than a parsed
// object because the tool protocol is strings anyway and the model reads
// JSON natively — re-encoding it would only launder hand-edits.
export function readFlowTool(roots, input = {}) {
  const picked = pickRoot(roots, input.root)
  if (picked.error) return picked.error

  if (input.name == null) {
    const names = []
    for (const root of input.root ? [picked.root] : roots) {
      let entries = []
      try {
        entries = readdirSync(flowsDir(root))
      } catch {
        continue // no .tome/flows yet — an empty workspace, not an error
      }
      for (const f of entries) {
        if (!f.endsWith(SUFFIX)) continue
        const name = f.slice(0, -SUFFIX.length)
        names.push(roots.length > 1 ? `${name} (in ${root})` : name)
      }
    }
    return names.length ? names.join('\n') : 'No flows exist yet.'
  }

  const bad = badName(input.name)
  if (bad) return bad
  for (const root of input.root ? [picked.root] : roots) {
    const abs = flowPath(root, input.name)
    if (abs && existsSync(abs)) return readFileSync(abs, 'utf8')
  }
  return `No flow named "${input.name}". Call read_flow without a name to list them.`
}

// Returns { text } — or { text, openPath } when the file is new, so the
// conductor can ask the renderer to open a pane for it exactly once; every
// later overwrite reaches the already-open pane through the disk watcher
// (flow.js onDiskChanged) instead.
export function draftFlowTool(roots, input = {}) {
  const bad = badName(input.name)
  if (bad) return { text: bad }
  const picked = pickRoot(roots, input.root)
  if (picked.error) return { text: picked.error }

  const flow = input.flow
  if (!flow || typeof flow !== 'object' || Array.isArray(flow)) {
    return { text: 'draft_flow needs a flow object: {version, name, nodes, edges}.' }
  }
  if (flow.nodes == null) flow.nodes = []
  if (flow.edges == null) flow.edges = []
  if (!Array.isArray(flow.nodes) || !Array.isArray(flow.edges)) {
    return { text: 'flow.nodes and flow.edges must be arrays.' }
  }
  if (flow.version == null) flow.version = 1
  // The document's own name follows the vetted filename — flow.name becomes
  // Run's handoff folder, so the two must never diverge into a state where
  // the filename passed the guard and the folder name didn't.
  flow.name = input.name

  const { errors, warnings } = validateFlow(flow)
  if (errors.length) {
    return { text: 'Refused — structural errors (nothing written):\n- ' + errors.join('\n- ') }
  }
  autoLayout(flow)

  const abs = flowPath(picked.root, input.name)
  if (!abs) return { text: `Flow name "${input.name}" does not resolve inside .tome/flows.` }
  const created = !existsSync(abs)
  mkdirSync(flowsDir(picked.root), { recursive: true })
  // Same serialization as FlowPanel.save() — the pane's onDiskChanged
  // compares text to spot its own writes, so matching the format keeps a
  // no-op overwrite from reading as a disk conflict.
  writeFileSync(abs, JSON.stringify(flow, null, 2) + '\n')

  let text = `${created ? 'Created' : 'Updated'} "${input.name}" (${flow.nodes.length} nodes, ${flow.edges.length} edges).`
  if (warnings.length) {
    text += '\nContract warnings to raise with the user:\n- ' + warnings.join('\n- ')
  }
  return created ? { text, openPath: abs } : { text }
}
