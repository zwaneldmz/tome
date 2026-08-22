//! Document conversion read. Per the plan (§8, "Doc conversion"), mammoth
//! (.docx) and SheetJS (.xlsx) both move to the renderer — both are
//! browser-capable libraries, and running them there keeps their CVE
//! histories out of this privileged process entirely. This command's job
//! shrinks to exactly the confinement half of `src/main/index.js`'s old
//! `doc:read` handler: resolve `path` through the same symlink-safe
//! workspace/brain-vault check that handler ran — widened with the
//! store-named core-vault root, P5.4's fix, via
//! `confine::confined_real_path_in_store` — refuse any extension
//! neither browser library reads, and hand back raw bytes for the renderer
//! to parse — this file never touches mammoth/SheetJS itself, nor anything
//! resembling their logic.
//!
//! Renamed from the scaffold's `doc_read` stub to `doc_read_bytes` (and
//! `lock_gate::CHANNEL_OF_COMMAND`'s matching entry to `"doc:readBytes"`,
//! following that table's own stated mechanical `snake_case <->
//! "domain:camelCase"` convention) to name the new contract honestly: this
//! returns raw bytes, not the `{ html }` shape both the Electron handler
//! and this crate's Phase 1 stub carried.

use std::path::Path;

use tauri::{AppHandle, State};

use crate::{confine, lock_gate, state::AppState};

/// Extensions `doc_read_bytes` serves bytes for — exactly the set
/// `index.js`'s `doc:read` handler recognized before handing off to
/// mammoth/SheetJS (`.docx` -> mammoth, `.xlsx`/`.xls` -> SheetJS,
/// anything else -> `throw new Error('No viewer for ' + ext)`). Deliberately
/// NOT widened to `TOME_SERVE_EXT` (the wider allowlist the not-yet-ported
/// `tome://` protocol handler uses for images/pdf/text/source — see the
/// plan's "tome: protocol" bullet, Phase 6, not this slice): this command
/// has exactly one caller today (the renderer's docx/xlsx viewer), and a
/// generic confined "fetch me any file's bytes" primitive would be a wider
/// attack surface than index.js's own handler ever exposed. Dotted,
/// lowercase — matches `extname_lower`'s shape below.
const DOC_EXTENSIONS: &[&str] = &[".docx", ".xlsx", ".xls"];

/// `extname(path).toLowerCase()` — Node's exact shape (dot included, empty
/// string when there's no extension at all), unlike Rust's
/// `Path::extension()` (no dot, `None` rather than `""`) — so the "No
/// viewer for {ext}" error text below reads identically to the Electron
/// original's `'No viewer for ' + ext`.
fn extname_lower(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) => format!(".{}", e.to_lowercase()),
        None => String::new(),
    }
}

/// Same two-branch message `index.js`'s top-level `confinementError(what)`
/// helper built (`` `${what}: path is outside the open workspace folders`
/// `` / `` `${what}: workspace folders have not been reported yet` ``).
/// Duplicated locally rather than added to `confine.rs`, the same call
/// `ipc::shell::shell_open_path` already made (see that file's doc comment
/// for the rationale) — this is now the SECOND command needing the exact
/// text rather than the first; worth folding into `confine.rs` proper if a
/// third ever needs it.
fn confinement_error(what: &str, folders_synced: bool) -> String {
    if folders_synced {
        format!("{what}: path is outside the open workspace folders")
    } else {
        format!("{what}: workspace folders have not been reported yet")
    }
}

/// Minimal RFC 4648 standard-alphabet base64 encoder (padded). Hand-rolled
/// rather than adding a `base64` crate dependency: `base64` already sits in
/// `Cargo.lock` as a transitive dep (reqwest/keyring pull it in), but never
/// as this crate's own direct one, and the phase 5a-docs task brief's "deps
/// present" list (reqwest/notify/notify-debouncer-mini/regex/serde_json)
/// deliberately doesn't include it — touching `Cargo.toml` risks a
/// `Cargo.lock` collision with whichever parallel slice's `cargo` run
/// touches it next, for a dependency this small a win doesn't justify.
/// Renderer side decodes with the platform's own `atob` (see
/// `src/renderer/doc-convert.js`'s `base64ToBytes`) — no matching JS
/// dependency needed either.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        out.push(match b1 {
            Some(b1) => ALPHABET[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char,
            None => '=',
        });
        out.push(match b2 {
            Some(b2) => ALPHABET[(b2 & 0x3f) as usize] as char,
            None => '=',
        });
    }
    out
}

