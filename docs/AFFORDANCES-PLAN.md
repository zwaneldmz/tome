# Tome — Affordances & UX Improvement Plan

**Date:** 2026-08-14 · **Context:** post-Tauri-rewrite release prep. Builds on
`docs/ux-council-report.md` (2026-08-08) and re-checks each finding against
the *current* renderer (several council items have since been fixed: native
menu bar exists, markdown chat rendering exists, chat persistence exists,
discard-on-close prompts exist, Touch ID is real, OS file drop is back).

An "affordance" here means: a capability the app *has* that the user can
*perceive, reach, and trust*. Tome's core properties — sandboxed agents,
the air gap, flows — are invisible when they work. The plan is organized so
that the things that make Tome *Tome* become visible without becoming noisy.

---

## 0. What already landed (do not re-do)

- Native menu bar on all platforms (Tauri port), ⌘, Settings, Setup Wizard.
- Markdown chat rendering, streaming, abort, conversation persistence.
- Dirty-editor discard prompts; popout close prompts (move-or-close).
- Touch ID unlock (real LAContext wiring, this session).
- OS file-drop-to-open (revived this session, dragDropEnabled + DnD toggle).
- Popout windows (real WebviewWindow-backed, this session).
- Status bar (git stats, active root, pane metadata) — `statusbar.js`.

---

## 1. Make the security model legible (the product's premise)

The air gap is the reason Tome exists; it is also the easiest thing to
misunderstand. Council E6's "jargon" note is the tip: every security
surface should answer three questions at a glance — *what is contained,
what just happened, what can I do about it.*

1.1 **Air-gap strip → security center.** Keep the persistent strip, add a
click-through popover: per-pane state (gapped/free), allowlisted hosts with
one-click revoke, and a running count of blocked attempts since unlock
(council: the 4 s blocked-host flash vanishes; the count must persist).
Copy pass: "free this pane" → "allow internet beyond model APIs";
"gapped" → "model APIs only".

1.2 **First-run trust narrative.** Onboarding currently configures the
lock; it should also *show the gap working*: spawn a demo pane, watch a
blocked request land in the strip, then let the user allowlist it. A
60-second interactive proof converts the premise from marketing copy into
muscle memory. (This doubles as the missing "live agent run" smoke —
see §5.)

1.3 **Consent diffs, not consent walls.** Repo allowlist consent should
show a *diff* of what changed since last consent (added hosts highlighted)
rather than the full list — the user should be able to answer "what's new?"
in one second. Revocation gets the same prominence as consent.

1.4 **Lock screen affordances.** Touch ID now fires immediately; add the
council's focus guard (only auto-prompt when the window is focused), keep
the one-Esc fallback, and surface 2FA enrollment in Settings (it's
currently a ghost button in the unlock modal).

## 2. Keyboard parity (mouse-first in a keyboard-first domain)

Council E1's 🔴 is still mostly open. Minimum viable set, in priority order:

2.1 **Command palette (⌘⇧P)** — the single highest-leverage addition:
every menu action, pane opener, flow run, and theme switch behind one
fuzzy search. It simultaneously solves discoverability (E2's invisible
features) and keyboard poverty.

2.2 **Quick-open file (⌘P)** over the workspace tree; **pane switching**
(⌘1–9 by tab position, ⌘⇧[/] cycle); **close pane** (⌘W with the existing
dirty/discard prompts); **toggle sidebar** (⌘B exists — verify), **terminal
zoom** (⌘+/⌘− adjusting xterm fontSize, currently hardcoded 12.5).

2.3 **Shortcut reference** — `?` overlay (GitHub-style) listing every
binding; doubles as the discoverability answer for glyph-only iconography
until icons get labels.

2.4 **Menu & modal keyboard correctness** — arrow-key nav + Esc in
`role="menu"` menus, focus trap + Esc in modals, `aria-expanded` on
triggers (E3). These are small, mechanical, and testable.

## 3. Pane-grid interactions (the core loop)

3.1 **Popout, now real** — verify the new WebviewWindow popouts: tear-off
past window edge, group-header ⧉ button, drag back into main window,
close-with-panes prompt, theme sync (`trackThemedDocument`), and layout
persistence across restart (popout groups should restore as popouts).
Then make the affordance visible: the ⧉ button is `opacity: 0` until
hover — raise to a dim resting state (0.35) so the capability is
discoverable without a coach mark.

3.2 **Drop-target teaching.** The first time a user drags a tab, show a
one-time hint overlay naming the three outcomes (split, tab, tear-off).
Persist "seen" in the store.

3.3 **Sidebar resize** — replace the 16 px CSS corner grip with a
full-height drag divider (E2).

3.4 **OS file drop** — now revived; verify on Linux/WebKitGTK (the one
runtime never exercised): drop opens panes, hover highlight appears,
pane drags don't regress (the disableDnd toggle).

