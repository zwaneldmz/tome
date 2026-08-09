// Pins flow-model.js: the id-generation scheme (lowest unused, not a
// counter), the exact edge refusals a future canvas UI must reuse verbatim
// (self-loop / dangling / duplicate port pair), the error-vs-warning split
// in validateFlow (an unrecognized kind or pinned model only warns —
// hand-edited flow.json files must still open, plan §2.2), topoSort's
// deterministic tie-break, and the literal text of the bootstrap prompt a
// flow run pastes into each spawned agent (plan §2.5), including the
// upstream handoff path an edge line must point at and the label clause it
// must leave out when the edge has no label. Also the one check here that
// reads the tree instead of a fixture: the starter flows shipped in
// examples/flows/ have to keep satisfying those same rules.
import { describe, it, expect } from 'vitest'
import { readFileSync, readdirSync } from 'node:fs'
import {
  createFlow,
  addNode,
  addEdge,
  edgeError,
  removeNode,
  validateFlow,
  topoSort,
  composeBootstrapPrompt,
  flowRoot,
  unsafeFolderName,
} from '../src/renderer/flow-model.js'

describe('createFlow', () => {
  it('returns a fresh, empty, version-1 document', () => {
    expect(createFlow('review-pipeline')).toEqual({
      version: 1,
      name: 'review-pipeline',
      nodes: [],
      edges: [],
    })
  })
})

describe('addNode', () => {
  it('assigns n1, n2, … in order and mutates flow.nodes', () => {
    const flow = createFlow('f')
    const a = addNode(flow, { kind: 'claude', name: 'A' })
    const b = addNode(flow, { kind: 'claude', name: 'B' })
    expect(a.id).toBe('n1')
    expect(b.id).toBe('n2')
    expect(flow.nodes).toEqual([a, b])
  })

  it('keeps an explicit id instead of generating one', () => {
    const flow = createFlow('f')
    const node = addNode(flow, { id: 'custom', kind: 'claude', name: 'A' })
    expect(node.id).toBe('custom')
  })

  it('reuses the lowest unused id rather than always growing', () => {
    const flow = createFlow('f')
    addNode(flow, { id: 'n1', kind: 'claude', name: 'A' })
    addNode(flow, { id: 'n2', kind: 'claude', name: 'B' })
    flow.nodes.shift() // drop n1, leaving only n2
    const reused = addNode(flow, { kind: 'claude', name: 'C' })
    expect(reused.id).toBe('n1')
  })
})

function twoNodeFlow() {
  const flow = createFlow('f')
  addNode(flow, { id: 'n1', kind: 'claude', name: 'Researcher', outputs: [{ name: 'report' }], inputs: [] })
  addNode(flow, { id: 'n2', kind: 'claude', name: 'Editor', outputs: [], inputs: [{ name: 'findings' }] })
  return flow
}

