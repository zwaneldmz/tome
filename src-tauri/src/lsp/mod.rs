//! Language servers. One process per (workspace root, server), spawned
//! lazily the first time a matching file is opened and reused for every
//! file after. Ports `src/main/lsp.js`'s `Server` class and module-level
//! pool as-is: `tokio::process`, hand-rolled Content-Length framing,
//! untyped `serde_json::Value` (skip `lsp-types`), the same 7 servers,
//! `lsp:missing` pushed once per absent binary. [`policy`] ports the
//! sibling `lib/lsp-policy.js` (root confinement + spawn-env policy,
//! TOME-003).
//!
//! Deliberately small: this speaks just enough LSP for what the editor
//! pane shows — diagnostics, hover, go-to-definition — with full-text
//! document sync rather than incremental.
//!
//! Servers are never bundled. If the binary is not on PATH the language
//! simply has no diagnostics, which is reported once (see
//! [`should_report_missing`]) and then left alone — a missing optional
//! tool must not nag on every keystroke.
//!
//! ## What's unit-tested directly, and what isn't
//!
//! Every pure decision this module makes is split out and covered by
//! `#[cfg(test)]` below without touching a real child process:
//! [`language_id_for`]/[`server_for`] (the 7-server table + extension
//! map), [`FrameParser`] (Content-Length framing, including split/partial
//! reads), [`extract_hover_text`]/[`extract_definition`] (response
//! shaping), [`advance_doc_open`]/[`advance_doc_change`]/
//! [`advance_doc_close`] (document version bookkeeping), and
//! [`should_report_missing`] (the once-per-absent-binary dedup).
//! [`spawn_child`] is exercised directly too — spawning a command that
//! genuinely does not exist on `PATH` is a real, deterministic OS-level
//! failure, no mock needed.
//!
//! What ISN'T unit-tested directly: [`Server::spawn_and_init`] and the
//! public pool entry points ([`did_open`]/[`hover`]/etc.), because they
//! need a live `AppHandle` to emit `lsp:diagnostics`/`lsp:missing` — this
//! crate has no `AppHandle`-mocking dependency anywhere yet (checked: no
//! `tauri::test`/`MockRuntime` usage exists in this tree), same documented
//! boundary `brain.rs`/`fs.rs`/`events.rs` already draw around their own
//! `AppHandle`-touching entry points.

pub mod policy;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

/// Default per-request timeout — `timeoutMs = 15000` in `lsp.js`'s
/// `Server.request`.
const REQUEST_TIMEOUT: Duration = Duration::from_millis(15000);

// ==================== the 7-server policy table ====================

/// One `SERVERS` entry — ports `lsp.js`'s array literal exactly: id, the
/// language ids it's registered for, and the stdio invocation.
pub struct ServerSpec {
    pub id: &'static str,
    pub langs: &'static [&'static str],
    pub cmd: &'static str,
    pub args: &'static [&'static str],
}

/// Verbatim port of `lsp.js`'s `SERVERS` array — 7 entries, checked
/// against `src/main/lsp.js` line for line. Order matters for nothing
/// (lookup is by `id`/`langs`, never by index), but is preserved anyway.
pub const SERVERS: &[ServerSpec] = &[
    ServerSpec {
        id: "typescript",
        langs: &[
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
        ],
        cmd: "typescript-language-server",
        args: &["--stdio"],
    },
    ServerSpec {
        id: "python",
        langs: &["python"],
        cmd: "pyright-langserver",
        args: &["--stdio"],
    },
    ServerSpec {
        id: "go",
        langs: &["go"],
        cmd: "gopls",
        args: &[],
    },
    ServerSpec {
        id: "rust",
        langs: &["rust"],
        cmd: "rust-analyzer",
        args: &[],
    },
    ServerSpec {
        id: "json",
        langs: &["json"],
        cmd: "vscode-json-language-server",
        args: &["--stdio"],
    },
    ServerSpec {
        id: "css",
        langs: &["css", "scss", "less"],
        cmd: "vscode-css-language-server",
        args: &["--stdio"],
    },
    ServerSpec {
        id: "html",
        langs: &["html"],
        cmd: "vscode-html-language-server",
        args: &["--stdio"],
    },
];

/// Ports `LANG_BY_EXT` verbatim (18 entries).
const LANG_BY_EXT: &[(&str, &str)] = &[
    ("ts", "typescript"),
    ("tsx", "typescriptreact"),
    ("mts", "typescript"),
    ("cts", "typescript"),
    ("js", "javascript"),
    ("jsx", "javascriptreact"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("py", "python"),
    ("pyi", "python"),
    ("go", "go"),
    ("rs", "rust"),
    ("json", "json"),
    ("jsonc", "json"),
    ("css", "css"),
    ("scss", "scss"),
    ("less", "less"),
    ("html", "html"),
    ("htm", "html"),
];

/// Ports `languageIdFor(path)`: `path.split('.').pop()?.toLowerCase()`,
/// then a table lookup, `null` for no/unknown extension. `path.split('.')`
/// on a path with no dot at all still yields the whole string as its sole
/// element (`.pop()` returns it) — matched here by `rsplit('.').next()`,
/// which has the same no-separator behavior; an empty result (a path
/// ending in `.`, for example `"file."`) is falsy in the JS original (`ext &&
/// ...`) and is handled the same way here.
pub fn language_id_for(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if ext.is_empty() {
        return None;
    }
    LANG_BY_EXT.iter().find(|(k, _)| *k == ext).map(|(_, v)| *v)
}

/// Ports `serverFor(langId)`: first (only, in practice — no `langs` array
/// overlaps another's) `SERVERS` entry whose `langs` contains `langId`.
fn server_for(lang_id: &str) -> Option<&'static ServerSpec> {
    SERVERS.iter().find(|s| s.langs.contains(&lang_id))
}

