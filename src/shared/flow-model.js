// Pure data model for Flows — directed graphs of agent nodes saved as
// `<name>.flow.json` (see docs/FEATURE-PLAN-file-creation-and-flows.md §2.2).
// No DOM, no IPC: the FlowPanel (a later slice) drives the canvas, this file
// only knows the shape of the document and the rules that keep it usable.
// Pure on purpose — imports only the shared kind and model lists, so vitest
// exercises it directly and the renderer can call the exact same edge-refusal
// check the UI needs while dragging a wire between two ports.
import { AGENTS } from './pane-kinds.js'
import { AGENT_MODELS } from './agent-models.js'

export function createFlow(name) {
  return { version: 1, name, nodes: [], edges: [] }
}

// Lowest-unused integer, not `count + 1` — so deleting n2 out of [n1, n2, n3]
// and adding a node reuses "n2" instead of the id space growing forever
// (matters once flows are hand-edited and nodes get deleted/re-added a lot).
function lowestUnusedId(items, prefix) {
  const used = new Set()
  const re = new RegExp(`^${prefix}(\\d+)$`)
  for (const item of items) {
    const m = item.id != null ? re.exec(String(item.id)) : null
    if (m) used.add(Number(m[1]))
  }
  let n = 1
  while (used.has(n)) n++
  return `${prefix}${n}`
}

// Mutates `flow` in place and returns the node (with its id filled in, if it
// wasn't already set) so the caller — e.g. a "drop a new node" UI action —
// can read the generated id straight off the object it just handed in.
export function addNode(flow, node) {
  if (node.id == null) node.id = lowestUnusedId(flow.nodes, 'n')
  flow.nodes.push(node)
  return node
}

// The prospective-edge check, factored out so the UI can call it *before*
// committing a drag-drawn edge (e.g. to show a red port while hovering an
// invalid target) without mutating the flow. addEdge below is just this
// check followed by the mutation. Returns a human-readable refusal, or null
// if the edge is fine to add.
export function edgeError(flow, edge) {
  if (!edge || edge.from === edge.to) return 'an edge cannot connect a node to itself'

  const fromNode = flow.nodes.find((n) => n.id === edge.from)
  const toNode = flow.nodes.find((n) => n.id === edge.to)
  if (!fromNode) return `edge references a missing node "${edge.from}"`
  if (!toNode) return `edge references a missing node "${edge.to}"`

  const duplicate = flow.edges.some(
    (e) =>
      e.from === edge.from &&
      e.to === edge.to &&
      e.fromOutput === edge.fromOutput &&
      e.toInput === edge.toInput
  )
  if (duplicate) {
    return `an edge already connects "${edge.fromOutput}" on ${edge.from} to "${edge.toInput}" on ${edge.to}`
  }

  return null
}

// Mutates `flow` in place on success. Same in-place-id convention as
// addNode: the passed-in edge object gets its id filled before being
// pushed, so the caller keeps a handle on the generated id.
export function addEdge(flow, edge) {
  const error = edgeError(flow, edge)
  if (error) return error
  if (edge.id == null) edge.id = lowestUnusedId(flow.edges, 'e')
  flow.edges.push(edge)
  return null
}

// Cascading delete: drops the node and every edge that touches it (either
// endpoint) together, in one place. A future edit that only filters one side
// (e.g. keeps `edge.from !== nodeId` but forgets `edge.to !== nodeId`) would
// leave a stale edge referencing a now-deleted node in flow.edges; Save
// persists that straight to disk, and the next time the file opens,
// validateFlow's structural "missing node" check (an error, not a warning)
// refuses to render it at all. Living here as a pure, exported function —
// rather than as the two-line filter FlowPanel.deleteSelection used to
// inline — is what lets that invariant be pinned by a test instead of only
// ever being exercised by clicking around the canvas by hand.
// Mutates `flow` in place; a nodeId that isn't present is a no-op.
export function removeNode(flow, nodeId) {
  flow.nodes = flow.nodes.filter((n) => n.id !== nodeId)
  flow.edges = flow.edges.filter((edge) => edge.from !== nodeId && edge.to !== nodeId)
}

// A flow's `name` becomes a literal filesystem path segment for Run's
// handoff folder (`.tome/flows/<name>/`, see runFlow in flow.js and
// handoffPath below) via straight string interpolation into an fs:mkdir
// call. Flows live FLAT in .tome/flows/ (no legitimate nested name), so any
// path separator or a bare ".." would turn a single click on Run into a
// write outside the workspace the user never typed or reviewed. Exported so
// runFlow can re-check the exact same rule right at the point that actually
// touches the filesystem, instead of only trusting validateFlow's load-time
// gate below.
export function unsafeFolderName(name) {
  return typeof name === 'string' && (name === '..' || /[\\/]/.test(name))
}

