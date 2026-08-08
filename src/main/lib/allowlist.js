// Provider allowlist for the air-gap proxies: hostname patterns where `*`
// matches exactly one DNS label ([a-z0-9-]+), case-insensitive, fully
// anchored — so `*.amazonaws.com` can never match `amazonaws.com.evil.com`.
// Extracted from airgap.js so the compiler is testable without module state.

export const DEFAULT_ALLOW = [
  'api.anthropic.com',
  'claude.ai',
  'console.anthropic.com',
  'statsig.anthropic.com',
  'api.openai.com',
  'auth.openai.com',
  'generativelanguage.googleapis.com',
  'oauth2.googleapis.com',
  'openrouter.ai',
  'router.requesty.ai',
  'api.deepseek.com',
  'api.moonshot.ai',
  'api.groq.com',
  'api.mistral.ai',
  'api.x.ai',
  'bedrock-runtime.*.amazonaws.com',
]

export function compileAllowlist(patterns) {
  return patterns.map((p) => {
    const re = p
      .split('*')
      .map((s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
      .join('[a-z0-9-]+')
    return new RegExp(`^${re}$`, 'i')
  })
}

// Parses the raw text of a repo's `.tome/airgap.json` into { hosts }.
// Throws on bad JSON or a non-array `allow` — main catches and treats the
// file as absent, so a malformed file can never widen the gap.
export function parseRepoAllowlist(text) {
  const cfg = JSON.parse(text)
  if (!Array.isArray(cfg.allow)) throw new Error('allow must be an array')
  return { hosts: cfg.allow }
}

// Validation for a repo's committed `.tome/airgap.json` — untrusted input,
// since anyone who can commit to the repo can edit it. The checks exist to
// stop a repo from silently punching the air gap wide open. The breadth rule
// is positional: a bare `*`, a wildcard TLD (`*.com`, `*.*`), and a leading
// wildcard with fewer than three labels (`*.co.uk` would pass — see below)
// are refused; interior single-label wildcards are allowed because the
// shipped Bedrock default (`bedrock-runtime.*.amazonaws.com`) needs one.
// There is no public-suffix awareness: `*.co.uk` is accepted, the same
// breadth class as `*.example.com` — the consent prompt is the backstop.
// Returns { ok, rejected[{pattern, reason}] } — never throws, so a hostile
// file degrades to per-entry rejections instead of breaking the boot path.
export function validateRepoAllowlist(patterns) {
  const ok = []
  const rejected = []
  const list = Array.isArray(patterns) ? patterns : []
  for (const pattern of list) {
    const reject = (reason) => rejected.push({ pattern, reason })
    if (typeof pattern !== 'string') {
      reject('not a string')
      continue
    }
    if (!pattern) {
      reject('empty pattern')
      continue
    }
    // 253 = max DNS name length; a longer pattern can never match a real
    // host and only bloats the matcher list.
    if (pattern.length > 253) {
      reject('over 253 characters')
      continue
    }
    if (/\s/.test(pattern)) {
      reject('contains whitespace')
      continue
    }
    // `://` catches schemes, `/` paths, `@` userinfo — a pattern must be a
    // bare hostname, or a proxy CONNECT target could be talked around the
    // anchored regex with URL syntax.
    if (pattern.includes('://')) {
      reject('contains a URL scheme — hostnames only')
      continue
    }
    if (pattern.includes('/')) {
      reject('contains a path — hostnames only')
      continue
    }
    if (pattern.includes('@')) {
      reject('contains userinfo — hostnames only')
      continue
    }
    const labels = pattern.split('.')
    if (labels.length < 2) {
      reject('single-label host — needs a dot (e.g. api.example.com)')
      continue
    }
    // Every label must be a literal DNS fragment or exactly `*` (one whole
    // label). Partial wildcards like `*api` compile to `[a-z0-9-]+api`,
    // which matches `evilapi` — too easy to smuggle breadth past a reader.
    const badLabel = labels.find((l) => l !== '*' && !/^[a-z0-9-]+$/i.test(l))
    if (badLabel !== undefined) {
      reject(`bad label "${badLabel}" — use * only as a whole label`)
      continue
    }
    if (pattern === '*') {
      reject('bare * matches every host')
      continue
    }
    // The last label is the effective TLD: wildcarding it (`*.com`, `*.*`)
    // matches whole TLDs, i.e. a large slice of the internet.
    if (labels[labels.length - 1] === '*') {
      reject('wildcard TLD matches whole slices of the internet')
      continue
    }
    if (labels[0] === '*' && labels.length < 3) {
      reject('wildcard base domain is too broad (e.g. *.com)')
      continue
    }
    ok.push(pattern)
  }
  return { ok, rejected }
}
