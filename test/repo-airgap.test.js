// Pins the repo-committed allowlist validator — .tome/airgap.json is
// untrusted input, so over-broad patterns must be refused with reasons.
import { describe, it, expect } from 'vitest'
import { compileAllowlist, validateRepoAllowlist } from '../src/main/lib/allowlist.js'

const okOf = (patterns) => validateRepoAllowlist(patterns).ok
const rejectedOf = (patterns) => validateRepoAllowlist(patterns).rejected

describe('validateRepoAllowlist accepts', () => {
  it.each([
    'api.example.com',
    '*.example.com',
    'bedrock-runtime.*.amazonaws.com',
    'deep.sub.domain.example.co.uk',
    'API.EXAMPLE.COM', // case-insensitive, like the compiler
  ])('valid hostname pattern: %s', (p) => {
    expect(okOf([p])).toEqual([p])
    expect(rejectedOf([p])).toEqual([])
  })

  it('keeps valid entries when mixed with invalid ones', () => {
    const r = validateRepoAllowlist(['api.example.com', '*', '*.example.com'])
    expect(r.ok).toEqual(['api.example.com', '*.example.com'])
    expect(r.rejected).toHaveLength(1)
    expect(r.rejected[0].pattern).toBe('*')
  })
})

describe('validateRepoAllowlist rejects', () => {
  it.each([
    ['*', 'bare'],
    ['*.com', 'wildcard base domain'],
    ['*.*', 'wildcard TLD'],
    ['localhost', 'single-label'],
    ['https://x.com', 'scheme'],
    ['x.com/path', 'path'],
    ['user@x.com', 'userinfo'],
    ['has space.com', 'whitespace'],
    ['tab\there.com', 'whitespace'],
    ['', 'empty'],
    ['api.example.com ', 'whitespace'],
  ])('%s (%s)', (p) => {
    expect(okOf([p])).toEqual([])
    expect(rejectedOf([p])).toHaveLength(1)
  })

  it('rejects non-strings', () => {
    const r = validateRepoAllowlist([42, null, undefined, {}, ['x.com']])
    expect(r.ok).toEqual([])
    expect(r.rejected).toHaveLength(5)
  })

  it('rejects over-long patterns (>253 chars)', () => {
    const long = `${'a'.repeat(250)}.com`
    expect(long.length).toBeGreaterThan(253)
    expect(okOf([long])).toEqual([])
    expect(rejectedOf([long])).toHaveLength(1)
  })

  it('rejects partial wildcards (*api compiles to a prefix match)', () => {
    expect(okOf(['*api.example.com'])).toEqual([])
    expect(okOf(['api*.example.com'])).toEqual([])
  })

  it('every rejection carries a human reason', () => {
    const r = validateRepoAllowlist(['*', 'localhost', 42, 'https://x.com'])
    for (const rej of r.rejected) {
      expect(typeof rej.reason).toBe('string')
      expect(rej.reason.length).toBeGreaterThan(0)
    }
  })

  it('treats a non-array input as empty, never throws', () => {
    expect(validateRepoAllowlist(null)).toEqual({ ok: [], rejected: [] })
    expect(validateRepoAllowlist('api.example.com')).toEqual({ ok: [], rejected: [] })
  })
})

// These pin the breadth boundary AS DESIGNED — not as an ideal. The rule is
// positional: a leading `*` needs ≥3 labels; interior single wildcards are
// allowed because the shipped bedrock-runtime.*.amazonaws.com default
// requires one. There is deliberately no public-suffix list.
describe('breadth boundary (pinned as-designed)', () => {
  it('accepts *.*.example.com — interior wildcards allowed', () => {
    // Matches multi-label subdomains; the same breadth class as the shipped
    // bedrock-runtime.*.amazonaws.com default.
    expect(okOf(['*.*.example.com'])).toEqual(['*.*.example.com'])
  })

  it('accepts *.co.uk — 3 labels with a leading wildcard', () => {
    // KNOWN boundary: the validator has no public-suffix list, so this is
    // the same class as *.example.com even though co.uk is a suffix.
    expect(okOf(['*.co.uk'])).toEqual(['*.co.uk'])
  })

  it('accepts a.*.com — interior wildcard', () => {
    expect(okOf(['a.*.com'])).toEqual(['a.*.com'])
  })

  it('accepts *.EXAMPLE.COM and matches case-insensitively', () => {
    expect(okOf(['*.EXAMPLE.COM'])).toEqual(['*.EXAMPLE.COM'])
    const [re] = compileAllowlist(['*.EXAMPLE.COM'])
    expect(re.test('api.example.com')).toBe(true)
    expect(re.test('API.EXAMPLE.COM')).toBe(true)
  })
})
