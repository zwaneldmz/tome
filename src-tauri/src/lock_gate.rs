use tauri::State;

use crate::state::AppState;

/// True while the app currently requires login before a gated command may
/// run. Direct port of `src/main/lib/ipc-lock-gate.js`'s `isLocked({
/// configured, unlocked, shotMode })` — kept as a pure function of three
/// independent booleans, not read off `AppState`/the environment directly,
/// for the same reason the JS original is: `shotMode` must never be
/// resolved from `TOME_SHOT`/`app.isPackaged` inside this function itself,
/// only threaded in by a caller that already confirmed the build is
/// unpackaged (see the JS doc comment this ports — a regression that reads
/// the env var in here would risk it leaking into a packaged build).
///
/// Nothing calls this yet: `AppState` has only a plain `locked: bool` flag
/// today (see `guard` below), not separate `configured`/`unlocked`/
/// `shotMode` concepts — those need the Phase 3 airgap+auth slice's real
/// port of `authlock.js`'s `initAuth`/`authStatus`/`isUnlocked`. Ported now,
/// with its exact JS test fixtures, so that slice only has to wire three
/// booleans through to an already-correct, already-tested predicate rather
/// than design one.
#[allow(dead_code)] // spec-ported for Phase 3; see doc comment above
fn is_locked(configured: bool, unlocked: bool, shot_mode: bool) -> bool {
    configured && !unlocked && !shot_mode
}

/// Pre-auth allowlist for Electron's fire-and-forget/sendSync `ipcMain.on`
/// channels — `OPEN_ON` in `src/main/lib/ipc-lock-gate.js`, wired into
/// index.js's `ipcMain.on` wrapper around line 484. `app:home` has no Tauri
/// command at all (boot data arrives via `window.__TOME_BOOT__`, injected
/// by `lib.rs`'s `boot_plugin` before any page script runs — see that
/// function's doc comment), so only `theme:set` survives the port as an
/// actual wire channel. Kept as its own set — rather than folded straight
/// into `OPEN_CHANNELS` below — so the ported vitest suite's exact-set
/// assertion on `OPEN_ON` stays meaningful on its own terms.
pub const OPEN_ON: &[&str] = &["app:home", "theme:set"];

/// True when `channel` must be refused because the app is locked and the
/// channel is not on the `OPEN_ON` allowlist. Direct port of
/// `shouldBlockIpcOn`. Not called by `guard` below — Tauri has one IPC
/// style (`invoke`, always promise-returning), not Electron's on/handle
/// split, so `guard` checks a single combined open-set (`OPEN_CHANNELS`)
/// rather than branching on which Electron style a channel used to be. Kept
/// anyway so the ported vitest behavior for THIS predicate — as opposed to
/// the port's own combined gate — stays independently checkable.
#[allow(dead_code)] // spec-ported; guard() below uses `blocked`, not this — see its doc comment
fn should_block_ipc_on(channel: &str, locked: bool) -> bool {
    locked && !OPEN_ON.contains(&channel)
}

/// Pre-auth allowlist for Electron's `ipcMain.handle` channels —
/// `OPEN_CHANNELS` in `src/main/index.js`, built inline in the
/// `whenReady` handler around line 502 (unlike `OPEN_ON`, never a named
/// export there).
///
/// Tauri collapses Electron's two wire styles (fire-and-forget `.on` vs.
/// promise-returning `.handle`) into one (`invoke`, always
/// promise-returning), so `guard` below checks a single combined set
/// rather than re-deriving which Electron style a channel used to be. This
/// is that combined set: every Electron `OPEN_CHANNELS` entry, plus
/// `OPEN_ON`'s one entry that still maps to a real Tauri command
/// (`theme:set` — `app:home` does not, see `OPEN_ON`'s doc comment).
///
/// `airgap:setup`/`store:get`/`store:set` are listed even though this
/// slice does not implement their real bodies (`airgap_setup` is still a
/// stub; `store_get`/`store_set` are a different slice's file) — the open
/// set is a property of the CHANNEL, independent of whether a body has
/// landed yet, and a stub command still calls `guard` first (see
/// `ipc::stub_command!`), so getting this set right now matters even for
/// not-yet-real commands.
pub const OPEN_CHANNELS: &[&str] = &[
    "auth:status",
    "auth:login",
    "auth:touchid",
    "airgap:setup",
    "airgap:state",
    "store:get",
    "store:set",
    "popout:close",
    "theme:set",
];

