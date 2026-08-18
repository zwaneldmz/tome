//! Touch ID (LocalAuthentication) — the real macOS wiring behind
//! `ipc::auth::auth_touchid` and `ipc::auth::auth_status`'s `touchId`
//! capability bit. Ports Electron's `systemPreferences.canPromptTouchID()`
//! / `systemPreferences.promptTouchID('unlock the Tome workspace')` pair
//! (`src/main/index.js` ~853-864) onto `objc2-local-authentication`'s
//! `LAContext`, which is what Electron's own implementation wraps on
//! macOS anyway.
//!
//! Platform shape: every symbol in this file exists on every OS so
//! `ipc::auth` needs no `cfg` of its own, but the non-macOS versions are
//! honest stubs — `can_prompt()` is `false` (so the lock screen never
//! offers the button) and `prompt()` always returns the same refusal the
//! pre-Touch-ID build returned. Only the macOS halves link
//! `objc2-local-authentication` (a `target.'cfg(target_os = "macos")'`
//! dependency in Cargo.toml), so Linux/Windows builds of this crate never
//! see the framework at all.
//!
//! Policy choice: `LAPolicy::DeviceOwnerAuthenticationWithBiometrics`
//! (biometrics only — Touch ID on every Mac that has it), NOT
//! `LAPolicyDeviceOwnerAuthentication` (biometrics-or-login-password).
//! Electron's `promptTouchID` uses the biometrics-only policy too
//! (`electron/shell/browser/api/electron_api_system_preferences_mac.mm`
//! passes `LAPolicy::DeviceOwnerAuthenticationWithBiometrics`), and the
//! passphrase fallback this app already has IS Tome's own passphrase —
//! falling back to the macOS login password inside the Touch ID prompt
//! would bypass Tome's throttle/TOTP layer entirely.
//!
//! Threading: `can_prompt` is a synchronous preflight safe to call on any
//! thread. `prompt` must not block a Tauri command's async worker on the
//! modal system prompt, so the macOS implementation hops to
//! `tokio::task::spawn_blocking` and waits on `evaluatePolicy`'s reply
//! block through a `std::sync::mpsc` rendezvous. `LAContext` is not
//! `Send`, so it is created, used, and dropped entirely inside the
//! blocking closure — nothing Objective-C crosses an await point.

/// The one string the system prompt shows: `systemPreferences.
/// promptTouchID('unlock the Tome workspace')`'s exact reason string.
#[cfg(target_os = "macos")]
const REASON: &str = "unlock the Tome workspace";

/// `systemPreferences.canPromptTouchID()`: true when this machine can
/// actually put up a biometric prompt (Touch ID hardware present, fingers
/// enrolled, not disabled by profile). Drives `auth_status`'s `touchId`
/// field — a `false` here means the lock screen never renders the
/// "Unlock with Touch ID" button at all, so a machine without the
/// hardware never offers a button that can only fail.
#[cfg(target_os = "macos")]
pub fn can_prompt() -> bool {
    use objc2_local_authentication::{LAContext, LAPolicy};

    let ctx = unsafe { LAContext::new() };
    // canEvaluatePolicy:error: is a pure preflight — it never prompts.
    // Any error (no hardware, not enrolled, biometry locked out, MDM
    // restriction) means "cannot prompt", matching Electron's
    // canPromptTouchID, which returns NO in exactly those cases.
    unsafe { ctx.canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthenticationWithBiometrics) }
        .is_ok()
}

#[cfg(not(target_os = "macos"))]
pub fn can_prompt() -> bool {
    false
}

/// `systemPreferences.promptTouchID(...)`: puts up the system biometric
/// prompt and resolves to `Ok(())` on a successful match, or `Err(<human
/// message>)` for every failure mode (user cancel, fallback button,
/// lockout, no hardware mid-call, …) — mirroring the JS original's
/// `catch (err) { return { ok: false, error: err.message || 'Touch ID
/// failed.' } }` shape, where the caller (`ipc::auth::auth_touchid`) owns
/// the `{ok, error}` wrapping.
#[cfg(target_os = "macos")]
pub async fn prompt() -> Result<(), String> {
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    tokio::task::spawn_blocking(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let ctx = unsafe { LAContext::new() };
        let reason = NSString::from_str(REASON);
        let handler =
            block2::RcBlock::new(move |success: objc2::runtime::Bool, err: *mut NSError| {
                let result = if success.as_bool() {
                    Ok(())
                } else {
                    // Mirror Electron's rejection: the NSError's
                    // localizedDescription is the message Electron surfaces
                    // ("Canceled by user.", "Biometry is locked out.", …).
                    // A nil error with success=NO is not a real combination,
                    // but don't unwrap on it — fall back to the JS original's
                    // own generic string.
                    let msg = unsafe { err.as_ref() }
                        .map(|e| e.localizedDescription().to_string())
                        .unwrap_or_else(|| "Touch ID failed.".to_string());
                    Err(msg)
                };
                // If the receiver hung up (caller dropped the future — for example
                // the app is quitting), dropping the send is the right move:
                // the prompt outcome no longer has anyone to report to.
                let _ = tx.send(result);
            });
        unsafe {
            ctx.evaluatePolicy_localizedReason_reply(
                LAPolicy::DeviceOwnerAuthenticationWithBiometrics,
                &reason,
                &handler,
            );
        }
        // Block this (blocking) thread until the reply block fires. The
        // LAContext stays alive across the wait — LocalAuthentication's
        // own docs warn that deallocating the context mid-evaluation
        // cancels the prompt, and `ctx` drops only after `rx.recv()`
        // returns here.
        rx.recv()
            .unwrap_or_else(|_| Err("Touch ID failed.".to_string()))
    })
    .await
    .map_err(|e| format!("Touch ID failed: {e}"))?
}

#[cfg(not(target_os = "macos"))]
pub async fn prompt() -> Result<(), String> {
    Err("Touch ID is not available on this platform.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the macOS dev host this is a real hardware query, so it only
    /// asserts the call doesn't crash and returns a bool — the actual
    /// value depends on the machine. Off macOS it must be `false`.
    #[test]
    fn can_prompt_returns_a_bool_without_panicking() {
        let v = can_prompt();
        #[cfg(not(target_os = "macos"))]
        assert!(!v);
        #[cfg(target_os = "macos")]
        let _ = v;
    }

    /// The non-macOS stub always refuses with the honest message — this
    /// is the whole contract `ipc::auth::auth_touchid` relies on there.
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn prompt_stub_refuses_off_macos() {
        let err = prompt().await.unwrap_err();
        assert!(err.contains("not available"));
    }
}