// ==================== file:// URI helpers ====================

/// Hand-rolled `file://` URI encoder for the POSIX-only paths this app's
/// two shipping targets (macOS + Linux — no Windows drive-letter handling)
/// ever produce. Not a general WHATWG URL implementation — ports the
/// *effect* of `pathToFileURL(path).href` for plain absolute paths, which
/// is all `lsp.js` ever feeds it (every caller gates through
/// [`policy::confine_to_root`] first, which only ever returns an absolute
/// root-derived path).
fn uri_of(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::from("file://");
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Inverse of [`uri_of`] — ports `pathOf(uri)`'s `try { fileURLToPath(uri)
/// } catch { return null }`: `None` for anything not a `file://` URI, or
/// whose percent-encoding doesn't decode to valid UTF-8.
fn path_of(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    percent_decode(rest).map(PathBuf::from)
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

// ==================== Content-Length framing ====================

/// Incremental Content-Length frame extractor — the pure core of `Server`'s
/// `onData(chunk)` in the JS original, minus the dispatch call itself, so
/// partial/split reads are testable without a real pipe. `push` may be
/// called with arbitrarily-sized chunks (including a chunk that splits a
/// header or a body in half); it returns every message that became
/// complete as a result of this call, in arrival order.
#[derive(Default)]
struct FrameParser {
    buf: Vec<u8>,
}

impl FrameParser {
    fn push(&mut self, bytes: &[u8]) -> Vec<Value> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(split) = find_subslice(&self.buf, b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&self.buf[..split]).into_owned();
            let Some(len) = parse_content_length(&header) else {
                // unparseable header, skip it — matches `this.buf =
                // this.buf.subarray(split + 4)` in the JS original.
                self.buf.drain(..split + 4);
                continue;
            };
            let start = split + 4;
            if self.buf.len() < start + len {
                break; // wait for the rest
            }
            let body = self.buf[start..start + len].to_vec();
            self.buf.drain(..start + len);
            if let Ok(msg) = serde_json::from_slice::<Value>(&body) {
                out.push(msg);
            }
            // an unparseable JSON body is silently dropped, matching the
            // JS original's `catch { continue }`.
        }
        out
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Ports `/content-length:\s*(\d+)/i.exec(header)` — a case-insensitive
/// search for `content-length:` anywhere in the header block (which may
/// hold other header lines too), not a strict single-line parse.
fn parse_content_length(header: &str) -> Option<usize> {
    let lower = header.to_ascii_lowercase();
    let idx = lower.find("content-length:")?;
    let after = &header[idx + "content-length:".len()..];
    let digits: String = after
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

// ==================== response shaping ====================

/// Ports `hover()`'s result-shaping block exactly: `res?.contents` may be
/// a bare string, a `MarkupContent`-ish `{ value }` object, or an array of
/// either — joined with `\n` when an array. `None` for missing/null
/// contents or an all-whitespace result (`text?.trim() || null`).
fn extract_hover_text(result: &Value) -> Option<String> {
    let contents = result.get("contents")?;
    if contents.is_null() {
        return None;
    }
    let text = if let Some(arr) = contents.as_array() {
        arr.iter()
            .map(|x| {
                x.as_str().map(str::to_string).unwrap_or_else(|| {
                    x.get("value")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                })
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else if let Some(s) = contents.as_str() {
        s.to_string()
    } else {
        contents
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// A resolved go-to-definition target — ports the `{ path, line, character
/// }` object `definition()` resolves to.
#[derive(Debug, Clone, PartialEq)]
pub struct DefinitionLocation {
    pub path: PathBuf,
    pub line: u64,
    pub character: u64,
}

/// Ports `definition()`'s result-shaping block: `res` may be a bare
/// `Location`/`LocationLink` or an array of them (first element wins,
/// `None` for an empty array); a `Location` carries `uri`/`range`, a
/// `LocationLink` carries `targetUri`/(`targetSelectionRange` preferred
/// over `targetRange`) — `first.uri || first.targetUri` and
/// `first.range || first.targetSelectionRange || first.targetRange` port
/// directly to a fallback chain.
fn extract_definition(result: &Value) -> Option<DefinitionLocation> {
    let first: &Value = if let Some(arr) = result.as_array() {
        arr.first()?
    } else {
        result
    };
    if first.is_null() {
        return None;
    }
    let uri = first
        .get("uri")
        .and_then(Value::as_str)
        .or_else(|| first.get("targetUri").and_then(Value::as_str))?;
    let range = first
        .get("range")
        .or_else(|| first.get("targetSelectionRange"))
        .or_else(|| first.get("targetRange"))?;
    let target = path_of(uri)?;
    let start = range.get("start")?;
    let line = start.get("line").and_then(Value::as_u64)?;
    let character = start.get("character").and_then(Value::as_u64)?;
    Some(DefinitionLocation {
        path: target,
        line,
        character,
    })
}

// ==================== document version bookkeeping ====================

/// The two things a `didOpen` call can turn into — ports the JS
/// original's `if (this.docs.has(path)) return this.didChange(path,
/// text)` branch inside `didOpen` itself.
enum OpenTransition {
    FreshOpen(u64),
    TreatedAsChange(u64),
}

/// Ports the version-bookkeeping half of `Server.didOpen` — pure `HashMap`
/// arithmetic, split out from the I/O (the `notify` call) so it's testable
/// without a live server.
fn advance_doc_open(docs: &mut HashMap<String, u64>, path: &str) -> OpenTransition {
    if docs.contains_key(path) {
        return OpenTransition::TreatedAsChange(advance_doc_change(docs, path));
    }
    docs.insert(path.to_string(), 1);
    OpenTransition::FreshOpen(1)
}

/// Ports `Server.didChange`'s version bump: `(this.docs.get(path) || 1) +
/// 1` — an unopened path still produces version 2, matching the JS
/// original's `|| 1` default rather than panicking or starting at 1.
fn advance_doc_change(docs: &mut HashMap<String, u64>, path: &str) -> u64 {
    let v = docs.get(path).copied().unwrap_or(1) + 1;
    docs.insert(path.to_string(), v);
    v
}

/// Ports `Server.didClose`'s `if (!this.docs.delete(path)) return` —
/// `true` iff the path was actually tracked (and is now removed).
fn advance_doc_close(docs: &mut HashMap<String, u64>, path: &str) -> bool {
    docs.remove(path).is_some()
}

// ==================== missing-binary bookkeeping ====================

/// Ports the pool's `if (!missing.has(mark)) { missing.add(mark);
/// notifyMissing(...) }` guard: `true` (and inserts) only the first time a
/// given key is seen, so a caller knows whether to actually push
/// `lsp:missing` — an absent optional tool must not nag on every
/// keystroke.
fn should_report_missing(missing: &mut HashSet<String>, key: String) -> bool {
    if missing.contains(&key) {
        false
    } else {
        missing.insert(key);
        true
    }
}

// ==================== process spawning ====================

/// Builds the `tokio::process::Command` for one server and spawns it —
/// split out from [`Server::spawn_and_init`] specifically so a missing
/// binary's spawn failure is testable on its own, with no `AppHandle`
/// needed: this is a real, deterministic OS-level failure (`ErrorKind::
/// NotFound`), not something that needs mocking. `env_clear()` +
/// `envs(resolved)` matches Node's `spawn(cmd, args, { env })`, which
/// REPLACES the child's environment with exactly the given object rather
/// than merging it onto the parent's — `Command`'s default is to inherit,
/// so this crate must clear first or a future narrowing of
/// `resolve_server_env` would silently leak the unnarrowed vars back in
/// via inheritance.
fn spawn_child(spec: &ServerSpec, root: &Path) -> std::io::Result<tokio::process::Child> {
    let base: HashMap<String, String> = std::env::vars().collect();
    let env = policy::resolve_server_env(root, &base);
    tokio::process::Command::new(spec.cmd)
        .args(spec.args)
        .current_dir(root)
        .env_clear()
        .envs(&env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

fn initialize_params(root: &Path) -> Value {
    let root_uri = uri_of(root);
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "workspaceFolders": [{ "uri": root_uri, "name": name }],
        "capabilities": {
            "textDocument": {
                "synchronization": { "dynamicRegistration": false },
                "publishDiagnostics": { "relatedInformation": false },
                "hover": { "contentFormat": ["plaintext", "markdown"] },
                "definition": { "linkSupport": false }
            },
            "workspace": { "workspaceFolders": true, "configuration": true }
        }
    })
}

// ==================== one server process ====================

/// One running language server — ports `lsp.js`'s `Server` class. Framing
/// (via [`FrameParser`]) and request/notify are hand-rolled over raw
/// stdio, matching the plan's "skip `lsp-types`" call.
struct Server {
    stdin: AsyncMutex<tokio::process::ChildStdin>,
    // Held so the child is not reaped by `kill_on_drop` the moment
    // `spawn_and_init` returns; `kill()` below is this struct's only
    // caller of `start_kill`.
    child: AsyncMutex<tokio::process::Child>,
    seq: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    docs: Mutex<HashMap<String, u64>>,
    dead: AtomicBool,
}

impl Server {
    /// Spawns the process, wires up its background reader + stderr-drain
    /// tasks, and completes the `initialize`/`initialized` handshake —
    /// ports `Server.start()`. A process that spawns but exits immediately
    /// (or never responds) is caught the same way a genuinely-missing
    /// binary is: the background reader task hits stdout EOF, calls
    /// [`Server::fail_all`], which resolves the in-flight `initialize`
    /// request's oneshot with an `Err` — no separate `child.wait()` race
    /// is needed, because that reader task already runs for the server's
    /// entire lifetime and is the sole place any exit (early or late) is
    /// noticed (mirrors the JS original's `proc.on('exit', fail)` net
    /// effect via a different, Rust-idiomatic mechanism).
    async fn spawn_and_init(
        spec: &'static ServerSpec,
        root: PathBuf,
        app: AppHandle,
    ) -> Result<Arc<Server>, String> {
        let mut child = spawn_child(spec, &root).map_err(|e| e.to_string())?;
        let stdin = child
            .stdin
            .take()
            .expect("spawn_child requests piped stdin");
        let stdout = child
            .stdout
            .take()
            .expect("spawn_child requests piped stdout");
        let mut stderr = child
            .stderr
            .take()
            .expect("spawn_child requests piped stderr");

        let server = Arc::new(Server {
            stdin: AsyncMutex::new(stdin),
            child: AsyncMutex::new(child),
            seq: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            docs: Mutex::new(HashMap::new()),
            dead: AtomicBool::new(false),
        });

        // Drain stderr — never surfaced (server logs are not ours to
        // show), but must be read continuously so a full pipe buffer can
        // never back-pressure the server's own writes. Matches
        // `this.proc.stderr.resume()`.
        tokio::spawn(async move {
            let mut sink = Vec::new();
            let _ = stderr.read_to_end(&mut sink).await;
        });

        let reader_server = server.clone();
        let reader_app = app.clone();
        tokio::spawn(async move {
            read_loop(stdout, &reader_server, &reader_app).await;
            reader_server.fail_all("language server exited");
        });

        let init = server
            .request("initialize", initialize_params(&root), REQUEST_TIMEOUT)
            .await;
        if let Err(e) = init {
            server.kill().await;
            return Err(e);
        }
        server.notify("initialized", json!({})).await;
        Ok(server)
    }

    async fn send(&self, payload: &Value) -> Result<(), String> {
        if self.dead.load(Ordering::SeqCst) {
            return Ok(()); // matches `if (this.dead || !stdin.writable) return`
        }
        let body = serde_json::to_vec(payload).expect("Value always serializes");
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(header.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.write_all(&body).await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    async fn notify(&self, method: &str, params: Value) {
        let _ = self
            .send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await;
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if self.dead.load(Ordering::SeqCst) {
            return Err("language server exited".to_string());
        }
        let id = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("Server.pending lock poisoned")
            .insert(id, tx);
        if let Err(e) = self
            .send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await
        {
            self.pending
                .lock()
                .expect("Server.pending lock poisoned")
                .remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            // sender dropped without a reply — only happens via fail_all
            Ok(Err(_)) => Err("language server exited".to_string()),
            Err(_) => {
                self.pending
                    .lock()
                    .expect("Server.pending lock poisoned")
                    .remove(&id);
                Err(format!("{method} timed out"))
            }
        }
    }

    /// Settles every in-flight request with `message` and marks the
    /// server dead — ports `fail(err)`. Idempotent: safe to call from both
    /// the reader task's EOF path and an explicit [`Server::kill`].
    fn fail_all(&self, message: &str) {
        self.dead.store(true, Ordering::SeqCst);
        for (_, tx) in self
            .pending
            .lock()
            .expect("Server.pending lock poisoned")
            .drain()
        {
            let _ = tx.send(Err(message.to_string()));
        }
    }

    /// Ports `Server.kill()`: signal the child and stop accepting new
    /// work. `start_kill` (fire-and-forget) rather than the awaiting
    /// `Child::kill()`, matching the JS original's synchronous,
    /// non-blocking `this.proc?.kill()`.
    async fn kill(&self) {
        self.fail_all("language server killed");
        let _ = self.child.lock().await.start_kill();
    }

    /// Ports `Server.dispatch(msg)` exactly, including its ordering: a
    /// `msg.id` is first tried against `pending` (a real reply to OUR
    /// request); only if that misses AND `msg` also carries a `method` is
    /// it treated as a server -> client request worth answering; anything
    /// else with an `id` is silently dropped (matches falling through both
    /// JS branches with neither firing).
    async fn dispatch(self: &Arc<Self>, msg: Value, app: &AppHandle) {
        let id = msg.get("id").and_then(Value::as_u64);
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);

        if let Some(id) = id {
            let tx = self
                .pending
                .lock()
                .expect("Server.pending lock poisoned")
                .remove(&id);
            if let Some(tx) = tx {
                let result = if let Some(err) = msg.get("error") {
                    let message = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("lsp error")
                        .to_string();
                    Err(message)
                } else {
                    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = tx.send(result);
                return;
            }
            if let Some(m) = method {
                // answer the few server -> client requests that block
                // startup if ignored.
                let result = if m == "workspace/configuration" {
                    json!([{}])
                } else {
                    Value::Null
                };
                let _ = self
                    .send(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
                    .await;
            }
            return;
        }

        if method.as_deref() == Some("textDocument/publishDiagnostics") {
            if let Some(params) = msg.get("params") {
                if let Some(path) = params.get("uri").and_then(Value::as_str).and_then(path_of) {
                    let diagnostics = params
                        .get("diagnostics")
                        .cloned()
                        .unwrap_or_else(|| json!([]));
                    let _ = app.emit(
                        "lsp:diagnostics",
                        json!({ "path": path.to_string_lossy(), "diagnostics": diagnostics }),
                    );
                }
            }
        }
    }

    async fn did_open(&self, path: &str, lang_id: &str, text: &str) {
        let transition = {
            let mut docs = self.docs.lock().expect("Server.docs lock poisoned");
            advance_doc_open(&mut docs, path)
        };
        let version = match transition {
            OpenTransition::FreshOpen(v) => {
                self.notify(
                    "textDocument/didOpen",
                    json!({"textDocument": {
                        "uri": uri_of(Path::new(path)), "languageId": lang_id, "version": v, "text": text
                    }}),
                )
                .await;
                return;
            }
            OpenTransition::TreatedAsChange(v) => v,
        };
        self.notify_did_change(path, version, text).await;
    }

    async fn did_change(&self, path: &str, text: &str) {
        let version = {
            let mut docs = self.docs.lock().expect("Server.docs lock poisoned");
            advance_doc_change(&mut docs, path)
        };
        self.notify_did_change(path, version, text).await;
    }

    async fn notify_did_change(&self, path: &str, version: u64, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri_of(Path::new(path)), "version": version },
                "contentChanges": [{ "text": text }],
            }),
        )
        .await;
    }

    async fn did_close(&self, path: &str) {
        let removed = {
            let mut docs = self.docs.lock().expect("Server.docs lock poisoned");
            advance_doc_close(&mut docs, path)
        };
        if !removed {
            return;
        }
        self.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": uri_of(Path::new(path))}}),
        )
        .await;
    }
}

async fn read_loop(mut stdout: tokio::process::ChildStdout, server: &Arc<Server>, app: &AppHandle) {
    let mut framer = FrameParser::default();
    let mut chunk = [0u8; 8192];
    loop {
        let n = match stdout.read(&mut chunk).await {
            Ok(0) | Err(_) => return, // EOF or read error — server gone
            Ok(n) => n,
        };
        for msg in framer.push(&chunk[..n]) {
            server.dispatch(msg, app).await;
        }
    }
}

// ==================== pool ====================

/// One pool slot: `None` until a server has been successfully spawned for
/// this (root, server id) key; the outer `AsyncMutex` serializes
/// concurrent first-spawns for the SAME key (so two files of the same
/// language opened at once don't race two processes into existence)
/// without blocking lookups for a DIFFERENT key — mirrors the JS
/// original's `servers` Map, but per-key-locked rather than guarded by
/// JS's single-threaded event loop.
type PoolSlot = AsyncMutex<Option<Arc<Server>>>;

static POOL: OnceLock<Mutex<HashMap<String, Arc<PoolSlot>>>> = OnceLock::new();
fn pool() -> &'static Mutex<HashMap<String, Arc<PoolSlot>>> {
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

static MISSING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
fn missing() -> &'static Mutex<HashSet<String>> {
    MISSING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Ports `serverOf(path, folders)`: resolves language + server spec +
/// confined root, short-circuits on an already-known-missing binary,
/// reuses a live pooled server or spawns+registers a fresh one, and — on a
/// failed spawn — records it as missing (reporting via `lsp:missing`
/// exactly the first time) and returns `None`. `None` is also the answer
/// for a path with no recognized language, no folders open, or a path
/// outside every open folder — every one of those is a silent no-op to
/// the caller, matching the JS original's optional-chaining call sites
/// (`s?.server.didOpen(...)`, etc.).
async fn server_of(
    app: &AppHandle,
    path: &str,
    folders: &[PathBuf],
) -> Option<(Arc<Server>, &'static str)> {
    let lang_id = language_id_for(path)?;
    let spec = server_for(lang_id)?;
    let root = policy::confine_to_root(path, folders)?;

    let missing_key = format!("{} {}", root.display(), spec.cmd);
    if missing()
        .lock()
        .expect("MISSING lock poisoned")
        .contains(&missing_key)
    {
        return None;
    }

    let pool_key = format!("{} {}", root.display(), spec.id);
    let slot = pool()
        .lock()
        .expect("POOL lock poisoned")
        .entry(pool_key)
        .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
        .clone();

    let mut guard = slot.lock().await;
    if let Some(s) = guard.as_ref() {
        if !s.dead.load(Ordering::SeqCst) {
            return Some((s.clone(), lang_id));
        }
        *guard = None; // dead — fall through and respawn
    }

    match Server::spawn_and_init(spec, root, app.clone()).await {
        Ok(server) => {
            *guard = Some(server.clone());
            Some((server, lang_id))
        }
        Err(_) => {
            drop(guard);
            // treat a server that does not start as absent: report once,
            // then stay quiet — matches `catch { ...; if (!missing.has(mark))
            // { missing.add(mark); notifyMissing(...) } return null }`.
            if should_report_missing(
                &mut missing().lock().expect("MISSING lock poisoned"),
                missing_key,
            ) {
                let _ = app.emit("lsp:missing", json!({"cmd": spec.cmd, "langId": lang_id}));
            }
            None
        }
    }
}

/// `lsp:didOpen` — ports `export async function didOpen(path, text,
/// folders)`.
pub async fn did_open(app: &AppHandle, path: &str, text: &str, folders: &[PathBuf]) {
    if let Some((server, lang_id)) = server_of(app, path, folders).await {
        server.did_open(path, lang_id, text).await;
    }
}

/// `lsp:didChange`.
pub async fn did_change(app: &AppHandle, path: &str, text: &str, folders: &[PathBuf]) {
    if let Some((server, _)) = server_of(app, path, folders).await {
        server.did_change(path, text).await;
    }
}

/// `lsp:didClose`.
pub async fn did_close(app: &AppHandle, path: &str, folders: &[PathBuf]) {
    if let Some((server, _)) = server_of(app, path, folders).await {
        server.did_close(path).await;
    }
}

/// `lsp:hover` — ports `hover()`; `None` for no server, a request error,
/// or empty/whitespace-only content (any of which the JS original also
/// collapses to `null`, via its own `try { ... } catch { return null }`).
pub async fn hover(
    app: &AppHandle,
    path: &str,
    line: u64,
    character: u64,
    folders: &[PathBuf],
) -> Option<String> {
    let (server, _) = server_of(app, path, folders).await?;
    let result = server
        .request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri_of(Path::new(path)) },
                "position": { "line": line, "character": character },
            }),
            REQUEST_TIMEOUT,
        )
        .await
        .ok()?;
    extract_hover_text(&result)
}

/// `lsp:definition` — ports `definition()`.
pub async fn definition(
    app: &AppHandle,
    path: &str,
    line: u64,
    character: u64,
    folders: &[PathBuf],
) -> Option<DefinitionLocation> {
    let (server, _) = server_of(app, path, folders).await?;
    let result = server
        .request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri_of(Path::new(path)) },
                "position": { "line": line, "character": character },
            }),
            REQUEST_TIMEOUT,
        )
        .await
        .ok()?;
    extract_definition(&result)
}

