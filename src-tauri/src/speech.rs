//! Apple on-device speech recognition (Speech framework) — the macOS batch
//! transcription backend behind `ipc::stt::stt_transcribe`. Mirrors the
//! platform shape of `touchid.rs`: every public symbol exists on every OS
//! so `ipc::stt` needs no `cfg` of its own, but the non-macOS halves are
//! honest stubs — `apple_available()` is `false` and `transcribe_wav()`
//! always refuses. Only the macOS halves link `objc2-speech` and
//! `objc2-avf-audio` (target-gated dependencies in Cargo.toml), so
//! Linux/Windows builds never see the Speech/AVFAudio frameworks at all.
//!
//! The renderer sends a finished 16-bit-PCM mono WAV (canonical 44-byte
//! `fmt `+`data` header) for both push-to-talk and ambient voice. This
//! module parses that into Float32 mono samples, feeds them to an
//! `SFSpeechRecognizer` in on-device mode
//! (`requiresOnDeviceRecognition = true` — audio never leaves the Mac, the
//! same local-only promise whisper.cpp made), and returns the best
//! transcription as plain text.
//!
//! Task 3 adds a streaming variant alongside the batch path: `begin`/
//! `append`/`finish`/`cancel` drive a session-lifetime worker thread that
//! feeds the recognizer live PCM chunks and returns partial results over
//! the `stt:partial` event before handing back the final text at VAD
//! endpoint — see the "streaming recognition" section below.
//!
//! Threading: `apple_available` is a synchronous preflight safe to call on
//! any thread. `transcribe_wav` must not block a Tauri command's async
//! worker on the recognizer, so the macOS implementation hops to
//! `tokio::task::spawn_blocking` and waits on the recognition
//! result-handler block through a `std::sync::mpsc` rendezvous. All
//! Objective-C state (`SFSpeechRecognizer`, the request, the audio buffer,
//! the result-handler block) is created, used, and dropped entirely inside
//! the blocking closure — nothing Objective-C crosses an await point,
//! exactly like `touchid.rs`'s `LAContext`. The recognition rendezvous is
//! bounded by `stt::DEFAULT_TIMEOUT` so a framework that never fires its
//! result handler can't hang the thread forever; only the user-driven
//! authorization wait is unbounded (the user may take as long as they
//! like).

/// The one string shown when Speech Recognition authorization is denied or
/// restricted — the user has to flip it in System Settings; nothing in-app
/// can re-request it once denied.
#[cfg(target_os = "macos")]
const DENIED: &str = "Speech Recognition is disabled for Tome. Enable it in \
                       System Settings → Privacy & Security → Speech Recognition, then try again.";

/// The error surfaced when the recognizer produced no text.
#[cfg(target_os = "macos")]
const NO_TEXT: &str = "Speech recognition returned no text.";

/// The error surfaced when the WAV header isn't the canonical RIFF/WAVE/fmt
/// layout the renderer produces — a malformed payload fails loudly rather
/// than running recognition over garbage and returning an empty transcript.
const MALFORMED: &str = "Speech recognition received malformed audio.";

/// The error surfaced when the WAV is a well-formed header with no sample
/// data at all.
const NO_AUDIO: &str = "Speech recognition received no audio.";

/// The on-device readiness probe shared by [`apple_available`] (status
/// reporting) and [`transcribe_on_device`] (the pre-task availability
/// check): the recognizer must report itself available AND able to
/// recognize without a network. A pure read, never a prompt.
#[cfg(target_os = "macos")]
fn supports_on_device(recognizer: &objc2_speech::SFSpeechRecognizer) -> bool {
    // SAFETY: both getters are pure reads that never prompt; the caller
    // guarantees `recognizer` is a valid, retained instance.
    unsafe { recognizer.isAvailable() && recognizer.supportsOnDeviceRecognition() }
}

