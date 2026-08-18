//! `tome://` custom protocol — serves confined, extension-allowlisted file
//! bytes to the sandboxed document-viewer iframe the doc panel opens
//! (`src/renderer/panels/doc.js:14`: `'tome://local/?p=' +
//! encodeURIComponent(path)`, for `mode: 'pdf'`/`mode: 'img'`).
//!
//! Ports two things from `src/main/index.js`:
//!
//! 1. The privileged scheme registration (~line 258):
//!    ```js
//!    protocol.registerSchemesAsPrivileged([
//!      { scheme: 'tome', privileges: { standard: true, secure: true, stream: true } },
//!    ])
//!    ```
//!    Tauri v2 has no per-scheme privilege-flag config to port: a scheme
//!    registered via [`tauri::Builder::register_asynchronous_uri_scheme_protocol`]
//!    (called from `lib.rs::run()`) is simply available, globally, to every
//!    webview — the Electron `standard`/`secure` flags (relative-URL/
//!    secure-context treatment) have no Tauri config-level equivalent to
//!    set, and `stream` (Electron's opt-in for `ReadableStream` response
//!    bodies / HTTP Range support) has nothing to port TO: this handler,
//!    like `ipc::doc::doc_read_bytes`, reads a whole file into one
//!    in-memory `Vec<u8>` response and does not implement Range/206
//!    partial-content handling. `tauri.conf.json` needed no new scheme
//!    declaration either — confirmed by reading `tauri` 2.11.5's own
//!    config schema (`tauri-utils::config`): the only scheme-shaped config
//!    key that exists (`DeepLinkProtocol::schemes`) is the OS-level
//!    "open this app when a `my-app://` link is clicked externally"
//!    registration for the (unused-here) deep-link plugin, unrelated to an
//!    in-webview resource-loading protocol like this one.
//!
//! 2. The handler itself (~line 528):
//!    ```js
//!    protocol.handle('tome', async (req) => {
//!      const p = decodeURIComponent(new URL(req.url).searchParams.get('p') || '')
//!      const deny = () => new Response(confinementError('tome'), { status: 403 })
//!      const ext = extname(p).slice(1).toLowerCase()
//!      if (!TOME_SERVE_EXT.has(ext)) return deny()
//!      const real = await confinedRealPath(p)
//!      if (!real) return deny()
//!      return net.fetch(pathToFileURL(real).toString())
//!    })
//!    ```
//!    Same two-gate order (cheap extension check before the filesystem-
//!    touching confinement check), same confinement primitive
//!    (`crate::confine::confined_real_path`, already real and tested —
//!    Phase 3's `confine.rs`), same `TOME_SERVE_EXT` allowlist (verbatim,
//!    below), same 403-and-a-short-body denial shape.
//!
//! # Security invariant this handler is half of (read before touching)
//!
//! This scheme was the subject of a real finding (`reviews/kimi-k3-review.
//! txt`, `reviews/pi-review.md`): an unconfined `tome://` handler + `img-
//! src`/`frame-src`/`default-src` CSP entries is a display primitive, but
//! becomes a full read-any-file exfiltration primitive the moment `tome:`
//! is ever added to `connect-src` (which would let renderer `fetch()`/XHR
//! read response *bytes*, not just render them). The fix has two halves,
//! split across two files, and BOTH must hold:
//!
//! - **This file**: confine to open workspace folders/brain vaults +
//!   `TOME_SERVE_EXT` allowlist, so even a CSP mistake can only leak an
//!   already-open-workspace file of an allowlisted (already-would-be-
//!   displayed) type, not arbitrary filesystem contents.
//! - **`src/renderer/index.html`'s CSP meta tag**: `connect-src` must keep
//!   omitting `tome:` (verified present as of this port: `connect-src
//!   'self' ws: wss: ipc: http://ipc.localhost` — no `tome:`), so renderer
//!   JS structurally cannot `fetch()`/read `tome://` response bytes even
//!   though `default-src`/`img-src`/`frame-src` all allow `tome:` for
//!   embedding. Do not add `tome:` to `connect-src`.
//!
//! Neither half alone is the fix — this file's confinement is defense in
//! depth for the day the CSP is loosened by mistake; the CSP is defense in
//! depth for the day this file's confinement has a bug. Preserve both.
//!
//! # One deliberate behavioral deviation from the JS source
//!
//! The JS handler calls `decodeURIComponent` TWICE on the `p` value: once
//! implicitly inside `URLSearchParams.get()` (which percent-decodes per
//! the WHATWG URL spec while parsing the query string), and once more
//! explicitly on the already-decoded result. For every request this
//! handler's only caller (`doc.js`) actually sends — `encodeURIComponent`
//! never emits a literal `%` in its output, so there is nothing left for
//! the second pass to decode — the second call is a no-op. For a
//! (hypothetical, never-sent-by-this-caller) path containing a literal `%`
//! followed by non-hex characters, that redundant second pass would
//! instead THROW (`URIError: URI malformed`), turning a should-be-clean
//! 403 into an unhandled rejection. This port decodes once — matching
//! [`extract_path_param`]/[`percent_decode`] below to `URLSearchParams`'s
//! own (lenient, never-throwing) percent-decode semantics exactly, single
//! pass — which is both the behavior every real caller observes today and
//! strictly safer than replicating the redundant second pass.
//!
//! # Reused, not reimplemented
//!
//! `confine::is_confined` was widened from private to `pub(crate)`
//! (behavior unchanged) so this module's own tests can exercise the exact
//! same lexical confinement decision `confined_real_path` makes — see this
//! file's `confinement_denies_a_dotdot_escape_outside_open_folders` test —
//! rather than re-deriving a parallel copy of that logic just to test it.
//! The realpath/symlink-following half (`confine::confined_real_path`
//! itself, called from [`build_response`] below) is exercised end-to-end
//! only by a running app (needs live `AppState`/filesystem) — boot-
//! verified, not unit-tested here; its own symlink-escape scenarios are
//! already covered by `confine.rs`'s test suite, which this handler calls
//! into rather than duplicates.

