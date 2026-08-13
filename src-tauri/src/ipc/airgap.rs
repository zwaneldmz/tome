//! The air gap: proxy lifecycle, allowlist, consents, unlock/relock, repo
//! allowlist consent. Ports `src/main/airgap.js` (proxy/tunnel/consent
//! semantics — see the plan's "Critical files"). Phase 3/4 work: macOS
//! parity first (seatbelt already exists and stays), then Linux enforcement
//! via bubblewrap + `tome-shim` (the plan's "critical new build").
//!
//! `airgap_state` is filled in now rather than left a stub — see its own
//! doc comment.

use tauri::State;

use crate::ipc::stub_command;
use crate::{lock_gate, state::AppState};

stub_command!(airgap_unlock, "airgap:unlock");
stub_command!(airgap_relock, "airgap:relock");
stub_command!(airgap_setup, "airgap:setup");
stub_command!(airgap_enroll_totp, "airgap:enrollTotp");
stub_command!(airgap_confirm_totp, "airgap:confirmTotp");
stub_command!(airgap_read_repo_allowlist, "airgap:readRepoAllowlist");
stub_command!(airgap_consent_repo_allowlist, "airgap:consentRepoAllowlist");
stub_command!(airgap_revoke_repo_allowlist, "airgap:revokeRepoAllowlist");

/// Mirrors `{ ...airgap.getState(), auth: authlock.authStatus() }`
/// (`src/main/index.js`'s `airgap:state` handler) for the only state this
/// phase can honestly report: no panes, no user allowlist override, no
/// repo consents, nothing configured. `airgap.js`'s own module-level state
/// (`panes`, `appliedRepos`) starts exactly this empty at boot, and
/// `defaultMinutes` mirrors its `DEFAULT_UNLOCK_MINUTES` constant (not yet
/// user-configurable in either implementation). `auth` is bare
/// `authlock.authStatus()` — `{configured, totp}` only, NOT the fuller
/// `auth:status` shape (`ipc::auth::auth_status` additionally reports
/// `unlocked`/`touchId`, but the Electron `airgap:state` handler merges in
/// only the bare `authStatus()` call, not the full `auth:status` handler's
/// output) — so this intentionally does not just reuse
/// `ipc::auth::auth_status`'s body.
#[tauri::command]
pub async fn airgap_state(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "airgap:state")?;
    Ok(serde_json::json!({
        "panes": {},
        "defaultMinutes": 15,
        "repo": [],
        "auth": { "configured": false, "totp": false },
    }))
}