/// The real on-device availability probe (`resolve_engine` calls this, not a
/// stub): true when an `SFSpeechRecognizer` can be built for the system
/// locale AND it reports on-device support.
/// This must never raise the TCC prompt — it is a pure status probe used to
/// drive `stt:status`/`stt:engine` readiness and the `auto` engine choice,
/// so it only reads `isAvailable()`/`supportsOnDeviceRecognition()` and
/// never touches authorization.
#[cfg(target_os = "macos")]
pub fn apple_available() -> bool {
    use objc2::AnyThread;
    use objc2_speech::SFSpeechRecognizer;

    // `init`/`alloc` (not `new()`): `+[SFSpeechRecognizer new]` returns nil
    // when the system locale isn't a supported dictation locale, and
    // objc2's `new()` panics on a nil return — `init` returns `Option`, so
    // a nil recognizer is handled as "not available" instead of crashing.
    // SAFETY: `alloc` is always safe (it never raises the TCC prompt), and
    // `init` merely constructs the recognizer — nil is its documented
    // "not supported" signal, returned as `None` here.
    let recognizer = unsafe { SFSpeechRecognizer::init(SFSpeechRecognizer::alloc()) };
    let Some(recognizer) = recognizer else {
        return false;
    };
    supports_on_device(&recognizer)
}

#[cfg(not(target_os = "macos"))]
pub fn apple_available() -> bool {
    false
}

/// Creates a ready `SFSpeechRecognizer` and resolves availability +
/// authorization, or returns the friendly reason it can't. Factored out of
/// [`transcribe_on_device`] (batch) so the streaming path ([`begin`]'s
/// preflight and the session worker) performs the exact same availability +
/// TCC dance instead of drifting. Must run on a non-async thread: the
/// `NotDetermined` branch blocks on the user's answer to the system TCC
/// prompt (unbounded by design — see [`transcribe_on_device`]'s own doc
/// comment) and creates a `!Send` recognizer, so the caller either runs it
/// inside `spawn_blocking` or on the streaming worker's dedicated thread.
#[cfg(target_os = "macos")]
fn prepare_recognizer() -> Result<objc2::rc::Retained<objc2_speech::SFSpeechRecognizer>, String> {
    use objc2::AnyThread;
    use objc2_speech::{SFSpeechRecognizer, SFSpeechRecognizerAuthorizationStatus};

    // SAFETY: `alloc` is always safe (it never raises the TCC prompt), and
    // `init` merely constructs the recognizer — nil is its documented
    // "not supported" signal, returned as `None` here.
    let recognizer = unsafe { SFSpeechRecognizer::init(SFSpeechRecognizer::alloc()) };
    let Some(recognizer) = recognizer else {
        return Err(crate::stt::APPLE_UNAVAILABLE.to_string());
    };
    if !supports_on_device(&recognizer) {
        return Err(crate::stt::APPLE_UNAVAILABLE.to_string());
    }
    // SAFETY: `authorizationStatus` is a pure class-method read of the app's
    // current TCC state; it never raises the prompt.
    let status = unsafe { SFSpeechRecognizer::authorizationStatus() };
    if status == SFSpeechRecognizerAuthorizationStatus::Denied
        || status == SFSpeechRecognizerAuthorizationStatus::Restricted
    {
        return Err(DENIED.to_string());
    }
    if status == SFSpeechRecognizerAuthorizationStatus::NotDetermined {
        let (tx, rx) = std::sync::mpsc::channel::<SFSpeechRecognizerAuthorizationStatus>();
        let handler = block2::RcBlock::new(move |status: SFSpeechRecognizerAuthorizationStatus| {
            // If the receiver hung up (caller dropped the future), dropping
            // the send is the right move — the prompt outcome has no one
            // left to report to.
            let _ = tx.send(status);
        });
        // SAFETY: `requestAuthorization` posts a request and returns
        // immediately; the reply block (`handler`) is a valid heap block that
        // outlives the call (it lives until this function returns).
        unsafe { SFSpeechRecognizer::requestAuthorization(&handler) };
        // Unbounded by design: this waits on the user's answer to the system
        // TCC prompt (mirroring touchid.rs's `evaluatePolicy` wait) — the
        // user may take as long as they like, and the reply block fires only
        // once they respond, so a timeout would be wrong here.
        let status = rx
            .recv()
            .unwrap_or(SFSpeechRecognizerAuthorizationStatus::Denied);
        if status != SFSpeechRecognizerAuthorizationStatus::Authorized {
            return Err(DENIED.to_string());
        }
    }
    Ok(recognizer)
}