/// `lsp.shutdownAll()` — called from the quit path (see `lib.rs`) so no
/// language server outlives the app. `start_kill` inside [`Server::kill`]
/// is non-blocking, so this stays fast enough to run inside the same
/// 1.5s-capped quit handshake `shutdown_all_proxies` already does.
pub async fn shutdown_all() {
    let slots: Vec<Arc<PoolSlot>> = pool()
        .lock()
        .expect("POOL lock poisoned")
        .values()
        .cloned()
        .collect();
    for slot in slots {
        let mut guard = slot.lock().await;
        if let Some(server) = guard.take() {
            server.kill().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ================= SERVERS table (argv exactness) =================

    #[test]
    fn servers_table_has_exactly_seven_entries() {
        assert_eq!(SERVERS.len(), 7);
    }

    #[test]
    fn servers_table_matches_lsp_js_argv_exactly() {
        let expected: &[(&str, &[&str], &str, &[&str])] = &[
            (
                "typescript",
                &[
                    "typescript",
                    "typescriptreact",
                    "javascript",
                    "javascriptreact",
                ],
                "typescript-language-server",
                &["--stdio"],
            ),
            ("python", &["python"], "pyright-langserver", &["--stdio"]),
            ("go", &["go"], "gopls", &[]),
            ("rust", &["rust"], "rust-analyzer", &[]),
            (
                "json",
                &["json"],
                "vscode-json-language-server",
                &["--stdio"],
            ),
            (
                "css",
                &["css", "scss", "less"],
                "vscode-css-language-server",
                &["--stdio"],
            ),
            (
                "html",
                &["html"],
                "vscode-html-language-server",
                &["--stdio"],
            ),
        ];
        for (spec, (id, langs, cmd, args)) in SERVERS.iter().zip(expected.iter()) {
            assert_eq!(spec.id, *id);
            assert_eq!(spec.langs, *langs);
            assert_eq!(spec.cmd, *cmd);
            assert_eq!(spec.args, *args);
        }
    }

    // ================= language_id_for / server_for =================

    #[test]
    fn language_id_for_maps_every_known_extension() {
        for (ext, lang) in LANG_BY_EXT {
            assert_eq!(
                language_id_for(&format!("file.{ext}")),
                Some(*lang),
                "extension {ext}"
            );
        }
    }

    #[test]
    fn language_id_for_is_case_insensitive() {
        assert_eq!(language_id_for("Component.TSX"), Some("typescriptreact"));
    }

    #[test]
    fn language_id_for_returns_none_for_an_unknown_extension() {
        assert_eq!(language_id_for("data.xyz"), None);
    }

    #[test]
    fn language_id_for_returns_none_for_no_extension_at_all() {
        assert_eq!(language_id_for("Makefile"), None);
    }

    #[test]
    fn language_id_for_returns_none_for_a_path_ending_in_a_bare_dot() {
        assert_eq!(language_id_for("file."), None);
    }

    #[test]
    fn language_id_for_uses_the_last_extension_of_a_multi_dot_path() {
        assert_eq!(language_id_for("archive.tar.gz"), None); // "gz" is unknown
        assert_eq!(
            language_id_for("component.test.tsx"),
            Some("typescriptreact")
        );
    }

    #[test]
    fn server_for_resolves_every_registered_language_to_the_right_spec() {
        assert_eq!(server_for("rust").map(|s| s.id), Some("rust"));
        assert_eq!(server_for("scss").map(|s| s.id), Some("css"));
        assert_eq!(
            server_for("javascriptreact").map(|s| s.id),
            Some("typescript")
        );
    }

    #[test]
    fn server_for_returns_none_for_an_unregistered_language() {
        assert!(server_for("ruby").is_none());
    }

    // ================= file:// URI round-trip =================

    #[test]
    fn uri_of_builds_a_file_uri_for_an_absolute_posix_path() {
        assert_eq!(
            uri_of(Path::new("/workspace/proj/src/index.ts")),
            "file:///workspace/proj/src/index.ts"
        );
    }

    #[test]
    fn uri_of_percent_encodes_spaces() {
        assert_eq!(uri_of(Path::new("/a b/c.ts")), "file:///a%20b/c.ts");
    }

    #[test]
    fn path_of_round_trips_uri_of() {
        let p = Path::new("/workspace/proj/a b/index.ts");
        assert_eq!(path_of(&uri_of(p)), Some(p.to_path_buf()));
    }

    #[test]
    fn path_of_returns_none_for_a_non_file_uri() {
        assert_eq!(path_of("https://example.com/a"), None);
    }

    // ================= Content-Length framing =================

    #[test]
    fn frame_parser_extracts_a_single_complete_message() {
        let mut fp = FrameParser::default();
        let body = br#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let frame = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut bytes = frame.into_bytes();
        bytes.extend_from_slice(body);
        let msgs = fp.push(&bytes);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["id"], 1);
    }

    #[test]
    fn frame_parser_waits_for_a_message_split_across_two_pushes() {
        let mut fp = FrameParser::default();
        let body = br#"{"jsonrpc":"2.0","id":2,"result":true}"#;
        let frame = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut bytes = frame.into_bytes();
        bytes.extend_from_slice(body);

        let split_at = bytes.len() - 5;
        assert!(
            fp.push(&bytes[..split_at]).is_empty(),
            "must not emit a partial message"
        );
        let msgs = fp.push(&bytes[split_at..]);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["id"], 2);
    }

    #[test]
    fn frame_parser_extracts_two_messages_delivered_in_one_chunk() {
        let mut fp = FrameParser::default();
        let mut bytes = Vec::new();
        for id in [1, 2] {
            let body = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":null}}"#);
            bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
            bytes.extend_from_slice(body.as_bytes());
        }
        let msgs = fp.push(&bytes);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["id"], 1);
        assert_eq!(msgs[1]["id"], 2);
    }

    #[test]
    fn frame_parser_skips_an_unparseable_header_and_recovers() {
        let mut fp = FrameParser::default();
        let mut bytes = b"garbage-header-no-content-length\r\n\r\n".to_vec();
        let body = br#"{"jsonrpc":"2.0","id":9,"result":null}"#;
        bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        bytes.extend_from_slice(body);
        let msgs = fp.push(&bytes);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["id"], 9);
    }

    #[test]
    fn parse_content_length_is_case_insensitive_and_tolerates_whitespace() {
        assert_eq!(parse_content_length("Content-Length: 42"), Some(42));
        assert_eq!(parse_content_length("CONTENT-LENGTH:7"), Some(7));
        assert_eq!(
            parse_content_length("Content-Type: foo\r\ncontent-length:  13"),
            Some(13)
        );
    }

    #[test]
    fn parse_content_length_returns_none_without_a_header() {
        assert_eq!(parse_content_length("Content-Type: application/json"), None);
    }

    // ================= extract_hover_text =================

    #[test]
    fn extract_hover_text_handles_a_plain_string() {
        assert_eq!(
            extract_hover_text(&json!({"contents": "hello"})),
            Some("hello".to_string())
        );
    }

    #[test]
    fn extract_hover_text_handles_a_markup_content_object() {
        assert_eq!(
            extract_hover_text(&json!({"contents": {"kind": "markdown", "value": "**bold**"}})),
            Some("**bold**".to_string())
        );
    }

    #[test]
    fn extract_hover_text_joins_an_array_of_mixed_entries() {
        let v = json!({"contents": ["a", {"value": "b"}, "c"]});
        assert_eq!(extract_hover_text(&v), Some("a\nb\nc".to_string()));
    }

    #[test]
    fn extract_hover_text_returns_none_for_null_contents() {
        assert_eq!(extract_hover_text(&json!({"contents": null})), None);
    }

    #[test]
    fn extract_hover_text_returns_none_for_a_missing_contents_key() {
        assert_eq!(extract_hover_text(&json!({})), None);
    }

    #[test]
    fn extract_hover_text_returns_none_for_whitespace_only_text() {
        assert_eq!(extract_hover_text(&json!({"contents": "   \n  "})), None);
    }

    #[test]
    fn extract_hover_text_trims_surrounding_whitespace() {
        assert_eq!(
            extract_hover_text(&json!({"contents": "  hi  "})),
            Some("hi".to_string())
        );
    }

    // ================= extract_definition =================

    #[test]
    fn extract_definition_handles_a_location_shape() {
        let v = json!({
            "uri": "file:///workspace/proj/a.ts",
            "range": {"start": {"line": 4, "character": 2}, "end": {"line": 4, "character": 8}},
        });
        assert_eq!(
            extract_definition(&v),
            Some(DefinitionLocation {
                path: PathBuf::from("/workspace/proj/a.ts"),
                line: 4,
                character: 2
            })
        );
    }

    #[test]
    fn extract_definition_prefers_target_selection_range_over_target_range() {
        let v = json!({
            "targetUri": "file:///workspace/proj/b.ts",
            "targetRange": {"start": {"line": 0, "character": 0}},
            "targetSelectionRange": {"start": {"line": 10, "character": 3}},
        });
        assert_eq!(
            extract_definition(&v),
            Some(DefinitionLocation {
                path: PathBuf::from("/workspace/proj/b.ts"),
                line: 10,
                character: 3
            })
        );
    }

    #[test]
    fn extract_definition_takes_the_first_element_of_an_array_response() {
        let v = json!([
            {"uri": "file:///a.ts", "range": {"start": {"line": 1, "character": 1}}},
            {"uri": "file:///b.ts", "range": {"start": {"line": 2, "character": 2}}},
        ]);
        assert_eq!(
            extract_definition(&v).map(|d| d.path),
            Some(PathBuf::from("/a.ts"))
        );
    }

    #[test]
    fn extract_definition_returns_none_for_an_empty_array() {
        assert_eq!(extract_definition(&json!([])), None);
    }

    #[test]
    fn extract_definition_returns_none_for_null() {
        assert_eq!(extract_definition(&Value::Null), None);
    }

    #[test]
    fn extract_definition_returns_none_when_range_is_missing() {
        assert_eq!(extract_definition(&json!({"uri": "file:///a.ts"})), None);
    }

    // ================= document version bookkeeping =================

    #[test]
    fn advance_doc_open_starts_a_fresh_document_at_version_1() {
        let mut docs = HashMap::new();
        assert!(matches!(
            advance_doc_open(&mut docs, "a.ts"),
            OpenTransition::FreshOpen(1)
        ));
        assert_eq!(docs.get("a.ts"), Some(&1));
    }

    #[test]
    fn advance_doc_open_on_an_already_open_document_behaves_as_a_change() {
        let mut docs = HashMap::from([("a.ts".to_string(), 1u64)]);
        assert!(matches!(
            advance_doc_open(&mut docs, "a.ts"),
            OpenTransition::TreatedAsChange(2)
        ));
        assert_eq!(docs.get("a.ts"), Some(&2));
    }

    #[test]
    fn advance_doc_change_increments_the_version_each_call() {
        let mut docs = HashMap::from([("a.ts".to_string(), 1u64)]);
        assert_eq!(advance_doc_change(&mut docs, "a.ts"), 2);
        assert_eq!(advance_doc_change(&mut docs, "a.ts"), 3);
    }

    #[test]
    fn advance_doc_change_on_a_never_opened_document_starts_at_2() {
        // Mirrors the JS Map default: `(this.docs.get(path) || 1) + 1`.
        let mut docs = HashMap::new();
        assert_eq!(advance_doc_change(&mut docs, "never-opened.ts"), 2);
    }

    #[test]
    fn advance_doc_close_reports_whether_the_document_was_tracked() {
        let mut docs = HashMap::from([("a.ts".to_string(), 1u64)]);
        assert!(advance_doc_close(&mut docs, "a.ts"));
        assert!(!docs.contains_key("a.ts"));
        assert!(!advance_doc_close(&mut docs, "a.ts")); // already gone
    }

    // ================= missing-binary bookkeeping =================

    #[test]
    fn should_report_missing_is_true_only_the_first_time_a_key_is_seen() {
        let mut set = HashSet::new();
        assert!(should_report_missing(&mut set, "root gopls".to_string()));
        assert!(!should_report_missing(&mut set, "root gopls".to_string()));
        assert!(!should_report_missing(&mut set, "root gopls".to_string()));
    }

    #[test]
    fn should_report_missing_tracks_keys_independently() {
        let mut set = HashSet::new();
        assert!(should_report_missing(&mut set, "root gopls".to_string()));
        assert!(should_report_missing(
            &mut set,
            "root rust-analyzer".to_string()
        ));
    }

    // ================= spawn_child (real, deterministic OS failure) =================

    #[tokio::test]
    async fn spawn_child_errors_for_a_binary_that_does_not_exist_on_path() {
        let spec = ServerSpec {
            id: "test-missing",
            langs: &[],
            cmd: "definitely-not-a-real-lsp-binary-tome-test-xyz",
            args: &[],
        };
        let root = std::env::temp_dir();
        assert!(spawn_child(&spec, &root).is_err());
    }

    #[tokio::test]
    async fn spawn_child_succeeds_for_a_real_binary_on_path() {
        // `cat` with no args just waits on stdin — enough to prove
        // spawn_child's plumbing (piped stdio, cwd, env replacement) works
        // for a binary that DOES exist, not only the missing-binary path.
        let spec = ServerSpec {
            id: "test-cat",
            langs: &[],
            cmd: "cat",
            args: &[],
        };
        let root = std::env::temp_dir();
        let mut child =
            spawn_child(&spec, &root).expect("cat is on PATH in this dev/CI environment");
        let _ = child.start_kill();
    }
}
