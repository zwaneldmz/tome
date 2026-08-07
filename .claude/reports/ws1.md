# WS1 — Test floor (vitest)

**Branch:** `improve/ws1` · **Status:** ✅ complete — `npm test` green (38/38), `npm run build` green.

## What was done

1. **vitest 4.0.18** added as devDependency; `"test": "vitest run"` in package.json.
2. **Pure-logic extraction** (additive, behavior-preserving) into `src/main/lib/`:
   - `lib/totp.js` — `hotp`, `totp`, `b32encode`, `b32decode` (from `authlock.js`, which re-imports them; `hotp` re-exported for compatibility)
   - `lib/allowlist.js` — `DEFAULT_ALLOW`, `compileAllowlist` (from `airgap.js`)
   - `lib/confine.js` — `confine` (from `brain.js`)
   - `lib/terminal-text.js` — `stripAnsi`, `stripControlChars` (from `conductor.js`; the auto-run guard now calls the named helper)
3. **Tests** (`test/`, 4 files, 38 tests):
   - `authlock.test.js` (15) — all 10 RFC 4226 HOTP vectors (secret "12345678901234567890", counters 0–9) ✅; TOTP round-trip + step-math check; base32 round-trip over 20 lengths, lowercase input, padding tolerance
   - `airgap.test.js` (8) — `bedrock-runtime.*.amazonaws.com` matches `…us-east-1…`, rejects `….amazonaws.com.evil.com`, bare `amazonaws.com`, and multi-label spans; exact-host exactness; `DEFAULT_ALLOW` contains `router.requesty.ai` + `api.anthropic.com`
   - `brain.test.js` (8) — `confine()` blocks `..` (incl. backslash form), absolute paths, sibling-prefix escapes (`../foobar/x`), non-strings; allows vault-relative paths; `requireMd` enforcement
   - `conductor.test.js` (7) — `stripAnsi` removes CSI/OSC/stray escapes, preserves `\n`/`\t`; control-char stripper kills CR, LF, Ctrl-C, Ctrl-D, ESC, DEL, NUL; preserves tab
4. **CI**: `.github/workflows/build.yml` now runs `npm test` between `npm ci` and `npx electron-vite build`.

## Verification

- `npm test` → 4 files, 38 tests, all pass.
- `npm run build` → main + preload + renderer all build clean.
- One test-authoring fix during bring-up: my initial `stripAnsi` expectation was wrong (BEL is stripped, not preserved); corrected the test to pin actual (correct) behavior.

## Deviations

- **Offline vitest install.** This worker ran inside an air-gapped agent pane (CONNECT proxy allows only provider domains; registry.npmjs.org → 403). vitest 4.0.18 and its full dependency closure were copied from the local bun cache (`~/.bun/install/cache`) into `node_modules`, and lockfile entries for the 25 genuinely-new packages were generated offline with `resolved: "bun-cache:…"` markers plus a `comment` noting they should be regenerated via `npm install` once registry access is available. All pre-existing lockfile entries are byte-identical to `main`. **Follow-up for the orchestrator: run `npm install` on a networked machine to normalize the lockfile before merging** (and CI's `npm ci` needs real registry entries for those 25 packages).
- `node_modules` in the worktree was assembled from the main checkout's copy (same commit) plus the vitest overlay; project-pinned vite 6.4.3 / esbuild 0.25.12 / rollup 4.62.4 were kept (vitest 4 accepts vite ^6, tests run fine on it).
- No `vitest.config` added — defaults suffice (test/ glob is covered by vitest's default include).