describe('addEdge / edgeError', () => {
  it('assigns e1, e2, … in order and mutates flow.edges', () => {
    const flow = twoNodeFlow()
    const result = addEdge(flow, { from: 'n1', to: 'n2', fromOutput: 'report', toInput: 'findings', label: 'x' })
    expect(result).toBeNull()
    expect(flow.edges).toHaveLength(1)
    expect(flow.edges[0].id).toBe('e1')
  })

  it('refuses a self-loop', () => {
    const flow = twoNodeFlow()
    const edge = { from: 'n1', to: 'n1', fromOutput: 'report', toInput: 'findings' }
    expect(edgeError(flow, edge)).toBe('an edge cannot connect a node to itself')
    expect(addEdge(flow, edge)).toBe('an edge cannot connect a node to itself')
    expect(flow.edges).toHaveLength(0)
  })

  it('refuses a dangling endpoint', () => {
    const flow = twoNodeFlow()
    const fromDangling = { from: 'n99', to: 'n2', fromOutput: 'report', toInput: 'findings' }
    const toDangling = { from: 'n1', to: 'n99', fromOutput: 'report', toInput: 'findings' }
    expect(edgeError(flow, fromDangling)).toBe('edge references a missing node "n99"')
    expect(edgeError(flow, toDangling)).toBe('edge references a missing node "n99"')
    expect(addEdge(flow, fromDangling)).toBe('edge references a missing node "n99"')
    expect(flow.edges).toHaveLength(0)
  })

  it('refuses a duplicate edge on the same (from, fromOutput, to, toInput) port pair', () => {
    const flow = twoNodeFlow()
    addEdge(flow, { from: 'n1', to: 'n2', fromOutput: 'report', toInput: 'findings', label: 'first' })
    const dupe = { from: 'n1', to: 'n2', fromOutput: 'report', toInput: 'findings', label: 'second' }
    expect(edgeError(flow, dupe)).toBe('an edge already connects "report" on n1 to "findings" on n2')
    expect(addEdge(flow, dupe)).toBe('an edge already connects "report" on n1 to "findings" on n2')
    expect(flow.edges).toHaveLength(1) // the duplicate never got pushed
  })

  it('edgeError never mutates the flow, whether the edge is valid or not', () => {
    const flow = twoNodeFlow()
    const edge = { from: 'n1', to: 'n2', fromOutput: 'report', toInput: 'findings' }
    expect(edgeError(flow, edge)).toBeNull()
    expect(flow.edges).toHaveLength(0)
  })

  // The duplicate check compares all four of (from, to, fromOutput, toInput).
  // A regression that drops any ONE of those four comparisons would start
  // wrongly rejecting a legitimate edge that only shares the other three —
  // each case below shares exactly 3 of 4 fields with the base edge and
  // differs in only the one named, so dropping that field's comparison (and
  // only that one) is what would turn this test red.
  function nearDuplicateFixture() {
    const flow = createFlow('f')
    addNode(flow, { id: 'n1', kind: 'claude', name: 'A', outputs: [{ name: 'report' }, { name: 'report2' }], inputs: [] })
    addNode(flow, { id: 'n2', kind: 'claude', name: 'B', outputs: [{ name: 'report' }], inputs: [] })
    addNode(flow, {
      id: 'n3',
      kind: 'claude',
      name: 'C',
      outputs: [],
      inputs: [{ name: 'findings' }, { name: 'findings2' }],
    })
    addNode(flow, { id: 'n4', kind: 'claude', name: 'D', outputs: [], inputs: [{ name: 'findings' }] })
    addEdge(flow, { from: 'n1', to: 'n3', fromOutput: 'report', toInput: 'findings', label: 'base' })
    return flow
  }

  it('does not reject a near-duplicate that differs only in "from"', () => {
    const flow = nearDuplicateFixture()
    expect(edgeError(flow, { from: 'n2', to: 'n3', fromOutput: 'report', toInput: 'findings' })).toBeNull()
  })

  it('does not reject a near-duplicate that differs only in "to"', () => {
    const flow = nearDuplicateFixture()
    expect(edgeError(flow, { from: 'n1', to: 'n4', fromOutput: 'report', toInput: 'findings' })).toBeNull()
  })

  it('does not reject a near-duplicate that differs only in "fromOutput"', () => {
    const flow = nearDuplicateFixture()
    expect(edgeError(flow, { from: 'n1', to: 'n3', fromOutput: 'report2', toInput: 'findings' })).toBeNull()
  })

  it('does not reject a near-duplicate that differs only in "toInput"', () => {
    const flow = nearDuplicateFixture()
    expect(edgeError(flow, { from: 'n1', to: 'n3', fromOutput: 'report', toInput: 'findings2' })).toBeNull()
  })
})

