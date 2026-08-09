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
//
// buildHeadlessSpawn at the bottom is the background-flow-run counterpart:
// same allowlist, same vetting, different shape — an argv array rather than a
// shell command line. The difference is spelled out where it matters (above
// HEADLESS), because it is the whole reason a composed brief is allowed to
// ride along there and not here.
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

// The single vetting step, shared by both builders so the pty command line
// and a background run can never disagree about which models are allowed.
// Returns the ALLOWLIST'S copy of the alias, or null to mean "spawn on the
// CLI's default" — dropped rather than refused, because a flow pinning a
// model this build no longer lists (a renamed alias, an older file, a hand
// edit) should still run rather than fail to spawn at all. `where` names the
// spawn path in the warning ('pty' / 'flow-run'), since that warning is the
// user's only trace of the substitution.
function vetModel(cmd, model, where) {
  // Nothing but a string can be an alias, and a non-string is also the one
  // shape that can make the warnings below throw on their way to being
  // interpolated: `{ toString: 'haiku' }` crosses IPC intact (structured clone
  // drops functions, not string-valued properties) and has no callable
  // toString, so `${model}` on it raises. Named by type rather than by value,
  // which is all there is to say about it anyway.
  if (typeof model !== 'string') {
    console.warn(`${where}: ignoring non-string model (${typeof model}) for ${cmd}; spawning on the CLI default`)
    return null
  }
  // Kinds whose model catalogs are resolved dynamically ship an empty list
  // (agent-models.js), so every model named for them lands here as a miss —
  // which is the intended behaviour, not an oversight.
  const models = AGENT_MODELS[cmd]?.models || []
  const found = models.indexOf(model)
  if (found < 0) {
    console.warn(`${where}: ignoring model "${model}" for ${cmd} — not an allowlisted alias; spawning on the CLI default`)
    return null
  }
  const vetted = models[found]
  if (!SAFE_MODEL.test(vetted)) {
    // Only reachable when the allowlist itself grew an entry that isn't a bare
    // alias, i.e. a mistake in this repo rather than in anyone's flow file.
    // Worth its own wording: "your model was ignored" would send whoever reads
    // the log hunting through the wrong file.
    console.warn(`${where}: allowlisted model "${vetted}" for ${cmd} is not a bare [a-z0-9-] alias — refusing to build a command line from it`)
    return null
  }
  return vetted
}

// Returns the command string for `-c`, or null when the kind spawns no agent
// (a plain 'terminal' pane, or anything unrecognized) — null rather than '' so
// the caller branches on "is there a command" instead of on the emptiness of
// one.
//
// buildAgentSpawnFrom is the generalized form: it takes the agent list to
// match against — mergeAgents' normalized entries (custom-agents.js) — so
// main can resolve kind against built-ins PLUS vetted custom CLIs per spawn
// without this file ever reading the store. The invariant at the top of the
// file is unchanged: an incoming kind is only ever COMPARED against the list
// and then thrown away, and the string that reaches the command line is the
// list's own copies — the entry's bin, its (character-guarded) args, the
// allowlist's model alias — never a byte the renderer handed us.
export function buildAgentSpawnFrom(list, kind, { model } = {}) {
  const entry = (Array.isArray(list) ? list : []).find((e) => e.id === kind)
  if (!entry) return null
  // The entry's own bin, never the caller's kind string — for built-ins they
  // spell the same word, and for customs the bin is what vetCustomAgent
  // already proved is a bare command name. Args ride along verbatim: they
  // were vetted as inert single tokens (no spaces, no shell metacharacters)
  // at the custom-agents.js door, which is the only reason joining them here
  // is safe.
  const base = [entry.bin, ...(entry.args || [])].join(' ')
  // Absent is the overwhelmingly common case and the schema's only way of
  // saying "whatever the CLI defaults to" (flow-model.js), so it short-circuits
  // before any of the vetting below. '' means the same thing.
  if (!model) return base
  // Model pinning needs BOTH halves: the kind must declare which flag its
  // CLI takes (customs only get one by declaring modelFlag, and AGENT_MODELS
  // only lists aliases for kinds that speak --model) AND the model must be
  // on the shared allowlist for the kind. Customs start with empty model
  // lists — the same posture as opencode/pi — so a pin on a custom lands in
  // vetModel as an ordinary miss and is dropped to the CLI's default.
  const flag = entry.custom ? entry.modelFlag : MODEL_FLAG
  if (!flag) {
    console.warn(`pty: ignoring model "${typeof model === 'string' ? model : typeof model}" for ${entry.bin} — no model flag declared; spawning on the CLI default`)
    return base
  }
  const vetted = vetModel(entry.id, model, 'pty')
  if (!vetted) return base
  return `${base} ${flag} ${vetted}`
}

