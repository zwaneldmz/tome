# Example flows

Starter `.flow.json` files — the same shape Tome saves when you build a flow on the canvas.

- **airgap-demo** — the 60-second demo as a flow: a scout probes the air-gap proxy, a builder codes offline, a terminal verifies.
- **review-pipeline** — planner (claude) → implementer (opencode) → adversarial reviewer (pi) → verify (terminal).
- **docs-sweep** — read docs, grep for stale claims, draft minimal fixes.

## Use one

Copy a file into your project's `.tome/flows/` and open it from the Flows tree. Run spawns one pane per node and hands off between them with files under `.tome/flows/<name>/` — no shared memory, no new IPC, so unmodified agent CLIs work.

## The contract

- A flow is `{ version: 1, name, nodes, edges }`; `name` becomes a folder name, so no `/`, `\`, or `..`.
- Nodes carry `kind` (`claude`, `opencode`, `pi`, `terminal`), `instructions`, `expects`/`produces` (the human-readable contract), and named `inputs`/`outputs` ports.
- Edges wire ports, not nodes: `{ from, to, fromOutput, toInput }` — both port names must exist on their nodes or validation warns.
- Handoff is a file per output: `.tome/flows/<name>/<node>-<output>.md`. Downstream nodes read upstream files; that's the whole mechanism.

The demo script these support is item #6 in `docs/adoption-council-report.md` (E7's 60-second air-gap clip).
