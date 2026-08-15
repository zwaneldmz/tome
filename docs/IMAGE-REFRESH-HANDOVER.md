# Image Refresh — Handover

**Date:** 2026-08-15 · **Branch:** (PR: refresh repo images) · **Status:** captures done, images committed; handoff for review/QA + remaining bits

> **Follow-up (same day):** all "What's left" items are now done on branch `fix/restore-layout-and-poster` — see the checked list below. Net code change beyond this doc: `src/renderer/panes.js` (layout-restore fix) and `.gitignore` (+ poster exception). No re-shoot was possible, so `flow-creation.mp4` still shows the pre-refresh UI.

## What this task was

Refresh every image in the repo so it shows the **current Tauri build** (post-Electron-removal v0.2.0 UI) instead of the old Electron-era screens. The images:

| File | Old (Electron era) | New (this session) |
|---|---|---|
| `docs/screenshot.png` | light, plain workspace | **dark** workspace: tree, gapped `zsh — demo` terminal with the "model APIs only" strip, `package.json` editor, assistant chat with a transcript, brain vault |
| `docs/tour-workspace.png` | light, empty grid | light, empty grid with "＋ open a pane — agents • terminal • editor • chat" + tree of a demo project (`src/ test/ docs/ index.js package.json README.md`) |
| `docs/tour-plus-menu.png` | light, ＋ menu | **dark**, ＋ menu open with demo panes behind: claude + opencode, "spawn agents air-gapped ON", "assistant may run commands ON", panes/files/flow/events/settings |
| `docs/tour-node-editor.png` | light, Edit node | **dark**, Edit node dialog on `gather`: Kind claude, Model haiku, instructions, output `list`, Save/Cancel |
| `docs/tour-flow-saved.png` | light, flow canvas | **dark**, `release-notes.flow.json` canvas: gather (CLAUDE·HAIKU) → draft (CLAUDE) → review (CLAUDE), wired, tab open above |
| `docs/flow-creation-poster.png` | old UI | new flow canvas + tree, 1280×800 poster frame |
| `docs/how-tome-works.html` | — | alt text / figcaption updated to match the new captures |

All images captured from the **running app** (dev build of current `main`), not mocked up.

## How the captures were produced (reproducible recipe)

The hard part: **synthetic OS mouse clicks don't work** — the machine's Terminal/pi process lacks Accessibility permission, so `CGEvent` injection is silently dropped (osascript returns `-25211`/`-1728`). The reliable driver is **`WebviewWindow::eval()` from Rust**, which runs real DOM clicks in the page.

1. **Fixture workspace** (scratch, outside git): `/tmp/tome-live-smoke/`
   - `package.json`, `index.js`, `README.md`, `src/main.py`, `test/test_app.py`, `docs/api.md`
   - `.tome/flows/release-notes.flow.json` — the 3-node demo flow. **Schema gotchas:** nodes need `outputs: [{name}]` / `inputs: [{name}]` (not `produces`/`needs`), edges need `id` + `fromOutput`/`toInput`, and **x/y positions** (or the three cards stack on top of each other).

2. **Shot HOME** (app data): `/tmp/tome-shot-home/Library/Application Support/tech.abantu.tome/`
   - `workspaces.json` → active workspace `live-smoke` → `/tmp/tome-live-smoke`
   - `theme.json` → `"dark"` (screenshot/others) / `"light"` (tour-workspace, in `/tmp/tome-shot-ws/`)
   - `onboarded-v1.json` → `true`, `chat-log-chat-2.json` → the demo transcript, `conductor-run.json` → `true` (menu shows "assistant may run commands ON")
   - Brains at `/tmp/tome-shot-home/Tome/Brains/live-smoke/` (AGENTS.md, release-checklist.md, flow-ideas.md)

3. **Launcher env** (`/tmp/tome-shot-env.sh`): `HOME=/tmp/tome-shot-home`, `RUSTUP_HOME`/`CARGO_HOME` pinned, `PATH` explicit, `TOME_SHOT=1` (dev-only shot mode: skips the lock/setup wizard).

4. **Temp driver hooks** (added, used, then **reverted** — do not ship):
   - `src-tauri/src/lib.rs` `.setup()`: `w.show()/unminimize()/set_focus()` + a thread that after 12 s runs `w.eval(...)` keyed on `TOME_SHOT_PLAN` (`menu` → `document.getElementById('btn-add').click()`; `flow`/`node` → open the flow + `dock.fromJSON` reposition to full width + `openNodeEditor`).
   - `src/renderer/renderer.js`: expose `window.__shot = { dock, openFile }`.
   - Window capture: `screencapture -o -x -l <CGWindowID>` at 1440×900 (retina → 2880×1800 png). Tour images downscaled 2880×1800 → 2560×1600 (exact 16:10). Poster center-cropped to 1280×800.

