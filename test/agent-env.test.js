// Pins buildAgentBaseEnv() (TOME-007): before it existed, index.js's
// buildAgentEnv spread the ENTIRE main-process environment into every
// pty — agent or plain terminal, gapped or not — before adding provider
// secrets on top. Any launch-time value sitting in Tome's own env (a
// screenshot path, a profiling flag, a stray credential) was therefore
// readable by every agent CLI. The tests below are the acceptance test from
// the council report: a sentinel secret in the parent environment is absent
// from the built base env, while PATH/locale/terminal settings survive.
import { describe, it, expect } from 'vitest'
import { AGENT_ENV_ALLOWLIST, buildAgentBaseEnv } from '../src/main/lib/agent-env.js'

const SENTINEL_ENV = {
  PATH: '/usr/bin:/bin:/opt/homebrew/bin',
  HOME: '/Users/tester',
  USER: 'tester',
  LOGNAME: 'tester',
  SHELL: '/bin/zsh',
  LANG: 'en_US.UTF-8',
  TZ: 'America/New_York',
  TMPDIR: '/tmp',
  TERM: 'xterm-256color',
  COLORTERM: 'truecolor',
  LC_ALL: 'en_US.UTF-8',
  LC_CTYPE: 'en_US.UTF-8',
  XDG_CONFIG_HOME: '/Users/tester/.config',
  XDG_DATA_HOME: '/Users/tester/.local/share',
  // Sentinels: must never survive into an agent's environment via the base
  // spread — only resolveAgentSecrets()'s exact-key harvest may add a
  // provider credential, and only for agent (not terminal) panes.
  TOME_SHOT: '/Users/tester/Desktop/shot.png',
  TOME_PROFILE: '1',
  SUPER_SECRET_TOKEN: 'placeholder-value-must-not-leak',
  GITHUB_TOKEN: 'placeholder-value-must-not-leak',
  AWS_SECRET_ACCESS_KEY: 'placeholder-value-must-not-leak', // provider creds come from the login shell, not here
  NPM_TOKEN: 'placeholder-value-must-not-leak',
  DIGITALOCEAN_TOKEN: 'placeholder-value-must-not-leak',
}

describe('buildAgentBaseEnv()', () => {
  const result = buildAgentBaseEnv(SENTINEL_ENV)

  it('keeps PATH, HOME, and the other exact allowlisted keys', () => {
    expect(result.PATH).toBe(SENTINEL_ENV.PATH)
    expect(result.HOME).toBe(SENTINEL_ENV.HOME)
    expect(result.USER).toBe(SENTINEL_ENV.USER)
    expect(result.LOGNAME).toBe(SENTINEL_ENV.LOGNAME)
    expect(result.SHELL).toBe(SENTINEL_ENV.SHELL)
    expect(result.LANG).toBe(SENTINEL_ENV.LANG)
    expect(result.TZ).toBe(SENTINEL_ENV.TZ)
    expect(result.TMPDIR).toBe(SENTINEL_ENV.TMPDIR)
    expect(result.TERM).toBe(SENTINEL_ENV.TERM)
    expect(result.COLORTERM).toBe(SENTINEL_ENV.COLORTERM)
  })

  it('keeps LC_* and XDG_* prefix-matched keys', () => {
    expect(result.LC_ALL).toBe(SENTINEL_ENV.LC_ALL)
    expect(result.LC_CTYPE).toBe(SENTINEL_ENV.LC_CTYPE)
    expect(result.XDG_CONFIG_HOME).toBe(SENTINEL_ENV.XDG_CONFIG_HOME)
    expect(result.XDG_DATA_HOME).toBe(SENTINEL_ENV.XDG_DATA_HOME)
  })

  it('drops every sentinel secret — the acceptance test this closes', () => {
    expect(result.TOME_SHOT).toBeUndefined()
    expect(result.TOME_PROFILE).toBeUndefined()
    expect(result.SUPER_SECRET_TOKEN).toBeUndefined()
    expect(result.GITHUB_TOKEN).toBeUndefined()
    expect(result.AWS_SECRET_ACCESS_KEY).toBeUndefined()
    expect(result.NPM_TOKEN).toBeUndefined()
    expect(result.DIGITALOCEAN_TOKEN).toBeUndefined()
    expect(Object.keys(result)).not.toContain('SUPER_SECRET_TOKEN')
  })

  it('does not prefix-match a key that merely contains LC_ or XDG_ mid-string', () => {
    const env = buildAgentBaseEnv({ MYLC_FOO: 'x', FOO_XDG_BAR: 'y', PATH: '/bin' })
    expect(env.MYLC_FOO).toBeUndefined()
    expect(env.FOO_XDG_BAR).toBeUndefined()
    expect(env.PATH).toBe('/bin')
  })

  it('returns a fresh object and does not mutate the input', () => {
    const before = { ...SENTINEL_ENV }
    expect(result).not.toBe(SENTINEL_ENV)
    expect(SENTINEL_ENV).toEqual(before)
  })

  it('handles a missing/empty environment without throwing', () => {
    expect(buildAgentBaseEnv(undefined)).toEqual({})
    expect(buildAgentBaseEnv({})).toEqual({})
  })

  it('the allowlist itself contains exactly the documented exact-match keys', () => {
    expect([...AGENT_ENV_ALLOWLIST].sort()).toEqual(
      ['PATH', 'HOME', 'USER', 'LOGNAME', 'SHELL', 'LANG', 'TZ', 'TMPDIR', 'TERM', 'COLORTERM'].sort(),
    )
  })
})
