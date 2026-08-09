// Custom agent CLIs — user-declared pane kinds ('aider', 'codex', …) that
// widen the spawn allowlist by explicit user consent, WITHOUT weakening its
// invariant: main builds every command line from its own vetted copies, and
// the renderer can never supply a binary or arguments. This file is the
// vetting half of that; agent-spawn.js's buildAgentSpawnFrom is the building
// half. Electron-free so the rules can be read — and tested — without a main
// process behind them.
//
// The shape of the threat: the custom list is stored in the same JSON store
// the renderer writes through 'store:set' (index.js), and store:set is one
// of the channels open pre-login — so neither the write path nor the stored
// bytes are trustworthy. The defense is that this module is the ONLY thing
// that ever turns stored entries into spawnable kinds, and it re-vets every
// entry on every read: an entry that fails any rule below is dropped, not
// repaired, so a poisoned store degrades to "fewer kinds in the ＋ menu"
// rather than to a command line. What survives vetting is inert by
// construction: `bin` is a bare command name resolved by the login shell's
// PATH exactly the way the built-ins are (never an absolute path, never a
// renderer-supplied one), and every `args` token is a literal with no shell
// metacharacters, because the result is joined into the same `zsh -l -c`
// line the built-ins run on — the character guard here is load-bearing for
// the same reason SAFE_MODEL is in agent-spawn.js.
import { AGENTS } from '../../shared/pane-kinds.js'

// Kinds that are already spoken for: the built-in agent CLIs, plus every
// non-agent pane kind the renderer's conductor:open switch and menus treat
// as reserved words. A custom id colliding with any of these would shadow a
// built-in in the merged list (or confuse a switch that never expects an
// agent there), so it is refused outright.
const RESERVED_IDS = new Set([
  ...AGENTS,
  'terminal',
  'chat',
  'brain',
  'flow',
  'runs',
  'doc',
  'editor',
  'events',
])

// Each rule is a bare regex/length check for the same reason vetKey uses
// one: a rule you can see entire is a rule you cannot smuggle past.
const ID_RE = /^[a-z0-9][a-z0-9-]{0,31}$/
const BIN_RE = /^[a-z0-9][a-z0-9._-]{0,63}$/i // bare command name — no path separators
const MODELFLAG_RE = /^--[a-z-]{2,20}$/
const MAX_ARGS = 8
const MAX_ARG_LEN = 64
// An arg is joined into the `zsh -l -c` line verbatim, so it must be an
// inert literal: printable ASCII only (no control chars), none of the shell
// metacharacters below (they would chain commands, redirect, substitute, or
// quote their way out of being one token), and no spaces — single
// space-separated tokens only, which keeps the join auditable: the command
// line is always `bin arg arg …` and never something a shell re-parses into
// more than that.
const ARG_BAD_RE = /[^\x20-\x7e]|[;&|`$<>"'\\\s]/

// vetCustomAgent(raw) → { ok, agent } | { ok: false, error }.
// `agent` is a freshly built object holding only the vetted fields — the
// caller's copy of `raw` is read and thrown away, never carried forward,
// which is the same "compare, then pass the allowlist's own copy" posture
// agent-spawn.js takes toward model aliases.
export function vetCustomAgent(raw) {
  if (!raw || typeof raw !== 'object') return { ok: false, error: 'not an object' }
  const { id, label, bin, args, modelFlag } = raw
  if (typeof id !== 'string' || !ID_RE.test(id))
    return { ok: false, error: 'id must be 1–32 chars of [a-z0-9-], starting with a letter or digit' }
  if (RESERVED_IDS.has(id)) return { ok: false, error: `id "${id}" is a built-in pane kind` }
  if (typeof label !== 'string' || !label || label.length > 40 || !/^[\x20-\x7e]+$/.test(label))
    return { ok: false, error: 'label must be 1–40 chars of printable ASCII' }
  if (typeof bin !== 'string' || !BIN_RE.test(bin))
    return { ok: false, error: 'bin must be a bare command name (no path separators)' }
  const agent = { id, label, bin }
  if (args !== undefined) {
    if (!Array.isArray(args) || args.length > MAX_ARGS)
      return { ok: false, error: `args must be an array of at most ${MAX_ARGS} tokens` }
    for (const a of args) {
      if (typeof a !== 'string' || !a || a.length > MAX_ARG_LEN || ARG_BAD_RE.test(a))
        return {
          ok: false,
          error: `args must be single inert tokens (≤${MAX_ARG_LEN} chars, no spaces or shell metacharacters)`,
        }
    }
    if (args.length) agent.args = [...args]
  }
  if (modelFlag !== undefined) {
    if (typeof modelFlag !== 'string' || !MODELFLAG_RE.test(modelFlag))
      return { ok: false, error: 'modelFlag must look like --model (/--[a-z-]{2,20}/)' }
    agent.modelFlag = modelFlag
  }
  return { ok: true, agent }
}

// mergeAgents(builtins, customs) → the combined spawnable list as a list of
// NORMALIZED entries — { id, bin, custom } for built-ins, { id, label, bin,
// args?, modelFlag?, custom: true } for customs — so every consumer (the
// spawn builder, agents:list, the conductor's kind descriptions) reads one
// shape instead of branching on string-vs-object. Customs are re-vetted
// HERE, on the way in, so callers can hand over raw store bytes without
// trusting them — a bad entry is dropped silently (the store is
// user-editable JSON; "fewer agents than the file lists" is the correct
// failure mode, not a thrown spawn path). Duplicate custom ids keep the
// first entry, so a later duplicate can never shadow an earlier one — and no
// custom can shadow a built-in, because vetCustomAgent already refused
// those ids.
export function mergeAgents(builtins, customs) {
  const out = builtins.map((name) => ({ id: name, bin: name, custom: false }))
  const seen = new Set(builtins)
  for (const raw of Array.isArray(customs) ? customs : []) {
    const { ok, agent } = vetCustomAgent(raw)
    if (!ok || seen.has(agent.id)) continue
    seen.add(agent.id)
    out.push({ ...agent, custom: true })
  }
  return out
}