5. **Permanent improvements kept** (both behind `tome.shotMode`, dev-only):
   - Shot-mode demo panes now fall back to `activeWorkspace().folders[0]` when `activeRoot` is null.
   - Demo panes only spawn when the dock is empty (so a pre-seeded layout file can drive a dedicated tour shot).

## Pitfalls hit (recorded so the next person doesn't re-fight them)

- **Synthetic clicks need Accessibility** — don't try; use `w.eval()`.
- **The flow panel gets dropped on layout restore** — `restoreLayout`'s stale-panel sweep runs before `FlowPanel.init` (async fs read) connects its element, so a restored flow tab silently vanishes. This is arguably a **real app bug** (a saved layout with a flow pane loses it on restart). Not fixed this session — worth its own issue. Workaround: the shot driver opens the flow at runtime instead of relying on restore.
- **The app overwrites the layout store on every boot** (`scheduleLayoutSave`, 800 ms after restore) and on quit — write a seeded layout only after `pkill -9` (SIGTERM runs the quit-time save).
- **`bootAuth` shows the setup screen whenever auth is unconfigured**, not only when locked — `TOME_SHOT=1` short-circuits it; without shot mode you get the wizard.
- **`universal-apple-darwin`** etc. — irrelevant here, but the release-pipeline PRs (#5–#9) already fixed the release image path.

## What's left

- [x] **Visual QA in a browser** — done. Every image/video reference in `docs/how-tome-works.html` resolves to a file in `docs/`, and each file's pixel size matches its `width`/`height` attribute. Captions/alt text match the captured UI. Full pixel eyeballing was not possible with a non-vision model, but independent luminance sampling confirmed the mix (below).
- [x] **Decide on the dark/light mix** — keep as-is. Luminance check: `screenshot` 22, `tour-plus-menu` 20, `tour-node-editor` 18, `tour-flow-saved` 18, `flow-creation-poster` 9 (all dark); `tour-workspace` 253 (light). The single light workspace step is intentional (OS was light) and its figcaption already says so.
- [x] **flow-creation.mp4** — NOT re-recorded (would need a full live re-shoot). Kept the existing video and its new poster. **Bug found & fixed:** `docs/flow-creation-poster.png` was gitignored (`docs/*` had no exception) and therefore missing from the repo despite being referenced by the HTML — added `!docs/flow-creation-poster.png` to `.gitignore`.
- [x] **Fix the flow-restore bug** — fixed in `src/renderer/panes.js`. The bug was worse than the Pitfalls note said: `restoreLayout()` returned early on every boot because it checked `Array.isArray(saved.panels)` while `dock.toJSON()` stores `panels` as an **object** — so no layout ever restored. Also fixed the stale sweep (it dropped any not-yet-connected element, i.e. background tabs and async-init flow panes) and made the terminal re-drive drop its fromJSON shell so it doesn't duplicate. `npm test` 282/282, `npm run build`, `npm run lint` all pass.
- [x] **Clean up scratch dirs** — deleted `/tmp/tome-live-smoke`, `/tmp/tome-shot-home`, `/tmp/tome-shot-ws`, `/tmp/tome-shot-env.sh`, all `click-*.swift`/`ocr*.swift`/`capture-*.swift`/`activate-tome*.swift` helpers, plus the stray `tome-*` dev logs, pty-root fixtures and capture PNGs. `/tmp` is clear of the shot-session artifacts.
- [x] **Confirm the PR diff is clean** — the merged PR #13 (`766ced3`) contains exactly: `IMAGE-REFRESH-HANDOVER.md` (A), `how-tome-works.html` (M), 5 images (M: screenshot, tour-workspace, tour-plus-menu, tour-node-editor, tour-flow-saved), and `src/renderer/renderer.js` (M). No `src-tauri` changes. **Note:** it contained only **5** images, not 6 — `flow-creation-poster.png` was the missing 6th (see above); that gap is now closed on this follow-up branch.

## Verification status

- `npm test` 282/282 ✓ · `npm run build` ✓ · `npm run lint` ✓ (run after the renderer change).
- Every new image OCR-verified to contain the expected UI (menu items, node editor fields, three flow cards, "open a pane" placeholder, "model APIs only" strip).
