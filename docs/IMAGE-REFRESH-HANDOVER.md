# Image Refresh — Handover

**Date:** 2026-08-15 · **Branch:** (PR: refresh repo images) · **Status:** captures done, images committed; handoff for review/QA + remaining bits

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

- [ ] **Visual QA in a browser** — open `docs/how-tome-works.html` (or the GitHub Pages build) and eyeball each step's screenshot: the dark-mode set is a deliberate look change from the old light set; confirm the tour page reads well (the workspace step is still light by design — the figcaption says "light theme because the OS was light").
- [ ] **Decide on the dark/light mix** — screenshot + plus-menu/node/flow are dark, tour-workspace is light. If the whole tour should be one mode, re-run the recipe with `theme.json` flipped and re-crop.
- [ ] **flow-creation.mp4** — the video itself still shows the old UI (only its poster was refreshed). Re-record the "create a flow" walkthrough against current main, then regenerate the poster from the new video (or keep the current poster if the video isn't re-shot).
- [ ] **Fix the flow-restore bug** (see Pitfalls) — a persisted layout containing a flow pane loses it on restart. File it or fix `restoreLayout`'s stale sweep to tolerate async-init components.
- [ ] **Clean up scratch dirs** — `/tmp/tome-live-smoke`, `/tmp/tome-shot-home`, `/tmp/tome-shot-ws`, `/tmp/tome-shot-env.sh`, the `click-*.swift`/`ocr*.swift` helpers are all outside git; delete when no longer needed. The repo itself contains **no** scratch fixtures (the demo project lives only in `/tmp`).
- [ ] **Confirm the PR diff is clean** — should be exactly: 6 images + `docs/how-tome-works.html` + the shot-mode improvement in `src/renderer/renderer.js`. No `src-tauri` changes (temp hooks reverted).

## Verification status

- `npm test` 282/282 ✓ · `npm run build` ✓ · `npm run lint` ✓ (run after the renderer change).
- Every new image OCR-verified to contain the expected UI (menu items, node editor fields, three flow cards, "open a pane" placeholder, "model APIs only" strip).
