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

## Execution log
- WS1 (P0 safety rails): dirty close guard, destructive confirms, passphrase min — see git history.
- WS2 (keyboard core), WS3 (a11y menus/modals), WS4 (chat markdown), WS5 (discoverability polish) — delegated to headless subagents, verified with build+test after each.
