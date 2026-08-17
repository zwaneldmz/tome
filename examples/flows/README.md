# Example flows

Starter `.flow.json` files — the same shape Tome saves when you build a flow on the canvas.

- **release-notes** — the smallest useful graph: gather (claude on haiku) → draft → review. Start here.
- **codebase-onboarding** — fan-out/fan-in: three parallel readers (architecture, conventions, gotchas) → one synthesizer that writes a first-day guide.
- **airgap-demo** — the 60-second demo as a flow: a scout probes the air-gap proxy, a builder codes offline, a terminal verifies.
- **review-pipeline** — triage (claude on haiku) → planner (claude) → implementer (opencode) → adversarial reviewer (pi) → verify (terminal).
- **docs-sweep** — read docs, grep for stale claims, draft minimal fixes.

## Use one

Copy a file into your project's `.tome/flows/` and open it from the Flows tree. Run spawns one pane per node and hands off between them with files under `.tome/flows/<name>/` — no shared memory, no new IPC, so unmodified agent CLIs work.

## Graph engineering

The shape of the graph is where the engineering happens — the instructions matter, but the
shape decides what each agent ever gets to see. Patterns these starters demonstrate:

- **Pipeline** (release-notes, review-pipeline) — each node hands a narrow contract to the
  next. The drafter never sees the git plumbing; the implementer never sees the issue
  tracker. Several small contexts doing one job each beat one big prompt doing five.
- **Evidence edge** (release-notes) — the reviewer takes *two* inputs: the draft it's
  checking **and** the raw change list it checks the draft against. An edge that skips a
  level like this is how you give a checker its evidence instead of asking it to trust
  the node before it.
- **Fan-out / fan-in** (codebase-onboarding) — independent questions become independent
  nodes with no edges between them, so Run works them in parallel (two at a time — the cap
  keeps your machine usable) and a synthesizer merges the reports. Decompose by *question*,
  not by directory.
- **Cheap-model shoveling** (all of them) — pin `haiku` on nodes that gather, grep, and
  list; leave the nodes that think on the CLI's default. The gather node's output is a
  commit list either way — paying frontier-model prices for it buys nothing.
- **Adversarial pair** (review-pipeline) — the diff is reviewed by a *different* CLI with
  instructions to break it, not admire it. Same-model review tends to nod along.
- **Terminal ground truth** (airgap-demo, review-pipeline) — end with a `terminal` node
  that runs the tests. Agents grade each other on vibes; terminals exit non-zero.

To build your own, work backwards from the artifact you want. Name the file the last node
writes, then ask what that node needs as evidence — each answer becomes an upstream node,
and each edge a one-sentence contract (`expects`/`produces` in the node editor). A contract
you can't write in one sentence means the node is doing two jobs: split it.

## The contract

- A flow is `{ version: 1, name, nodes, edges }`; `name` becomes a folder name, so no `/`, `\`, or `..`.
- Nodes carry `kind` (`claude`, `opencode`, `pi`, `terminal`), `instructions`, `expects`/`produces` (the human-readable contract), and named `inputs`/`outputs` ports.
- A node may pin `model` — that is how review-pipeline's triage node runs on `haiku` while the rest of the pipeline takes whatever the CLI would have chosen. Leave the key out rather than writing `""`; an alias Tome doesn't recognise only warns, and that node spawns unpinned.
- Edges wire ports, not nodes: `{ from, to, fromOutput, toInput }` — both port names must exist on their nodes or validation warns.
- Handoff is a file per output: `.tome/flows/<name>/<node>-<output>.md`. Downstream nodes read upstream files; that's the whole mechanism.

The demo script these support is item #6 in `docs/adoption-council-report.md` (E7's 60-second air-gap clip).