describe('removeNode', () => {
  it('removes the node and every edge touching it (either endpoint), keeping unrelated edges', () => {
    const flow = createFlow('f')
    addNode(flow, { id: 'n1', kind: 'claude', name: 'A', outputs: [{ name: 'o' }], inputs: [] })
    addNode(flow, { id: 'n2', kind: 'claude', name: 'B', outputs: [{ name: 'o' }], inputs: [{ name: 'i' }] })
    addNode(flow, { id: 'n3', kind: 'claude', name: 'C', outputs: [], inputs: [{ name: 'i' }] })
    addEdge(flow, { from: 'n1', to: 'n2', fromOutput: 'o', toInput: 'i', label: 'a' }) // n2 is the target
    addEdge(flow, { from: 'n2', to: 'n3', fromOutput: 'o', toInput: 'i', label: 'b' }) // n2 is the source
    addEdge(flow, { from: 'n1', to: 'n3', fromOutput: 'o', toInput: 'i', label: 'c' }) // untouched by n2's removal

    removeNode(flow, 'n2')

    expect(flow.nodes.map((n) => n.id)).toEqual(['n1', 'n3'])
    expect(flow.edges.map((e) => e.label)).toEqual(['c'])
  })

  it('is a no-op when the node id is not present', () => {
    const flow = twoNodeFlow()
    addEdge(flow, { from: 'n1', to: 'n2', fromOutput: 'report', toInput: 'findings', label: 'x' })
    removeNode(flow, 'n99')
    expect(flow.nodes).toHaveLength(2)
    expect(flow.edges).toHaveLength(1)
  })
})

describe('unsafeFolderName', () => {
  it('rejects a name containing "/" or "\\\\"', () => {
    expect(unsafeFolderName('a/b')).toBe(true)
    expect(unsafeFolderName('a\\b')).toBe(true)
  })

  it('rejects a bare ".."', () => {
    expect(unsafeFolderName('..')).toBe(true)
  })

  it('accepts an ordinary name', () => {
    expect(unsafeFolderName('review-pipeline')).toBe(false)
  })

  it('does not flag a non-string (e.g. a missing name) as unsafe', () => {
    expect(unsafeFolderName(undefined)).toBe(false)
    expect(unsafeFolderName(2)).toBe(false)
  })
})

