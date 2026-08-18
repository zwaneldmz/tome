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
//! exactly like `touchid.rs`'s `LAContext`.

/// The one string shown when Speech Recognition authorization is denied or
/// restricted — the user has to flip it in System Settings; nothing in-app
/// can re-request it once denied.
#[cfg(target_os = "macos")]
const DENIED: &str = "Speech Recognition is disabled for Tome. Enable it in \
                       System Settings → Privacy & Security → Speech Recognition, then try again.";

/// The error surfaced when the recognizer produced no text. Kept here (not
/// in `stt.rs`) because it is Apple-path-specific, unlike the shared
/// `stt::APPLE_UNAVAILABLE` availability message.
#[cfg(target_os = "macos")]
const NO_TEXT: &str = "Speech recognition returned no text.";

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
    let recognizer = unsafe { SFSpeechRecognizer::init(SFSpeechRecognizer::alloc()) };
    let Some(recognizer) = recognizer else {
        return false;
    };
    // SAFETY: both getters are pure reads that never prompt; the recognizer
    // is a valid, retained instance at this point.
    unsafe { recognizer.isAvailable() && recognizer.supportsOnDeviceRecognition() }
}

#[cfg(not(target_os = "macos"))]
pub fn apple_available() -> bool {
    false
}

/// Parses a canonical 44-byte-header mono int16 WAV into Float32 samples
/// plus the sample rate. Pure and testable; the renderer only ever sends
/// the standard `fmt `+`data` layout, so the data payload is simply
/// everything after the 44-byte header (no `LIST`/`INFO` chunk walk
/// needed).
#[cfg(target_os = "macos")]
fn parse_wav(wav: &[u8]) -> Result<(Vec<f32>, u32), String> {
    if wav.len() < 44 {
        return Err("stt: bad audio payload".to_string());
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
        return Err("stt: bad audio payload".to_string());
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
    unsafe {
        // floatChannelData() returns a pointer to `channelCount` pointers to
        // float; deinterleaved mono has exactly one channel, so deref the
        // array once to reach channel 0's buffer and copy the run in. The
        // destination holds exactly `frameCapacity` floats (we just allocated
        // it with `samples.len()`), so `copy_nonoverlapping` is in bounds and
        // the two regions can never alias.
        let channel = *buffer.floatChannelData();
        std::ptr::copy_nonoverlapping(samples.as_ptr(), channel.as_ptr(), samples.len());
        buffer.setFrameLength(frame_count);
    }

    // 3. The batch request: on-device only, no partial results, all audio
    //    up front.
    let request = unsafe { SFSpeechAudioBufferRecognitionRequest::new() };
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
    let recognizer = unsafe { SFSpeechRecognizer::init(SFSpeechRecognizer::alloc()) };
    let Some(recognizer) = recognizer else {
        return Err(crate::stt::APPLE_UNAVAILABLE.to_string());
    };
    let available = unsafe { recognizer.isAvailable() && recognizer.supportsOnDeviceRecognition() };
    if !available {
        return Err(crate::stt::APPLE_UNAVAILABLE.to_string());
    }
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
        // requestAuthorization raises the one-time TCC prompt; its reply
        // block delivers the user's answer on an arbitrary queue, which the
        // mpsc rendezvous hands back to this blocking thread.
        unsafe { SFSpeechRecognizer::requestAuthorization(&handler) };
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
    // alive too: they all drop only after rx.recv() returns below.
    let _task = unsafe {
        recognizer.recognitionTaskWithRequest_resultHandler(request.as_super(), &handler)
    };

    let text = rx
        .recv()
        .unwrap_or_else(|_| Err("Speech recognition failed.".to_string()))?;
    if text.trim().is_empty() {
        return Err(NO_TEXT.to_string());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ---- parse_wav — the one piece of pure logic in this module, worth
    // pinning on the macOS host since the real recognizer can't run in CI.

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_wav_reads_sample_rate_and_converts_int16_pcm_to_f32() {
        let sample_rate = 16_000u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
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
        wav.extend_from_slice(&4u32.to_le_bytes()); // data length = 2 samples
        wav.extend_from_slice(&16_384i16.to_le_bytes());
        wav.extend_from_slice(&(-32_768i16).to_le_bytes());

        let (samples, rate) = parse_wav(&wav).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(samples.len(), 2);
        assert!((samples[0] - 0.5).abs() < f32::EPSILON, "{}", samples[0]);
        assert_eq!(samples[1], -1.0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_wav_rejects_a_truncated_header() {
        assert!(parse_wav(&[0u8; 20]).is_err());
    }
}
