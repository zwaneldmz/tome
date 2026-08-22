// Single source of truth for the one thing a flow node may pin on the agent it
// spawns: a model. Optional — a node without one runs on the CLI's own
// default, which is why every existing .flow.json stays valid with no version
// bump and no migration.
//
// A FIXED list rather than anything discovered at runtime, because this is a
// security boundary and not a convenience: main builds the pty command line
// itself precisely so a compromised renderer can't request arbitrary binaries
// or arguments (see the comment above 'pty:create' in src/main/index.js). The
// way that property survives growing a flag is that only strings spelled out
// here ever reach the spawn line — main compares the incoming value against
// this list and then passes its OWN copy through, never the string it was
// handed. Shared for the same reason AGENTS is: the node editor, validateFlow
// and main's vetting have to agree exactly, and a second drifted copy would
// let a value pass the editor only to be dropped without explanation at spawn
// time.
//
// The values are each CLI's own vocabulary, read off its `--help`: claude
// takes `--model <alias>` and accepts the family aliases below. opencode and
// pi resolve models from a dynamic provider catalog (whatever the user has
// configured), so there is no fixed set to vet against — they ship
// deliberately empty in v1, and an empty list is exactly what hides that
// select in the node editor. main's Rust vetting (tome-flow agent_spawn.rs)
// accepts the `provider/model` shape for these kinds (format-vetted:
// [a-z0-9-]+/[a-z0-9._-]+), so a pane or headless run can pin one even
// though the node editor's fixed-list select stays hidden until a list is
// filled in here.
export const AGENT_MODELS = {
  claude: { models: ['sonnet', 'opus', 'haiku', 'fable'] },
  opencode: { models: [] },
  pi: { models: [] },
}