describe('validateFlow', () => {
  function baseFlow() {
    return {
      version: 1,
      name: 'review-pipeline',
      nodes: [
        { id: 'n1', kind: 'claude', name: 'Researcher', outputs: [{ name: 'report', description: 'x' }], inputs: [] },
        { id: 'n2', kind: 'claude', name: 'Editor', outputs: [], inputs: [{ name: 'findings', description: 'y' }] },
      ],
      edges: [{ id: 'e1', from: 'n1', to: 'n2', fromOutput: 'report', toInput: 'findings', label: 'findings report' }],
    }
  }

  it('reports no errors or warnings for a valid flow', () => {
    expect(validateFlow(baseFlow())).toEqual({ errors: [], warnings: [] })
  })

  it('errors on duplicate node ids', () => {
    const flow = baseFlow()
    flow.nodes.push({ id: 'n1', kind: 'claude', name: 'Dupe', outputs: [], inputs: [] })
    expect(validateFlow(flow).errors).toContain('duplicate node id "n1"')
  })

  it('errors on an edge endpoint referencing a missing node', () => {
    const flow = baseFlow()
    flow.edges[0].to = 'n99'
    expect(validateFlow(flow).errors).toContain('edge "e1" references a missing node "n99"')
  })

  it('errors on duplicate edge ids', () => {
    const flow = baseFlow()
    flow.edges.push({ id: 'e1', from: 'n2', to: 'n1', fromOutput: 'x', toInput: 'y', label: 'back' })
    expect(validateFlow(flow).errors).toContain('duplicate edge id "e1"')
  })

  it('warns (does not error) when fromOutput is not among the source node\'s outputs', () => {
    const flow = baseFlow()
    flow.edges[0].fromOutput = 'nope'
    const result = validateFlow(flow)
    expect(result.errors).toEqual([])
    expect(result.warnings).toContain('edge "e1": "nope" is not an output of node "n1"')
  })

  it('warns (does not error) when toInput is not among the target node\'s inputs', () => {
    const flow = baseFlow()
    flow.edges[0].toInput = 'nope'
    const result = validateFlow(flow)
    expect(result.errors).toEqual([])
    expect(result.warnings).toContain('edge "e1": "nope" is not an input of node "n2"')
  })

  it('warns (does not error) on an unrecognized node kind', () => {
    const flow = baseFlow()
    flow.nodes[0].kind = 'gpt'
    const result = validateFlow(flow)
    expect(result.errors).toEqual([])
    expect(result.warnings).toContain('node "n1" has unknown kind "gpt"')
  })

  it('does not warn on the "terminal" kind — it is exempt from the unknown-kind check', () => {
    const flow = baseFlow()
    flow.nodes[0].kind = 'terminal'
    expect(validateFlow(flow)).toEqual({ errors: [], warnings: [] })
  })

  it('accepts a node with no model at all — absent means the agent CLI\'s own default', () => {
    // baseFlow pins nothing, which is the shape of every flow written before
    // the field existed and of every node the editor saves as "(default)":
    // the common case has to stay silent, or opting out would look broken.
    expect(validateFlow(baseFlow())).toEqual({ errors: [], warnings: [] })
  })

  it('treats an empty-string model as absent, not as a bad value', () => {
    // The other half of a cross-layer contract: main's spawn vetting
    // short-circuits on a falsy model before it ever consults the allowlist
    // (buildAgentSpawn, pinned in agent-spawn.test.js), so '' spawns exactly
    // the default its author asked for. If this side started treating '' as a
    // *present* value the two layers would disagree about what the empty
    // string means — a banner here calling the model unknown, and a process
    // over there running happily on the default. A tightening as small as
    // `'model' in node` in place of the falsy check is all it takes, and the
    // README has to warn people off writing '' precisely because it is a
    // spelling hand-edited files do reach for.
    const flow = baseFlow()
    flow.nodes[0].model = ''
    expect(validateFlow(flow)).toEqual({ errors: [], warnings: [] })
  })

  it('accepts a model that is on the allowlist for the node\'s kind', () => {
    const flow = baseFlow()
    flow.nodes[0].model = 'haiku'
    expect(validateFlow(flow)).toEqual({ errors: [], warnings: [] })
  })

  it('warns (does not error) on a model outside the allowlist for the node\'s kind', () => {
    const flow = baseFlow()
    flow.nodes[0].model = 'gpt-5'
    const result = validateFlow(flow)
    expect(result.errors).toEqual([])
    expect(result.warnings).toContain('node "n1" has unknown model "gpt-5" for kind "claude"')
  })

  it('warns on a model pinned to a "terminal" node — a login shell takes no model flag', () => {
    const flow = baseFlow()
    flow.nodes[0].kind = 'terminal'
    flow.nodes[0].model = 'haiku'
    const result = validateFlow(flow)
    expect(result.errors).toEqual([])
    expect(result.warnings).toContain('node "n1" has unknown model "haiku" for kind "terminal"')
  })

  it('warns on a model pinned to an unknown kind, which has no allowlist entry to look up', () => {
    // The kind that isn't in the allowlist map at all is the case a naive
    // AGENT_MODELS[kind].models lookup would throw on, taking the whole
    // validate — and with it the file's ability to open — down with it.
    const flow = baseFlow()
    flow.nodes[0].kind = 'gpt'
    flow.nodes[0].model = 'gpt-5'
    const result = validateFlow(flow)
    expect(result.errors).toEqual([])
    expect(result.warnings).toContain('node "n1" has unknown kind "gpt"')
    expect(result.warnings).toContain('node "n1" has unknown model "gpt-5" for kind "gpt"')
  })

  it('warns (does not error) on an unrecognized version', () => {
    const flow = baseFlow()
    flow.version = 2
    const result = validateFlow(flow)
    expect(result.errors).toEqual([])
    expect(result.warnings).toContain('unknown flow version "2" (expected 1)')
  })

  it('errors on a flow name that is not safe to use as a folder name', () => {
    const flow = baseFlow()
    flow.name = '../escape'
    const result = validateFlow(flow)
    expect(result.errors).toContain(
      'flow name "../escape" can\'t be used as a folder name (no "/", "\\", or "..")'
    )
  })

  it('does not error on an ordinary flow name', () => {
    expect(validateFlow(baseFlow()).errors).toEqual([])
  })
})