use std::path::Path;

use tauri::{http, AppHandle, Manager, State, UriSchemeContext, UriSchemeResponder};

use crate::{confine, state::AppState};

/// `index.js`'s `TOME_SERVE_EXT` (~line 268), verbatim: displayable content
/// only — images, pdf, plain text/markdown, and common source files.
/// Deliberately NOT the wider or narrower set `ipc::doc::DOC_EXTENSIONS`
/// uses (docx/xlsx/xls, mammoth/SheetJS's targets) — that command has one
/// caller (the docx/xlsx viewer) and a different confinement contract; see
/// that file's own doc comment for why the two allowlists don't merge.
const TOME_SERVE_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "avif", "pdf", "md", "markdown",
    "txt", "json", "js", "mjs", "cjs", "ts", "tsx", "jsx", "css", "html", "py", "rb", "go", "rs",
    "c", "h", "cpp", "java", "sh", "yml", "yaml", "toml", "xml", "csv",
];

/// Same two-branch message `index.js`'s top-level `confinementError(what)`
/// helper builds. A third local copy of the exact helper `ipc::doc`'s and
/// `ipc::shell`'s own doc comments already carry (both note this scheme
/// handler would want it too) — left as its own copy rather than centralized
/// into `confine.rs`, same call the two earlier copies made, to keep this
/// slice's diff to the files it owns.
fn confinement_error(what: &str, folders_synced: bool) -> String {
    if folders_synced {
        format!("{what}: path is outside the open workspace folders")
    } else {
        format!("{what}: workspace folders have not been reported yet")
    }
}

