# Tome — release notes (since v0.1.0)

**Flows now run in the background.** Run executes the whole pipeline headless —
real layer sequencing (a node starts only once every upstream exits 0, two at a
time), one `claude -p` per node, spawned argv-style with no shell anywhere near
the brief, inside the same air gap a pane would get. Logs and `run.json` land
under `.tome/flows/<name>/runs/<id>/`, and every transition is recorded in the
event log. Cancel signals the node's process group, escalating SIGTERM→SIGKILL;
quitting the app reaps orphans.

**A new Flow runs pane** draws each run pipeline-style: layers as columns,
status pills with connectors, live per-node log tails. Air-gap state rides on
every row, and the status bar counts live runs. The flow panel's Run is now a
split button — background by default, "Run in terminals" keeps the original
gated-pane path. Dirty flows save first, because the runner reads the file, not
the screen.

The contract is narrower and now written down: a flow submits only the composed
brief, only on Run, only headless — gapped exactly as a pane would be, with the
gap state surfaced.

**Pin a model per node.** A flow node can pin a same-family model
(claude: sonnet / opus / haiku / fable) — set it in the node editor's Model
select, read it off the card badge ("claude · haiku"), and restored layouts
respawn pinned. The spawn line is provably literals plus the allowlist's own
copies: an incoming value is compared and thrown away, never interpolated;
off-list values fall back to the CLI default with a warning.

## Fixes

- Flow panel: wiring an edge now updates the toolbar's edge count, the
  "no nodes yet" placeholder appears and disappears when it should, and port
  labels no longer print over the kind badge.
- `brain.js` path checks no longer throw `ReferenceError` on an untested branch
  (`sep` was never imported); the eslint globals allowlist catches up with
  Node 18 and browser APIs.

## Docs

- The how-Tome-works tour is rebuilt as one clickable worked example — empty
  workspace to audited event log, through the shipped review-pipeline flow.
  Every artifact is real, the page is self-contained, keyboard-navigable, and
  AA-contrast in both themes.
- The flows feature is on film: a 62-second uncut recording of the real app
  building a flow — nodes, edges, a pinned model, save — plus four current
  stills.

Suite: 254 tests green.
