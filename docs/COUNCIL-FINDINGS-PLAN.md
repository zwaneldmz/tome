# Council Findings — Remediation Plan

**Date:** 2026-08-15 · **Source:** `docs/technical-council.html` (five-expert review)
**State:** plan → addressable findings are being executed in this session.

## Priorities

| # | Sev | Finding | Action | Owner | Status |
|---|---|---|---|---|---|
| F1 | crit | SECURITY/THREATMODEL/perf docs are Electron-era | Rewrite for the Tauri/Rust code | subagent + review | in progress |
| F2 | high | Linux sandbox is egress-only (`TODO(landlock)`) | Add Landlock file confinement to `tome-shim` | maintainer | planned / attempted |
| F3 | high | Linux proof is Ubuntu-only | Extend `linux-sandbox.yml` to Ubuntu 24.04 + Fedora | subagent (CI) | in progress |
| F4 | high | No macOS Rust gate in CI | Add `cargo test` to `build.yml` | subagent (CI) | in progress |
| F5 | high | macOS releases unsigned/notarized | Wire Apple secrets + `spctl`/`stapler` step | needs Apple credentials | blocked on secrets |
| F6 | med | version drift (0.1.0 vs v0.2.0) | Bump and keep package.json/Cargo.toml/docs in sync | maintainer | in progress |
| F7 | med | no DOM/E2E test layer | Add a DOM smoke test for `restoreLayout` | maintainer | in progress |
| F8 | med | `xlsx` CDN pin surface | Re-verify integrity in the audit gate | maintainer | in progress |
| F9 | low | voice needs manual whisper install | Document (already in README) | maintainer | done in README |
| F10 | low | unmerged restore-layout fix | Merge + runtime-verify | maintainer | in progress |

## Phase 1 — this session (no external resources)

1. **F1** Rewrite `SECURITY.md`, `THREATMODEL.md`, `docs/perf.md` against `src-tauri/*`
   (PTYs in `pty.rs`, lock gate `lock_gate.rs`, auth `authlock.rs`, sandbox
   `airgap/seatbelt.rs` + `airgap/linux.rs`, proxy `airgap/proxy.rs`, conductor
   `conductor/`, confinement `confine.rs`). Remove every `node-pty` / `electron-vite` /
   `electron-rebuild` / `src/main/` / `sandbox: true` reference.
2. **F4** Add a macOS Rust gate to `.github/workflows/build.yml` (`cargo test --lib`,
   or `cargo check` if full tests are too slow on CI).
3. **F3** Extend `.github/workflows/linux-sandbox.yml` with Ubuntu 24.04 (AppArmor
   userns) and Fedora; note Linux aarch64 as follow-up.
4. **F6** Bump `package.json` (and `src-tauri/Cargo.toml` if it carries a version) to
   a single `0.2.0` and keep docs in line.
5. **F7** Add a minimal DOM smoke test for `restoreLayout` (jsdom or happy-dom) so a
   layout round-trip is asserted in CI.
6. **F8** Add an integrity re-check note/step for the `xlsx` CDN pin.
7. **F10** Merge `fix/restore-layout-and-poster`; run `npm test`/`lint`/`build`.

## Phase 2 — follow-ups (need secrets, hardware, or larger effort)

- **F2** Landlock: implement best-effort file-write denial in `tome-shim/src/linux.rs`
  (gated on kernel support), mirroring the macOS seatbelt's `userData` write denial
  and credential read denial. Needs a real-Linux test pass (the `linux-sandbox.yml`
  matrix from F3 is the proving ground).
- **F5** Apple Developer ID signing + notarization: needs `APPLE_CERTIFICATE`,
  `APPLE_API_KEY`, etc. The `release-tauri.yml` scaffolding already exists; add a
  `codesign --verify` / `spctl -a` / `stapler validate` gate once secrets are set.
- **F3** (remainder) Linux aarch64 bundles and the bwrap self-unshare "rung 2"
  integration coverage.

## Verification gate (every change)

`npm test` (282) · `npm run lint` · `npm run build` · `~/.cargo/bin/cargo check`
for any Rust change.