## 4. Conductor & chat (the assistant surface)

4.1 **Tool-call transparency** — make tool chips clickable: expand to show
exactly what the conductor ran (command, args, result summary). This is
the trust affordance for the whole "assistant acts in my workspace"
premise.

4.2 **Streaming affordance** — elapsed-time + token counter in the
composer while a run streams; keep ■ stop visible (not hover-only).

4.3 **Voice dictation** — whisper-cli path is now verified end-to-end
(this session); surface mic state honestly (recording / transcribing /
unavailable-with-reason) and add the model-download flow to onboarding
instead of a bare error string.

4.4 **Provider fallbacks** — the Anthropic `fallbacks: "default"` wire is
now verified; expose a per-provider "server-side fallback" toggle in
Settings → Assistant rather than only the hardcoded beta path, so
non-beta Anthropic users can opt in knowingly.

## 5. Live-proof gaps (engineering affordances)

These are the "never exercised end-to-end" items, reframed as work the
*product* needs before release:

5.1 **First-live-run checklist** (manual, pre-release): spawn a real
claude pane → confirm a gapped pane reaches only allowlisted hosts
(curl matrix covers this in CI now — spot-check one real agent) → run a
flow DAG → drive the conductor with a real API key. Record the session;
fix what breaks.

5.2 **Linux runtime smoke** — the Phase 0 de-risk spikes (xterm perf on
WebKitGTK, speechSynthesis, tome:// protocol, iframe sandbox) have never
run on Linux. One afternoon on a real Ubuntu box (or a VM) with a
checklist; file issues, don't fix blind.

5.3 **Rung-2 honesty** — on no-bwrap Linux systems the fallback rung
lacks Landlock file-hiding (TODO(landlock) in tome-shim). Until that
lands, the security UI should *say so*: detect the active rung at spawn
time and show "network-contained only" vs "fully contained" per pane.
Honest degradation beats silent weakness.

## 6. Release readiness (GitHub) — **DONE 2026-08-14**

6.1 ✅ Merged `rewrite/tauri` → `main` (PR #1, plus the smoke-test
follow-up PR #2 with the verification evidence in the body — CI runs,
STT output, SDK-type verification of `fallbacks`).

6.2 ✅ Electron removal replayed (PR #3): `release.yml` deleted,
`src/main` + `src/preload` + their 20 vitest suites removed, Electron
deps dropped, `release-tauri.yml` is the sole tag pipeline. Required two
fixes found by CI: build.yml's smoke check pointed at electron-vite's
`out/renderer` (now `dist-web`, minification-safe marker) and a
fire-and-forget log-write race in the flow runner (flush + poll).

6.3 ✅ Tagged `v0.2.0` (v0.1.0 was taken by the Electron-era release).
release-tauri.yml builds macOS universal + Linux x86_64 bundles,
SHA256SUMS, attestation. Four real failures found and fixed en route:
rustup can't install the `universal-apple-darwin` pseudo-target (PR #5),
the tome-shim sidecar needed staging per-arch (PR #6), tauri-action
passes the pseudo-target verbatim (PR #7) and the bundler wants a
lipo'd `tome-shim-universal-apple-darwin` (PR #8), and an unset
`APPLE_SIGNING_IDENTITY` must fall back to ad-hoc `-` (PR #9). Release
published unsigned with verification steps in the notes (checksums +
the Rekor transparency-log entry — `gh attestation verify` does not
surface this repo's attestations, so the notes walk the Rekor lookup).

6.4 ✅ `main` force-synced to the GitLab mirror (via merge PR #4 —
GitLab rejects force-push on protected main); the `release.yml`
phantom-failure noise is gone with the file; README screenshot refreshed
from the released v0.2.0 dmg (PR #10).

---

## Sequencing

| Phase | Items | Gate |
|-------|-------|------|
| Now | §6.1 merge, §6.2 Electron removal, §6.3 tag | linux-sandbox green |
| Pre-release | §5.1 live run, §5.2 Linux smoke, §3.4 Linux drop check | one afternoon each |
| Release +1 | §1.1 strip popover, §2.1 palette, §2.2 keys, §3.1 popout verification | 1–2 weeks |
| Release +2 | §1.2 onboarding proof, §1.3 consent diffs, §4.1 tool chips, §5.3 rung honesty | 2–4 weeks |
| Ongoing | §2.3/§2.4 a11y mechanics, §3.2 hints, §4.3/§4.4 | rolling |

The through-line: **Tome's differentiators are invisible by default.**
Every item above either makes a working safety property perceivable
(§1, §5.3), makes a working feature reachable (§2, §3), or makes an
assistant action auditable (§4). Nothing here adds a new subsystem —
it spends the capabilities already built.
