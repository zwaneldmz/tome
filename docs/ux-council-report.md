# Tome — UX/UI Council Report

**Date:** 2026-08-08 · **Method:** seven-expert council review of `src/renderer/` (4,200 LOC), `style.css`, and interaction flows.
**Verdict:** Visual craft is genuinely strong (token system, motion discipline, reduced-motion support, AA-fixed contrast, per-group tabs, popout windows). Weaknesses: **keyboard poverty, destructive-action safety, discoverability** — the app is mouse-first in a keyboard-first domain.

---

## ⌨️ E1 — Power User / Dev-Tool Veteran — 🔴 Critical
- Only three shortcuts exist (⌘B, ⌘S, Enter-send). No command palette (⌘⇧P), no quick-open (⌘P), no pane/tab keys (⌘W, ⌘1–9, ⌘⇧[/]), no terminal zoom (fontSize hardcoded 12.5 in `panels/terminal.js`), no shortcut reference.

## 🧭 E2 — Interaction Designer — 🔴 / 🟠
- 🔴 Closing a dirty editor silently discards work (dirty dot warns, nothing intercepts close).
- 🔴 "Delete workspace" and tree `×` remove-folder execute instantly — no confirm, no undo.
- 🟠 Tear-off and per-group tabs are invisible: group header ＋/⧉ are `opacity: 0` until hover.
- 🟠 Glyph-only iconography (♪ ◐ ⇤ ▚) is undecodable for new users; needs coach marks/richer tooltips.
- 🟡 Sidebar resize grip is a 16px corner (CSS `resize: horizontal`) — needs a full-height drag divider.

## ♿ E3 — Accessibility — 🟠
- Menus have `role="menu"` but no arrow-key nav, no Esc, no `aria-expanded` on triggers.
- Missing `aria-label`s on glyph buttons (btn-notifs, btn-add, ws-chip, git-chip).
- Modals don't trap focus and don't close on Escape.
- Toasts not announced; notification log should be `aria-live="polite"`.
- Dark `--faint` (3.2:1) borderline where used for information.

## 💬 E4 — Chat/Assistant — 🟠
- No markdown rendering — code answers render as plain wall-of-text (`textContent` bubbles).
- No streaming affordance (typing indicator/elapsed); ■ stop button undiscoverable.
- No conversation persistence across restarts despite layout restore.
- Composer row getting crowded (brain, speak, stop, send).
- Tool chips good — make them clickable to reveal what the conductor ran (transparency).

## 🍎 E5 — macOS HIG — 🟡
- No native menu bar (shortcuts discoverability, popout window management).
- No ⌘, Preferences window — settings scattered as menu toggles/store keys.
- Touch ID auto-fires on lock screen — consider firing only when focused.
- No drag-and-drop file open.

## 🔒 E6 — Security UX — 🟡
- Air-gap strip is excellent (persistent, glanceable, one-click). Copy is jargon: "free this pane" → plain language ("model APIs only — allow internet").
- Blocked-host flash vanishes in 4 s — keep a count on the strip.
- 🔒 Inconsistent passphrase minimums: lock setup 8 chars, air-gap setup modal still 4.
- 2FA enrollment hidden as ghost button in unlock modal — belongs in Preferences.

## 🎨 E7 — Visual / Design Systems — 🟡 polish
- Strongest area: real tokens, two coherent personalities, documented type scale.
- `--dv-separator-border: transparent` — adjacent light-mode cards blur together; use 1px `var(--line)`.
- Raw Unicode glyphs render inconsistently across fonts — consider a tiny inline SVG set.
- No status bar — opportunity to declutter topbar (git stats, active root, panel metadata).

---

## Consensus roadmap

| # | Pri | Item | Effort |
|---|-----|------|--------|
| 1 | 🔴 P0 | Dirty-editor close guard (confirm or autosave) | S |
| 2 | 🔴 P0 | Confirm destructive actions (delete workspace, remove folder) | S |
| 3 | 🔴 P0 | Keyboard core: ⌘W, ⌘P quick-open, ⌘1–9 tabs, palette skeleton | M |
| 4 | 🟠 P1 | Native macOS menu bar exposing commands + shortcuts | M |
| 5 | 🟠 P1 | Menu keyboard nav + modal focus trap + Esc | M |
| 6 | 🟠 P1 | Chat markdown + code blocks + copy, streaming indicator | M |
| 7 | 🟠 P1 | Group header buttons visible at rest; sidebar drag divider | S |
| 8 | 🟠 P1 | aria-labels on glyph buttons; aria-live notification log | S |
| 9 | 🟡 P2 | ⌘, Preferences pane | M |
| 10 | 🟡 P2 | Air-gap copy pass + blocked-count persistence | S |
| 11 | 🟡 P2 | Chat persistence; drag-and-drop file open | M |
| 12 | 🟡 P2 | Status bar; SVG icon set; light-mode pane separators | M |
| — | 🔒 fix | Align air-gap setup passphrase min (4→8) | XS |

**Summary:** Tome looks and thinks like a mature tool — but operates like a mouse-driven prototype. Give it a keyboard spine and safety rails first; everything else is refinement.

## Execution log — **COMPLETE** (2026-08-08)

All P0 + P1 items shipped. Build green, 228/228 tests green, app boots clean (TOME_SHOT smoke).

