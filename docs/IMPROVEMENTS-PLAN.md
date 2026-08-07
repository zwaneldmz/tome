# Tome — Master Improvements Plan

**Source:** `reviews/pi-review.md` (pi independent council review, 2026-08-07) + `reviews/kimi-k3-review.txt` (Kimi K3 council report)
**Goal:** Execute the prioritized recommendations from both reviews without breaking the build.
**Golden rule:** `npm run build` must pass after every workstream. Source of truth is `src/` — the root `index.js` is a stray build artifact, never edit it.

## Workstreams

### WS1 — Test floor (Review rec #1) — `worktree-ws1`
Add vitest; pin behavior that is correct *today* so future edits own regressions.

- [ ] `npm i -D vitest`; add `"test": "vitest run"` to package.json
- [ ] `test/authlock.test.js` — HOTP against all 10 RFC 4226 vectors; TOTP round-trip; base32 lowercase round-trip
- [ ] `test/airgap.test.js` — wildcard compiler: `bedrock-runtime.*.amazonaws.com` matches `bedrock-runtime.us-east-1.amazonaws.com`, rejects `….amazonaws.com.evil.com` and bare `amazonaws.com`; default allowlist sanity
- [ ] `test/brain.test.js` — `confine()` blocks `..`, absolute paths, sibling-prefix; allows normal vault-relative paths
- [ ] `test/conductor.test.js` — `stripAnsi`; control-char stripper kills CR/LF/Ctrl-C/Ctrl-D/ESC, preserves tab
- [ ] Export what needs exporting from the main modules for testability (keep exports additive, no behavior change)
- [ ] Wire `npm test` into `.github/workflows/build.yml` before the build step

### WS2 — Security hardening (recs #2, #3, #4, #5, #9) — `worktree-ws2`
- [ ] **Per-provider secret allowlist** in `resolveAgentSecrets()`: only forward `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `REQUESTY_API_KEY`, `OPENROUTER_API_KEY`, `DEEPSEEK_API_KEY`, `MOONSHOT_API_KEY`, `GROQ_API_KEY`, `MISTRAL_API_KEY`, `XAI_API_KEY`, `GOOGLE_API_KEY`/`GEMINI_API_KEY`, `AWS_*` (bedrock) — not every `*_TOKEN`/`*_KEY` in the login shell
- [ ] **Confine conductor `open_file` + `doc:read`** to open workspace folders (reuse/extend brain's confine approach)
- [ ] **Constrain `tome://` by design**: drop `corsEnabled`/`supportFetchAPI` from scheme privileges; confine served paths to workspace folders + extension allowlist (images, pdf, md, txt, source files)
- [ ] **Login throttling**: attempt counters with exponential backoff on `auth:login` and `airgap:unlock`; raise passphrase minimum 4→8 on both setup screens
- [ ] **`safeStorage`** encrypt the TOTP secret at rest (fall back gracefully when unavailable, e.g. Linux w/o keychain)
- [ ] **Gate `TOME_SHOT`** on `!app.isPackaged` so the lock bypass can't ship in packaged builds
- [ ] Strip hop-by-hop headers (`Proxy-Authorization`, `Connection`, etc.) in the plain-HTTP proxy branch
- [ ] `brain.confine()`: add `realpath` check so symlinks inside a vault can't escape it

### WS3 — Layout persistence, perf & UX (recs #6, #8, #11, #13) — `worktree-ws3`
- [ ] **Persist dockview layout per workspace** via `toJSON`/`fromJSON`, stored per workspace path; restore editors/docs/chat/brain panes on reopen (terminals can't restore — reopen as fresh shells or skip, document choice)
- [ ] **`requestSingleInstanceLock()`** in main; second instance focuses the existing window
- [ ] **Async boot**: never `execFileSync` on the launch path — parallelize shell-outs, move secret resolution to first agent spawn
- [ ] **Coalesce pty data events** (~4 ms window) before the IPC hop to the renderer
- [ ] **Chat stop button** + cumulative token budget across the 8-turn tool loop; keep failed message in input for resend instead of silent `history.pop()`
- [ ] **Toast history**: missed toasts (esp. "airgap blocked") retrievable — a minimal notification log panel or persistent entry

### WS4 — Structure, docs & CI (recs #7, #10, #12, #14) — `worktree-ws4`
- [ ] **Split `renderer.js`** (1,664 LOC) along its seams: `panels/` (terminal, editor, doc, chat, brain, graph), `menus`, `tree`, `git`, `modals`; share one `el()` helper; unify `modalShell` and `lock.js`'s `overlay()`
- [ ] **Shared pane-kind constants module** used by main `AGENTS`, conductor tool description, and renderer switch (one import graph, no hand-synced lists)
- [ ] **`docs/THREATMODEL.md`**: collect the load-bearing invariants scattered in comments — store-keys-open-pre-login ⇒ reserved keys; login-proves-passphrase ⇒ pane-unlock-is-second-factor; brain-outside-userData ⇒ seatbelt; air gap widens proxy never sandbox; scrollback→model→tool-call confused-deputy loop
- [ ] **CI**: add lint + renderer smoke test + `npm run package` gate; document the SheetJS CDN dependency (package.json comment or README note)
- [ ] Fix renderer unhandled rejections (`tome.chat.send`, `tome.pty.create` need `.catch`); gate `refreshGit` interval on unlock

## Execution
- Orchestrator: main pi agent (interactive session)
- Workers: `git worktree` per workstream + `pi -p` headless subagents
- Merge order: WS1 → WS2 → WS3 → WS4 (rebase/merge each, `npm run build` + `npm test` after each)
- Tracker: `docs/IMPROVEMENTS-STATUS.md` (updated after each merge)
