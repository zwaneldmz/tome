//! Speech-to-text commands. Thin wrappers over `crate::stt` (that module
//! name is distinct from this file's own path, `crate::ipc::stt` — the
//! pure/testable logic lives in the former, this file owns only the glue
//! the pure module can't provide) — see that module's doc comment for the
//! Electron source these port (`src/main/lib/stt.js`) and the pure/async
//! split.
//!
//! This file resolves `AppHandle` paths (`app_data_dir` for the model,
//! `temp_dir` for the scratch WAV), the `TOME_WHISPER_BIN` env override,
//! and the `voice-warmup`/`stt-engine` store-key gates, then hands plain
//! values down to `crate::stt` — porting `src/main/index.js`'s `stt:*`
//! handlers (~lines 1211-1264) verbatim in shape: `stt:transcribe` and
//! `stt:warmup` never reject (failures come back as `{ error }`/
//! `{ skipped: true }` values, matching the originals' try/catch-shaped
//! bodies — the lock-gate check is the one `Err` either can still
//! produce, same as every other gated command), `stt:status` never spawns
//! a process, and `stt:engine` is the Task 1 engine-resolver surface that
//! reports the resolved Apple/whisper engine from the `stt-engine`
//! preference.

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

use crate::{lock_gate, state::AppState, store, stt};

/// `TOME_WHISPER_BIN`, treated as unset when absent *or* empty — mirrors
/// JS's `if (env.TOME_WHISPER_BIN)`, which treats `""` as falsy the same
/// as `undefined`. `crate::stt::whisper_bin` takes the already-resolved
/// value (not a raw env lookup) so it stays a pure, directly testable
/// function.
fn whisper_bin_override() -> Option<String> {
    std::env::var("TOME_WHISPER_BIN")
        .ok()
        .filter(|v| !v.is_empty())
}

/// JS falsy check narrowed to what `store::get(dir, "voice-warmup", _)`
/// can actually return: `null` (never set) or a stored `false` both gate
/// the warmup off, matching `if (!(await readStore('voice-warmup')))`.
/// `pty.rs`'s `egress-default` read ports the opposite-polarity `!==
/// false` idiom ("anything but literal false wins") for its own,
/// differently-defaulted JS call site — this key defaults off, that one
/// defaults on, so each gets its own small helper rather than sharing one.
fn warmup_enabled(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

/// Reads the `stt-engine` preference from the stored value, defaulting to
/// `"auto"` when the value is null, empty, or not a string — mirroring the
/// renderer's own `(await store.get('stt-engine')) || 'auto'` fallback, so
/// a never-set or cleared key resolves to the same auto behavior on both
/// sides.
fn engine_preference(v: &Value) -> String {
    match v.as_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "auto".to_string(),
    }
}

/// `stt:transcribe`. One finished WAV buffer in, `{ text }` or
/// `{ error }` out — main picks the binary, model path, and temp
/// filename itself; failures come back as values, not throws ("install
/// whisper" is advice for the user, not an exception for the console).
#[tauri::command]
pub async fn stt_transcribe(
    app: AppHandle,
    state: State<'_, AppState>,
    wav: Vec<u8>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "stt:transcribe")?;

    // ~10 minutes of 16 kHz mono int16; anything bigger is not push-to-talk.
    if wav.is_empty() || wav.len() > 20_000_000 {
        return Ok(json!({ "error": "stt: bad audio payload" }));
    }

    let bin = stt::whisper_bin(whisper_bin_override().as_deref());
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let model = stt::model_path(&dir);
    if let Some(why) = stt::stt_unavailable(bin.as_deref(), &model) {
        return Ok(json!({ "error": why }));
    }
    let temp_dir = app.path().temp_dir().map_err(|e| e.to_string())?;
    // stt_unavailable(bin.as_deref(), _) just returned None above, which
    // only happens when `bin` matched its `Some(non-empty)` arm.
    let bin = bin.unwrap_or_default();

    let result = stt::transcribe(stt::TranscribeRequest {
        wav: &wav,
        bin: &bin,
        model: &model,
        temp_dir: &temp_dir,
        timeout: stt::DEFAULT_TIMEOUT,
    })
    .await;

    Ok(match result {
        Ok(text) => json!({ "text": text }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({ "error": stt::NO_BIN }),
        Err(e) => json!({ "error": format!("stt: {e}") }),
    })
}

