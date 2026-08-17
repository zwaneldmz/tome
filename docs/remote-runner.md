# Remote flow runner (`tome-runner`)

`tome-runner` is a headless binary that executes one `flow.json` to
completion outside the desktop app — no Tauri, no renderer, no lock screen,
no human clicking "Allow." It exists so a flow can run unattended on a
server: a `git checkout` of a project, driven by `systemd --user` timers,
producing the exact same `out/<runId>/` products, `manifest.json`, and
`runs-index.json` a background run inside the desktop app would.

Source: `src-tauri/crates/tome-runner/`. It depends only on `tome-flow`
(the tauri-free flow-run engine the desktop app's own `runs:*` commands are
built on — see `src-tauri/crates/tome-flow/src/lib.rs`), `tokio`, and
`serde_json`. It carries no Tauri dependency and no IPC surface; the only
way to talk to it is its own two subcommands.

## Prerequisites

- **A git checkout of the project on the server**, at whatever path you'll
  pass to `tome-runner run`. `tome-runner` reads flow files straight off
  disk — there is no sync step.
- **`cargo build --workspace`** run once on that checkout, so `tome-runner`
  and `tome-shim` (its Linux sandbox helper — see "The air gap" below) land
  side by side in the same `target/{debug,release}/` directory.
  `tome-runner` resolves `tome-shim` as its own sibling; there is no
  install step or `$PATH` lookup for it.
- **An agent CLI on `$PATH`** for whichever `kind` your flow's nodes use
  (`claude`, `opencode`, `pi`, or a vetted custom agent — see
  `tome_flow::agent_spawn`). `tome-runner` resolves `$PATH` the same way
  the desktop app does: by shelling out to your login shell once per
  process and harvesting its interactive `PATH`
  (`tome_flow::login_env::login_env`), so `~/.local/bin` and similar
  user-installed locations are found even when the systemd unit's own
  environment is minimal.
- **`ANTHROPIC_API_KEY`** (and any other provider credentials your flow's
  agents need) in the `EnvironmentFile` — see "Credentials" below. Provider
  keys are never read from this repo's own files or from flow content.
- **`bwrap` (bubblewrap), or a kernel that allows unprivileged user
  namespaces, on Linux.** Every run this binary starts is air-gapped,
  unconditionally (see "The air gap" below) — on Linux that gap is
  enforced by bubblewrap when it's installed, or by `tome-shim` unsharing
  its own namespaces when it isn't. If neither is available,
  `tome-runner run` refuses to start anything and exits non-zero,
  printing an install hint. On macOS the air gap is enforced by the
  seatbelt profile (`sandbox-exec`), which every macOS install ships —
  there is no equivalent refusal path there.

## The air gap

