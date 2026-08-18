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
    use objc2_speech::{
        SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognizer,
        SFSpeechRecognizerAuthorizationStatus,
    };

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

    // 4. Recognizer + availability + authorization, all before the task
    //    starts so a denied/unavailable device never spins one up.
    // SAFETY: `alloc`/`init` construct the recognizer (nil → `None`, handled
    // below); this reads availability/auth status only, never prompts.
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
}