/// `stt:warmup`. Runs `whisper-cli` once over 0.1s of generated silence so
/// the first real dictation skips the model load, gated on the
/// `voice-warmup` store key (default off — see [`warmup_enabled`]) so a
/// renderer alone can't make the app spawn whisper on launch. Every
/// failure — key unset, binary/model missing, path resolution failure,
/// spawn error, timeout — collapses to `{ skipped: true }`, matching the
/// Electron original's blanket `catch { return { skipped: true } }`; only
/// a real, complete run reports `{ warmed: true }`.
#[tauri::command]
pub async fn stt_warmup(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "stt:warmup")?;

    let Ok(dir) = app.path().app_data_dir() else {
        return Ok(json!({ "skipped": true }));
    };
    let locked = *state.locked.read().unwrap();
    let dir_for_store = dir.clone();
    let enabled =
        tokio::task::spawn_blocking(move || store::get(&dir_for_store, "voice-warmup", locked))
            .await
            .unwrap_or(Value::Null);
    if !warmup_enabled(&enabled) {
        return Ok(json!({ "skipped": true }));
    }

    let bin = stt::whisper_bin(whisper_bin_override().as_deref());
    let model = stt::model_path(&dir);
    if stt::stt_unavailable(bin.as_deref(), &model).is_some() {
        return Ok(json!({ "skipped": true }));
    }
    let Ok(temp_dir) = app.path().temp_dir() else {
        return Ok(json!({ "skipped": true }));
    };
    let bin = bin.unwrap_or_default();

    let result = stt::transcribe(stt::TranscribeRequest {
        wav: &stt::warmup_silence(),
        bin: &bin,
        model: &model,
        temp_dir: &temp_dir,
        timeout: stt::WARMUP_TIMEOUT,
    })
    .await;

    Ok(if result.is_ok() {
        json!({ "warmed": true })
    } else {
        json!({ "skipped": true })
    })
}

/// `stt:status`. No spawn — the onboarding Voice step's status row reads
/// this to show whether speech is ready before the user presses Test, and
/// which of the two installs (binary, model) is missing for whisper. The
/// top-level `ready`/`bin`/`model` keys are the original whisper-shaped
/// result, kept verbatim so existing readers (onboarding) don't regress;
/// `engine`/`preference`/`apple`/`whisper`/`why` are the Task 1 engine
/// additions the Settings surface and future Apple backend consume.
#[tauri::command]
pub async fn stt_status(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "stt:status")?;

    let bin = stt::whisper_bin(whisper_bin_override().as_deref());
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let model = stt::model_path(&dir);

    // Same store read shape as `stt_warmup`: the `stt-engine` key gate goes
    // through `store::get` inside `spawn_blocking` (it is a file read, so it
    // must not block the async runtime).
    let locked = *state.locked.read().unwrap();
    let dir_for_store = dir.clone();
    let pref = tokio::task::spawn_blocking(move || store::get(&dir_for_store, "stt-engine", locked))
        .await
        .unwrap_or(Value::Null);
    let preference = engine_preference(&pref);

    let whisper_why = stt::stt_unavailable(bin.as_deref(), &model);
    let whisper_ready = whisper_why.is_none();
    let bin_ok = bin.as_deref().map(stt::bin_exists).unwrap_or(false);
    let model_ok = stt::model_exists(&model);

    let apple_available = stt::apple_available();
    let engine = stt::engine_kind(&preference, apple_available, whisper_ready);

    let ready = match engine {
        stt::Engine::Apple => apple_available,
        stt::Engine::Whisper => whisper_ready,
    };
    let why = match engine {
        stt::Engine::Apple if !apple_available => {
            Some("On-device speech recognition is unavailable on this Mac.".to_string())
        }
        stt::Engine::Whisper => whisper_why,
        stt::Engine::Apple => None,
    };

    Ok(json!({
        "ready": ready,
        "bin": bin_ok,
        "model": model_ok,
        "engine": engine.as_str(),
        "preference": preference,
        "apple": { "available": apple_available },
        "whisper": { "ready": whisper_ready, "bin": bin_ok, "model": model_ok },
        "why": why,
    }))
}

/// `stt:engine`. Reports the stored preference and the resolved engine (plus
/// whether it is available) without spawning anything — a synchronous,
/// read-only companion to `stt:status` for the Settings surface's
/// engine-select row, which needs the resolution but not the full
/// availability shape. Gated like every other command.
#[tauri::command]
pub fn stt_engine(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "stt:engine")?;

    let bin = stt::whisper_bin(whisper_bin_override().as_deref());
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let model = stt::model_path(&dir);
    let locked = *state.locked.read().unwrap();
    let preference = engine_preference(&store::get(&dir, "stt-engine", locked));

    let whisper_ready = stt::stt_unavailable(bin.as_deref(), &model).is_none();
    let apple_available = stt::apple_available();
    let engine = stt::engine_kind(&preference, apple_available, whisper_ready);
    let available = match engine {
        stt::Engine::Apple => apple_available,
        stt::Engine::Whisper => whisper_ready,
    };

    Ok(json!({
        "preference": preference,
        "engine": engine.as_str(),
        "available": available,
    }))
}
