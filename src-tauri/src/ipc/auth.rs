//! Airgap-passphrase auth commands (status/login/Touch ID). Ports
//! `src/main/authlock.js`'s `auth:status`/`auth:login`/`auth:touchid`
//! handler bodies from `src/main/index.js` (~849-876) byte-for-byte in
//! return shape. `authlock::AuthLock` (this phase's sibling slice) does the
//! actual scrypt/TOTP verification; this file is purely the IPC-shaped glue
//! — throttle check, verify, record success/failure, flip session state —
//! plus the two `AppState` fields (`locked`/`auth_unlocked`) that make
//! login state visible to `lock_gate::guard` and to this file's own
//! `auth_status`. See `state.rs`'s doc comments on both fields for exactly
//! how they relate and why they are two fields, not one.
//!
//! Touch ID (`auth_touchid`) is wired through `crate::touchid`
//! (`objc2-local-authentication`'s `LAContext`, the same framework
//! Electron's `systemPreferences.promptTouchID` wraps). On macOS with
//! Touch ID hardware it puts up the real system prompt; everywhere else
//! (and on Macs without enrolled biometrics) `auth_status`'s `touchId`
//! bit is `false`, the lock screen never offers the button, and
//! `auth_touchid` itself returns the same honest refusal shape the
//! pre-Touch-ID build returned on every platform.

use serde_json::{json, Value};
use tauri::State;

use crate::{lock_gate, state::AppState};

/// How many whole seconds a caller should wait before retrying, rounded up
/// — mirrors `Math.ceil(waitMs / 1000)`. `pub(crate)`: `ipc::airgap`'s
/// `airgap_unlock` formats the identical "Too many attempts" message off
/// the same `AuthLock::throttle_retry_in` shape.
pub(crate) fn ceil_seconds(wait_ms: u64) -> u64 {
    wait_ms.div_ceil(1000)
}

/// Mirrors `authlock.authStatus()` merged with the `auth:status` handler's
/// own additions (`{ ...authlock.authStatus(), unlocked: authlock.isUnlocked(),
/// touchId: process.platform === 'darwin' && systemPreferences.canPromptTouchID() }`).
/// `touchId` is `crate::touchid::can_prompt()` — the direct port of
/// `canPromptTouchID()` (both wrap `LAContext canEvaluatePolicy:`): `true`
/// only on macOS with usable biometrics, so the lock screen's Touch ID
/// button appears exactly when the prompt behind it can actually work.
#[tauri::command]
pub async fn auth_status(state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "auth:status")?;
    let status = {
        let guard = state.auth.lock().expect("AppState.auth lock poisoned");
        guard.as_ref().map(|a| a.status())
    };
    let (configured, totp) = status
        .map(|s| (s.configured, s.totp))
        .unwrap_or((false, false));
    let unlocked = *state
        .auth_unlocked
        .read()
        .expect("AppState.auth_unlocked lock poisoned");
    Ok(json!({
        "configured": configured,
        "totp": totp,
        "unlocked": unlocked,
        "touchId": crate::touchid::can_prompt(),
    }))
}

/// Mirrors `ipcMain.handle('auth:login', (e, { passphrase, code }) => {...})`
/// exactly: throttle first (an already-backed-off caller never even reaches
/// a real verify attempt, so it can't extend its own backoff by retrying
/// during it), then passphrase AND (if TOTP is enrolled) code, recording
/// failure/success on the SAME `'auth:login'` throttle purpose either way.
/// `code` absent when TOTP isn't active behaves exactly like the JS
/// original's `authlock.verifyTotp(undefined)` would — irrelevant, since
/// `!totpActive() || ...` short-circuits before ever consulting it.
#[tauri::command]
pub async fn auth_login(
    state: State<'_, AppState>,
    passphrase: String,
    code: Option<String>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "auth:login")?;
    let mut guard = state.auth.lock().expect("AppState.auth lock poisoned");
    let auth = guard
        .as_mut()
        .ok_or_else(|| "auth: not initialized".to_string())?;

    let wait = auth.throttle_retry_in("auth:login");
    if wait > 0 {
        return Ok(json!({
            "ok": false,
            "error": format!("Too many attempts — try again in {}s.", ceil_seconds(wait)),
        }));
    }

    let pass_ok = auth.verify_passphrase(&passphrase);
    let totp_ok = !auth.totp_active() || code.as_deref().is_some_and(|c| auth.verify_totp(c));
    if !pass_ok || !totp_ok {
        auth.record_failure("auth:login");
        let error = if pass_ok {
            "Wrong 2FA code."
        } else {
            "Wrong passphrase."
        };
        return Ok(json!({"ok": false, "error": error}));
    }
    auth.record_success("auth:login");
    drop(guard);

    mark_unlocked(&state);
    Ok(json!({"ok": true}))
}