/// The decision `guard` makes once it has resolved `locked` from
/// `AppState` — split out as its own pure function so it is unit-testable
/// without a live `State<AppState>` (constructing one outside a running
/// Tauri app needs the `tauri` crate's `test` feature, which is not among
/// this crate's declared dependency features — see `Cargo.toml`, not this
/// slice's file to add to). Same shape as `should_block_ipc_on` above,
/// checked against `OPEN_CHANNELS` instead of `OPEN_ON`.
fn blocked(channel: &str, locked: bool) -> bool {
    locked && !OPEN_CHANNELS.contains(&channel)
}

/// Pre-command authorization gate. Every `#[tauri::command]` in `ipc/*`
/// calls this first, passing the exact Electron wire-channel string it
/// replaces (see `CHANNEL_OF_COMMAND` below).
///
/// Port of the `ipcMain.handle` wrapper `src/main/index.js` installs around
/// line 521-526: `if (!OPEN_CHANNELS.has(channel) && isLockedNow()) throw
/// new Error('Tome is locked.')`. That thrown `Error` is exactly what an
/// Electron `invoke()` caller sees as a rejected promise, which is exactly
/// what returning `Err` here does for a Tauri `invoke()` caller (see
/// `tome-ipc.js`'s `call()` — it turns a command `Err(String)` into a real
/// `Error`), so the mapping from JS throw to Rust `Err` is direct and the
/// message is carried over verbatim.
///
/// `state.locked` reads `false` until the Phase 3 airgap+auth slice ports
/// `authlock.js`'s real `initAuth`/login flow and starts flipping it (via
/// `is_locked` above, once `AppState` grows the `configured`/`shotMode`
/// inputs that predicate needs) — every command is reachable in the
/// meantime, which matches this same phase's `auth_status` body always
/// reporting `configured: false` (see `ipc::auth::auth_status`'s doc
/// comment): a workspace with no passphrase ever configured has nothing
/// for this gate to lock.
pub fn guard(state: &State<'_, AppState>, channel: &str) -> Result<(), String> {
    debug_assert!(
        CHANNEL_OF_COMMAND.iter().any(|(_, ch)| *ch == channel),
        "guard() called with a channel missing from CHANNEL_OF_COMMAND: {channel}"
    );
    let locked = *state
        .locked
        .read()
        .expect("lock_gate::guard: AppState.locked lock poisoned");
    if blocked(channel, locked) {
        return Err("Tome is locked.".to_string());
    }
    Ok(())
}

