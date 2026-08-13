//! Airgap-passphrase auth commands (status/login/Touch ID). Ports
//! `src/main/authlock.js` and, for `auth_touchid`, `objc2-local-authentication`
//! behind the same command name. Phase 3 (airgap+auth parity) work — scrypt
//! compatibility must be fixture-proven against Node's output before this
//! fills in (see the plan's "scrypt/safeStorage migration" risk).
//!
//! `auth_status` is the one exception, filled in now rather than left a
//! stub: `src/renderer/lock.js`'s `bootAuth()` calls it unconditionally on
//! every boot, before it knows whether a passphrase was ever set, so a
//! stubbed `Err("unimplemented")` here would fail the app's very first
//! screen instead of just one feature. See its own doc comment for the
//! exact shape and why `auth_login`/`auth_touchid` are safe to leave as
//! stubs this phase.

use tauri::State;

use crate::ipc::stub_command;
use crate::{lock_gate, state::AppState};

stub_command!(auth_login, "auth:login");
stub_command!(auth_touchid, "auth:touchid");

/// Mirrors `authlock.authStatus()` merged with the `auth:status` handler's
/// own additions (`src/main/index.js`: `{ ...authlock.authStatus(),
/// unlocked: authlock.isUnlocked(), touchId: ... }`) for the only state
/// this phase can honestly report: no `airgap-auth.json` has ever been
/// written (`authStatus()`'s `configured: !!auth?.hash` with `auth` still
/// `null`), so nothing is configured, nothing is unlocked, and there is no
/// second factor.
///
/// `src/renderer/lock.js`'s `bootAuth()` reads `configured` first and, when
/// false, shows the skippable first-run setup screen rather than the login
/// screen — `unlocked`/`touchId` are never actually read by the renderer in
/// this state, but both are still filled in honestly rather than left to
/// whatever a stub would produce: `unlocked: false` matches
/// `authlock.isUnlocked()`'s real starting value (nobody has called
/// `markUnlocked()` — `airgap_setup`, the only path that would, is still
/// `Err("unimplemented")` this phase), and `touchId: false` doesn't
/// advertise a capability `auth_touchid` can't back yet (Touch ID wiring is
/// Phase 3, via `objc2-local-authentication`).
///
/// Provably safe to leave `auth_login`/`auth_touchid` as stubs alongside
/// this: `lock.js`'s `lockScreen()` (the only caller of `tome.auth.login`/
/// `tome.auth.touchid`) only renders when `st.configured === true`, and
/// `configured` can never become `true` in this phase — this command always
/// reports it `false`, and the one path that would flip it for real
/// (`airgap_setup`) is untouched by this slice.
#[tauri::command]
pub async fn auth_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "auth:status")?;
    Ok(serde_json::json!({
        "configured": false,
        "totp": false,
        "unlocked": false,
        "touchId": false,
    }))
}
