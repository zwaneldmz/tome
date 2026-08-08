# Tome — Session Handoff (to opencode)

**Date:** 2026-08-09 · **From:** pi session · **To:** opencode session
**Read first:** `docs/FEATURE-PLAN-file-creation-and-flows.md` §0 (architecture primer + conventions), `docs/THREATMODEL.md`, and this file. Source of truth is `src/` — the root `index.js` is a stray build artifact, **never edit it**.

---

## 1. What changed in this session (already on disk, uncommitted)

### Flow node cards: compact + hover tooltip (visual bug fix)
Bug: agent node cards wrapped/overflowed when all fields (name, instructions, expects, produces, ports) were filled. Fix: cards now show **kind badge + name + ports only**; the first line of `instructions` appears in a styled hover tooltip.

- `src/renderer/panels/flow.js`
  - `NODE_H` 140 → 64; comment updated.
  - `buildNodeCard()`: removed the always-on `.flow-node-body` div; appends `.flow-node-tip` (tooltip) when `node.instructions` is non-empty.
  - New module helper `firstLine(text)` — first non-empty line, capped at 140 chars.
- `src/renderer/style.css`
  - `.flow-node`: dropped `min-height: 140px`.
  - `.flow-node-body` ruleset replaced by `.flow-node-tip` (absolute, below card, `opacity 0→1` on `:hover` with a 250 ms strobe-guard delay, `pointer-events: none`, hidden while `.dragging`).
  - `.flow-ports`: `top/bottom` 40px/10px → 14px/14px (compact card).

**Verified:** `npm run build` ✓ · `npm test` 132/132 ✓. Not yet visually smoke-tested in the running app — do one `TOME_SHOT` or manual check of a flow with filled fields before committing.

## 2. New artifacts from this session

- `docs/adoption-council-report.md` — 7-expert open-source adoption review (audience: tech leads). **This is the spec for the roadmap work below.**
- `docs/adoption-council.html` — clickable presentation of the same.
- `docs/adoption-council-report.pdf` + `reviews/render_adoption_pdf.py` — PDF generator (rerun: `python3 reviews/render_adoption_pdf.py`).

## 3. Your mandate: adoption roadmap (from the council report)

Ranked by impact/effort. P0 first, in order. Keep `npm run build` green after every change; run `npm test` before committing.

| # | Pri | Item | Notes / where to start |
|---|-----|------|------------------------|
| 1 | 🔴 P0 | Signed + notarized builds, GitHub release, Homebrew cask | `package.json` build.mac `identity: null` is the blocker. Use electron-builder notarize (`@electron/notarize`, needs Apple creds in env — **ask the user**, do not invent). Add a GitHub Actions release workflow (`.github/workflows/build.yml` exists — extend it). Cask lives in a `homebrew-tap` repo; generate with `brew create-cask` after first release. |
| 2 | 🔴 P0 | README reorder | New order: governance hero line ("Run your coding agents behind an air gap, in one workspace") → air-gap demo (screenshot/GIF) → install → conductor → then existing feature sections. Move air-gap section up from the bottom. Add badges row (CI, tests, license). Link `docs/how-tome-works.html` in the first screen. |
| 3 | 🔴 P0 | `SECURITY.md` + `CONTRIBUTING.md` + issue templates; flip `"private": true` | SECURITY.md is evaluation-facing (for a security-team approver) — adapt from `docs/THREATMODEL.md`, don't copy it; keep THREATMODEL as maintainer notes. Cite the two independent 8/10 reviews (`reviews/`). |
| 4 | 🟠 P1 | Committable team config | `.tome/` in a project root already hosts `flows/`. Extend: workspace definition + air-gap allowlist loadable from `.tome/airgap.json` (repo) with `userData/airgap.json` as override. Touch points: `src/main/airgap.js` (allowlist load), `src/renderer/workspaces.js`. **Security-sensitive: a repo-committed allowlist must be treated as untrusted input — validate host patterns with the same wildcard compiler (`airgap.js`), and surface "this repo wants to allow N hosts" to the user before honoring it.** |
| 5 | 🟠 P1 | Persistent event log | Log conductor tool calls, air-gap unlocks/relocks, blocked-egress attempts (host, pane, timestamp) to `userData/events.jsonl`; render a read-only log pane; keep the 4 s toast as-is. Main process owns the file; renderer reads via a new vetted IPC channel (`domain:verb` naming, add to `src/preload/index.js`). |
| 6 | 🟠 P1 | Demo clip + flows example repo | 60-s script in council report E7. Needs user (recording + a separate GitHub repo) — prepare the example `.flow.json` files only. |
| 7 | 🟠 P1 | Linux statement in README | One paragraph: macOS-first; the allowlist proxy is platform-neutral, sandbox is seatbelt-only (Linux would need bwrap/namespaces — unbuilt). |
| 8–10 | 🟡 P2 | Policy-as-data presets · CI-built artifacts + release cadence · perf one-pager | See council report roadmap table. |

## 4. Conventions that matter (from FEATURE-PLAN §0 + THREATMODEL)

- **IPC naming** `domain:verb`; every channel added in `src/preload/index.js`; the renderer only ever calls `window.tome.*`.
- **Model-reachable writes go through confinement** (`isConfinedPath`/`confinedRealPath` in `src/main/index.js`). User-driven fs follows the existing `fs:writeFile` precedent.
- **Never weaken the seatbelt after spawn** — the air gap widens the proxy, never the sandbox. Pane unlock requires a second factor; don't convenience it down.
- **`TOME_SHOT`** must stay gated on `!app.isPackaged` — it's a full auth bypass.
- Comments explain *why*, not *what*. Match the existing voice.
- Golden rule: `npm run build` must pass after every workstream; `npm test` (vitest) before commit.

## 5. Parked: voice assistant (local model)

User wants to **speak** to the Tome assistant in natural language, locally if possible. Not started — design notes only:

- **TTS already exists** (replies spoken via macOS voices — see chat panel). Missing half is **STT + a push-to-talk loop**.
- **Local STT options:** whisper.cpp (Metal-accelerated on macOS, tiny/base models are near-real-time; ship as a sidecar binary spawned by main, streamed over stdin/stdout) or Apple's Speech framework via a small native helper (best accuracy/latency on macOS, but a new native dependency).
- **Sequencing suggestion:** v1 = push-to-talk button in the chat composer → record (getUserMedia in renderer) → main-process whisper.cpp sidecar → transcript inserted into composer (never auto-sent — same "propose, don't dispose" posture as the conductor's auto-run guard). v2 = wake word / VAD-driven hands-free.
- **Threat-model note:** audio leaves the renderer only as a local file/pipe to the sidecar; no new egress. The transcript is text input to the existing chat path — no new model-trust surface beyond what scrollback already has.
- Model files (~40–150 MB for whisper tiny/base) must not be bundled in the app — download-on-first-use into `userData/models/` with a checksum, mirroring the SheetJS integrity-pin approach (THREATMODEL §7).