// Errors vs. warnings is a hard line, not a style choice: errors mean the
// *graph* is broken (topoSort/rendering can't trust node/edge references),
// so Run must refuse them. Warnings mean only the declared *contract* is
// off (a port name that doesn't exist, a kind we don't recognize) — the
// graph itself still stands, and hand-edited flow.json files must still be
// able to open and render (plan §2.2), so those never block loading.
export function validateFlow(flow) {
  const errors = []
  const warnings = []

  if (flow.version !== 1) {
    warnings.push(`unknown flow version "${flow.version}" (expected 1)`)
  }

  // Hard error, not a warning: unlike a stale port name or an unrecognized
  // kind, an unsafe name isn't just a wrong *declared contract* — Run's
  // handoff-folder mkdir (flow.js) uses this string as-is, so letting a
  // structurally "valid" flow through with a traversal-shaped name would
  // leave nothing standing between "open a file" and "write outside the
  // workspace" except remembering not to click Run.
  if (unsafeFolderName(flow.name)) {
    errors.push(`flow name "${flow.name}" can't be used as a folder name (no "/", "\\", or "..")`)
  }

  const nodeById = new Map()
  const seenNodeIds = new Set()
  for (const node of flow.nodes) {
    if (seenNodeIds.has(node.id)) errors.push(`duplicate node id "${node.id}"`)
    seenNodeIds.add(node.id)
    nodeById.set(node.id, node)

    if (node.kind !== 'terminal' && !AGENTS.includes(node.kind)) {
      warnings.push(`node "${node.id}" has unknown kind "${node.kind}"`)
    }

    // A pinned model is a declared contract like a port name, not structure,
    // so it warns for the same reason an unknown kind does: a flow written
    // against a newer build — or by hand, against an alias this one hasn't
    // heard of — must still open and render. Nothing is lost by loading it
    // either, because main vets the value again against this same list at
    // spawn time and falls back to the CLI's default on a miss, so the worst
    // an unrecognized model costs is a node that runs on defaults. Kinds with
    // no entry here (terminal, and anything unrecognized) have no allowlist at
    // all, so any model on them warns — a plain login shell takes no --model.
    // Falsy is treated as absent rather than as a bad value: '' pins nothing
    // and spawns exactly the default the author asked for, so warning about it
    // would be a false alarm.
    if (node.model && !(AGENT_MODELS[node.kind]?.models || []).includes(node.model)) {
      warnings.push(`node "${node.id}" has unknown model "${node.model}" for kind "${node.kind}"`)
    }
  }

  const seenEdgeIds = new Set()
  for (const edge of flow.edges) {
    if (seenEdgeIds.has(edge.id)) errors.push(`duplicate edge id "${edge.id}"`)
    seenEdgeIds.add(edge.id)

    const fromNode = nodeById.get(edge.from)
    const toNode = nodeById.get(edge.to)
    if (!fromNode) errors.push(`edge "${edge.id}" references a missing node "${edge.from}"`)
    if (!toNode) errors.push(`edge "${edge.id}" references a missing node "${edge.to}"`)

    if (fromNode && !(fromNode.outputs || []).some((o) => o.name === edge.fromOutput)) {
      warnings.push(`edge "${edge.id}": "${edge.fromOutput}" is not an output of node "${edge.from}"`)
    }
    if (toNode && !(toNode.inputs || []).some((i) => i.name === edge.toInput)) {
      warnings.push(`edge "${edge.id}": "${edge.toInput}" is not an input of node "${edge.to}"`)
    }
  }

  return { errors, warnings }
}

// Kahn's algorithm. Cycles are allowed in the *model* (plan §2.2 — a
// hand-edited or in-progress graph can be cyclic) but Run must refuse to
// execute one, so this returns null instead of throwing: the caller decides
// how to surface that (a toast, in FlowPanel's case).
//
// Deterministic tie-break: nodes with no unmet dependency become runnable in
// the order they appear in flow.nodes, and a node's outgoing edges are
// walked in flow.edges order — i.e. ties resolve by insertion order, not by
// id string or any other derived ordering. Disconnected nodes have no
// incoming edges, so they're runnable from the start and fall out in their
// array position like everything else.
export function topoSort(flow) {
  const indegree = new Map()
  const outgoing = new Map()
  for (const node of flow.nodes) {
    indegree.set(node.id, 0)
    outgoing.set(node.id, [])
  }
  for (const edge of flow.edges) {
    if (!outgoing.has(edge.from) || !indegree.has(edge.to)) continue // dangling — validateFlow already flags it, ignore for ordering
    outgoing.get(edge.from).push(edge.to)
    indegree.set(edge.to, indegree.get(edge.to) + 1)
  }

  const queue = flow.nodes.filter((n) => indegree.get(n.id) === 0).map((n) => n.id)
  const order = []
  for (let head = 0; head < queue.length; head++) {
    const id = queue[head]
    order.push(id)
    for (const dest of outgoing.get(id)) {
      const remaining = indegree.get(dest) - 1
      indegree.set(dest, remaining)
      if (remaining === 0) queue.push(dest)
    }
  }

  if (order.length !== flow.nodes.length) return null // some node never reached indegree 0 — a cycle

  const nodeById = new Map(flow.nodes.map((n) => [n.id, n]))
  return order.map((id) => nodeById.get(id))
}

