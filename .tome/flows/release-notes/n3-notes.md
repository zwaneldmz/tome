# Tome — unreleased (changes since v0.2.0)

## Breaking changes

None. One naming change to know about: the safety boundary formerly called the **air gap** is now the **containment cell** (#12) — it's an OS sandbox plus an allowlisted loopback proxy, not a true air gap, and the docs now say so. The rename is docs-only: config files (`airgap.json` and friends in the app data directory) and all settings are unchanged, and existing setups keep working.

## New

### Mentor mode

Tome's assistant can now teach instead of just doing. Turn on mentor mode (per workspace, or globally as the default) and the assistant works from a catalog of bundled skills, pausing at key moments to check your understanding with short test gates — multiple choice, true/false, short answer, or code. Multiple-choice and true/false answers are scored locally; short-answer and code answers are judged by the model. A Skip button always lets you through, and a small understanding-score ring in the status bar tracks how you're doing. There's also a comprehension gate before `git commit` and `git push`: explain the change in your own words before it ships. Which gates are active is configurable in Mentor settings.

Mentor mode also brings a **review report**: a read-only pane that summarizes your session's local signals into an LLM-written report, which you can promote into the workspace's brain.

### Git, without leaving the app

Stage files, write commits, and push from a new commit UI, with the optional mentor gate described above in front of commit and push.

### Command palette

Quick-open grew into a command palette on `Cmd/Ctrl+K`: files, open panes, and app actions in one place.

### Meet Viibi

A small mascot now lives in the status bar and mirrors what the app is doing — resting when idle, reading while flows or chat are busy, on guard while the containment cell is holding, and visibly unhappy when something is blocked or failing. Click it for a companion popover.

### More chat providers

- **DeepSeek** (V4 Pro and V4 Flash) joins the built-in provider list.
- A **custom provider** slot accepts any OpenAI- or Anthropic-compatible endpoint: set the base URL, model, and key in Preferences → Assistant. The key is stored alongside the other providers' keys in the app's store.

## Fixed

- **Saved layouts now restore.** A guard bug made layout restore silently do nothing on every launch. Restoring also no longer produces duplicate terminal tabs, and background tabs and slow-loading panes (like a saved flow tab) survive a restart instead of being dropped.

## Internal

CI was overhauled end to end — hardened GitHub workflows with format/lint/audit gates, Rust checks on macOS, macOS signing and notarization verification, a wider Linux sandbox test matrix plus a Fedora proof, and a new GitLab pipeline — alongside a tooling migration from npm to bun, dependency upgrades, a workspace-wide rustfmt pass, and refreshed docs and screenshots for the current build (#10–#13).