/// Parses a canonical 44-byte-header mono int16 WAV into Float32 samples
/// plus the sample rate. Pure and cross-platform; the renderer only ever
/// sends the standard `fmt `+`data` layout, so the data payload is simply
/// everything after the 44-byte header (no `LIST`/`INFO` chunk walk
/// needed). The RIFF/WAVE/fmt magic check rejects non-WAV input loudly
/// instead of transcribing garbage into an empty result.
fn parse_wav(wav: &[u8]) -> Result<(Vec<f32>, u32), String> {
    if wav.len() < 44 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" || &wav[12..16] != b"fmt "
    {
        return Err(MALFORMED.to_string());
    }
    let sample_rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
    let samples: Vec<f32> = wav[44..]
        .chunks_exact(2)
        .map(|c| {
            // int16 LE → [-1, 1]; the clamp is a no-op for int16 input but
            // keeps the invariant explicit if a stray sample ever overflows.
            (i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).clamp(-1.0, 1.0)
        })
        .collect();
    if samples.is_empty() {
        return Err(NO_AUDIO.to_string());
    }
    Ok((samples, sample_rate))
}

/// `stt_transcribe`'s Apple path: parse the WAV, then run recognition on a
/// blocking thread and hand the text back. `Ok(text)` is the collapsed
/// transcription; every failure mode (unavailable, denied, no text, …) is
/// an `Err(<human message>)` the caller wraps as `{ error }`.
#[cfg(target_os = "macos")]
pub async fn transcribe_wav(wav: &[u8]) -> Result<String, String> {
    let (samples, sample_rate) = parse_wav(wav)?;
    tokio::task::spawn_blocking(move || transcribe_on_device(&samples, sample_rate))
        .await
        .map_err(|e| format!("Speech recognition failed: {e}"))?
}

#[cfg(not(target_os = "macos"))]
pub async fn transcribe_wav(_wav: &[u8]) -> Result<String, String> {
    Err("Apple speech recognition is not available on this platform.".to_string())
}