| WS | Scope | How | Status |
|----|-------|-----|--------|
| WS1 | 🔴 P0 safety rails: dirty-editor close guard, destructive-action confirms (delete workspace / remove folder), air-gap passphrase min 4→8 | orchestrator (main) | ✅ merged `17fbafd` |
| WS2 | 🔴 P0 keyboard spine: ⌘W close (via shared close-guard), ⌘1–9 tabs, ⌘⇧[/] cycle, ⌘P quick-open fuzzy palette, terminal zoom ⌘=/-/0 (persisted), shortcut-reference modal | subagent `ux2-keys` | ✅ merged `8dce27e` |
| WS3 | 🟠 P1 a11y: menu arrow/Esc nav + `aria-expanded`, modal focus-trap + Esc + focus restore, aria-labels on glyph buttons, `aria-live` toast region, focus rings | subagent `ux3-a11y` | ✅ merged `2ea139d` |
| WS4 | 🟠 P1 chat: dependency-free safe markdown (code blocks + copy button, headings, lists, inline code, bold/italic), streaming typing indicator + elapsed, stop-button focus | subagent `ux4-chat` | ✅ merged `f7fa180` (style.css conflict resolved — kept both appended blocks) |
| WS5 | 🟠 P1 discoverability: group ＋/⧉ visible at rest (0.4 opacity), full-height sidebar drag divider (persisted width, replaces 16px corner grip), air-gap copy de-jargoned, blocked-count tally on strip, richer tooltips | subagent `ux5-polish` | ✅ merged `98ba896` (index.html + panes.js conflicts resolved — merged aria-labels with richer tooltips) |

**Integration note:** WS2 refactored WS1's click-only close-guard into a shared `closePanel()` in `panes.js` used by both the tab ✕ and ⌘W — one confirm path.

**Method:** WS1 done directly (touches shared `panes.js`/`menus.js`); WS2–WS5 dispatched as 4 parallel headless `pi -p` subagents in isolated `git worktree`s, merged sequentially with build+test after each. Two additive CSS/markup conflicts resolved by keeping both sides and merging aria + tooltip attributes.

## P2 execution — **COMPLETE** (2026-08-08)

All four P2 tracks shipped. Build green, tests green (0 failures), app boots clean (TOME_SHOT smoke).

| WS | Scope | How | Status |
|----|-------|-----|--------|
| WS9 | Status bar (active root · open-pane count · air-gap network state) + light-mode pane separators (`--dv-separator-border: var(--line)`) | orchestrator (main) | ✅ `24f4bb3` |
| WS6 | **⌘, Preferences modal** (`preferences.js`): Appearance picker, terminal font stepper (shared `term-font-size` key, live via `setTermFontSize`), Security toggles + 2FA enroll, sidebar-width reset. Opened via ⌘, and ＋ menu | subagent `p2-prefs` | ✅ `c63bf35` |
| WS7 | **Chat persistence** (transcript → `chat-log-<chatId>`, debounced, capped 100, replayed via safe markdown; restored panes keep their chatId) + **drag-and-drop file open** (`webUtils.getPathForFile`, accent drop-highlight, gated on Files) | subagent `p2-chat` | ✅ `dbd1cb1` |
| WS8 | **Native macOS menu bar** (darwin-only, `Menu.buildFromTemplate`): app/Edit/View/Window/Pane menus; custom items → single `menu:action` IPC → `menu-bridge.js` dispatch table reusing existing functions; ⌘B/⌘W/⌘P/⌘, now native accelerators | subagent `p2-menu` | ✅ `e302a86` |

**Integration resolutions (orchestrator):**
- WS6 vs WS8 both built a ⌘, Preferences → kept WS6's full `preferences.js`; rewired WS8's `menu-bridge.js` to call it and deleted WS8's smaller duplicate modal.
- WS8 removed the renderer ⌘W/⌘P/⌘, keydown handlers (now native menu accelerators routed via menu-bridge); pruned the then-unused `preferencesModal` import from `keys.js`.
- WS7 fixed a latent bug: restored chat panes previously got a fresh chatId, orphaning the layout shell — they now reuse `saved.params.chatId`.

**Test-count note:** earlier "76/190/228" figures were vitest globbing duplicate test files inside leftover `.claude/worktrees/*`. The tracked suite (`test/`, 4 files) passes with **0 failures** throughout.

**Remaining (out of scope / future):** menu-bar live radio state for Appearance (currently opens the picker).

## Final polish — **COMPLETE** (2026-08-08)

The two deferred items plus folder icons, done directly by the orchestrator (all touch the shared visual layer — `icons.js`/`tree.js`/`style.css`/`panes.js` — so no parallel subagents).

| Item | Delivered | Commit |
|------|-----------|--------|
| **Inline SVG icon set** | New `src/renderer/icons.js`: stroke-based `currentColor` icons (16×16, 1.6px round-cap) tracking design tokens across light/dark. Topbar (sidebar, theme sun/moon/half-system, bell, add, git-branch) and group ＋/⧉ now SVG; theme icon swaps live. Kept `▚` sigil + `⛨/⛉` shields as text (brand). | `eefb992` |
| **Folder icons in tree** | Closed/open folder icons (accent) + file glyphs per row, flex layout with ellipsis; folder icon toggles on expand/collapse. | `eefb992` |
| **Status-bar per-panel metadata** | Panels expose optional `statusMeta() → { icon, text, title }`; new `#sb-context` item renders the active pane's context. Editor: live `Ln N, Col M` (CodeMirror updateListener); Terminal: kind + cwd. `dock.onDidActivePanelChange` drives refresh. | `963f5cf` |

Build green, tests green (0 failures), app boots clean (TOME_SHOT smoke, no renderer JS errors).
