# Linux Landlock File Confinement — Design (F2)

**Status:** design only, not implemented. Needs a real-Linux test loop to land safely.
**Owner:** `src-tauri/crates/tome-shim` (mechanism) + `src-tauri/src/egress/linux.rs` (argv builder).

## Problem

On macOS the seatbelt profile (a **deny-list**) denies agent **writes** under
the app config dir and **reads** of `egress-auth.json`. On Linux rung 2
(`tome-shim --self-unshare`), only the network-namespace egress kill is
enforced today; file confinement is the `TODO(landlock)` in
`crates/tome-shim/src/linux.rs`.

## Why the existing flags don't map 1:1

`--deny-write <dir>` / `--deny-read <file>` express a **deny-list**. Landlock is
an **allow-list** LSM: a ruleset *handles* a set of access rights, then grants
them per-path via `PathBeneath` rules. Handled-but-ungranted rights are denied;
unhandled rights pass through; and there is no "deny this one subtree" or
"except" rule. So "allow everything except the config dir" is inexpressible as a
Landlock ruleset.

## Correct posture: a whitelist

Handle the access rights the sandbox cares about and allow them only beneath the
paths the agent may legitimately touch:

- **Read broadly** — system roots (`/usr`, `/etc`, `/bin`, `/opt`), the
  workspace, `node_modules`, and the agent's *own* config dirs
  (`~/.claude`, `~/.config/opencode`, `~/.config/pi`).
- **Write narrowly** — the workspace, the brain vault, `/tmp`, and the agent's
  own config dirs.

The app config dir is excluded from both, which transitively makes the store
and `egress-auth.json` unreadable/unwritable without needing a per-file rule.

## Change set

1. Thread the allow-set through the spawn spec: `GappedSpawnSpec`
   (`egress/linux.rs`) → `build_self_unshare_argv` → new repeatable
   `--allow-read` / `--allow-write` flags → `ShimArgs`.
2. Add the `landlock` crate as a `[target.'cfg(target_os = "linux")'.dependencies]`.
3. Implement `apply_landlock(allow_read: &[PathBuf], allow_write: &[PathBuf])`:
   - `Ruleset::default().handle_access(AccessFs::from_all(abi))` with best-effort
     ABI detection (fall back to a lower ABI on older kernels).
   - `PathBeneath` allow rules for each allowed root.
   - `set_no_new_privs(true)` + `restrict_self()`.
   - On `ENOSYS`/unsupported kernel, log a NOTE and continue egress-only
     (fail-open on *file* confinement — the netns egress kill stays the
     load-bearing control, matching today's honest behavior).
4. Call it in `run()` **after** `self_unshare()` and **before** spawning the
   child (Landlock restrictions are inherited by descendants).
5. Keep `--deny-write`/`--deny-read` for wire compatibility but reinterpret them:
   the config dir/auth file become the implicit *excluded* roots of the
   allow-set, so the existing argv builder keeps emitting them unchanged.

## Testing

Extend the `#[ignore]`d `linux_sandbox_integration_tests.rs` curl-matrix (now
run on ubuntu-22.04/24.04 + fedora) with three assertions:

- the agent **cannot read** `egress-auth.json` (rung 2);
- the agent **cannot write** a file under the app config dir;
- the agent **can still write** under the workspace and `/tmp` (no
  over-restriction that would break agent CLIs).

## Kernel/ABI notes

- ABI v1 (Linux 5.13): file access rights. v2 (5.19): network. v4 (6.7): ioctl +
  truncate. Use the highest supported ABI and degrade.
- Landlock is inert until `landlock_restrict_self` is called, is unprivileged,
  and stacks cleanly with the existing network namespace.
