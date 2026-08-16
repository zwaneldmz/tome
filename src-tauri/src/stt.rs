//! Local speech-to-text for push-to-talk: write the renderer's WAV to a
//! temp file, run the whisper.cpp CLI over it, hand back its
//! (whitespace-collapsed) stdout. Ports `src/main/lib/stt.js` — see that
//! file's own doc comment for why this is local-only by design (HANDOFF
//! §5): audio never leaves the machine, so there is no new egress path or
//! allowlist host to add, and the transcript is ordinary composer text
//! once it lands.
//!
//! Everything below is a plain function over explicit values (no
//! `AppHandle`, no Tauri `State`) so it's directly unit-testable — the
//! same pure-core/thin-glue split `store.rs`/`git.rs` already use.
//! `ipc::stt` is the only caller: it resolves the `AppHandle`-derived
//! paths (`app_data_dir` for the model, `temp_dir` for the scratch WAV —
//! Electron's `app.getPath('temp')` is the *shared OS temp directory*,
//! not an app-specific cache folder, and Tauri's `PathResolver::temp_dir`
//! is documented to resolve to exactly `std::env::temp_dir()`, so that
//! one is the faithful match, not `app_cache_dir`) and the
//! `TOME_WHISPER_BIN` env override, then hands plain values down to the
//! functions here.
//!
//! One deliberate addition beyond a literal port: [`whisper_bin`] backs
//! its bare-name fallback with a real `$PATH` walk ([`path_lookup`]/
//! [`find_on_path`]). The JS original can't afford one without a
//! dependency (Node's stdlib has no `which`) and settles for
//! optimistically assuming a bare candidate is present, deferring to a
//! real exec's ENOENT — see that function's own doc comment there. Linux
//! has no absolute homebrew-style candidate to fall back on at all (the
//! macOS-only "GUI apps launch with a stripped PATH" problem the absolute
//! candidates exist for doesn't apply the same way), so a real lookup is
//! the difference between `stt:status` always optimistically reporting
//! "ready" on Linux and it actually meaning something there.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The one model this build supports — same name `src/main/lib/stt.js`
/// hard-codes.
const MODEL: &str = "ggml-base.en.bin";
const MODEL_URL_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

/// Ports `CANDIDATES`: two absolute homebrew homes, then a bare name as
/// the final, always-accepted fallback. GUI apps on macOS launch with a
/// PATH that lacks the homebrew prefixes, so a bare `whisper-cli` alone
/// fails in the packaged app even when a terminal finds it fine; the bare
/// name stays as the last resort for PATH-correct launches (dev runs,
/// Linux).
const CANDIDATES: &[&str] = &[
    "/opt/homebrew/bin/whisper-cli",
    "/usr/local/bin/whisper-cli",
    "whisper-cli",
];

pub const NO_BIN: &str =
    "whisper-cli not found. Install it (brew install whisper-cpp) or point TOME_WHISPER_BIN at the binary.";

/// `transcribe`'s default hard timeout — ports `timeoutMs = 60_000`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// `stt:warmup`'s override — ports its call site's explicit `timeoutMs: 30_000`.
pub const WARMUP_TIMEOUT: Duration = Duration::from_secs(30);

/// `<app_data_dir>/models/ggml-base.en.bin` — ports `modelPath(userData)`.
pub fn model_path(app_data: &Path) -> PathBuf {
    app_data.join("models").join(MODEL)
}

/// Pure core of the `$PATH` walk — takes the `PATH` value explicitly
/// rather than reading `std::env::var` itself, so it's unit-testable
/// without mutating the real process environment (`std::env::set_var`
/// from a `#[test]` races every other test in the same binary that reads
/// `PATH` concurrently, which is exactly why this crate avoids it
/// everywhere else). Checks both existence and the executable bit — a
/// `which`-equivalent, not just `existsSync` — since a non-executable
/// same-named file should not count as "found". [`path_lookup`] is the
/// thin wrapper real callers go through.
fn find_on_path(path_var: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path_var).find_map(|dir| {
        let candidate = dir.join(name);
        let meta = std::fs::metadata(&candidate).ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(candidate)
    })
}

fn path_lookup(name: &str) -> Option<PathBuf> {
    find_on_path(&std::env::var_os("PATH")?, name)
}

