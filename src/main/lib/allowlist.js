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
