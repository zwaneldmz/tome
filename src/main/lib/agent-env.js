// The base environment every pty (agent or plain terminal) gets, before
// index.js's buildAgentEnv() layers on TERM/COLORTERM, provider secrets from
// resolveAgentSecrets(), TOME_BRAIN/TOME_CORE_VAULT, and — for gapped panes —
// the proxy vars. Extracted so the allowlist is testable without an Electron
// main process (TOME-007); index.js is the only caller.
//
// Before this, buildAgentEnv spread the ENTIRE main process environment
// ({ ...process.env, ... }) into every pty, gapped or not — so any
// launch-time value sitting in Tome's own env (TOME_SHOT, TOME_PROFILE, a
// stray CI credential) was readable by every agent CLI regardless of the
// 16-key AGENT_SECRET_KEYS filter that narrows the login-shell harvest.
// That filter only ever governed what got harvested FROM the login shell —
// it never touched this base spread.
//
// Keep this to what a shell/CLI needs to behave like a normal terminal:
// locale, terminal capabilities, and enough identity/path info to find
// binaries and a home directory. Provider credentials are added separately,
// by exact key, from resolveAgentSecrets() — never by widening this list.
export const AGENT_ENV_ALLOWLIST = new Set([
  'PATH',
  'HOME',
  'USER',
  'LOGNAME',
  'SHELL',
  'LANG',
  'TZ',
  'TMPDIR',
  'TERM',
  'COLORTERM',
])
// Whole families of locale/desktop-integration variables rather than one key
// each (LC_ALL, LC_CTYPE, LC_COLLATE, ...; XDG_CONFIG_HOME, XDG_CACHE_HOME,
// ...) — same least-privilege intent as AGENT_ENV_ALLOWLIST, just prefix-
// matched because the exact set varies by OS/desktop environment.
const AGENT_ENV_PREFIXES = ['LC_', 'XDG_']

// Pure: copies only allowlisted keys (exact match, or one of the prefixes
// above) out of `processEnv`. Callers layer overrides, secrets, and
// workspace vars onto the returned object afterward.
export function buildAgentBaseEnv(processEnv) {
  const env = {}
  for (const [key, val] of Object.entries(processEnv || {})) {
    if (AGENT_ENV_ALLOWLIST.has(key) || AGENT_ENV_PREFIXES.some((prefix) => key.startsWith(prefix)))
      env[key] = val
  }
  return env
}