/// Ports `whisperBin(env)`: `TOME_WHISPER_BIN` wins outright when set
/// (`override_bin` is the already-resolved value — callers filter out an
/// empty string themselves, mirroring JS's `if (env.TOME_WHISPER_BIN)`,
/// which treats `""` the same as unset). Otherwise the first existing
/// absolute candidate; otherwise a `$PATH` lookup for the bare name (this
/// module's addition — see the module doc comment); otherwise the bare
/// name itself, unconditionally. That last fallback matches the JS
/// original's guarantee that this always returns a truthy value (its
/// `CANDIDATES.find()` can only fail to match before its own final,
/// unconditionally-true bare-name entry) — a resolved path here simply
/// means [`bin_exists`]/[`stt_unavailable`] downstream get a real
/// `Path::exists()` check instead of a bare name's always-true shortcut.
pub fn whisper_bin(override_bin: Option<&str>) -> Option<String> {
    if let Some(o) = override_bin {
        if !o.is_empty() {
            return Some(o.to_string());
        }
    }
    for c in CANDIDATES {
        if c.contains('/') {
            if Path::new(c).exists() {
                return Some((*c).to_string());
            }
        } else {
            return Some(
                path_lookup(c)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| (*c).to_string()),
            );
        }
    }
    None // unreachable: CANDIDATES' last entry is always bare (see above)
}

/// Ports `binExists`: a resolved (path-separator-containing) bin is
/// checked for real; a bare name is presumed present. By the time a bare
/// name reaches here unresolved, `whisper_bin`'s own `$PATH` lookup
/// already failed to confirm it — a real invocation's ENOENT remains the
/// backstop, same as the JS original's own doc comment describes.
pub fn bin_exists(bin: &str) -> bool {
    !bin.is_empty() && (!bin.contains('/') || Path::new(bin).exists())
}

/// Ports `modelExists`.
pub fn model_exists(model: &Path) -> bool {
    model.exists()
}

/// Ports `sttUnavailable`: `None` means ready to run. No bin, or a
/// resolved bin that doesn't exist, is [`NO_BIN`]; otherwise a missing
/// model file gets the exact one-time download command (`stt:status`'s
/// onboarding UI shows this verbatim).
pub fn stt_unavailable(bin: Option<&str>, model: &Path) -> Option<String> {
    let bin_ok = match bin {
        Some(b) if !b.is_empty() => !(b.contains('/') && !Path::new(b).exists()),
        _ => false,
    };
    if !bin_ok {
        return Some(NO_BIN.to_string());
    }
    if !model.exists() {
        let dir = model.parent().unwrap_or_else(|| Path::new(""));
        return Some(format!(
            "Speech model missing. Download it once:\n  mkdir -p \"{}\" && curl -L -o \"{}\" {MODEL_URL_BASE}{MODEL}",
            dir.display(),
            model.display(),
        ));
    }
    None
}

/// One `transcribe()` call's inputs — ports the JS original's
/// `{ wav, bin, model, tempDir, timeoutMs }` options object. `timeout` is
/// required (not defaulted internally) since Rust has no default
/// parameters; callers pass [`DEFAULT_TIMEOUT`]/[`WARMUP_TIMEOUT`] or
/// their own.
pub struct TranscribeRequest<'a> {
    pub wav: &'a [u8],
    pub bin: &'a str,
    pub model: &'a Path,
    pub temp_dir: &'a Path,
    pub timeout: Duration,
}

/// Ports `transcribe({ wav, bin, model, tempDir, timeoutMs })`: writes
/// `wav` to a freshly named temp file under `temp_dir` (main invents this
/// path itself — the renderer/caller only ever supplies bytes, matching
/// the JS original's own comment), execs `bin` over it, then collapses
/// the captured stdout to a single run of whitespace. The temp file is
/// removed whenever it was actually written — success, spawn failure,
/// non-zero exit, or timeout — mirroring the JS original's `try { ... }
/// finally { await unlink(file).catch(() => {}) }`: a resolved `Ok` here
/// therefore always means the temp file is really gone, not merely
/// queued for cleanup. If the initial write itself fails, there is
/// nothing to clean up, same as the JS original (whose `await
/// writeFile(...)` sits *outside* its own try/finally).
pub async fn transcribe(req: TranscribeRequest<'_>) -> io::Result<String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let file = req
        .temp_dir
        .join(format!("tome-stt-{}-{stamp}.wav", std::process::id()));
    tokio::fs::write(&file, req.wav).await?;

    let result = run_whisper(req.bin, req.model, &file, req.timeout).await;
    let _ = tokio::fs::remove_file(&file).await; // best-effort, matches `.catch(() => {})`
    result
}

