# Contributing

## Dev setup

```bash
npm install     # rebuilds node-pty for Electron's ABI (needs Xcode CLT)
npm run dev     # run the app
npm test        # vitest
npm run build   # electron-vite build — must stay green after every change
```

If `npm run dev` fails with `Error: Electron uninstall`, run
`npm run fix:electron` once.

## The golden rules

- **IPC naming is `domain:verb`** (`fs:readDir`, `dialog:pickFolder`). Every
  new channel must be registered in `src/preload/index.js` — the renderer
  only ever talks to main through `window.tome.*`.
- **Pane kinds live in `src/shared/pane-kinds.js`** — the single source of
  truth imported by both main and renderer. New kinds go there, never as
  ad-hoc strings.
- **The panel contract:** a panel class has `element` and
  `init({ params, api })` (optional `isDirty()`, `dispose()`). The `params`
  object is what survives layout persistence — `componentOf()` must be able
  to re-derive the component from it on restore.
- **Model-reachable writes go through confinement.** New write-capable IPC
  that the assistant/conductor can trigger must be vetted against open
  workspace folders + brain vaults (`isConfinedPath`/`confinedRealPath` in
  `src/main/index.js`). User-driven tree actions follow the existing
  `fs:writeFile` precedent.
- **Never weaken the seatbelt after spawn.** Unlocking a pane widens the
  proxy, never the sandbox — keep it that way.
- **`TOME_SHOT` stays gated on `!app.isPackaged`.** It is a full lock-gate
  bypass for dev screenshots and must never be reachable in a packaged build.
- **Comments explain *why***, including the bugs a naive approach would hit.
  See `src/renderer/panes.js` for the house style.
- `npm run build` green after every change; `npm test` before committing.

## Where the specs live

Feature plans, the threat model, review reports, and improvement tracking all
live in [docs/](docs/). Read the architecture primer (§0) in
[docs/FEATURE-PLAN-file-creation-and-flows.md](docs/FEATURE-PLAN-file-creation-and-flows.md)
before touching anything structural, and check
[docs/THREATMODEL.md](docs/THREATMODEL.md) if your change is near a listed
invariant.

## Pull requests

- Small, one concern per PR.
- Tests for pure logic in `src/main` / `src/main/lib` (vitest, in `test/`).
- No renderer DOM tests — house convention.
- The root `index.js` is a stray build artifact — never edit it.
