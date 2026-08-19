# Security

Tome is a desktop coding harness: agent CLIs (Claude Code, opencode, pi),
terminals, editors, and an assistant in one Tauri window — a Rust backend
(`src-tauri/src/`) and a single webview renderer (`src/renderer/`). Its
security claim in one sentence: **agent panes run inside an OS sandbox with
no direct network egress, and the only route out — a per-pane allowlist
proxy — widens only behind a second factor, never the sandbox itself.**

This document is the evaluation-facing summary. The maintainer-facing
invariants live in [docs/THREATMODEL.md](docs/THREATMODEL.md).

## Trust boundaries

1. **Renderer ↔ backend.** The renderer is a Tauri webview — there is no
   Electron `sandbox: true` flag; the trust boundary is renderer ↔ Rust
   over Tauri IPC, gated fail-closed. Every `#[tauri::command]` calls
   `lock_gate::guard` (`src-tauri/src/lock_gate.rs`) and refuses while the
   app is locked except an explicit `OPEN_CHANNELS` allowlist; the store
   channels that stay open pre-login accept only slug-shaped keys
   (`src-tauri/src/store_keys.rs`) and can never reach the credential file
   or the egress allowlist (`RESERVED_KEYS`).
2. **Agent pane ↔ host.** Agent CLIs run under the macOS seatbelt
   (`sandbox-exec`, profile built in `src-tauri/src/egress/seatbelt.rs`) or
   the Linux bubblewrap/unshare ladder (`src-tauri/src/egress/linux.rs`)
   with all direct egress denied, DNS included. The only way out is a
   per-pane CONNECT proxy on `127.0.0.1`
   (`src-tauri/src/egress/proxy.rs`) enforcing the model-provider
   allowlist (`src-tauri/src/egress/allowlist.rs`). The sandbox also
   confines the filesystem: Linux rung 1 mounts a curated allow-list
   (no more `--dev-bind / /`), rung 2 enforces the same list via Landlock,
   and the macOS seatbelt profile denies the Docker socket by path — a
   gapped pane can't reach a container-runtime daemon and escape. Freeing a
   pane widens
   the *proxy*, never the sandbox — the seatbelt profile / bwrap wrap is
   fixed at spawn and no code path weakens it afterward; the proxy remains
   the only route out even when open.
3. **Model ↔ tools.** The assistant (conductor, `src-tauri/src/conductor/`)
   reads terminal scrollback and issues tool calls against the workspace — a
   classic confused deputy, since any program can print text that looks like
   instructions. Caps: scrollback is capped (`SCROLL_CAP`) and
   ANSI/control-stripped before the model sees it; the tool loop is bounded
   to 8 turns (`MAX_TURNS`); `type_in_terminal` only submits (`\r`) when the
   user has explicitly enabled auto-run, and with auto-run off, control
   characters that would submit or signal on their own are stripped from
   typed text; file opens on the model's behalf are confinement-checked
   against open workspace folders and brain vaults (`src-tauri/src/confine.rs`).

## Key invariants

- **Second-factor pane unlock.** App login already proves the passphrase (or
  Touch ID); freeing a contained pane therefore demands a *second* factor —
  the TOTP code when enrolled, the passphrase again otherwise
  (`src-tauri/src/authlock.rs`).
- **Credentials and allowlist are unreachable from panes.** The seatbelt
  denies sandboxed panes reads AND writes of the whole app config dir
  (scrypt passphrase hash + TOTP secret, the allowlist, repo consents,
  event log, store files — all 0600), not just the auth file (F-03). On
  Linux, the bwrap wrap replaces the config dir with a fresh tmpfs, and
  the self-unshare rung enforces a Landlock `PathBeneath` whitelist
  that never includes the config dir (F-02). The note vault lives outside
  the app config dir (`~/Tome/Brains/<ws>`, `src-tauri/src/brain.rs`)
  precisely so agents get full read/write of it with zero sandbox changes.
- **Container-runtime sockets are unreachable.** The Linux allow-list
  (`egress::linux::default_landlock_allow_set`) and the macOS seatbelt
  profile both exclude the Docker socket (`~/.docker`, rootless
  `$XDG_RUNTIME_DIR/docker.sock`, `/var/run/docker.sock`), closing the
  "spawn a privileged container and mount the host" escape on both
  platforms.
- **Vetted pane kinds.** The renderer names a pane kind from a shared
  allowlist; the Rust backend builds the command line
  (`src-tauri/src/agent_spawn.rs`, `src-tauri/src/custom_agents.rs`). A
  compromised renderer cannot request arbitrary binaries or arguments.
- **Secrets are scoped.** Provider keys are read once from an interactive
  login shell (`src-tauri/src/login_env.rs`) and merged into the env of
  agent panes only; the chat API key lives in the Rust backend and never
  enters the renderer.
- **Repo allowlists are untrusted input.** A repo's `.tome/egress.json` is
  validated by the same wildcard compiler as the user allowlist (over-broad
  patterns refused, `src-tauri/src/egress/allowlist.rs`), and honored only
  after user consent. Consent is verified and stored in the backend
  (`src-tauri/src/egress/mod.rs`), pinned to the file's SHA-1: the backend
  re-hashes the file at consent time, at every boot, and at every workspace
  sync — a post-consent edit re-prompts, and deleting the file is a real
  revocation. A compromised renderer can only *ask* the backend to re-check
  the file; it cannot widen egress.
- **The event log records actions, never payloads.**
  `<app_data_dir>/events.jsonl` keeps a capped, append-only audit of
  security-relevant actions — conductor tool calls (name, ids, outcome),
  pane unlocks/relocks, blocked egress hosts
  (`src-tauri/src/eventlog.rs`, cap 5000; `src-tauri/src/events.rs`). Tool
  inputs/outputs and typed text stay out by design: they may contain
  secrets. The renderer reads it only through the lock-gated `events:list`
  channel.

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
development screenshots, gated on `tauri::is_dev()` (see
`src-tauri/src/lib.rs`'s `boot_auth_and_egress`) and documented in the
threat model; issues requiring physical access or an already-compromised
host; the `xlsx` package's CDN distribution pin (deliberate, integrity-pinned
— see [docs/THREATMODEL.md](docs/THREATMODEL.md)).

---

*[docs/THREATMODEL.md](docs/THREATMODEL.md) is the maintainer-facing
companion.*
