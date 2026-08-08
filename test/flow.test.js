// Pins flow-model.js: the id-generation scheme (lowest unused, not a
// counter), the exact edge refusals a future canvas UI must reuse verbatim
// (self-loop / dangling / duplicate port pair), the error-vs-warning split
// in validateFlow (hand-edited flow.json files must still open — plan
// §2.2), topoSort's deterministic tie-break, and the literal text of the
// bootstrap prompt a flow run pastes into each spawned agent (plan §2.5),
// including the upstream handoff path an edge line must point at.
import { describe, it, expect } from 'vitest'
import {
  createFlow,
  addNode,
  addEdge,
  edgeError,
  validateFlow,
  topoSort,
  composeBootstrapPrompt,
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

  it('warns (does not error) on an unrecognized version', () => {
    const flow = baseFlow()
    flow.version = 2
    const result = validateFlow(flow)
    expect(result.errors).toEqual([])
    expect(result.warnings).toContain('unknown flow version "2" (expected 1)')
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
})