/// Ports `index.js`'s `doc:read` handler:
///
/// ```js
/// ipcMain.handle('doc:read', async (e, path) => {
///   const real = await confinedRealPath(path)
///   if (!real) throw new Error(confinementError('doc:read'))
///   path = real
///   const ext = extname(path).toLowerCase()
///   if (ext === '.docx') { const { value } = await (await loadMammoth()).convertToHtml({ path }); return { html: docCss() + value } }
///   if (ext === '.xlsx' || ext === '.xls') { ... SheetJS ... ; return { html: docCss() + parts.join('') } }
///   throw new Error('No viewer for ' + ext)
/// })
/// ```
///
/// with the parsing branches (and `docCss`) deleted — both moved to
/// `src/renderer/doc-convert.js` — and a plain confined byte read in their
/// place. `confine::confined_real_path_in_store` (the display form, which
/// additionally admits the store-named core vault — P5.4) already
/// re-resolves symlinks fresh on every call (see that function's own
/// TOCTOU doc comment), so nothing extra is needed here for the
/// "resolve, don't trust a cached realpath" property the original had.
#[tauri::command]
pub async fn doc_read_bytes(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "doc:readBytes")?;
    let real =
        confine::confined_real_path_in_store(&app, &state, Path::new(&path)).map_err(|_| {
            let synced = *state
                .folders_synced
                .read()
                .expect("doc_read_bytes: AppState.folders_synced lock poisoned");
            confinement_error("doc:readBytes", synced)
        })?;
    let ext = extname_lower(&real);
    if !DOC_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!("No viewer for {ext}"));
    }
    // std::fs::read on a blocking-pool thread, not tokio::fs::read directly
    // — matches this crate's established convention for a sync fs call made
    // from inside an async command (see ipc::store::store_get/store_set).
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(&real))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "base64": base64_encode(&bytes) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- base64_encode — RFC 4648 §10 test vectors, verbatim ----

    #[test]
    fn base64_encode_matches_rfc_4648_test_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encode_round_trips_every_byte_value() {
        let bytes: Vec<u8> = (0..=255).collect();
        let encoded = base64_encode(&bytes);
        // Decoded with a second, independently-written decoder rather than
        // a hardcoded 344-char expected string — a shared bug in both this
        // function and a copy-pasted decoder would otherwise "round-trip"
        // undetected.
        assert_eq!(decode_for_test(&encoded), bytes);
    }

    fn decode_for_test(s: &str) -> Vec<u8> {
        fn val(c: u8) -> u32 {
            match c {
                b'A'..=b'Z' => (c - b'A') as u32,
                b'a'..=b'z' => (c - b'a' + 26) as u32,
                b'0'..=b'9' => (c - b'0' + 52) as u32,
                b'+' => 62,
                b'/' => 63,
                _ => 0,
            }
        }
        let clean: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
        let mut out = Vec::new();
        for chunk in clean.chunks(4) {
            let mut n = 0u32;
            for (i, &c) in chunk.iter().enumerate() {
                n |= val(c) << (18 - 6 * i);
            }
            out.push((n >> 16) as u8);
            if chunk.len() > 2 {
                out.push((n >> 8) as u8);
            }
            if chunk.len() > 3 {
                out.push(n as u8);
            }
        }
        out
    }

    // ---- extname_lower — Node's extname().toLowerCase() shape ----

    #[test]
    fn extname_lower_includes_the_dot_and_lowercases() {
        assert_eq!(extname_lower(Path::new("/a/b/Report.DOCX")), ".docx");
        assert_eq!(extname_lower(Path::new("/a/b/data.xlsx")), ".xlsx");
    }

    #[test]
    fn extname_lower_is_empty_for_no_extension() {
        assert_eq!(extname_lower(Path::new("/a/b/Makefile")), "");
    }

    #[test]
    fn extname_lower_takes_the_last_segment_of_a_compound_extension() {
        assert_eq!(extname_lower(Path::new("archive.tar.gz")), ".gz");
    }

    // ---- DOC_EXTENSIONS ----

    #[test]
    fn doc_extensions_covers_exactly_what_the_original_handler_served() {
        assert!(DOC_EXTENSIONS.contains(&".docx"));
        assert!(DOC_EXTENSIONS.contains(&".xlsx"));
        assert!(DOC_EXTENSIONS.contains(&".xls"));
        assert_eq!(DOC_EXTENSIONS.len(), 3);
    }

    // ---- confinement_error ----

    #[test]
    fn confinement_error_distinguishes_not_synced_from_outside() {
        assert_eq!(
            confinement_error("doc:readBytes", true),
            "doc:readBytes: path is outside the open workspace folders"
        );
        assert_eq!(
            confinement_error("doc:readBytes", false),
            "doc:readBytes: workspace folders have not been reported yet"
        );
    }
}