/// Minimal RFC 3986 percent-decoder + `application/x-www-form-urlencoded`
/// `+`-as-space, matching what `URLSearchParams.get()` actually does while
/// parsing a query string (see this module's top doc comment for why this
/// single pass — not JS's own redundant extra `decodeURIComponent` call —
/// is what gets ported). Deliberately infallible/lenient like the spec
/// algorithm it mirrors: a malformed `%` escape (truncated, or not
/// followed by two hex digits) is passed through as a literal `%` rather
/// than erroring, and the resulting bytes are interpreted as UTF-8 lossily
/// (invalid sequences become U+FFFD) rather than failing — a decoded
/// string that happens to contain replacement characters never names
/// a real file, and falls through to a 403 via the ordinary
/// extension/confinement gates below, no special-casing needed.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => match s
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                Some(byte) => {
                    out.push(byte);
                    i += 3;
                }
                None => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `decodeURIComponent(new URL(req.url).searchParams.get('p') || '')`,
/// ported (see the module doc comment for the one deliberate deviation).
/// Ignores everything about `uri` except its query string — same as the
/// JS original, which never looks at `req.url`'s host (`doc.js` always
/// sends `local`, but nothing here or there depends on that value).
/// Returns `""` when `p` is absent, exactly like JS's `|| ''` fallback
/// (which also catches the "present but empty" case, since `'' || ''` is
/// `''` too — this function collapses both cases the same way, needing no
/// separate `Option` layer).
fn extract_path_param(uri: &http::Uri) -> String {
    let Some(query) = uri.query() else {
        return String::new();
    };
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        if parts.next() != Some("p") {
            continue;
        }
        return percent_decode(parts.next().unwrap_or(""));
    }
    String::new()
}

/// `extname(p).slice(1).toLowerCase()` — Node's shape minus the leading
/// dot (unlike `ipc::doc::extname_lower`, which keeps it; `TOME_SERVE_EXT`'s
/// own entries are dotless, so this matches its shape instead). Operates
/// purely on the string `path` — no filesystem access, same as Node's
/// `path.extname` — so it can run on the raw, not-yet-confined query value
/// (the gate order below needs exactly that, same as the JS original).
fn ext_lower_no_dot(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default()
}

/// Content-Type for a byte response, inferred from an (already-allowlisted)
/// extension — this module's replacement for what Chromium's `net.fetch`
/// infers automatically when the JS original calls
/// `net.fetch(pathToFileURL(real).toString())`. [`build_response`] below
/// calls this with `real`'s own extension (the resolved, post-symlink
/// path), not the original query value's — matching `net.fetch`, which
/// infers from the file it actually opens. Those two can differ: a
/// `photo.png` symlink whose target is confined but happens to be named
/// for example `cache.bin` passes the allowlist gate on the link name (`png`) but
/// would be served with whatever `mime_for_ext("bin")` (that is the catch-all
/// below) returns for the target — matching the original's own behavior
/// in that same edge case, not a new decision made here.
///
/// Every extension with a well-known, browser/IANA-recognized type gets
/// it; everything else `TOME_SERVE_EXT` allows (plain text and every
/// source/config language it lists — none of which has a standard MIME
/// type of its own) falls back to `text/plain`, which every engine renders
/// inline rather than prompting a download — all a preview iframe needs.
fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/vnd.microsoft.icon",
        "avif" => "image/avif",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    }
}

/// A denied request: same status for every denial reason (extension not
/// allowlisted, confinement refused, or — beyond what the JS original
/// distinguished — the confined file failed to read), same body shape
/// (`confinementError('tome')`'s message), matching `index.js`'s single
/// shared `deny()` closure. Nothing on the renderer side ever reads this
/// body (the response only ever backs an `<img>`/`<iframe src>` load, not
/// a `fetch()` — see the module doc comment's CSP note), so its exact text
/// is a debugging aid, not a contract.
fn deny_response<R: tauri::Runtime>(app: &AppHandle<R>) -> http::Response<Vec<u8>> {
    let state = app.state::<AppState>();
    let synced = *state
        .folders_synced
        .read()
        .expect("protocol::deny_response: AppState.folders_synced lock poisoned");
    let body = confinement_error("tome", synced).into_bytes();
    http::Response::builder()
        .status(http::StatusCode::FORBIDDEN)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body)
        .unwrap_or_else(|_| http::Response::new(Vec::new()))
}

