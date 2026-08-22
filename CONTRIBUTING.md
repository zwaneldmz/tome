# Contributing

## Dev setup

Use **bun** (CI pins 1.3.10 in `.github/workflows/build.yml`) — the repo
has no `package-lock.json`, so `npm install` cannot resolve it at all.

```bash
bun install --frozen-lockfile   # install deps; byte-for-byte what CI runs
bun run lint    # eslint over src/ — CI's hygiene gate
bun run dev     # run the app
bun run test    # vitest
bun run build   # vite build — must stay green after every change
```

Install troubleshooting: if a package 403s from the registry (this bit
`eslint` once, 2026-08-07), a machine- or user-level registry override is
diverting installs to a mirror that doesn't carry the tarball. The project
`.npmrc` pins `registry=https://registry.npmjs.org/` (bun and npm both
honor it), so check for stale `~/.npmrc`/`~/.bunfig.toml` overrides
(`npm config get registry`) before anything else. `xlsx` comes from an
explicit `https://cdn.sheetjs.com/...` tarball URL, not the registry, so
the pin cannot affect it.

## The golden rules

- **IPC naming is `domain:verb`** (`fs:readDir`, `dialog:pickFolder`). Every
  new channel must be registered in `src-tauri/src/lock_gate.rs`
  (`CHANNEL_OF_COMMAND`) and `src-tauri/src/lib.rs` (`generate_handler!`) —
  the renderer only ever talks to main through `window.tome.*` (the
  `tome-ipc.js` bridge).
- **Pane kinds live in `src/shared/pane-kinds.js`** — the single source of
  truth imported by both main and renderer. New kinds go there, never as
  ad-hoc strings.
- **The panel contract:** a panel class has `element` and
  `init({ params, api })` (optional `isDirty()`, `dispose()`). The `params`
  object is what survives layout persistence — `componentOf()` must be able
  to re-derive the component from it on restore.
- **Model-reachable writes go through confinement.** New write-capable IPC
  that the assistant/conductor can trigger must be vetted against open
  workspace folders + brain vaults (`confined_real_path`/
  `confined_write_path` in `src-tauri/src/confine.rs`). User-driven tree
  actions follow the existing `fs:writeFile` precedent.
- **Never weaken the seatbelt after spawn.** Unlocking a pane widens the
  proxy, never the sandbox — keep it that way.
- **`TOME_SHOT` stays gated on `!app.isPackaged`.** It is a full lock-gate
  bypass for dev screenshots and must never be reachable in a packaged build.
- **Comments explain *why***, including the bugs a naive approach would hit.
  See `src/renderer/panes.js` for the house style.
- `bun run lint` and `bun run build` green after every change; `bun run test` before committing.

## Where the specs live

Feature plans, the threat model, review reports, and improvement tracking all
live in [docs/](docs/). Read the architecture primer (§0) in
[docs/FEATURE-PLAN-file-creation-and-flows.md](docs/FEATURE-PLAN-file-creation-and-flows.md)
before touching anything structural, and check
[docs/THREATMODEL.md](docs/THREATMODEL.md) if your change is near a listed
invariant.

## Pull requests

- Small, one concern per PR.
- Tests for pure logic: Rust (`#[cfg(test)]` in `src-tauri/src/`) and vitest
  (in `test/`, for `src/shared` and renderer logic).
- No renderer DOM tests — house convention.