// The built-in wrapper: every pre-customs caller (and the existing test
// suite) keeps spelling it this way. AGENTS is mapped to the normalized
// shape mergeAgents emits for built-ins, so both spellings of the list agree
// exactly.
export function buildAgentSpawn(kind, opts) {
  return buildAgentSpawnFrom(
    AGENTS.map((name) => ({ id: name, bin: name, custom: false })),
    kind,
    opts
  )
}

// ---- headless (background flow runs) ----
// Per-kind template for running an agent NON-interactively: one prompt in,
// one answer out, process exits. Only claude in v1 — a kind with no entry
// here is not backgroundable, buildHeadlessSpawn returns null for it, and the
// runner refuses the whole run naming the node rather than half-executing a
// pipeline (the user can still Run in terminals). Teaching this file about
// another CLI is one line here plus that CLI's own headless flag.
//
// WHY THE BRIEF MAY BE IN HERE AT ALL. buildAgentSpawn's output is handed to
// `zsh -l -c` — a shell parses it, which is what makes SAFE_MODEL above
// load-bearing. What this function returns goes to child_process.spawn as an
// ARGV ARRAY: cmd and args reach execvp untouched, with no shell anywhere in
// the chain, so the brief is a single element the kernel hands the process as
// argv[2]. No byte of it can become a second command, a redirect, a flag, or
// anything else — it is data, start to finish. That is why there is no
// character guard on the brief: not an oversight, and not something to "fix"
// later by quoting it. The corollary is the rule for anyone editing this
// file: the moment a template joins these into a string for a shell, the
// brief needs the same treatment a model gets, and it cannot have it — a
// composed brief is arbitrary prose by construction.
const HEADLESS = {
  claude: (cmd, brief, model) => ({
    cmd,
    args: ['-p', brief, ...(model ? [MODEL_FLAG, model] : [])],
  }),
}

// { cmd, args } for child_process.spawn, or null when this kind has no
// headless template (see HEADLESS) or the brief isn't usable.
export function buildHeadlessSpawn(kind, { model, brief } = {}) {
  const at = AGENTS.indexOf(kind)
  if (at < 0) return null
  const cmd = AGENTS[at] // the allowlist's own copy, never the caller's string
  const template = HEADLESS[cmd]
  if (!template) return null
  // A brief that isn't a non-empty string is a bug upstream, not a prompt —
  // and `claude -p ''` is the worst possible way to find out: with no prompt
  // to answer it reads a stdin that is a pipe nobody will ever write to, i.e.
  // a background node that hangs forever with nothing in its log to say why.
  if (typeof brief !== 'string' || !brief) {
    console.warn(`flow-run: refusing to run ${cmd} headless with a ${typeof brief} brief`)
    return null
  }
  // Same short-circuit as buildAgentSpawn: absent (or '') means the CLI's own
  // default and never warns. Vetted identically otherwise — the argv shape
  // makes the character guard belt-and-braces here rather than load-bearing,
  // but a pin that would be dropped from a pane must be dropped from a
  // background node too, or the two spawn paths would disagree about what a
  // flow file means.
  return template(cmd, brief, model ? vetModel(cmd, model, 'flow-run') : null)
}