/// The whole on-device recognition dance, run inside `spawn_blocking`. All
/// Objective-C objects live and die in this one closure; the mpsc
/// rendezvous hands the single result (or error) back to the caller.
#[cfg(target_os = "macos")]
fn transcribe_on_device(samples: &[f32], sample_rate: u32) -> Result<String, String> {
    use objc2::{AnyThread, ClassType};
    use objc2_avf_audio::{AVAudioFormat, AVAudioPCMBuffer};
    use objc2_foundation::NSError;
    use objc2_speech::{SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult};

    // 1. An AVAudioFormat matching the WAV: parsed sample rate, mono,
    //    deinterleaved Float32.
    // SAFETY: `alloc`/`initStandardFormatWithSampleRate_channels` only build
    // an immutable format descriptor from the caller's sample rate/channel
    // count — no audio, no prompt — and a nil return (handled below) is its
    // only failure mode.
    let format = unsafe {
        AVAudioFormat::initStandardFormatWithSampleRate_channels(
            AVAudioFormat::alloc(),
            f64::from(sample_rate),
            1,
        )
    };
    let Some(format) = format else {
        return Err("Speech recognition: unsupported audio format.".to_string());
    };

    // 2. An AVAudioPCMBuffer sized to hold every sample, filled in-place.
    let frame_count = samples.len() as u32;
    // SAFETY: `alloc`/`initWithPCMFormat_frameCapacity` allocate a plain PCM
    // buffer for the given format; a nil return (handled below) is its only
    // failure mode, and no samples are read or written here.
    let buffer = unsafe {
        AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
            AVAudioPCMBuffer::alloc(),
            &format,
            frame_count,
        )
    };
    let Some(buffer) = buffer else {
        return Err("Speech recognition: could not allocate an audio buffer.".to_string());
    };
    // SAFETY: floatChannelData() returns a pointer to `channelCount` pointers
    // to float; deinterleaved mono has exactly one channel, so deref the
    // array once to reach channel 0's buffer and copy the run in. The
    // destination holds exactly `frameCapacity` floats (we just allocated it
    // with `samples.len()`), so `copy_nonoverlapping` is in bounds and the
    // two regions can never alias.
    unsafe {
        let channel = *buffer.floatChannelData();
        std::ptr::copy_nonoverlapping(samples.as_ptr(), channel.as_ptr(), samples.len());
        buffer.setFrameLength(frame_count);
    }

    // 3. The batch request: on-device only, no partial results, all audio
    //    up front.
    // SAFETY: `new()` allocates an empty request object — no prompt, no
    // audio — and cannot return nil for this class.
    let request = unsafe { SFSpeechAudioBufferRecognitionRequest::new() };
    // SAFETY: these setters/append/end only configure the freshly created
    // request with already-owned values; no borrowed pointer escapes the
    // call, so nothing can outlive `buffer`/`request` below.
    unsafe {
        // requiresOnDeviceRecognition/shouldReportPartialResults live on the
        // base SFSpeechRecognitionRequest — reach them through as_super().
        request.as_super().setRequiresOnDeviceRecognition(true);
        request.as_super().setShouldReportPartialResults(false);
        request.appendAudioPCMBuffer(&buffer);
        request.endAudio();
    }

    // 4. A ready recognizer — availability + authorization resolved before
    //    the task starts so a denied/unavailable device never spins one up
    //    (see [`prepare_recognizer`]).
    let recognizer = prepare_recognizer()?;

    // 5. Run recognition. Because partial results are off, the handler fires
    //    exactly once with the final result (or an error) — no accumulation
    //    needed.
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let handler = block2::RcBlock::new(
        move |result: *mut SFSpeechRecognitionResult, err: *mut NSError| {
            // SAFETY: the Speech framework hands this block either a non-null
            // result or a non-null error; `as_ref` only reads that pointer's
            // pointee while it is valid for the duration of the callback.
            if let Some(result) = unsafe { result.as_ref() } {
                let text = unsafe { result.bestTranscription().formattedString().to_string() };
                let _ = tx.send(Ok(text));
            } else if let Some(err) = unsafe { err.as_ref() } {
                // Mirror Electron-style rejections: the NSError's
                // localizedDescription is the message the user should see.
                let _ = tx.send(Err(err.localizedDescription().to_string()));
            } else {
                let _ = tx.send(Err("Speech recognition failed.".to_string()));
            }
        },
    );
    // Keep the task alive until the block fires — dropping it cancels
    // recognition mid-flight. recognizer/request/buffer/format/handler stay
    // alive too: they all drop only after the recv below returns.
    // SAFETY: `recognitionTaskWithRequest_resultHandler` starts recognition
    // on the (valid, retained) recognizer with the (valid, retained) request
    // and the heap block; every argument outlives the call.
    let _task = unsafe {
        recognizer.recognitionTaskWithRequest_resultHandler(request.as_super(), &handler)
    };

    // The whisper path guards with stt::DEFAULT_TIMEOUT (60s); the Speech
    // framework makes no completion guarantee, so the recognition rendezvous
    // gets the same hard cap — a result handler that never fires must not
    // hang this blocking thread forever.
    let text = match rx.recv_timeout(crate::stt::DEFAULT_TIMEOUT) {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err("Speech recognition timed out.".to_string()),
    };
    if text.trim().is_empty() {
        return Err(NO_TEXT.to_string());
    }
    Ok(text)
}