/// The actual `whisper-cli -m <model> -f <wav> --no-timestamps` exec,
/// killed with SIGKILL if `timeout` elapses. `kill_on_drop(true)` plus
/// dropping the timed-out future is sufficient for that (the same
/// established pattern `git.rs`'s `git()` helper already uses — see its
/// doc comment): `Command::output()`'s spawned `Child` lives inside the
/// future `tokio::time::timeout` drops on expiry, and Rust's
/// `Child::kill()` sends SIGKILL unconditionally on Unix, matching the JS
/// original's explicit `killSignal: 'SIGKILL'`. A non-zero exit surfaces
/// as `Err` too (matches Node's `execFile`, which invokes its callback's
/// `err` for a bad exit code, not only for a spawn failure) — no test
/// pins its exact message, so this favors trimmed stderr when present.
async fn run_whisper(bin: &str, model: &Path, wav: &Path, timeout: Duration) -> io::Result<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("-m")
        .arg(model)
        .arg("-f")
        .arg(wav)
        .arg("--no-timestamps")
        .kill_on_drop(true);

    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(res) => res?,
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{bin} timed out"),
            ))
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let trimmed = stderr.trim();
        return Err(io::Error::other(if trimmed.is_empty() {
            format!("{bin} exited with {}", output.status)
        } else {
            trimmed.to_string()
        }));
    }
    Ok(collapse_whitespace(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// `String(stdout).replace(/\s+/g, ' ').trim()` — whisper prints one
/// padded line per segment; dictated text wants a single run of prose.
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Minimal 16-bit PCM mono WAV header + `n_samples` zero samples — enough
/// for `stt_warmup`'s one call site (`encodeWav(new Float32Array(1600))`
/// in `index.js`, via `src/shared/wav.js`'s general Float32-samples-in
/// encoder). This is *not* a port of `wav.js` itself: that file stays
/// renderer-side, untouched, still exercised by `test/wav.test.js` (the
/// plan is explicit that `src/shared/**` stays vitest) — the renderer's
/// real microphone capture keeps calling the JS original. This is a
/// private, silence-only twin scoped to warmup's one need, specialized
/// because every sample warmup ever passes is `0.0` (the JS original's
/// `s < 0 ? s * 0x8000 : s * 0x7fff` collapses to `0` on either branch for
/// that input, so there is no clamping/rounding behavior left to
/// replicate).
fn silence_wav(n_samples: u32, sample_rate: u32) -> Vec<u8> {
    let data_len = n_samples * 2;
    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    buf.resize(buf.len() + data_len as usize, 0); // n_samples of silence
    buf
}

/// `ipc::stt::stt_warmup` is the only real caller ([`silence_wav`] is
/// private to this module); this thin wrapper pins the exact
/// `encodeWav(new Float32Array(1600))` shape (1600 samples, default
/// 16 kHz) at the one call site so that command doesn't need to know the
/// magic numbers.
pub fn warmup_silence() -> Vec<u8> {
    silence_wav(1_600, 16_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- model_path ----

    #[test]
    fn model_path_derives_the_path_under_app_data_models() {
        assert_eq!(
            model_path(Path::new("/ud")),
            PathBuf::from("/ud/models/ggml-base.en.bin")
        );
    }

    // ---- find_on_path / path_lookup ----

    fn make_executable(path: &Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, contents).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn find_on_path_returns_the_first_executable_match() {
        let miss_dir = tempfile::tempdir().unwrap();
        let hit_dir = tempfile::tempdir().unwrap();
        let hit = hit_dir.path().join("whisper-cli");
        make_executable(&hit, "#!/bin/sh\n");

        let path_var = std::env::join_paths([miss_dir.path(), hit_dir.path()]).unwrap();
        assert_eq!(find_on_path(&path_var, "whisper-cli"), Some(hit));
    }

    #[test]
    fn find_on_path_skips_a_non_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("whisper-cli"), "not executable").unwrap();
        let path_var = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(find_on_path(&path_var, "whisper-cli"), None);
    }

    #[test]
    fn find_on_path_returns_none_when_nowhere_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let path_var = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(
            find_on_path(&path_var, "definitely-not-a-real-binary-xyz"),
            None
        );
    }

    // ---- whisperBin() / whisper_bin ----

    #[test]
    fn whisper_bin_lets_override_win() {
        assert_eq!(
            whisper_bin(Some("/x/y/whisper")),
            Some("/x/y/whisper".to_string())
        );
    }

    #[test]
    fn whisper_bin_falls_back_to_a_path_lookup_name_at_worst() {
        // Mirrors the JS suite's own "falls back... at worst" test: on a
        // machine with no whisper-cli anywhere (the CI/dev default), this
        // still returns a truthy value rather than None.
        assert!(whisper_bin(None).is_some_and(|b| !b.is_empty()));
    }

    #[test]
    fn whisper_bin_treats_an_empty_override_as_unset() {
        // JS: `if (env.TOME_WHISPER_BIN)` — "" is falsy, same as absent.
        assert!(whisper_bin(Some("")).is_some_and(|b| !b.is_empty()));
    }

    // ---- sttUnavailable() / stt_unavailable ----

    #[test]
    fn stt_unavailable_names_the_install_fix_when_the_binary_path_is_dead() {
        assert_eq!(
            stt_unavailable(Some("/nope/whisper-cli"), Path::new("/nope/model.bin")),
            Some(NO_BIN.to_string())
        );
    }

    #[test]
    fn stt_unavailable_treats_no_bin_the_same_as_a_dead_path() {
        assert_eq!(
            stt_unavailable(None, Path::new("/nope/model.bin")),
            Some(NO_BIN.to_string())
        );
    }

    #[test]
    fn stt_unavailable_gives_the_exact_download_command_when_only_the_model_is_missing() {
        let why =
            stt_unavailable(Some("/bin/ls"), Path::new("/nope/models/ggml-base.en.bin")).unwrap();
        assert!(
            why.contains("curl -L -o \"/nope/models/ggml-base.en.bin\""),
            "{why}"
        );
        assert!(why.contains("mkdir -p \"/nope/models\""), "{why}");
    }

    #[test]
    fn stt_unavailable_is_satisfied_by_an_existing_binary_and_model_file() {
        assert_eq!(stt_unavailable(Some("/bin/ls"), Path::new("/bin/ls")), None);
    }

    #[test]
    fn stt_unavailable_is_optimistic_about_an_unresolved_bare_name() {
        // Mirrors binExists/whisperBin's own documented punt: a bare name
        // that this module's own PATH lookup couldn't confirm still isn't
        // treated as "definitely missing" here — a real exec's ENOENT is
        // the backstop, same as the JS original.
        assert_eq!(
            stt_unavailable(
                Some("definitely-not-a-real-binary-xyz"),
                Path::new("/bin/ls")
            ),
            None
        );
    }

    // ---- binExists() / bin_exists, modelExists() / model_exists ----

    #[test]
    fn bin_exists_checks_the_real_path_for_an_absolute_bin() {
        assert!(bin_exists("/bin/ls"));
        assert!(!bin_exists("/nope/whisper-cli"));
    }

    #[test]
    fn bin_exists_assumes_a_bare_name_is_present() {
        assert!(bin_exists("whisper-cli"));
    }

    #[test]
    fn bin_exists_rejects_empty() {
        assert!(!bin_exists(""));
    }

    #[test]
    fn model_exists_checks_the_real_path() {
        assert!(model_exists(Path::new("/bin/ls")));
        assert!(!model_exists(Path::new("/nope/model.bin")));
    }

    // ---- transcribe() — arg order, stdout collapsing, temp-file cleanup ----
    //
    // /bin/echo stands in for whisper-cli: its output IS its argv, which
    // pins the argument order without needing a model or a mic — same
    // trick test/stt.test.js's own suite uses.

    #[tokio::test]
    async fn transcribe_spawns_bin_with_expected_args_returns_collapsed_stdout_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = vec![0u8; 4];
        let out = transcribe(TranscribeRequest {
            wav: &wav,
            bin: "/bin/echo",
            model: Path::new("/m.bin"),
            temp_dir: tmp.path(),
            timeout: DEFAULT_TIMEOUT,
        })
        .await
        .unwrap();

        assert!(out.starts_with("-m /m.bin -f "), "{out}");
        assert!(out.contains("tome-stt-"), "{out}");
        assert!(out.ends_with(".wav --no-timestamps"), "{out}");
        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            0,
            "temp wav must be removed"
        );
    }

    #[tokio::test]
    async fn transcribe_cleans_up_the_temp_wav_even_when_the_spawn_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = vec![0u8; 4];
        let err = transcribe(TranscribeRequest {
            wav: &wav,
            bin: "/nope/whisper-cli",
            model: Path::new("/m.bin"),
            temp_dir: tmp.path(),
            timeout: DEFAULT_TIMEOUT,
        })
        .await
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            0,
            "temp wav must be removed"
        );
    }

    #[tokio::test]
    async fn transcribe_rejects_on_timeout_instead_of_hanging() {
        let tmp = tempfile::tempdir().unwrap();
        let slow = tmp.path().join("slow.sh");
        make_executable(&slow, "#!/bin/sh\nsleep 5\n");

        let wav = vec![0u8; 4];
        let result = transcribe(TranscribeRequest {
            wav: &wav,
            bin: slow.to_str().unwrap(),
            model: Path::new("/m.bin"),
            temp_dir: tmp.path(),
            timeout: Duration::from_millis(100),
        })
        .await;

        assert!(result.is_err());
    }

    // ---- silence_wav — backs stt_warmup, not itself a wav.js port (see
    // the module doc comment); just enough coverage to catch a header
    // mistake, not a re-run of test/wav.test.js's own full suite.

    #[test]
    fn silence_wav_produces_a_well_formed_header_and_a_silent_payload() {
        let buf = silence_wav(4, 16_000);
        assert_eq!(&buf[0..4], b"RIFF");
        assert_eq!(&buf[8..12], b"WAVE");
        assert_eq!(&buf[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes([buf[20], buf[21]]), 1); // PCM
        assert_eq!(u16::from_le_bytes([buf[22], buf[23]]), 1); // mono
        assert_eq!(
            u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]),
            16_000
        );
        assert_eq!(&buf[36..40], b"data");
        assert_eq!(buf.len(), 44 + 4 * 2);
        assert!(buf[44..].iter().all(|&b| b == 0));
    }

    #[test]
    fn warmup_silence_is_1600_samples_at_16khz() {
        assert_eq!(warmup_silence().len(), 44 + 1_600 * 2);
    }

    // ---- real whisper-cli smoke (opt-in, not run by the normal gate) ----

    /// Exercises the real binary end to end. Ignored by default: neither
    /// `whisper-cli` nor the ~140MB `ggml-base.en.bin` model is available
    /// on a fresh checkout or a CI runner. Run manually, e.g.:
    ///
    /// ```text
    /// TOME_WHISPER_BIN=/opt/homebrew/bin/whisper-cli \
    /// TOME_STT_SMOKE_MODEL="$HOME/Library/Application Support/tome/models/ggml-base.en.bin" \
    /// cargo test -p tome real_whisper_cli -- --ignored
    /// ```
    #[tokio::test]
    #[ignore = "needs a real whisper-cli binary + downloaded ggml model — see doc comment"]
    async fn real_whisper_cli_transcribes_generated_silence() {
        let Ok(model) = std::env::var("TOME_STT_SMOKE_MODEL") else {
            eprintln!("skipping: set TOME_STT_SMOKE_MODEL to a real ggml model path");
            return;
        };
        let model = PathBuf::from(model);
        let bin = whisper_bin(std::env::var("TOME_WHISPER_BIN").ok().as_deref())
            .expect("whisper_bin always returns Some");
        if let Some(why) = stt_unavailable(Some(&bin), &model) {
            eprintln!("skipping: {why}");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let wav = warmup_silence();
        let text = transcribe(TranscribeRequest {
            wav: &wav,
            bin: &bin,
            model: &model,
            temp_dir: tmp.path(),
            timeout: DEFAULT_TIMEOUT,
        })
        .await
        .expect("real whisper-cli should succeed on a valid silent WAV");
        println!("whisper-cli transcribed generated silence as: {text:?}");
    }
}
