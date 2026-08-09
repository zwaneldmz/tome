// The agent half of the pty command line, extracted so the one rule that
// matters here can be read — and tested — without an Electron main process or
// a live pty behind it. createPty builds the command line in main precisely so
// a compromised renderer can't request arbitrary binaries or arguments (see
// the comment above 'pty:create' in index.js); the moment that line grew a
// flag derived from renderer input, the rule stopped being self-evident from
// two lines of spawn code and earned a file of its own.
//
// The invariant, stated once: every byte of the returned string is either a
// literal spelled out below or an element of one of the shared allowlist
// arrays. An incoming value is only ever COMPARED against those arrays and
// then thrown away — the string that reaches the command line is always the
// allowlist's own copy, never the one the renderer handed us that merely
// compared equal. What comes back is passed to `zsh -l -c`, i.e. it is parsed
// as a shell command line, which is what makes the character guard below
// load-bearing rather than decorative: it is the second lock on the single
// place in this app where untrusted-adjacent input and a literal get
// concatenated into something a shell will interpret.
import { AGENTS } from '../../shared/pane-kinds.js'
import { AGENT_MODELS } from '../../shared/agent-models.js'

// Every CLI in AGENT_MODELS spells the flag this way. A literal rather than a
// per-kind field because an agent whose flag differs can't be pinned at all
// until someone teaches this file about it — and until then its models list is
// empty, so no flag is ever emitted for it.
const MODEL_FLAG = '--model'

// Belt to the allowlist's braces. Every vetted value already looks like this;
// the point is that if a bad edit ever put a space, a quote or a `;` into a
// models list, this refuses to build a command line out of it rather than
// handing the login shell a second command to run.
const SAFE_MODEL = /^[a-z0-9-]+$/

// Returns the command string for `-c`, or null when the kind spawns no agent
// (a plain 'terminal' pane, or anything unrecognized) — null rather than '' so
// the caller branches on "is there a command" instead of on the emptiness of
// one.
export function buildAgentSpawn(kind, { model } = {}) {
  const at = AGENTS.indexOf(kind)
  if (at < 0) return null
  const cmd = AGENTS[at]
  // Absent is the overwhelmingly common case and the schema's only way of
  // saying "whatever the CLI defaults to" (flow-model.js), so it short-circuits
  // before any of the vetting below. '' means the same thing.
  if (!model) return cmd
  // Nothing but a string can be an alias, and a non-string is also the one
  // shape that can make the warnings below throw on their way to being
  // interpolated: `{ toString: 'haiku' }` crosses IPC intact (structured clone
  // drops functions, not string-valued properties) and has no callable
  // toString, so `${model}` on it raises. Named by type rather than by value,
  // which is all there is to say about it anyway.
  if (typeof model !== 'string') {
    console.warn(`pty: ignoring non-string model (${typeof model}) for ${cmd}; spawning on the CLI default`)
    return cmd
  }

  // Kinds whose model catalogs are resolved dynamically ship an empty list
  // (agent-models.js), so every model named for them lands here as a miss —
  // which is the intended behaviour, not an oversight.
  const models = AGENT_MODELS[cmd]?.models || []
  const found = models.indexOf(model)
  if (found < 0) {
    // Dropped, not refused: a flow pinning a model this build no longer lists
    // — a renamed alias, an older file, a hand edit — should still run on the
    // CLI's default rather than fail to spawn at all. This warning is the only
    // trace the user gets of that substitution, so it names the value.
    console.warn(`pty: ignoring model "${model}" for ${cmd} — not an allowlisted alias; spawning on the CLI default`)
    return cmd
  }
  const vetted = models[found]
  if (!SAFE_MODEL.test(vetted)) {
    // Only reachable when the allowlist itself grew an entry that isn't a bare
    // alias, i.e. a mistake in this repo rather than in anyone's flow file.
    // Worth its own wording: "your model was ignored" would send whoever reads
    // the log hunting through the wrong file.
    console.warn(`pty: allowlisted model "${vetted}" for ${cmd} is not a bare [a-z0-9-] alias — refusing to build a command line from it`)
    return cmd
  }
  return `${cmd} ${MODEL_FLAG} ${vetted}`
}
