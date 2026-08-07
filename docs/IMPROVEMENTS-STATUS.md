# Improvements Execution Status

**Run complete: 2026-08-07.** All four workstreams merged to main; build green, 76/76 tests green.

| WS | Scope | Branch | Status | Build | Tests | Merged |
|----|-------|--------|--------|-------|-------|--------|
| WS1 | Test floor (vitest) | `improve/ws1` | ✅ done | ✅ | ✅ 76/76 (incl. WS2's added tests) | ✅ |
| WS2 | Security hardening | `improve/ws2` | ✅ done | ✅ | ✅ | ✅ |
| WS3 | Layout persistence, perf & UX | `improve/ws3` | ✅ done | ✅ | ✅ | ✅ |
| WS4 | Structure, docs & CI | `improve/ws4` | ✅ done (orchestrator finished tasks 3–5 after worker timeout) | ✅ | ✅ | ✅ |

## Log
- Plan created from `reviews/pi-review.md` → 4 worktrees + headless `pi -p` workers.
- WS1: vitest floor — pure logic extracted to `src/main/lib/`, 38 tests (RFC 4226 vectors, wildcard anchoring, confine traversal, control-char stripper), CI wiring.
- WS2: 9 security fixes, one commit each (see `.claude/reports/ws2.md`). Also shipped additional tests (76 total now).
- WS3: layout persistence (per-workspace dockview JSON, fresh-shell terminal restore), single-instance lock, async boot shell-outs, pty coalescing (4 ms/64 KB), chat abort + 400k token budget + resend, toast history bell.
- WS4: renderer split into 12 modules, `src/shared/pane-kinds.js` single source, `docs/THREATMODEL.md`, CI lint+smoke gates, unhandled-rejection fixes, unlock-gated git polling.
- Merge conflicts resolved by orchestrator: authlock.js (WS1 lib extraction × WS2 safeStorage — kept single TOTP impl in lib/), index.js (WS2 secret allowlist applied inside WS3's async ensureLoginEnv), renderer.js (WS4 modular shell + WS3 features grafted into panels/chat.js, util.js, menus.js, panes.js), conductor.js (both imports), build.yml (lint → test → build → smoke).
- Lockfile regenerated: WS1's offline install left `bun-cache:` resolved markers npm can't parse; rewritten to registry URLs.

## Known leftovers
- `eslint` devDependency is declared + config committed, but not installed locally (registry 403 on this machine) — `npm run lint` will work after a normal `npm install` with registry access; CI (`npm ci`) will install it fine.
- WS2 deviation noted: core-vault store key is not in the tome:///doc:read confinement set (see `.claude/reports/ws2.md`).
- Worktrees `.claude/worktrees/ws1..4` and branches `improve/ws1..4` retained for audit; safe to remove.