/// The actual gate-then-serve decision, ported from `protocol.handle`'s
/// body (see module doc comment). Async because both steps below can
/// block: `confine::confined_real_path` does a `realpath(3)` syscall,
/// and the file read is a real disk read — spawned onto the blocking pool
/// exactly the way `ipc::doc::doc_read_bytes` already spawns its own
/// `std::fs::read`, not called directly on whatever thread invoked this
/// handler (see [`handle`]'s doc comment for what thread that is).
async fn build_response<R: tauri::Runtime>(
    app: &AppHandle<R>,
    uri: &http::Uri,
) -> http::Response<Vec<u8>> {
    let raw_path = extract_path_param(uri);
    // Gate 1: extension allowlist, on the RAW (pre-confinement) path —
    // same order as the JS original, cheapest check first.
    if !TOME_SERVE_EXT.contains(&ext_lower_no_dot(&raw_path).as_str()) {
        return deny_response(app);
    }
    // Gate 2: confinement — open workspace folders + brain vaults,
    // symlink-resolved. Reused, not reimplemented; see module doc comment.
    let state: State<'_, AppState> = app.state::<AppState>();
    let real = match confine::confined_real_path(&state, Path::new(&raw_path)) {
        Ok(p) => p,
        Err(_) => return deny_response(app),
    };
    let bytes = {
        let real = real.clone();
        match tokio::task::spawn_blocking(move || std::fs::read(&real)).await {
            Ok(Ok(bytes)) => bytes,
            _ => return deny_response(app),
        }
    };
    // MIME from the RESOLVED path's own extension, not the query value's
    // — see mime_for_ext's doc comment for why those can differ.
    let mime = mime_for_ext(&ext_lower_no_dot(&real.to_string_lossy()));
    http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, mime)
        .body(bytes)
        .unwrap_or_else(|_| deny_response(app))
}