Every node `tome-runner` spawns is gapped — always. There is no flag to
turn this off, no store preference, nothing to read: unlike the desktop
app (which asks whether the user wants a background run gapped), an
unattended, remotely-triggered process has no one to ask, so it makes the
same choice the desktop app's own in-app scheduler makes for the identical
reason (`src-tauri/src/schedule.rs`'s `SCHEDULED_RUN_AIRGAP`). A gapped
node's only route to the network is its own per-node loopback proxy,
enforcing an allowlist of hostnames — see the next section for where that
allowlist comes from.

## `~/.config/tome-runner/airgap.json` — the egress allowlist

Every gapped node starts from the same shipped provider defaults every
other airgap consumer in this codebase uses (`api.anthropic.com`,
`api.openai.com`, and so on — the full list is
`tome_flow::airgap::allowlist::DEFAULT_ALLOW`). If your flow's agents need
to reach anything else — an internal API, a private package registry —
list it in `~/.config/tome-runner/airgap.json`:

```json
{
  "allow": [
    "internal-api.example.com",
    "*.corp.example.net"
  ]
}
```

Same shape, and the same pattern rules (`*` matches exactly one DNS label;
no bare `*`, no wildcard TLD, no scheme/path/userinfo — see
`tome_flow::airgap::allowlist`'s own doc comment), as the desktop app's own
allowlist override file and a repo's `.tome/airgap.json`. A missing file,
an unreadable file, malformed JSON, or an entry that fails validation all
fail closed to "no extra hosts" — never a wider gap by accident.

**This file lives under the server owner's own `$HOME`, and that is
deliberate — `tome-runner` never reads a repo's own `.tome/airgap.json`.**
The desktop app treats a repo's `.tome/airgap.json` as a CONSENT-gated
suggestion: a human reviews it and clicks Allow before its hosts are
applied. Nobody is at the keyboard for a run that fires at 3am, so
`tome-runner` never applies that consent step automatically — if it read a
repo-supplied allowlist unattended, a compromised or malicious commit
could ship its own wider `.tome/airgap.json` alongside itself and grant
its own future runs new egress with nobody to approve anything. Treat
`~/.config/tome-runner/airgap.json` as you would any other credential-
adjacent config: only the server owner should be able to write it, and
nothing inside the repo checkout ever should.

## Credentials

`~/.config/tome-runner/env` is a plain `KEY=value`-per-line file, read by
systemd's own `EnvironmentFile=` directive (see the generated unit below)
— **not** by `tome-runner` itself. A minimal one:

```
ANTHROPIC_API_KEY=sk-ant-...
```

Add whichever other provider keys `tome_flow::agent_env::AGENT_SECRET_KEYS`
lists that your flow's agents need
(`OPENAI_API_KEY`, `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`, and so on).

**Agent credentials on this server are the server owner's own
responsibility.** `tome-runner` does not manage, rotate, or validate this
file — it only ever reads whatever a spawned agent's own login-shell
environment or this `EnvironmentFile` already contains, exactly the way
the desktop app reads a user's own login-shell environment on their
laptop. Treat `~/.config/tome-runner/env` like any other secrets file on a
shared machine: restrict it to the account `tome-runner` runs as
(`chmod 600 ~/.config/tome-runner/env`), and rotate/revoke keys through
your provider the same way you would for any other unattended service.

## Running a flow once

```
tome-runner run <flow.json>
```

Runs the flow to completion and exits:

| Exit code | Meaning |
| --- | --- |
| `0` | The run settled `done`. |
| `1` | The run started but settled `failed` or `canceled`. |
| `2` | Usage/config error — bad argv, an unresolvable `$HOME`, the Linux sandbox ladder refusing (no bwrap, no unprivileged userns), or the flow itself being refused before any run id existed (not a flow file, a dependency cycle, a node kind with no headless template, a path outside the flow's own root). |

Progress and the final result are printed to stderr; nothing on stdout is
meant to be parsed. DAG scheduling, the fail-closed output contract (a
node that exits 0 still fails the run if it didn't actually write every
output it declared), product promotion into `out/<runId>/`,
`manifest.json`, and `runs-index.json` are all identical to a background
run started from the desktop app — `tome-runner` calls the same
`tome_flow::flow::runner::start_run` the app's own `runs:start` command
does, with its own headless environment wired in underneath.

Every run also appends to `~/.local/state/tome-runner/events.jsonl` — one
JSON object per line (`ts`, `kind`, then event-specific fields), the same
shape as the desktop app's own persistent event log. This file has no
built-in rotation or cap; point `logrotate` at it if you're running this
on a schedule long-term.

## Scheduling a flow

```
tome-runner schedule install <flow.json> --on-calendar "<systemd calendar expression>" [--unit-dir <dir>]
```

Writes a `systemd --user` service+timer pair (default `--unit-dir`:
`~/.config/systemd/user`) named after the flow file itself (`nightly.flow.json`
becomes `tome-flow-nightly.service`/`.timer`) and prints the commands you
still need to run yourself:

```
Installed tome-flow-nightly.service and tome-flow-nightly.timer in /home/tester/.config/systemd/user

Next steps:
  systemctl --user daemon-reload
  systemctl --user enable --now tome-flow-nightly.timer
  loginctl enable-linger "$USER"   # keep the timer running after you log out
```

`tome-runner` never runs `systemctl` on its own behalf — installing a unit
is a persistent, privileged change to what runs unattended on this
machine, and that stays an explicit action a person types. The generated
service is `Type=oneshot`, `ExecStart=<absolute tome-runner> run
<absolute flow.json>`, `EnvironmentFile=~/.config/tome-runner/env`; the
timer sets `OnCalendar=<your expression>` and `Persistent=true` (a tick
missed while the server was down for maintenance fires once the timer is
next loaded, rather than being silently skipped).

### Unit lifecycle

- **Install**: `tome-runner schedule install ...` (above) writes the two
  unit files. Re-running it for the same flow file overwrites them in
  place — useful after changing the schedule or moving the checkout.
- **Enable**: `systemctl --user daemon-reload && systemctl --user enable
  --now tome-flow-<name>.timer` — the commands `install` printed for you.
- **Inspect**: `systemctl --user list-timers` shows the next scheduled
  run; `journalctl --user -u tome-flow-<name>.service` shows a given run's
  stderr output; `~/.local/state/tome-runner/events.jsonl` (see above) has
  the structured event trail across every run.
- **Stop a run in progress**: `systemctl stop tome-flow-<name>.service` —
  systemd signals every process in the unit's cgroup, which reaches both
  `tome-runner` itself and whatever agent process (and its own process
  group) it spawned.
- **Disable**: `systemctl --user disable --now tome-flow-<name>.timer`.
- **Remove**: disable it first, then delete both unit files from the
  `--unit-dir` and run `systemctl --user daemon-reload`. `tome-runner` has
  no uninstall subcommand — this is the same two-line `rm` any other
  hand-installed systemd unit needs.
- **Survive logout**: `loginctl enable-linger "$USER"` (also printed by
  `install`) — without it, a `--user` unit's timers stop firing once your
  own login session ends, which defeats the point of scheduling something
  server-side.