/// Every `#[tauri::command]` name registered in `lib.rs`'s
/// `generate_handler!`, mapped to the exact Electron IPC channel string it
/// replaces. Colons and camelCase are preserved on the wire side (see
/// `src/preload/index.js`) even though the Rust command name is snake_case
/// — the mapping is mechanical: `fs_read_dir` <-> `"fs:readDir"`,
/// `app_quit_ready` <-> `"app:quit-ready"`.
///
/// This table does not include push-only main->renderer events (`pty:data`,
/// `chat:delta`, `events:appended`, ...) — those aren't commands and never
/// go through `guard`.
pub const CHANNEL_OF_COMMAND: &[(&str, &str)] = &[
    ("pty_create", "pty:create"),
    ("pty_write", "pty:write"),
    ("pty_resize", "pty:resize"),
    ("pty_kill", "pty:kill"),
    ("fs_read_dir", "fs:readDir"),
    ("fs_read_file", "fs:readFile"),
    ("fs_write_file", "fs:writeFile"),
    ("fs_mkdir", "fs:mkdir"),
    ("fs_create_file", "fs:createFile"),
    ("fs_watch", "fs:watch"),
    ("fs_unwatch", "fs:unwatch"),
    ("fmt_format", "fmt:format"),
    ("store_get", "store:get"),
    ("store_set", "store:set"),
    ("git_info", "git:info"),
    ("git_branches", "git:branches"),
    ("git_checkout", "git:checkout"),
    ("git_log", "git:log"),
    ("git_commit", "git:commit"),
    ("git_diff", "git:diff"),
    ("auth_status", "auth:status"),
    ("auth_login", "auth:login"),
    ("auth_touchid", "auth:touchid"),
    ("panes_sync", "panes:sync"),
    ("ws_sync", "ws:sync"),
    ("conductor_allow_run", "conductor:allowRun"),
    ("conductor_allow_read", "conductor:allowRead"),
    ("doc_read", "doc:read"),
    ("theme_set", "theme:set"),
    ("shell_open_path", "shell:openPath"),
    ("airgap_state", "airgap:state"),
    ("airgap_unlock", "airgap:unlock"),
    ("airgap_relock", "airgap:relock"),
    ("airgap_setup", "airgap:setup"),
    ("airgap_enroll_totp", "airgap:enrollTotp"),
    ("airgap_confirm_totp", "airgap:confirmTotp"),
    ("airgap_read_repo_allowlist", "airgap:readRepoAllowlist"),
    ("airgap_consent_repo_allowlist", "airgap:consentRepoAllowlist"),
    ("airgap_revoke_repo_allowlist", "airgap:revokeRepoAllowlist"),
    ("agents_list", "agents:list"),
    ("agents_customs", "agents:customs"),
    ("agents_changed", "agents:changed"),
    ("events_list", "events:list"),
    ("runs_start", "runs:start"),
    ("runs_cancel", "runs:cancel"),
    ("runs_list", "runs:list"),
    ("stt_transcribe", "stt:transcribe"),
    ("stt_warmup", "stt:warmup"),
    ("stt_status", "stt:status"),
    ("chat_send", "chat:send"),
    ("chat_abort", "chat:abort"),
    ("chat_providers", "chat:providers"),
    ("brain_open", "brain:open"),
    ("brain_close", "brain:close"),
    ("brain_index", "brain:index"),
    ("brain_read", "brain:read"),
    ("brain_write", "brain:write"),
    ("brain_delete", "brain:delete"),
    ("brain_core_info", "brain:coreInfo"),
    ("brain_promote", "brain:promote"),
    ("lsp_did_open", "lsp:didOpen"),
    ("lsp_did_change", "lsp:didChange"),
    ("lsp_did_close", "lsp:didClose"),
    ("lsp_hover", "lsp:hover"),
    ("lsp_definition", "lsp:definition"),
    ("dialog_pick_folder", "dialog:pickFolder"),
    ("dialog_pick_file", "dialog:pickFile"),
    ("app_quit_ready", "app:quit-ready"),
    ("popout_close", "popout:close"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ---- coverage: CHANNEL_OF_COMMAND vs. lib.rs's REAL registration ----
    //
    // The scaffold's original version of this test only checked that
    // CHANNEL_OF_COMMAND and a second hand-maintained list (`ALL_COMMANDS`,
    // removed here) agreed with each other — both lived in this file, so
    // the same mistake copy-pasted into both would still pass. This scrapes
    // `lib.rs`'s actual `generate_handler!` invocation as source text
    // instead: the one place command registration cannot drift from itself.

    fn commands_registered_in_lib_rs() -> HashSet<&'static str> {
        let lib_rs = include_str!("lib.rs");
        let marker = "generate_handler![";
        let start = lib_rs
            .find(marker)
            .unwrap_or_else(|| panic!("lib.rs: `{marker}` not found — did the macro get renamed?"))
            + marker.len();
        let rest = &lib_rs[start..];
        let end = rest
            .find("])")
            .expect("lib.rs: generate_handler! block's closing `])` not found");
        rest[..end]
            .lines()
            .filter_map(|raw| {
                // Strip a trailing `// comment` (none exist on command lines
                // today, but this keeps the scrape honest if one is added)
                // and any trailing comma, then take the segment after the
                // last `::` — "ipc::pty::pty_create," -> "pty_create". A
                // comment-only or blank line strips down to "" and is
                // dropped.
                let code = raw.split("//").next().unwrap_or("").trim();
                let code = code.trim_end_matches(',').trim();
                (!code.is_empty()).then(|| code.rsplit("::").next().unwrap())
            })
            .collect()
    }

    #[test]
    fn channel_table_matches_lib_rs_registration() {
        let registered = commands_registered_in_lib_rs();
        // Sanity floor on the scrape itself: if `generate_handler!` were
        // ever reformatted onto one line (or the marker text changed
        // subtly enough to still be found but parse wrong), this catches
        // "found something, but not what we think" rather than silently
        // passing on an empty or near-empty set.
        assert!(
            registered.len() > 60,
            "only parsed {} command(s) out of lib.rs's generate_handler! — the scrape above likely broke",
            registered.len()
        );

        let table: HashSet<&str> = CHANNEL_OF_COMMAND.iter().map(|(cmd, _)| *cmd).collect();
        assert_eq!(
            table.len(),
            CHANNEL_OF_COMMAND.len(),
            "CHANNEL_OF_COMMAND has a duplicate command name"
        );

        let missing_from_table: Vec<_> = registered.difference(&table).collect();
        assert!(
            missing_from_table.is_empty(),
            "lib.rs registers commands missing from CHANNEL_OF_COMMAND: {missing_from_table:?}"
        );

        let extra_in_table: Vec<_> = table.difference(&registered).collect();
        assert!(
            extra_in_table.is_empty(),
            "CHANNEL_OF_COMMAND has entries lib.rs does not register: {extra_in_table:?}"
        );
    }

    #[test]
    fn no_duplicate_wire_channels() {
        let channels: HashSet<&str> = CHANNEL_OF_COMMAND.iter().map(|(_, ch)| *ch).collect();
        assert_eq!(
            channels.len(),
            CHANNEL_OF_COMMAND.len(),
            "CHANNEL_OF_COMMAND maps two different commands to the same wire channel"
        );
    }

    // ---- src/main/lib/ipc-lock-gate.js port: OPEN_ON ----

    #[test]
    fn open_on_is_exactly_the_lock_screens_own_needs() {
        // A bypass here is a widen, so pin the exact set rather than just
        // membership: any addition (or removal) must be a deliberate edit.
        let expected: HashSet<&str> = ["app:home", "theme:set"].into_iter().collect();
        let actual: HashSet<&str> = OPEN_ON.iter().copied().collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn open_on_excludes_channels_that_must_never_run_pre_auth() {
        // ws:sync resets the fs-confinement roots; lsp:didOpen spawns a
        // language server — both were the original TOME-003 bypass and
        // must stay gated.
        for channel in ["ws:sync", "lsp:didOpen", "panes:sync", "conductor:allowRun"] {
            assert!(!OPEN_ON.contains(&channel), "{channel} must not be in OPEN_ON");
        }
    }

    // ---- src/main/lib/ipc-lock-gate.js port: isLocked() ----

    #[test]
    fn is_locked_true_only_when_configured_not_unlocked_not_shot_mode() {
        assert!(is_locked(true, false, false));
    }

    #[test]
    fn is_locked_false_when_never_configured() {
        // First run — no passphrase to unlock.
        assert!(!is_locked(false, false, false));
    }

    #[test]
    fn is_locked_false_once_unlocked() {
        assert!(!is_locked(true, true, false));
    }

    #[test]
    fn is_locked_false_in_shot_mode_even_if_configured_and_locked() {
        // shotMode must win regardless of the other two — index.js computes
        // it as `!!process.env.TOME_SHOT && !app.isPackaged`, so this can
        // only ever be true in an unpackaged dev build, never a shipped one.
        assert!(!is_locked(true, false, true));
    }

    #[test]
    fn is_locked_requires_every_condition_not_just_a_majority() {
        // A regression that swaps && for || would make any single true
        // input enough; check each pairwise-true / one-false combination
        // independently so that specific mistake fails here.
        assert!(!is_locked(false, true, true));
        assert!(!is_locked(true, true, true));
        assert!(!is_locked(true, false, true));
    }

    // ---- src/main/lib/ipc-lock-gate.js port: shouldBlockIpcOn() ----

    #[test]
    fn should_block_ipc_on_blocks_non_allowlisted_channel_while_locked() {
        assert!(should_block_ipc_on("ws:sync", true));
        assert!(should_block_ipc_on("lsp:didOpen", true));
    }

    #[test]
    fn should_block_ipc_on_never_blocks_an_open_on_channel() {
        assert!(!should_block_ipc_on("app:home", true));
        assert!(!should_block_ipc_on("theme:set", true));
    }

    #[test]
    fn should_block_ipc_on_blocks_nothing_once_unlocked() {
        assert!(!should_block_ipc_on("ws:sync", false));
        assert!(!should_block_ipc_on("app:home", false));
    }

    // ---- guard()'s own combined-set decision — no direct JS analog (see
    // OPEN_CHANNELS's doc comment for why Tauri needs one combined set
    // where Electron had two) ----

    #[test]
    fn blocked_passes_every_open_channel_even_while_locked() {
        for channel in OPEN_CHANNELS {
            assert!(!blocked(channel, true), "{channel} should stay open while locked");
        }
    }

    #[test]
    fn blocked_refuses_non_open_channels_only_while_locked() {
        assert!(blocked("agents:list", true));
        assert!(!blocked("agents:list", false));
    }

    #[test]
    fn open_channels_has_no_duplicates() {
        let set: HashSet<&str> = OPEN_CHANNELS.iter().copied().collect();
        assert_eq!(set.len(), OPEN_CHANNELS.len());
    }
}