// ---------- streaming recognition (voice-0.4 Task 3) ----------
//
// The batch path above transcribes one finished WAV. The streaming path
// below feeds the same `SFSpeechRecognizer` live audio: the renderer sends
// PCM chunks as they are captured, partial results stream back to it over
// the `stt:partial` event, and the final text is returned once the VAD
// endpoint fires (the renderer calls `stt_finish`, which is `finish` here).
//
// Lifecycle: `begin` cancels any in-flight session, preflights availability
// + authorization, then spawns a dedicated OS thread that owns all
// Objective-C state for the session's lifetime. `append`/`finish`/`cancel`
// reach it through a module-local static holding the session's channel
// handle — only that handle (`Send + Sync`) crosses threads; the
// recognizer, request, format, and result-handler block are created, used,
// and dropped entirely on the worker thread.

/// The error surfaced when a streaming call arrives with no active session.
const NO_SESSION: &str = "no active speech session";

/// The error surfaced when the worker has already exited (or never started)
/// and a call can no longer reach it.
#[cfg(target_os = "macos")]
const SESSION_ENDED: &str = "the speech session ended unexpectedly";

/// One command to the streaming worker. The channel is the only thing that
/// crosses threads; every Objective-C object stays on the worker.
#[cfg(target_os = "macos")]
enum Msg {
    /// Append one chunk of 16-bit mono PCM; the worker converts + buffers it.
    Append(Vec<u8>),
    /// End the request and hand the final transcription (or error) back.
    Finish(tokio::sync::oneshot::Sender<Result<String, String>>),
    /// Cancel recognition mid-flight.
    Cancel,
}

/// The renderer-facing handle to a live session — just the sender half of
/// the worker channel, so it is `Send + Sync` and cheap to stash in the
/// static below. The worker owns the receiver and all ObjC state.
#[cfg(target_os = "macos")]
struct Session {
    tx: tokio::sync::mpsc::UnboundedSender<Msg>,
}

/// The one live session, if any. A module-local static rather than an
/// `AppState` field for the same reason `ipc::chat` keeps its `HTTP_CLIENT`
/// module-local: the session is a process-wide singleton keyed by nothing
/// but "is a dictation in flight". A `std::sync::Mutex<Option<..>>`
/// (const-constructible, held only across a channel send/take) is all the
/// synchronization a single-handle swap needs.
#[cfg(target_os = "macos")]
static SESSION: std::sync::Mutex<Option<Session>> = std::sync::Mutex::new(None);

/// Pure helper: little-endian i16 mono samples → f32 (÷ 32768). The odd
/// trailing byte of a chunk with an incomplete final sample is dropped by
/// `chunks_exact(2)`; empty input → empty output. No clamp — int16 is
/// already within [-1, 1].
fn pcm16_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect()
}

