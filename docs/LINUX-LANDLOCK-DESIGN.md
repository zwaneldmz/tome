# Linux File Confinement — Design (F2 + Docker-escape hardening)

**Status:** implemented. One shared allow-list
(`src-tauri/crates/tome-flow/src/egress/linux.rs`'s
`default_landlock_allow_set`), enforced by **two mechanisms**:

- **Rung 1 (bwrap)** — `build_bwrap_mounts` mounts the same roots as a
  curated mount set, replacing the old `--dev-bind / /` (whole host root).
- **Rung 2 (self-unshare)** — `src-tauri/crates/tome-shim/src/linux.rs`
  (`apply_landlock`) enforces the same roots via a Landlock `PathBeneath`
  whitelist built from `--allow-read`/`--allow-write`.

CI-gated assertions (auth unreadable / config unwritable / workspace+tmp
writable / Docker socket unreachable / host files unwritable) live in
`src-tauri/src/linux_sandbox_integration_tests.rs` and run in
`.github/workflows/linux-sandbox.yml`.
**Owner:** `src-tauri/crates/tome-shim` (rung-2 mechanism) + `src-tauri/crates/tome-flow/src/egress/linux.rs` (shared allow-list + both argv builders).

## Problem

On macOS the seatbelt profile (a **deny-list**) denies agent **writes** under
the app config dir and **reads** of `egress-auth.json`. On Linux the bwrap
rung mounted the **entire host root** read-write (`--dev-bind / /`), so a
gapped pane could reach the Docker socket (or any host file) and escape; the
self-unshare rung had only the network-namespace egress kill, with file
confinement a `TODO(landlock)`.

## Why the existing flags don't map 1:1

`--deny-write <dir>` / `--deny-read <file>` express a **deny-list**. Landlock is
an **allow-list** LSM: a ruleset *handles* a set of access rights, then grants
them per-path via `PathBeneath` rules. Handled-but-ungranted rights are denied;
unhandled rights pass through; and there is no "deny this one subtree" or
"except" rule. So "allow everything except the config dir" is inexpressible as a
Landlock ruleset — and bwrap's `--dev-bind / /` was the deny-list's Linux
incarnation, with the whole host exposed.

## Correct posture: a whitelist

Handle the access rights the sandbox cares about and allow them only beneath the
paths the agent may legitimately touch:

- **Read broadly** — system roots (`/usr`, `/etc`, `/bin`, `/opt`), the
  workspace, `node_modules`, and the agent's *own* config dirs
  (`~/.claude`, `~/.config/opencode`, `~/.config/pi`).
- **Write narrowly** — the workspace, the brain vault, `/tmp`, and the agent's
  own config dirs, plus a curated set of common tool roots (`~/.ssh`,
  `~/.npm`, `~/.cargo`, `~/.local/share`, `~/.claude.json`, `~/.gitconfig`).
- **Never** the app config dir (which transitively makes the store and
  `egress-auth.json` unreadable/unwritable), and **never** `~/.docker` or a
  container-runtime socket — the Docker-escape exclusion.

## Change set

### Shared allow-list

1. `default_landlock_allow_set(cwd, home, brain, path_entries)` returns
   `(allow_read, allow_write)`, both consumed by rung 1 and rung 2.

### Rung 2 (Landlock)

2. Thread the allow-set through the spawn spec: `GappedSpawnSpec`
   (`egress/linux.rs`) → `build_self_unshare_argv` → repeatable
   `--allow-read` / `--allow-write` flags → `ShimArgs`.
3. `apply_landlock(allow_read, allow_write)`:
   - `Ruleset::default().handle_access(AccessFs::from_all(abi))` with best-effort
     ABI detection.
   - `PathBeneath` allow rules for each allowed root.
   - `set_no_new_privs(true)` + `restrict_self()`.
   - On unsupported kernel, log a NOTE and continue egress-only (fail-open on
     *file* confinement — the netns egress kill stays the load-bearing control).
4. Call it in `run()` **after** `self_unshare()` and **before** spawning the
   child (Landlock restrictions are inherited by descendants).
5. Keep `--deny-write`/`--deny-read` for wire compatibility but reinterpret them:
   the config dir/auth file become the implicit *excluded* roots.

### Rung 1 (bwrap curated mounts)

6. `build_bwrap_mounts(spec)` emits, in order:
   - special mounts — `--proc /proc`, `--dev /dev`, `--ro-bind-try /sys /sys`,
     `--tmpfs /tmp`, `--tmpfs /run`;
   - `--ro-bind` for `/usr`/`/etc` (hard) and `--ro-bind-try` for the other
     read roots (`/bin`/`/sbin`/`/lib`/`/lib64`/`/opt` + PATH entries);
   - `--bind-try` for every write root (workspace, brain, agent-config dirs,
     safe dirs, single files) so a missing `~/.npm` never aborts the spawn;
   - the proxy-socket bind and the config-dir `--tmpfs` last.
7. `build_bwrap_argv` splices that mount section in place of `--dev-bind / /`,
   keeping the shim tail (`-- <shim> --port P --sock … -- <inner>`) unchanged.

## Testing

The `#[ignore]`d `linux_sandbox_integration_tests.rs` (run on
ubuntu-22.04/24.04 + fedora) asserts, for **both rungs**:

- the agent **cannot read** `egress-auth.json`;
- the agent **cannot write** a file under the app config dir;
- the agent **can still write** under the workspace and `/tmp`;
- (rung 1) the agent **cannot reach** the Docker socket;
- (rung 1) the agent **cannot write** a host file outside the allow-set.

## Kernel/ABI notes

- ABI v1 (Linux 5.13): file access rights. v2 (5.19): network. v4 (6.7): ioctl +
  truncate. Use the highest supported ABI and degrade.
- Landlock is inert until `landlock_restrict_self` is called, is unprivileged,
  and stacks cleanly with the existing network namespace.