/// Entry point registered with
/// [`tauri::Builder::register_asynchronous_uri_scheme_protocol`] in
/// `lib.rs::run()`. Runs on whatever thread wry's platform backend calls a
/// registered async protocol handler on (its own docs: "here you can use a
/// tokio task, thread pool or anything… for example downloading files" —
/// `UriSchemeResponder`/`RequestAsyncResponder` is explicitly `Send` for
/// exactly this) — so, like every other async entry point in this crate,
/// the real work happens inside a spawned task, and this function itself
/// only extracts the `'static` pieces it needs (an owned `AppHandle` clone;
/// `request` is already fully owned, no borrow of `ctx` survives past this
/// call) before handing off.
pub fn handle<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: http::Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let response = build_response(&app, request.uri()).await;
        responder.respond(response);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;

    // ---- TOME_SERVE_EXT — extension allowlist decision ----

    #[test]
    fn tome_serve_ext_matches_index_js_exactly() {
        // Pinned to index.js's TOME_SERVE_EXT (~line 268) verbatim — any
        // addition or removal here must be a deliberate, reviewed edit,
        // same discipline lock_gate::tests applies to OPEN_ON/OPEN_CHANNELS.
        let expected: HashSet<&str> = [
            "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "avif", "pdf", "md",
            "markdown", "txt", "json", "js", "mjs", "cjs", "ts", "tsx", "jsx", "css", "html", "py",
            "rb", "go", "rs", "c", "h", "cpp", "java", "sh", "yml", "yaml", "toml", "xml", "csv",
        ]
        .into_iter()
        .collect();
        let actual: HashSet<&str> = TOME_SERVE_EXT.iter().copied().collect();
        assert_eq!(actual, expected);
        assert_eq!(
            TOME_SERVE_EXT.len(),
            expected.len(),
            "TOME_SERVE_EXT has a duplicate entry"
        );
    }

    #[test]
    fn a_disallowed_extension_is_rejected() {
        for ext in ["exe", "dll", "sh.bak", "docx", "xlsx", "zip", "dmg", ""] {
            assert!(
                !TOME_SERVE_EXT.contains(&ext),
                "{ext} must not be servable via tome://"
            );
        }
    }

    // ---- ext_lower_no_dot — Node's extname().slice(1).toLowerCase() shape ----

    #[test]
    fn ext_lower_no_dot_lowercases_and_drops_the_dot() {
        assert_eq!(ext_lower_no_dot("/a/b/Photo.PNG"), "png");
        assert_eq!(ext_lower_no_dot("/a/b/notes.MD"), "md");
    }

    #[test]
    fn ext_lower_no_dot_takes_the_last_segment_of_a_compound_extension() {
        assert_eq!(ext_lower_no_dot("archive.tar.gz"), "gz");
    }

    #[test]
    fn ext_lower_no_dot_is_empty_for_no_extension_a_dotfile_or_an_empty_path() {
        assert_eq!(ext_lower_no_dot("/a/b/Makefile"), "");
        assert_eq!(ext_lower_no_dot("/a/b/.gitignore"), "");
        assert_eq!(ext_lower_no_dot(""), "");
    }

    // ---- percent_decode / extract_path_param — query parsing ----

    #[test]
    fn percent_decode_decodes_percent_escapes() {
        assert_eq!(
            percent_decode("%2FUsers%2Ffoo%2Fbar.png"),
            "/Users/foo/bar.png"
        );
    }

    #[test]
    fn percent_decode_treats_plus_as_space_like_url_search_params() {
        assert_eq!(percent_decode("my+file.txt"), "my file.txt");
    }

    #[test]
    fn percent_decode_is_lenient_on_a_malformed_escape_not_panicking_or_erroring() {
        // decodeURIComponent would THROW on these; URLSearchParams's own
        // decode (what this mirrors — see module doc comment) does not.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("100%zz"), "100%zz");
        assert_eq!(percent_decode("%"), "%");
    }

    #[test]
    fn percent_decode_round_trips_ascii_and_multibyte_utf8() {
        // Every byte of a valid UTF-8 string, individually %XX-escaped (a
        // strictly harder input than `encodeURIComponent` actually
        // produces, which leaves a small unreserved set un-escaped) — the
        // reconstructed bytes are valid UTF-8 by construction, so
        // `from_utf8_lossy` must hand them back unchanged, byte for byte.
        let original = "hello /世界 — café 100% done";
        let encoded: String = original.bytes().map(|b| format!("%{b:02X}")).collect();
        assert_eq!(percent_decode(&encoded), original);
    }

    #[test]
    fn percent_decode_replaces_invalid_utf8_with_u_fffd_instead_of_failing() {
        // 0xFF is never a valid UTF-8 lead byte — this is the lossy half
        // of the lenient, never-panics/never-errors contract this function
        // documents (see its own doc comment): a decoded value that can't
        // name a real file is safe to let through as U+FFFD, because the
        // ordinary extension/confinement gates deny it downstream anyway.
        assert_eq!(percent_decode("%FF"), "\u{FFFD}");
    }

    #[test]
    fn extract_path_param_decodes_the_p_query_parameter() {
        let uri: http::Uri = "tome://local/?p=%2FUsers%2Ffoo%2Fbar.png".parse().unwrap();
        assert_eq!(extract_path_param(&uri), "/Users/foo/bar.png");
    }

    #[test]
    fn extract_path_param_defaults_to_empty_when_p_is_absent() {
        assert_eq!(extract_path_param(&"tome://local/".parse().unwrap()), "");
        assert_eq!(
            extract_path_param(&"tome://local/?other=1".parse().unwrap()),
            ""
        );
    }

    #[test]
    fn extract_path_param_takes_the_first_p_when_duplicated() {
        let uri: http::Uri = "tome://local/?p=%2Fa&p=%2Fb".parse().unwrap();
        assert_eq!(extract_path_param(&uri), "/a");
    }

    #[test]
    fn extract_path_param_ignores_the_host_same_as_the_js_original() {
        // doc.js always sends host "local", but neither this nor the JS
        // original's `new URL(req.url).searchParams` ever inspects it.
        let uri: http::Uri = "tome://anything-here/?p=%2Fx.png".parse().unwrap();
        assert_eq!(extract_path_param(&uri), "/x.png");
    }

    // ---- mime_for_ext — MIME mapping ----

    #[test]
    fn mime_for_ext_covers_every_allowlisted_extension_with_a_non_empty_type() {
        for ext in TOME_SERVE_EXT {
            assert!(
                !mime_for_ext(ext).is_empty(),
                "{ext} has no mapped MIME type"
            );
        }
    }

    #[test]
    fn mime_for_ext_pins_the_display_critical_types() {
        assert_eq!(mime_for_ext("png"), "image/png");
        assert_eq!(mime_for_ext("jpg"), "image/jpeg");
        assert_eq!(mime_for_ext("jpeg"), "image/jpeg");
        assert_eq!(mime_for_ext("svg"), "image/svg+xml");
        assert_eq!(mime_for_ext("pdf"), "application/pdf");
        assert_eq!(mime_for_ext("json"), "application/json");
    }

    #[test]
    fn mime_for_ext_falls_back_to_text_plain_for_source_and_config_files() {
        for ext in [
            "txt", "rs", "py", "go", "toml", "yml", "yaml", "ts", "tsx", "jsx", "sh", "c", "h",
            "cpp", "java", "rb",
        ] {
            assert_eq!(mime_for_ext(ext), "text/plain; charset=utf-8", "{ext}");
        }
    }

    #[test]
    fn mime_for_ext_defaults_unknown_extensions_to_text_plain_not_something_executable() {
        // Defense in depth for the symlink-divergent-extension case this
        // function's own doc comment describes: whatever comes out must
        // never be an executable/script-triggering type.
        assert_eq!(mime_for_ext("bin"), "text/plain; charset=utf-8");
        assert_eq!(mime_for_ext(""), "text/plain; charset=utf-8");
    }

    // ---- confinement rejection — the real decision, reused from
    // confine::is_confined (pub(crate) specifically for this — see module
    // doc comment), not a reimplementation ----

    #[test]
    fn confinement_denies_a_dotdot_escape_outside_open_folders() {
        let folders = vec![PathBuf::from("/workspace/proj")];
        assert!(!confine::is_confined(
            &folders,
            true,
            Path::new("/workspace/proj/../../etc/passwd"),
        ));
    }

    #[test]
    fn confinement_denies_an_absolute_path_outside_any_open_folder() {
        let folders = vec![PathBuf::from("/workspace/proj")];
        assert!(!confine::is_confined(
            &folders,
            true,
            Path::new("/etc/passwd")
        ));
    }

    #[test]
    fn confinement_allows_a_real_child_of_an_open_folder() {
        let folders = vec![PathBuf::from("/workspace/proj")];
        assert!(confine::is_confined(
            &folders,
            true,
            Path::new("/workspace/proj/src/main.rs"),
        ));
    }

    #[test]
    fn confinement_denies_everything_until_folders_synced() {
        let folders = vec![PathBuf::from("/workspace/proj")];
        assert!(!confine::is_confined(
            &folders,
            false,
            Path::new("/workspace/proj/f.png")
        ));
    }

    // ---- confinement_error ----

    #[test]
    fn confinement_error_distinguishes_not_synced_from_outside() {
        assert_eq!(
            confinement_error("tome", true),
            "tome: path is outside the open workspace folders"
        );
        assert_eq!(
            confinement_error("tome", false),
            "tome: workspace folders have not been reported yet"
        );
    }
}