function handoffPath(flowName, nodeId, outputName) {
  return `.tome/flows/${flowName}/${nodeId}-${outputName}.md`
}

// Where Run spawns its terminals. composeBootstrapPrompt's handoff paths
// (".tome/flows/<name>/<node>-<output>.md") are relative to whatever folder
// contains this flow's own ".tome" — not to the flow.json's own folder,
// which is two levels deeper (".tome/flows/"). A flow saved under a
// workspace's .tome walks back up to that workspace root; a hand-placed
// flow.json that was never put under a .tome at all (e.g. a test fixture, or
// one dragged in from elsewhere) falls back to its own directory so Run
// still has *some* cwd to spawn into, even though the handoff paths it types
// won't resolve to anything meaningful in that case.
export function flowRoot(path) {
  const marker = '/.tome/'
  // lastIndexOf, not indexOf: a flow.json can sit inside a nested workspace
  // that itself lives under another .tome/flows/ (a repo-in-a-repo layout),
  // producing more than one "/.tome/" in the path. The CLOSEST one to the
  // file — not the outermost — is the .tome that actually contains this
  // flow's own .tome/flows/<name>.flow.json, which is what Run's cwd and
  // composeBootstrapPrompt's relative handoff paths need to agree with.
  const i = path.lastIndexOf(marker)
  if (i !== -1) return path.slice(0, i)
  const slash = path.lastIndexOf('/')
  return slash === -1 ? '.' : path.slice(0, slash)
}

// Builds the text pasted into a freshly spawned agent terminal when a flow
// runs (plan §2.5 step 3). File-based handoff — rather than any new IPC or
// inter-pty channel — is deliberate: it works with unmodified agent CLIs
// (they already know how to write a file), is inspectable on disk, and
// needs nothing beyond the existing pty.write. Incoming-edge lines point at
// the *upstream* node's handoff path so a downstream agent knows exactly
// which file to read instead of guessing a name.
export function composeBootstrapPrompt(flow, node) {
  const nodeById = new Map(flow.nodes.map((n) => [n.id, n]))
  const incoming = flow.edges.filter((e) => e.to === node.id)
  const outputs = node.outputs || []

  const lines = []
  lines.push(`You are "${node.name}" in a Tome flow "${flow.name}".`)
  lines.push('')
  lines.push(`Instructions: ${node.instructions || '(none given)'}`)
  lines.push('')
  lines.push('You receive:')
  lines.push(node.expects || '(nothing declared)')
  for (const edge of incoming) {
    const upstream = nodeById.get(edge.from)
    const upstreamName = upstream ? upstream.name : edge.from
    const path = handoffPath(flow.name, edge.from, edge.fromOutput)
    // The label is optional and in practice almost always missing — the
    // edge-drag UI writes label: '' (flow.js) and nothing edits it afterwards
    // — so it can't be interpolated unconditionally: that typed a literal
    // "from Researcher: undefined" at every agent in every real run, and only
    // the always-labelled test fixture hid it. Absent label, the port name and
    // the handoff path already say everything the line needs to.
    const described = edge.label ? `: ${edge.label}` : ''
    lines.push(`- "${edge.fromOutput}" from ${upstreamName}${described} (read from ${path})`)
  }
  lines.push('')
  lines.push('You must produce:')
  lines.push(node.produces || '(nothing declared)')
  for (const output of outputs) {
    lines.push(`- ${output.name}`)
  }
  lines.push('')
  if (outputs.length === 0) {
    lines.push(
      `Hand off by writing each output to .tome/flows/${flow.name}/${node.id}-<output name>.md, then tell the user when you're done.`
    )
  } else {
    for (const output of outputs) {
      const path = handoffPath(flow.name, node.id, output.name)
      lines.push(`Hand off "${output.name}" by writing it to ${path}, then tell the user when you're done.`)
    }
  }

  return lines.join('\n')
}
