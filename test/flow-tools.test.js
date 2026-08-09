// Pins flow-tools.js — the conductor's model-driven flow writes. The
// invariant under test: the model can only ever touch
// <workspaceRoot>/.tome/flows/<name>.flow.json, and only with content
// validateFlow accepts structurally. Real temp filesystem, no mocks: the
// path guard IS the feature, so it should meet real resolve()/writeFileSync
// behaviour, not a stub's idea of it.
import { describe, it, expect, beforeEach, afterEach } from 'vitest'
import { mkdtempSync, rmSync, existsSync, readFileSync, readdirSync, mkdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { readFlowTool, draftFlowTool } from '../src/main/lib/flow-tools.js'

let root
beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), 'tome-flow-tools-'))
})
afterEach(() => {
  rmSync(root, { recursive: true, force: true })
})

const flowsDir = () => join(root, '.tome', 'flows')

function validFlow() {
  return {
    version: 1,
    name: 'anything', // draftFlowTool overwrites it with the vetted name
    nodes: [
      { id: 'n1', kind: 'claude', name: 'Researcher', instructions: 'dig', expects: 'a topic', produces: 'notes', inputs: [], outputs: [{ name: 'notes' }] },
      { id: 'n2', kind: 'claude', name: 'Writer', instructions: 'write', expects: 'notes', produces: 'a draft', inputs: [{ name: 'notes' }], outputs: [] },
    ],
    edges: [{ id: 'e1', from: 'n1', to: 'n2', fromOutput: 'notes', toInput: 'notes' }],
  }
}

describe('draftFlowTool', () => {
  it('refuses everything until a workspace folder is open', () => {
    const { text } = draftFlowTool([], { name: 'x', flow: validFlow() })
    expect(text).toMatch(/workspace folder/i)
    expect(existsSync(flowsDir())).toBe(false)
  })

  it('refuses traversal-shaped names without writing anything', () => {
    for (const name of ['../escape', 'a/b', 'a\\b', '..', '', '   ', 42, null]) {
      const { text, openPath } = draftFlowTool([root], { name, flow: validFlow() })
      expect(text).toMatch(/name/i)
      expect(openPath).toBeUndefined()
    }
    expect(existsSync(flowsDir())).toBe(false)
  })

  it('refuses an explicit root that is not an open folder, verbatim', () => {
    const { text } = draftFlowTool([root], { name: 'x', flow: validFlow(), root: root + '/sub' })
    expect(text).toMatch(/unknown root/i)
    expect(existsSync(flowsDir())).toBe(false)
  })

  it('refuses non-document flow shapes', () => {
    for (const flow of [null, 'a string', ['array'], { nodes: 'nope' }]) {
      const { text } = draftFlowTool([root], { name: 'x', flow })
      expect(text).toMatch(/flow/i)
    }
    expect(existsSync(flowsDir())).toBe(false)
  })

  it('refuses structural errors without writing', () => {
    const flow = validFlow()
    flow.nodes.push({ ...flow.nodes[0] }) // duplicate node id — an error, not a warning
    const { text } = draftFlowTool([root], { name: 'dup', flow })
    expect(text).toMatch(/structural errors/i)
    expect(text).toMatch(/duplicate node id/)
    expect(existsSync(flowsDir())).toBe(false)
  })

  it('writes a valid flow in the panel save format and reports create-then-update', () => {
    const first = draftFlowTool([root], { name: 'pipeline', flow: validFlow() })
    expect(first.text).toMatch(/^Created "pipeline" \(2 nodes, 1 edges\)/)
    expect(first.openPath).toBe(join(flowsDir(), 'pipeline.flow.json'))

    const raw = readFileSync(first.openPath, 'utf8')
    expect(raw).toBe(JSON.stringify(JSON.parse(raw), null, 2) + '\n') // FlowPanel.save's exact serialization
    const doc = JSON.parse(raw)
    expect(doc.name).toBe('pipeline') // vetted filename wins over flow.name
    expect(doc.version).toBe(1)

    const second = draftFlowTool([root], { name: 'pipeline', flow: validFlow() })
    expect(second.text).toMatch(/^Updated/)
    expect(second.openPath).toBeUndefined() // pane already open; disk watcher takes it from here
  })

  it('writes despite contract warnings and returns them for the conversation', () => {
    const flow = validFlow()
    flow.nodes[0].kind = 'mystery-cli'
    const { text } = draftFlowTool([root], { name: 'warned', flow })
    expect(text).toMatch(/^Created/)
    expect(text).toMatch(/warnings to raise with the user/i)
    expect(text).toMatch(/unknown kind "mystery-cli"/)
    expect(existsSync(join(flowsDir(), 'warned.flow.json'))).toBe(true)
  })

  it('defaults version and lays out coordinate-less nodes left to right by depth', () => {
    const flow = validFlow()
    delete flow.version
    const { text } = draftFlowTool([root], { name: 'laid-out', flow })
    expect(text).toMatch(/^Created/)
    const doc = JSON.parse(readFileSync(join(flowsDir(), 'laid-out.flow.json'), 'utf8'))
    expect(doc.version).toBe(1)
    const [a, b] = doc.nodes
    for (const n of [a, b]) {
      expect(Number.isFinite(n.x)).toBe(true)
      expect(Number.isFinite(n.y)).toBe(true)
    }
    expect(b.x).toBeGreaterThan(a.x) // n2 depends on n1, so it sits a column right
  })

  it('leaves hand-placed coordinates alone', () => {
    const flow = validFlow()
    flow.nodes[0].x = 7
    flow.nodes[0].y = 9
    draftFlowTool([root], { name: 'mixed', flow })
    const doc = JSON.parse(readFileSync(join(flowsDir(), 'mixed.flow.json'), 'utf8'))
    expect(doc.nodes[0]).toMatchObject({ x: 7, y: 9 })
    expect(Number.isFinite(doc.nodes[1].x)).toBe(true)
  })
})

describe('readFlowTool', () => {
  it('refuses until a workspace folder is open', () => {
    expect(readFlowTool([], {})).toMatch(/workspace folder/i)
  })

  it('lists nothing gracefully, then lists what draft_flow wrote', () => {
    expect(readFlowTool([root], {})).toBe('No flows exist yet.')
    draftFlowTool([root], { name: 'alpha', flow: validFlow() })
    draftFlowTool([root], { name: 'beta', flow: validFlow() })
    expect(readFlowTool([root], {}).split('\n').sort()).toEqual(['alpha', 'beta'])
  })

  it('returns the raw document text by name and a hint on a miss', () => {
    draftFlowTool([root], { name: 'alpha', flow: validFlow() })
    const raw = readFlowTool([root], { name: 'alpha' })
    expect(JSON.parse(raw).name).toBe('alpha')
    expect(readFlowTool([root], { name: 'ghost' })).toMatch(/No flow named "ghost"/)
  })

  it('applies the same name guard as draft', () => {
    expect(readFlowTool([root], { name: '../etc' })).toMatch(/name/i)
  })

  it('only reads .flow.json entries', () => {
    mkdirSync(flowsDir(), { recursive: true })
    writeFileSync(join(flowsDir(), 'notes.txt'), 'not a flow')
    draftFlowTool([root], { name: 'alpha', flow: validFlow() })
    expect(readFlowTool([root], {})).toBe('alpha')
    expect(readdirSync(flowsDir()).sort()).toEqual(['alpha.flow.json', 'notes.txt'])
  })
})