/// Direct port of the JS `auth:touchid` handler (`index.js` ~855):
/// `try { await systemPreferences.promptTouchID('unlock the Tome
/// workspace'); authlock.markUnlocked(); return { ok: true } } catch (err)
/// { return { ok: false, error: err.message || 'Touch ID failed.' } }` —
/// `crate::touchid::prompt()` is the `promptTouchID` call, `mark_unlocked`
/// is the same one-way flip `auth_login` uses, and the `catch` branch's
/// shape is preserved on every failure (user cancel, lockout, no hardware,
/// non-macOS stub). Like the original, a successful prompt unlocks the
/// session WITHOUT touching the passphrase/TOTP throttle counters — the
/// biometric prompt is its own rate-limiter (macOS locks biometry out
/// after repeated failures), and the JS original recorded no
/// `'auth:login'` failure for a cancelled Touch ID prompt either.
#[tauri::command]
pub async fn auth_touchid(state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "auth:touchid")?;
    match crate::touchid::prompt().await {
        Ok(()) => {
            mark_unlocked(&state);
            Ok(json!({"ok": true}))
        }
        Err(error) => Ok(json!({"ok": false, "error": error})),
    }
}

/// Flips both one-way session-unlock fields — the single place every login
/// success path (`auth_login` here; `airgap_setup`/a future real
/// `auth_touchid` in `ipc::airgap`/this file) calls, so the two fields
/// (see their own doc comments on `state.rs`) can never drift out of sync
/// with each other. `pub(crate)` so `ipc::airgap::airgap_setup` — a
/// sibling command with the identical `authlock.markUnlocked()` call in
/// its JS original — reuses this instead of re-deriving the two writes.
pub(crate) fn mark_unlocked(state: &AppState) {
    *state
        .auth_unlocked
        .write()
        .expect("AppState.auth_unlocked lock poisoned") = true;
    *state.locked.write().expect("AppState.locked lock poisoned") = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authlock::AuthLock;

    #[test]
    fn ceil_seconds_rounds_up_and_handles_the_exact_multiple() {
        assert_eq!(ceil_seconds(0), 0);
        assert_eq!(ceil_seconds(1), 1);
        assert_eq!(ceil_seconds(999), 1);
        assert_eq!(ceil_seconds(1000), 1);
        assert_eq!(ceil_seconds(1001), 2);
        assert_eq!(ceil_seconds(30_000), 30);
    }

    // ---- mark_unlocked / AppState field wiring (no live AppHandle needed —
    // AppState is constructible directly; only tauri::State<'_, AppState>
    // needs a running app, and these two fields don't need it) ----

    #[test]
    fn fresh_app_state_boots_with_locked_false_and_unlocked_false() {
        // Matches lib.rs's own boot invariant: an unconfigured install
        // (is_locked(false, false, shot_mode) == false) boots with the gate
        // open, but auth_status's raw `unlocked` bit must still read false
        // (a first-run install has not "logged in", it has nothing to log
        // into) — see state.rs's doc comment on why these are two fields.
        let state = AppState::new();
        assert!(!*state.locked.read().unwrap());
        assert!(!*state.auth_unlocked.read().unwrap());
    }

    // ---- AuthLock wiring sanity (real AuthLock::load — see this test's
    // note on why touching the real keychain is avoided) ----

    /// `AuthLock::load` (the only public constructor outside `authlock.rs`
    /// itself) uses the real OS-keychain-backed `KeyringProtector` — but
    /// `set_passphrase` only ever touches it via `migrate_totp_secret`,
    /// which is a no-op whenever no TOTP secret has been enrolled yet (see
    /// `authlock.rs`'s own `migrate_totp_secret` doc comment). So a
    /// passphrase-only round trip through the public API, as below, never
    /// touches the real keychain — safe to run in `cargo test`.
    #[test]
    fn verify_passphrase_roundtrip_through_the_public_authlock_api() {
        let dir = tempfile::tempdir().unwrap();
        let mut auth = AuthLock::load(dir.path());
        assert!(!auth.status().configured);
        auth.set_passphrase("hunter2-fake").unwrap();
        assert!(auth.status().configured);
        assert!(auth.verify_passphrase("hunter2-fake"));
        assert!(!auth.verify_passphrase("wrong"));
    }
}