/// Starts a streaming session (Apple path). Cancels any in-flight session,
/// then preflights availability + authorization before spawning the worker
/// thread. The preflight runs inside `spawn_blocking` (not on the async
/// worker) because the TCC prompt wait is unbounded — the same rule
/// [`transcribe_wav`] follows; the recognizer it builds is discarded (the
/// worker builds its own), so only the availability/auth verdict crosses
/// the thread boundary.
#[cfg(target_os = "macos")]
pub async fn begin(app: tauri::AppHandle, sample_rate: u32) -> Result<(), String> {
    cancel();

    tokio::task::spawn_blocking(|| prepare_recognizer().map(|_| ()))
        .await
        .map_err(|e| format!("Speech recognition failed: {e}"))??;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();
    *SESSION.lock().expect("speech session lock poisoned") = Some(Session { tx });

    // A dedicated OS thread, not a tokio task or a `spawn_blocking` job: the
    // recognizer/request/format/block are `!Send`, so they must live and die
    // on one thread, and that thread must live exactly as long as the
    // session. A `spawn_blocking` pool thread would tie session lifetime to
    // the pool's unrelated jobs, and a tokio task may migrate across runtime
    // workers mid-await. `std::thread::spawn` gives a session-lifetime thread
    // with no such migration.
    std::thread::spawn(move || worker_loop(app, sample_rate, rx));
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub async fn begin(_app: tauri::AppHandle, _sample_rate: u32) -> Result<(), String> {
    Err("Apple speech recognition is not available on this platform.".to_string())
}

/// Appends a raw 16-bit mono PCM chunk to the live session.
#[cfg(target_os = "macos")]
pub fn append(bytes: Vec<u8>) -> Result<(), String> {
    let session = SESSION.lock().expect("speech session lock poisoned");
    let Some(session) = session.as_ref() else {
        return Err(NO_SESSION.to_string());
    };
    session
        .tx
        .send(Msg::Append(bytes))
        .map_err(|_| SESSION_ENDED.to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn append(_bytes: Vec<u8>) -> Result<(), String> {
    Err(NO_SESSION.to_string())
}

/// Ends the live session and awaits the final transcription.
#[cfg(target_os = "macos")]
pub async fn finish() -> Result<String, String> {
    let session = SESSION
        .lock()
        .expect("speech session lock poisoned")
        .take()
        .ok_or_else(|| NO_SESSION.to_string())?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    session
        .tx
        .send(Msg::Finish(tx))
        .map_err(|_| SESSION_ENDED.to_string())?;
    rx.await.map_err(|_| SESSION_ENDED.to_string())?
}

#[cfg(not(target_os = "macos"))]
pub async fn finish() -> Result<String, String> {
    Err(NO_SESSION.to_string())
}

/// Cancels the live session, if any.
#[cfg(target_os = "macos")]
pub fn cancel() {
    if let Some(session) = SESSION.lock().expect("speech session lock poisoned").take() {
        let _ = session.tx.send(Msg::Cancel);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn cancel() {}

/// The session-lifetime worker. Owns every Objective-C object; loops over
/// the command channel until `Finish` or `Cancel`.
#[cfg(target_os = "macos")]
fn worker_loop(
    app: tauri::AppHandle,
    sample_rate: u32,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Msg>,
) {
    use objc2::{AnyThread, ClassType};
    use objc2_avf_audio::{AVAudioFormat, AVAudioPCMBuffer};
    use objc2_foundation::NSError;
    use objc2_speech::{SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult};
    use tauri::Emitter;

    // `begin` already preflighted availability + auth, so this only re-checks
    // defensively against a permission flip mid-session. On the (near-
    // impossible) failure there is no `finish` caller listening yet, so there
    // is nothing to report to — just exit and let a later `finish` surface
    // "the speech session ended unexpectedly".
    let recognizer = match prepare_recognizer() {
        Ok(r) => r,
        Err(_) => return,
    };

    // 1. The live-audio request: on-device, partial results reported, audio
    //    appended incrementally and ended on `Finish`.
    // SAFETY: `new()` allocates an empty request object — no prompt, no
    // audio — and cannot return nil for this class.
    let request = unsafe { SFSpeechAudioBufferRecognitionRequest::new() };
    // SAFETY: these setters only configure the freshly created request with
    // already-owned values; no borrowed pointer escapes the call.
    unsafe {
        request.as_super().setRequiresOnDeviceRecognition(true);
        request.as_super().setShouldReportPartialResults(true);
    }

    // 2. The format the renderer's PCM will be re-encoded into: `sample_rate`
    //    (the mic's actual rate) mono Float32.
    // SAFETY: `alloc`/`initStandardFormatWithSampleRate_channels` only build
    // an immutable format descriptor — no audio, no prompt — and a nil
    // return (handled below) is its only failure mode.
    let format = unsafe {
        AVAudioFormat::initStandardFormatWithSampleRate_channels(
            AVAudioFormat::alloc(),
            f64::from(sample_rate),
            1,
        )
    };
    let Some(format) = format else {
        return;
    };

    // 3. The result rendezvous + handler block. The framework calls the
    //    block once per partial result and once more for the final result
    //    (or an error); `final_tx` carries the final text (or error) back to
    //    the worker, and every result is also emitted to the renderer as a
    //    `stt:partial` event so push-to-talk can live-write it.
    let (final_tx, final_rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let app_for_handler = app.clone();
    let handler = block2::RcBlock::new(
        move |result: *mut SFSpeechRecognitionResult, err: *mut NSError| {
            // SAFETY: the framework hands this block either a non-null result
            // or a non-null error; `as_ref` only reads that pointer's pointee
            // while it is valid for the duration of the callback.
            if let Some(result) = unsafe { result.as_ref() } {
                let text = unsafe { result.bestTranscription().formattedString().to_string() };
                // SAFETY: `isFinal` is a pure getter on the valid result.
                if unsafe { result.isFinal() } {
                    let _ = final_tx.send(Ok(text.clone()));
                }
                // Emit every result (partials included) so the renderer can
                // live-write push-to-talk dictation; the final text is what
                // `finish` returns, so `autoSend` ignores these entirely.
                let _ = app_for_handler.emit("stt:partial", serde_json::json!({ "text": text }));
            } else if let Some(err) = unsafe { err.as_ref() } {
                let _ = final_tx.send(Err(err.localizedDescription().to_string()));
            }
        },
    );

    // 4. Start recognition. `task` (and request/recognizer/format/handler)
    //    stay bound until the loop below exits — dropping the task early
    //    would cancel recognition mid-flight.
    // SAFETY: `recognitionTaskWithRequest_resultHandler` starts recognition
    // on the (valid, retained) recognizer with the (valid, retained) request
    // and the heap block; every argument outlives the call.
    let task = unsafe {
        recognizer.recognitionTaskWithRequest_resultHandler(request.as_super(), &handler)
    };

    // 5. Serve the session.
    loop {
        match rx.blocking_recv() {
            Some(Msg::Append(bytes)) => {
                let samples = pcm16_to_f32(&bytes);
                if samples.is_empty() {
                    continue;
                }
                let frame_count = samples.len() as u32;
                // SAFETY: `alloc`/`initWithPCMFormat_frameCapacity` allocate
                // a plain PCM buffer for the given format; a nil return
                // (handled below) is its only failure mode.
                let buffer = unsafe {
                    AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                        AVAudioPCMBuffer::alloc(),
                        &format,
                        frame_count,
                    )
                };
                let Some(buffer) = buffer else {
                    continue;
                };
                // SAFETY: floatChannelData() returns a pointer to one channel
                // pointer (mono); the destination holds `frameCapacity`
                // floats, so the copy is in bounds and the two regions can
                // never alias.
                unsafe {
                    let channel = *buffer.floatChannelData();
                    std::ptr::copy_nonoverlapping(
                        samples.as_ptr(),
                        channel.as_ptr(),
                        samples.len(),
                    );
                    buffer.setFrameLength(frame_count);
                    request.appendAudioPCMBuffer(&buffer);
                }
            }
            Some(Msg::Finish(tx)) => {
                // SAFETY: `endAudio` only signals the framework that no more
                // audio is coming; safe on the (valid, retained) request.
                unsafe { request.endAudio() };
                // The framework makes no completion guarantee; the same hard
                // cap the batch path uses bounds the final-result rendezvous.
                let res = match final_rx.recv_timeout(crate::stt::DEFAULT_TIMEOUT) {
                    Ok(Ok(text)) => Ok(text),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err("Speech recognition timed out.".to_string()),
                };
                let _ = tx.send(res);
                break;
            }
            Some(Msg::Cancel) => {
                // SAFETY: `cancel` only signals the framework to stop; safe
                // on the (valid, retained) task.
                unsafe { task.cancel() };
                break;
            }
            // The sender halves (the static's `Session` handle) were all
            // dropped without a `Finish`/`Cancel` — nothing left to serve.
            None => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a canonical 44-byte-header mono int16 WAV around `data` at the
    /// given sample rate — the exact shape the renderer's encoder produces,
    /// so the parse tests exercise the real header layout.
    fn make_wav(sample_rate: u32, data: &[u8]) -> Vec<u8> {
        let data_len = data.len() as u32;
        let mut wav = Vec::with_capacity(44 + data.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(data);
        wav
    }

    /// On the macOS dev host this is a real hardware/framework probe, so it
    /// only asserts the call doesn't crash and returns a bool — the actual
    /// value depends on the machine. Off macOS it must be `false`.
    #[test]
    fn apple_available_returns_a_bool_without_panicking() {
        let v = apple_available();
        #[cfg(not(target_os = "macos"))]
        assert!(!v);
        #[cfg(target_os = "macos")]
        let _ = v;
    }

    /// The non-macOS stub always refuses with the honest message — this is
    /// the contract `ipc::stt::stt_transcribe` relies on off macOS.
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn transcribe_wav_stub_refuses_off_macos() {
        let err = transcribe_wav(&[0u8; 4]).await.unwrap_err();
        assert!(err.contains("not available"));
    }

    // ---- parse_wav — pure cross-platform logic, worth pinning on every OS
    // since the real recognizer can't run in CI at all.

    #[test]
    fn parse_wav_reads_sample_rate_and_converts_int16_pcm_to_f32() {
        let mut data = Vec::new();
        data.extend_from_slice(&16_384i16.to_le_bytes());
        data.extend_from_slice(&(-32_768i16).to_le_bytes());
        let wav = make_wav(16_000, &data);

        let (samples, rate) = parse_wav(&wav).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(samples.len(), 2);
        assert!((samples[0] - 0.5).abs() < f32::EPSILON, "{}", samples[0]);
        assert_eq!(samples[1], -1.0);
    }

    #[test]
    fn parse_wav_rejects_a_truncated_header() {
        assert!(parse_wav(&[0u8; 20]).is_err());
    }

    #[test]
    fn parse_wav_rejects_non_wav_magic_bytes() {
        // 64 zero bytes: right length, wrong RIFF/WAVE/fmt magic.
        let err = parse_wav(&[0u8; 64]).unwrap_err();
        assert!(err.contains("malformed"), "{err}");
    }

    #[test]
    fn parse_wav_rejects_a_header_with_no_samples() {
        let err = parse_wav(&make_wav(16_000, &[])).unwrap_err();
        assert!(err.contains("no audio"), "{err}");
    }

    // ---- pcm16_to_f32 — pure cross-platform helper ----

    #[test]
    fn pcm16_to_f32_converts_little_endian_i16_to_float() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&16_384i16.to_le_bytes());
        bytes.extend_from_slice(&(-32_768i16).to_le_bytes());
        bytes.extend_from_slice(&0i16.to_le_bytes());
        let out = pcm16_to_f32(&bytes);
        assert_eq!(out.len(), 3);
        assert!((out[0] - 0.5).abs() < f32::EPSILON, "{}", out[0]);
        assert_eq!(out[1], -1.0);
        assert_eq!(out[2], 0.0);
    }

    #[test]
    fn pcm16_to_f32_drops_an_odd_trailing_byte() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&16_384i16.to_le_bytes());
        bytes.push(0xff); // trailing half-sample — ignored, not a panic
        let out = pcm16_to_f32(&bytes);
        assert_eq!(out.len(), 1);
        assert!((out[0] - 0.5).abs() < f32::EPSILON, "{}", out[0]);
    }

    #[test]
    fn pcm16_to_f32_returns_empty_for_empty_input() {
        assert!(pcm16_to_f32(&[]).is_empty());
    }

    // ---- streaming stubs (non-macOS) ----
    //
    // `begin` is not unit-tested here even though its stub is a one-liner:
    // it takes a `tauri::AppHandle`, and this crate has no AppHandle-mocking
    // dependency (the same documented boundary lsp.rs draws around its own
    // AppHandle-touching entry points). `append`/`finish`/`cancel` need no
    // AppHandle, so the no-session contract is pinned directly.

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn append_stub_reports_no_session() {
        assert_eq!(append(vec![0u8; 2]).unwrap_err(), NO_SESSION);
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn finish_stub_reports_no_session() {
        assert_eq!(finish().await.unwrap_err(), NO_SESSION);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn cancel_stub_is_a_no_op() {
        cancel();
    }
}
