# Security

Tome is a desktop coding harness: agent CLIs (Claude Code, opencode, pi),
terminals, editors, and an assistant in one Electron workspace. Its security
claim in one sentence: **agent panes run inside an OS sandbox with no direct
network egress, and the only route out — a per-pane allowlist proxy — widens
only behind a second factor, never the sandbox itself.**

This document is the evaluation-facing summary. The maintainer-facing
invariants live in [docs/THREATMODEL.md](docs/THREATMODEL.md).

## Trust boundaries

1. **Renderer ↔ main.** The renderer is sandboxed (`sandbox: true`) and
   reaches main only over vetted IPC channels. Every handler refuses while
   the app is locked except an explicit pre-login allowlist (fail-closed lock
   gate); the channels that stay open pre-login accept only slug-shaped keys
   and can never reach the credential file or the egress allowlist.
2. **Agent pane ↔ host.** Agent CLIs run under the macOS seatbelt
   (`sandbox-exec`) with all direct egress denied, DNS included. The only way
   out is a per-pane CONNECT proxy on `127.0.0.1` enforcing the
   model-provider allowlist. Freeing a pane widens the *proxy*, never the
   sandbox — the seatbelt profile is fixed at spawn and no code path weakens
   it afterward; the proxy remains the only route out even when open.
3. **Model ↔ tools.** The assistant (conductor) reads terminal scrollback and
   issues tool calls against the workspace — a classic confused deputy, since
   any program can print text that looks like instructions. Caps: scrollback
   is ANSI/control-stripped before the model sees it; the tool loop is
   bounded to 8 turns; `type_in_terminal` only submits (`\r`) when the user
   has explicitly enabled auto-run, and with auto-run off, control characters
   that would submit or signal on their own are stripped from typed text;
   file opens on the model's behalf are confinement-checked against open
   workspace folders and brain vaults.

## Key invariants

- **Second-factor pane unlock.** App login already proves the passphrase (or
  Touch ID); freeing an air-gapped pane therefore demands a *second* factor —
  the TOTP code when enrolled, the passphrase again otherwise.
- **Credentials and allowlist are unreachable from panes.** The seatbelt
  denies sandboxed panes reads of the auth file (scrypt passphrase hash +
  TOTP secret, 0600) and writes under `userData` generally (allowlist
  tamper). The note vault lives outside `userData` precisely so agents get
  full read/write of it with zero sandbox changes.
- **Vetted pane kinds.** The renderer names a pane kind from a shared
  allowlist; main builds the command line. A compromised renderer cannot
  request arbitrary binaries or arguments.
- **Secrets are scoped.** Provider keys are read once from an interactive
  login shell and merged into the env of agent panes only; the chat API key
  lives in main and never enters the renderer.
- **Repo allowlists are untrusted input.** A repo's `.tome/airgap.json` is
  validated by the same wildcard compiler as the user allowlist (over-broad
  patterns refused), and honored only after user consent. Consent is verified
  and stored in main, pinned to the file's SHA-1: main re-hashes the file at
  consent time, at every boot, and at every workspace sync — a post-consent
  edit re-prompts, and deleting the file is a real revocation. A compromised
  renderer can only *ask* main to re-check the file; it cannot widen egress.
- **The event log records actions, never payloads.** `userData/events.jsonl`
  keeps a capped, append-only audit of security-relevant actions — conductor
  tool calls (name, ids, outcome), air-gap unlocks/relocks, blocked egress
  hosts. Tool inputs/outputs and typed text stay out by design: they may
  contain secrets. The renderer reads it only through the lock-gated
  `events:list` channel.

## Independent review

Two independent reviews scored the codebase **8.0 / 10**:

- [reviews/kimi-k3-review.txt](reviews/kimi-k3-review.txt) — five-expert
  council, 2026-08-07.
- [reviews/pi-review.md](reviews/pi-review.md) — independent second reviewer;
  scores assigned before comparison, 2026-08-07.

Findings from both are tracked in
[docs/IMPROVEMENTS-STATUS.md](docs/IMPROVEMENTS-STATUS.md).

## Reporting a vulnerability

- Use **GitHub Security Advisories** on
  [zwaneldmz/tome](https://github.com/zwaneldmz/tome/security/advisories).
- For non-sensitive bugs: the
  [GitHub issue tracker](https://github.com/zwaneldmz/tome/issues).

**In scope:** sandbox escapes (seatbelt or proxy bypass), lock-gate bypasses
(IPC reachable pre-auth that shouldn't be), consent bypasses on repo
allowlists, confused-deputy paths through the conductor that defeat the
auto-run guard.

**Out of scope:** `TOME_SHOT` dev mode — a lock-gate bypass that exists for
development screenshots, gated on `!app.isPackaged` and documented in the
threat model; issues requiring physical access or an already-compromised
host; the `xlsx` package's CDN distribution pin (deliberate, integrity-pinned
— see [docs/THREATMODEL.md](docs/THREATMODEL.md)).

---

*[docs/THREATMODEL.md](docs/THREATMODEL.md) is the maintainer-facing
companion.*