describe('topoSort', () => {
  it('pins the full deterministic order of a diamond graph', () => {
    const flow = createFlow('diamond')
    addNode(flow, { kind: 'claude', name: 'A', outputs: [{ name: 'o' }], inputs: [] }) // n1
    addNode(flow, { kind: 'claude', name: 'B', outputs: [{ name: 'o' }], inputs: [{ name: 'i' }] }) // n2
    addNode(flow, { kind: 'claude', name: 'C', outputs: [{ name: 'o' }], inputs: [{ name: 'i' }] }) // n3
    addNode(flow, { kind: 'claude', name: 'D', outputs: [], inputs: [{ name: 'i' }] }) // n4
    addEdge(flow, { from: 'n1', to: 'n2', fromOutput: 'o', toInput: 'i', label: 'a' })
    addEdge(flow, { from: 'n1', to: 'n3', fromOutput: 'o', toInput: 'i', label: 'b' })
    addEdge(flow, { from: 'n2', to: 'n4', fromOutput: 'o', toInput: 'i', label: 'c' })
    addEdge(flow, { from: 'n3', to: 'n4', fromOutput: 'o', toInput: 'i', label: 'd' })

    expect(topoSort(flow).map((n) => n.id)).toEqual(['n1', 'n2', 'n3', 'n4'])
  })

  it('returns null for a cyclic graph', () => {
    const flow = createFlow('cyclic')
    addNode(flow, { kind: 'claude', name: 'A', outputs: [{ name: 'o' }], inputs: [{ name: 'i' }] }) // n1
    addNode(flow, { kind: 'claude', name: 'B', outputs: [{ name: 'o' }], inputs: [{ name: 'i' }] }) // n2
    addEdge(flow, { from: 'n1', to: 'n2', fromOutput: 'o', toInput: 'i', label: 'x' })
    addEdge(flow, { from: 'n2', to: 'n1', fromOutput: 'o', toInput: 'i', label: 'y' })

    expect(topoSort(flow)).toBeNull()
  })

  it('includes disconnected nodes, in their array position', () => {
    const flow = createFlow('disconnected')
    addNode(flow, { kind: 'claude', name: 'A', outputs: [{ name: 'o' }], inputs: [] }) // n1
    addNode(flow, { kind: 'terminal', name: 'B', outputs: [], inputs: [] }) // n2 — no edges at all
    addNode(flow, { kind: 'claude', name: 'C', outputs: [], inputs: [{ name: 'i' }] }) // n3
    addEdge(flow, { from: 'n1', to: 'n3', fromOutput: 'o', toInput: 'i', label: 'z' })

    expect(topoSort(flow).map((n) => n.id)).toEqual(['n1', 'n2', 'n3'])
  })

  it('resolves ties by array insertion order, not by id string value', () => {
    const flow = createFlow('ids-out-of-lexical-order')
    // Explicit, deliberately non-lexically-sorted ids: every other test in
    // this file builds ids via addNode, which only ever produces already
    // sorted n1, n2, n3… sequences, so array order and id-string order always
    // happen to agree there. Inserted here in this order — "n2" before
    // "n10" — while lexicographically "n10" < "n2" (comparing the second
    // character, "1" < "2"), the opposite of insertion order. A regression
    // that reimplements the tie-break as "sort runnable ids as strings"
    // instead of preserving flow.nodes order would flip this pair and still
    // pass every other topoSort test here.
    addNode(flow, { id: 'n2', kind: 'claude', name: 'A', outputs: [], inputs: [] })
    addNode(flow, { id: 'n10', kind: 'claude', name: 'B', outputs: [], inputs: [] })
    expect(topoSort(flow).map((n) => n.id)).toEqual(['n2', 'n10'])
  })
})

