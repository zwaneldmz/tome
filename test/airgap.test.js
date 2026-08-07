// Pins the air-gap allowlist wildcard compiler — verified correctly anchored
// today (pi review §2); guards against suffix-bypass regressions.
import { describe, it, expect } from 'vitest'
import { DEFAULT_ALLOW, compileAllowlist } from '../src/main/lib/allowlist.js'

const matches = (patterns, host) => compileAllowlist(patterns).some((re) => re.test(host))

describe('wildcard hostname compiler', () => {
  const pattern = ['bedrock-runtime.*.amazonaws.com']

  it('matches a real regional endpoint', () => {
    expect(matches(pattern, 'bedrock-runtime.us-east-1.amazonaws.com')).toBe(true)
    expect(matches(pattern, 'bedrock-runtime.eu-central-1.amazonaws.com')).toBe(true)
  })

  it('rejects suffix-bypass hostnames', () => {
    expect(matches(pattern, 'bedrock-runtime.us-east-1.amazonaws.com.evil.com')).toBe(false)
  })

  it('rejects the bare suffix (wildcard must consume a label)', () => {
    expect(matches(pattern, 'amazonaws.com')).toBe(false)
    expect(matches(pattern, 'bedrock-runtime.amazonaws.com')).toBe(false)
  })

  it('wildcard does not span multiple labels', () => {
    expect(matches(pattern, 'bedrock-runtime.a.b.amazonaws.com')).toBe(false)
  })

  it('is case-insensitive', () => {
    expect(matches(pattern, 'Bedrock-Runtime.US-East-1.AmazonAWS.com')).toBe(true)
  })
})

describe('exact hosts', () => {
  it('match exactly, no more', () => {
    expect(matches(['api.anthropic.com'], 'api.anthropic.com')).toBe(true)
    expect(matches(['api.anthropic.com'], 'api.anthropic.com.evil.com')).toBe(false)
    expect(matches(['api.anthropic.com'], 'evil-api.anthropic.com')).toBe(false)
    expect(matches(['api.anthropic.com'], 'anthropic.com')).toBe(false)
    expect(matches(['api.anthropic.com'], 'apixanthropicxcom')).toBe(false)
  })
})

describe('DEFAULT_ALLOW', () => {
  it('contains the chat providers the app depends on', () => {
    expect(DEFAULT_ALLOW).toContain('router.requesty.ai')
    expect(DEFAULT_ALLOW).toContain('api.anthropic.com')
  })

  it('every default pattern compiles and matches its own literal form', () => {
    for (const p of DEFAULT_ALLOW) {
      const literal = p.replace(/\*/g, 'x')
      expect(matches([p], literal)).toBe(true)
    }
  })
})
