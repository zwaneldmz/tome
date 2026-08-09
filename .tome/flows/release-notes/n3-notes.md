# Tome — release notes (since v0.1.0)

**Flows now run in the background.** Run executes the whole pipeline headless:
a node starts only once every upstream exits 0, at most two at a time, one
`claude -p` per node — spawned argv-style, no shell near the brief, inside the
same air gap a pane would get. Logs and `run.json` land under
`.tome/flows/<name>/runs/<id>/`; every transition is recorded in the event log.
Cancel signals each node's process group (SIGTERM, then SIGKILL); quitting the
app reaps orphans.

**A new Flow runs pane** draws each run pipeline-style: layers as columns,
status pills with connectors, live per-node log tails, air-gap state on every
row. The status bar counts live runs. The flow panel's Run is now a split
button — background by default, "Run in terminals" keeps the gated-pane path.
Dirty flows save first; the runner reads the file, not the screen.

The contract is narrow and written down: a flow submits only the composed
brief, only on Run.

**Pin a model per node.** A node can pin a same-family model (claude: sonnet /
opus / haiku / fable) — set in the node editor's Model select, shown on the
card badge ("claude · haiku"), respawned with restored layouts. The spawn line
is literals plus the allowlist's own copies — an incoming value is compared,
never interpolated; off-list values fall back to the CLI default with a
warning.

## Fixes

- Flow panel: wiring an edge updates the toolbar's edge count, the "no nodes
  yet" placeholder comes and goes when it should, and port labels no longer
  print over the kind badge.
- `brain.js` path checks no longer throw `ReferenceError` on an untested
  branch (`sep` was never imported); the eslint globals allowlist catches up
  with Node 18 and browser APIs.

## Docs

- The how-Tome-works tour is rebuilt as one clickable worked example — empty
  workspace to audited event log, through the shipped review-pipeline flow.
  Self-contained, keyboard-navigable, AA-contrast in both themes.
- The flows feature on film: a 62-second uncut recording of the real app
  building a flow — nodes, edges, a pinned model, save — plus four current
  stills.

Suite: 254 tests green.