// The shipped starters, checked against the rules above. Every mistake these
// files can carry — a port name that no longer matches its edge, a model alias
// this build no longer lists, an unrecognized kind — is a *warning*, by
// design: hand-edited flows must still open. Which means nothing else in this
// suite, the build, or the app would ever go red over a broken example; it
// would just quietly ship, and greet everyone who copies it into .tome/flows/
// (as examples/flows/README.md tells them to) with a banner on the file the
// repo advertises as the reference shape. Read off disk rather than restated
// as a fixture, because a fixture that drifts from the shipped file is the
// very bug this exists to catch.
describe('shipped example flows', () => {
  const dir = new URL('../examples/flows/', import.meta.url)
  const examples = readdirSync(dir).filter((f) => f.endsWith('.flow.json'))

  it('finds the examples the README points people at', () => {
    // it.each over an empty list is a silent pass, so the discovery gets its
    // own assertion: a renamed or moved folder has to fail loudly rather than
    // turn the checks below into zero tests.
    expect(examples.length).toBeGreaterThan(0)
  })

  it.each(examples)('%s validates clean and topo-sorts', (file) => {
    const flow = JSON.parse(readFileSync(new URL(file, dir), 'utf8'))
    expect(validateFlow(flow)).toEqual({ errors: [], warnings: [] })
    expect(topoSort(flow)).not.toBeNull() // a starter that Run refuses is not a starter
  })
})

describe('flowRoot', () => {
  it('walks back to the folder containing .tome for a flow saved under it', () => {
    expect(flowRoot('/Users/x/proj/.tome/flows/review-pipeline.flow.json')).toBe('/Users/x/proj')
  })

  it('handles .tome nested more than one level deep in the workspace', () => {
    expect(flowRoot('/a/b/c/.tome/flows/f.flow.json')).toBe('/a/b/c')
  })

  it('walks back to the CLOSEST .tome when the path contains more than one (a nested workspace)', () => {
    // A repo-in-a-repo layout: "nested" is itself a workspace root with its
    // own .tome/flows/. The outer .tome (indexOf's answer) is NOT what
    // contains this flow.json — the inner one, right before "flows/f...", is.
    expect(flowRoot('/repoA/.tome/flows/nested/.tome/flows/f.flow.json')).toBe('/repoA/.tome/flows/nested')
  })

  it('falls back to the dirname when the path never passes through .tome', () => {
    expect(flowRoot('/tmp/fixtures/f.flow.json')).toBe('/tmp/fixtures')
  })

  it('falls back to "." for a bare filename with no directory', () => {
    expect(flowRoot('f.flow.json')).toBe('.')
  })
})

describe('composeBootstrapPrompt', () => {
  function pipeline() {
    const flow = createFlow('review-pipeline')
    const researcher = addNode(flow, {
      kind: 'claude',
      name: 'Researcher',
      instructions: 'Survey the codebase for auth code.',
      expects: 'A ticket description from the user.',
      produces: 'A findings report.',
      inputs: [],
      outputs: [{ name: 'report', description: 'findings markdown' }],
    })
    const editor = addNode(flow, {
      kind: 'claude',
      name: 'Editor',
      instructions: 'Polish the report.',
      expects: 'The findings report from research.',
      produces: 'An edited report.',
      inputs: [{ name: 'findings', description: 'raw findings' }],
      outputs: [{ name: 'edited', description: 'polished report' }],
    })
    addEdge(flow, { from: 'n1', to: 'n2', fromOutput: 'report', toInput: 'findings', label: 'findings report' })
    return { flow, researcher, editor }
  }

  it('opens with the node-name + flow-name header', () => {
    const { flow, editor } = pipeline()
    expect(composeBootstrapPrompt(flow, editor)).toContain(
      'You are "Editor" in a Tome flow "review-pipeline".'
    )
  })

  it('includes an Instructions line', () => {
    const { flow, editor } = pipeline()
    expect(composeBootstrapPrompt(flow, editor)).toContain('Instructions: Polish the report.')
  })

  it('lists each incoming edge under "You receive", pointing at the upstream handoff path', () => {
    const { flow, editor } = pipeline()
    const prompt = composeBootstrapPrompt(flow, editor)
    expect(prompt).toContain('You receive:')
    expect(prompt).toContain('The findings report from research.')
    expect(prompt).toContain(
      '- "report" from Researcher: findings report (read from .tome/flows/review-pipeline/n1-report.md)'
    )
  })

  it('drops the label clause when the edge label is empty, rather than typing "undefined"', () => {
    // The labelled fixture above is the exception, not the rule: the
    // edge-drag UI writes label: '' and nothing in the app ever edits it, so
    // this is the line every real run actually pastes into the terminal.
    const { flow, editor } = pipeline()
    flow.edges[0].label = ''
    const prompt = composeBootstrapPrompt(flow, editor)
    expect(prompt).toContain('- "report" from Researcher (read from .tome/flows/review-pipeline/n1-report.md)')
    expect(prompt).not.toContain('undefined')
  })

  it('drops the label clause for an edge with no label property at all', () => {
    // The hand-written flow.json shape — the key simply isn't there.
    const { flow, editor } = pipeline()
    delete flow.edges[0].label
    const prompt = composeBootstrapPrompt(flow, editor)
    expect(prompt).toContain('- "report" from Researcher (read from .tome/flows/review-pipeline/n1-report.md)')
    expect(prompt).not.toContain('undefined')
  })

  it('has no incoming-edge line for a node with no incoming edges', () => {
    const { flow, researcher } = pipeline()
    const prompt = composeBootstrapPrompt(flow, researcher)
    expect(prompt).toContain('You receive:')
    expect(prompt).not.toContain('read from') // the marker unique to an edge-mapping line
  })

  it('lists produces + output names under "You must produce"', () => {
    const { flow, editor } = pipeline()
    const prompt = composeBootstrapPrompt(flow, editor)
    expect(prompt).toContain('You must produce:')
    expect(prompt).toContain('An edited report.')
    expect(prompt).toContain('- edited')
  })

  it('gives the handoff instruction with <nodeId>-<outputName>.md', () => {
    const { flow, editor, researcher } = pipeline()
    expect(composeBootstrapPrompt(flow, editor)).toContain(
      'Hand off "edited" by writing it to .tome/flows/review-pipeline/n2-edited.md, then tell the user when you\'re done.'
    )
    expect(composeBootstrapPrompt(flow, researcher)).toContain(
      'Hand off "report" by writing it to .tome/flows/review-pipeline/n1-report.md, then tell the user when you\'re done.'
    )
  })

  it('falls back to a generic hand-off instruction when the node declares no outputs', () => {
    // A sink/terminal node is a normal case with outputs: [] — every fixture
    // above gives both nodes at least one output, so only the per-output
    // branch is ever exercised without this test.
    const flow = createFlow('sink-flow')
    const sink = addNode(flow, {
      kind: 'claude',
      name: 'Sink',
      instructions: 'Consume everything, produce nothing.',
      expects: 'Whatever upstream sends.',
      produces: '',
      inputs: [{ name: 'in', description: 'anything' }],
      outputs: [],
    })
    expect(composeBootstrapPrompt(flow, sink)).toContain(
      'Hand off by writing each output to .tome/flows/sink-flow/n1-<output name>.md, then tell the user when you\'re done.'
    )
  })
})
