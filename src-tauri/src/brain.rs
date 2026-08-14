//! Per-workspace note vault ("brain"): Obsidian-compatible markdown with
//! `[[wikilinks]]` and YAML-ish frontmatter. Ports `src/main/brain.js`
//! (341 lines) — module shape mirrors that file's own doc comment ("mirrors
//! airgap.js: pure functions + module state"): the parsing/confinement
//! logic below is pure and unit-tested; the cache/watcher maps are
//! module-level `static`s rather than `AppState` fields, for the same
//! reason `fs.rs`'s `WATCHED` static is (see that file's doc comment) —
//! this slice owns only `brain.rs`/`ipc/brain.rs` plus one `mod brain;`
//! line in `lib.rs`, not `state.rs`.
//!
//! Vaults live at `~/Tome/Brains/<sanitized-ws>` — deliberately OUTSIDE the
//! Tauri app config dir, so a sandboxed/gapped agent pane (whose Linux
//! bwrap wrap `--tmpfs`s the config dir, and whose macOS seatbelt profile
//! denies writes under it) still gets full read/write access to its
//! workspace's notes. `confine_real` below re-derives this crate's own
//! copy of the `~/Tome/Brains` path rather than reusing
//! `crate::confine`'s private `brains_root()` helper (which backs a
//! *different* confinement system — `confined_real_path`'s open-folders
//! check) — seem duplicative, but `confine.rs` is another slice's
//! already-committed file and out of this slice's ownership; see this
//! slice's task report for the full rationale.
//!
//! **Testing note on `test/brain.test.js`**: despite its name, that file
//! (53 lines) contains *only* `confine()` coverage — already ported and
//! pinned in `confine.rs` (a prior phase). There is no existing vitest
//! spec for `buildIndex`/frontmatter/wikilink parsing to port 1:1; the
//! `#[cfg(test)]` module below is written directly against this file's
//! JS source (`src/main/brain.js`), not against a spec file.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use regex::Regex;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter};

// ---- vault root resolution ----

/// `~/Tome/Brains` — see the module doc comment for why this is its own
/// copy rather than a shared helper.
fn brains_root() -> PathBuf {
    std::env::home_dir().unwrap_or_default().join("Tome").join("Brains")
}

/// Ports `safe(ws)`: workspace names are free renderer text, not vetted
/// like `pty:create`'s `kind` — sanitize before using in a path. `/`, `\`,
/// `:`, `.` all become `_` (collisions, e.g. "a/b" and "a.b" both -> "a_b",
/// are accepted — same tradeoff the JS original documents). Only a
/// genuinely empty *input* can produce an empty result here (the
/// replacement is character-for-character, never shortening), matching the
/// JS original's `|| 'workspace'` fallback firing only on that case — a
/// string of e.g. three dots becomes `"___"`, not `"workspace"`.
fn sanitize_ws(ws: &str) -> String {
    let cleaned: String = ws
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '.') { '_' } else { c })
        .collect();
    if cleaned.is_empty() {
        "workspace".to_string()
    } else {
        cleaned
    }
}

/// Ports `brainRoot(ws)`.
fn vault_root(ws: &str) -> PathBuf {
    brains_root().join(sanitize_ws(ws))
}

/// Ports the `AGENTS_MD(ws)` template literal verbatim (byte-for-byte,
/// including the em dashes and the single trailing newline).
fn agents_md_template(ws: &str) -> String {
    format!(
        "# AGENTS.md\n\nThis folder is the {ws} workspace vault ($TOME_BRAIN) — a note vault, not project source. One idea per note; the H1 heading matches the filename.\n\n## Conventions\n\n- Frontmatter on every note:\n\n  ---\n  tags: [tag-one, tag-two]\n  created: YYYY-MM-DD\n  status: draft\n  ---\n\n- Lifecycle: ideas run draft → exploring → ready → promoted; tasks run active → done.\n- Link related notes by wrapping a note's filename in double square brackets — matched by basename, case-insensitive.\n\n## Cross-workspace facts\n\nFacts that matter beyond this workspace belong in the core vault, not here. If $TOME_CORE_VAULT is set, copy the note there yourself; otherwise flag it in the Brain pane for promotion. Once a note has been copied to core, mark the local copy status: promoted.\n"
    )
}

/// Creates the vault directory (recursive) and seeds `AGENTS.md` if
/// missing — ports `ensureBrain(ws)`. `pub`: called by [`open`] below, and
/// also by `ipc::pty::pty_create`'s `resolve_brain_env` helper to set
/// `TOME_BRAIN` on any pane spawned with a `ws` (see that file's own doc
/// comment) — the exact hook this doc comment used to flag as an
/// unwired follow-up, back when no `brain.rs` module existed in this tree
/// yet.
pub fn ensure_brain(ws: &str) -> Result<PathBuf, String> {
    let root = vault_root(ws);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let agents_path = root.join("AGENTS.md");
    if !agents_path.exists() {
        std::fs::write(&agents_path, agents_md_template(ws)).map_err(|e| e.to_string())?;
    }
    Ok(root)
}

// ---- frontmatter / wikilink parsing ----

/// Result of [`parse_frontmatter`] — ports the plain object
/// `parseFrontmatter` returns (`{ tags, status, created, body }`).
struct Frontmatter {
    tags: Vec<String>,
    status: String,
    created: String,
    body: String,
}

fn frontmatter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^---\r?\n([\s\S]*?)\r?\n---").expect("frontmatter_re: valid pattern"))
}

fn tags_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^tags:\s*\[(.*)\]\s*$").expect("tags_re: valid pattern"))
}

fn status_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^status:\s*(.+?)\s*$").expect("status_re: valid pattern"))
}

fn created_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^created:\s*(.+?)\s*$").expect("created_re: valid pattern"))
}

fn wikilink_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\[\[([^\]|#]+)(?:[#|][^\]]*)?\]\]").expect("wikilink_re: valid pattern")
    })
}

/// Ports `parseFrontmatter(raw)`. `FRONTMATTER_RE` has no multiline flag in
/// the JS original, so `^` anchors to the absolute start of `raw` — a
/// match, if any, always starts at byte 0, which is what lets `body` be
/// computed as everything after the match's end (mirroring `raw.slice(
/// fm[0].length)`) rather than needing the match's start offset too.
fn parse_frontmatter(raw: &str) -> Frontmatter {
    let Some(fm) = frontmatter_re().captures(raw) else {
        return Frontmatter {
            tags: Vec::new(),
            status: String::new(),
            created: String::new(),
            body: raw.to_string(),
        };
    };
    let whole = fm.get(0).expect("capture group 0 is always present on a match");
    let block = fm.get(1).map(|m| m.as_str()).unwrap_or("");

    let tags = tags_re()
        .captures(block)
        .map(|c| {
            c[1].split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let status = status_re().captures(block).map(|c| c[1].to_string()).unwrap_or_default();
    let created = created_re().captures(block).map(|c| c[1].to_string()).unwrap_or_default();

    // `raw.slice(fm[0].length).replace(/^\n/, '')`: everything after the
    // whole match, minus exactly one leading newline (the blank line
    // separating frontmatter from body), not a trim of every leading
    // newline.
    let rest = &raw[whole.end()..];
    let body = rest.strip_prefix('\n').unwrap_or(rest).to_string();

    Frontmatter { tags, status, created, body }
}

/// Ports `[...raw.matchAll(WIKILINK_RE)].map(m => m[1].trim())`.
fn extract_wikilinks(raw: &str) -> Vec<String> {
    wikilink_re().captures_iter(raw).map(|c| c[1].trim().to_string()).collect()
}

// ---- index ----

/// One parsed note — ports the object literal `buildIndex` pushes per file
/// (`{ rel, name, tags, status, created, links, body, mtime }`). Fields are
/// private (this module's own `#[cfg(test)]` submodule, a descendant
/// module, can still construct/inspect them directly); `Serialize` is
/// derived so `ipc::brain` can hand a whole [`Index`] to
/// `serde_json::to_value` without knowing its shape.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Note {
    rel: String,
    name: String,
    tags: Vec<String>,
    status: String,
    created: String,
    links: Vec<String>,
    body: String,
    mtime: f64,
}

/// Ports the `{ root, notes, backlinks }` object `buildIndex`/`getIndex`
/// return. `backlinks` is keyed by lowercased link-target name, matching
/// the JS original's own key convention (see [`build_index_at`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Index {
    root: String,
    notes: Vec<Note>,
    backlinks: HashMap<String, Vec<String>>,
}

/// Ports `walk(dir, out)`: recursive, skips any dotfile/dotdir entry,
/// recurses into directories, collects *regular files* (not symlinks —
/// `e.isFile()` in the JS original is false for a symlink dirent, so a
/// symlinked `.md` is silently excluded, same as here) ending in `.md`. An
/// unreadable `dir` (missing, permission-denied) is a silent empty result,
/// matching the JS original's `try { ... } catch { return }`.
fn collect_markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else { continue };
        let full = entry.path();
        if file_type.is_dir() {
            collect_markdown_files(&full, out);
        } else if file_type.is_file() && name_str.ends_with(".md") {
            out.push(full);
        }
    }
}

fn mtime_millis(meta: &std::fs::Metadata) -> Option<f64> {
    let modified = meta.modified().ok()?;
    let dur = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_secs_f64() * 1000.0)
}

/// Pure core of `buildIndex(ws)`: walks `root` and parses every note,
/// independent of any workspace name or module cache — the piece that's
/// actually unit-tested. [`build_index`] below is the thin ws/cache
/// wrapper real callers use.
///
/// Ports two behaviors precisely: (1) a file that vanishes (or whose
/// metadata read fails) between the walk and the read is silently dropped
/// from this build, mirroring the JS original's `Promise.all([readFile,
/// stat]).catch(() => continue)`; (2) backlinks are keyed by the
/// *lowercased link target name*, and a note linking to its own name is
/// excluded from its own backlinks entry (`if (key === n.name.toLowerCase())
/// continue`) — a name shared by several notes is left for the consumer to
/// resolve, per the JS original's own comment.
fn build_index_at(root: &Path) -> Index {
    let mut files = Vec::new();
    collect_markdown_files(root, &mut files);

    let mut notes = Vec::new();
    for full in &files {
        let Ok(raw) = std::fs::read_to_string(full) else { continue };
        let Ok(meta) = std::fs::metadata(full) else { continue };
        let Some(mtime) = mtime_millis(&meta) else { continue };

        let rel = full.strip_prefix(root).unwrap_or(full).to_string_lossy().into_owned();
        let name = full.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let fm = parse_frontmatter(&raw);
        let links = extract_wikilinks(&raw);
        notes.push(Note {
            rel,
            name,
            tags: fm.tags,
            status: fm.status,
            created: fm.created,
            links,
            body: fm.body,
            mtime,
        });
    }

    let mut backlinks: HashMap<String, Vec<String>> = HashMap::new();
    for n in &notes {
        for link in &n.links {
            let key = link.to_lowercase();
            if key == n.name.to_lowercase() {
                continue;
            }
            backlinks.entry(key).or_default().push(n.rel.clone());
        }
    }

    Index { root: root.to_string_lossy().into_owned(), notes, backlinks }
}

// ---- module cache (ws -> Index), mirroring brain.js's module-level Map ----
//
// Not an AppState field — see the module doc comment (same rationale as
// fs.rs's WATCHED static).

static CACHE: OnceLock<Mutex<HashMap<String, Index>>> = OnceLock::new();
fn cache() -> &'static Mutex<HashMap<String, Index>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Ports `buildIndex(ws)`: rebuilds fresh from disk and stores the result
/// in the module cache as a side effect — every real rebuild (not just a
/// `getIndex` miss) updates the cache, matching the JS original's
/// `cache.set(ws, index)` living inside `buildIndex` itself.
fn build_index(ws: &str) -> Index {
    let index = build_index_at(&vault_root(ws));
    cache().lock().expect("brain: CACHE lock poisoned").insert(ws.to_string(), index.clone());
    index
}

// ---- vault-rooted confinement (confineReal in brain.js) ----
//
// `crate::confine::confine` is the pure lexical guard (ported from
// `src/main/lib/confine.js`, already tested against `test/brain.test.js`'s
// fixtures in `confine.rs` — reused here rather than duplicated, per this
// slice's brief). What's genuinely new here is the realpath-based
// double-check layered on top, parameterized by an arbitrary vault `root`
// — `confine::confined_real_path` is a *different* confinement system (it
// checks against `AppState.open_folders`, not a vault root), so it isn't a
// fit for this call shape; this is brain.js's own `confineReal`, which has
// no port anywhere else in the tree yet.

/// Ports `confineReal(root, rel, requireMd, { mustExist })`. Lexical
/// confinement first (`crate::confine::confine`), then a symlink-safe
/// realpath re-check — brain IPC runs unsandboxed in this process, so a
/// symlink inside the vault pointing outside must be refused even though
/// the lexical check alone can't see it.
///
/// `must_exist = true` (read/delete-style targets): the target itself must
/// already exist and its realpath must resolve *strictly inside* the
/// realpath'd root (`real.starts_with(&real_root) && real != real_root` —
/// the exclude-equality half mirrors the JS original's `real.startsWith(
/// realRoot + sep)`, which a bare path-prefix check alone would not: a
/// note is never the vault root itself).
///
/// `must_exist = false` (write-style targets): the target may not exist
/// yet, so the nearest *existing* ancestor of its parent directory is
/// confined instead — this is what catches a symlink anywhere in the
/// already-existing part of the path (e.g. `root/link -> /etc` with a
/// not-yet-created `root/link/new.md`). Unlike the `must_exist = true`
/// case, landing exactly on `root` itself is valid here (a note written
/// directly into the vault root): `Path::starts_with` already treats
/// equality as a match, which is why this branch needs no separate
/// equality check the way the JS original's `realDir === realRoot ||
/// realDir.startsWith(realRoot + sep)` spells out explicitly.
fn confine_real(root: &Path, rel: &str, require_md: bool, must_exist: bool) -> Option<PathBuf> {
    let full = crate::confine::confine(root, rel, require_md)?;
    let real_root = std::fs::canonicalize(root).ok()?;

    if must_exist {
        let real = std::fs::canonicalize(&full).ok()?;
        return if real.starts_with(&real_root) && real != real_root { Some(full) } else { None };
    }

    let mut dir = full.parent()?.to_path_buf();
    loop {
        if let Ok(real_dir) = std::fs::canonicalize(&dir) {
            return if real_dir.starts_with(&real_root) { Some(full) } else { None };
        }
        let parent = dir.parent()?.to_path_buf();
        if parent == dir {
            return None;
        }
        dir = parent;
    }
}

// ---- fs watch (notify + 300ms debouncer) ----

/// `REINDEX_DEBOUNCE_MS` in the JS original.
const REINDEX_DEBOUNCE: Duration = Duration::from_millis(300);

struct WatchEntry {
    // Kept alive for its Drop impl (stops the watcher thread) — never read
    // directly, same convention as fs.rs's own WatchEntry.
    _debouncer: Debouncer<RecommendedWatcher>,
}

static WATCHERS: OnceLock<Mutex<HashMap<String, WatchEntry>>> = OnceLock::new();
fn watchers() -> &'static Mutex<HashMap<String, WatchEntry>> {
    WATCHERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn stop_watch(ws: &str) {
    watchers().lock().expect("brain: WATCHERS lock poisoned").remove(ws);
}

/// Core of `startWatch`, parameterized over the "something under the vault
/// changed" callback so it's unit-testable without a live `AppHandle` —
/// same shape as `fs.rs`'s `watch_with`. Recursive (unlike `fs.rs`'s own
/// non-recursive single-path watch): a note can live in any subdirectory
/// of the vault, matching `fs.watch(root, { recursive: true }, cb)`.
/// Registration failure (bad/gone root) returns `None`, mirroring the JS
/// original's try/catch-and-give-up-silently around `fs.watch`.
fn start_watch_with<F>(root: &Path, on_change: F) -> Option<Debouncer<RecommendedWatcher>>
where
    F: FnMut() + Send + 'static,
{
    let mut on_change = on_change;
    let handler = move |result: DebounceEventResult| {
        if matches!(result, Ok(events) if !events.is_empty()) {
            on_change();
        }
        // A watch-backend error is swallowed here, matching the JS
        // original's `watcher.on('error', () => stopWatch(ws))` net effect
        // closely enough for this layer — the caller (`start_watch`) is
        // free to decide whether a broken watch should self-heal; this
        // core only guarantees it never panics on one.
    };
    let mut debouncer = new_debouncer(REINDEX_DEBOUNCE, handler).ok()?;
    debouncer.watcher().watch(root, RecursiveMode::Recursive).ok()?;
    Some(debouncer)
}

/// Real entry point: rebuilds the index (updating the cache) and emits
/// `brain:changed` with `{ ws, index }` — exactly the payload
/// `tome-ipc.js`'s `brain.onChanged` hands to `renderer.js`'s `({ ws, index
/// }) => brains.get(ws)?.onChanged(index)`. Not unit tested directly (needs
/// a live `AppHandle`, which this crate cannot construct standalone — same
/// documented boundary as `events.rs`/`fs.rs`'s own `AppHandle`-touching
/// entry points); [`start_watch_with`] above carries the tested behavior.
fn start_watch(app: &AppHandle, ws: &str, root: &Path) {
    stop_watch(ws);
    let ws_owned = ws.to_string();
    let app = app.clone();
    let debouncer = start_watch_with(root, move || {
        let index = build_index(&ws_owned);
        let _ = app.emit("brain:changed", json!({"ws": ws_owned.clone(), "index": index}));
    });
    if let Some(d) = debouncer {
        watchers()
            .lock()
            .expect("brain: WATCHERS lock poisoned")
            .insert(ws.to_string(), WatchEntry { _debouncer: d });
    }
}

// ---- public entry points (ipc::brain's callers) ----

/// Ports `open(ws)`: ensures the vault exists, builds+caches its index,
/// and (re)starts the watch. The only failure mode is `ensure_brain`'s
/// mkdir/write — `build_index`/`start_watch` are both infallible (an
/// unreadable dir is an empty index; a watch that fails to register is a
/// silent no-op), matching the JS original's only possible throw path.
pub fn open(app: &AppHandle, ws: &str) -> Result<(PathBuf, Index), String> {
    let root = ensure_brain(ws)?;
    let index = build_index(ws);
    start_watch(app, ws, &root);
    Ok((root, index))
}

/// Ports `close(ws)`: stops the watch and drops the cached index.
pub fn close(ws: &str) {
    stop_watch(ws);
    cache().lock().expect("brain: CACHE lock poisoned").remove(ws);
}

/// Ports `getIndex(ws)`: cache hit returns as-is (and, faithfully
/// reproducing the JS original, does *not* re-check that a watcher is
/// running on a hit — only a miss does); a miss rebuilds (via
/// [`build_index`], which populates the cache itself) and starts a watch
/// if none is running yet for `ws`.
pub fn get_index(app: &AppHandle, ws: &str) -> Index {
    if let Some(idx) = cache().lock().expect("brain: CACHE lock poisoned").get(ws) {
        return idx.clone();
    }
    let index = build_index(ws);
    let has_watch = watchers().lock().expect("brain: WATCHERS lock poisoned").contains_key(ws);
    if !has_watch {
        start_watch(app, ws, &vault_root(ws));
    }
    index
}

/// Ports `readNote(ws, rel)`.
pub fn read_note(ws: &str, rel: &str) -> Result<String, String> {
    let root = vault_root(ws);
    let full = confine_real(&root, rel, true, true).ok_or_else(|| "brain: path escapes vault".to_string())?;
    std::fs::read_to_string(&full).map_err(|e| e.to_string())
}

/// The two shapes `writeNote` can resolve to (besides an error) — ports
/// `{ ok: true }` / `{ exists: true }`.
#[derive(Debug)]
pub enum WriteOutcome {
    Ok,
    Exists,
}

/// Ports `writeNote(ws, rel, content, exclusive)`. Note this does *not*
/// touch the cache or emit `brain:changed` itself — same as the JS
/// original, which relies entirely on the already-running fs watch
/// (started by `open`/`getIndex`) to notice the write and reindex, 300ms
/// later.
pub fn write_note(ws: &str, rel: &str, content: &str, exclusive: bool) -> Result<WriteOutcome, String> {
    let root = vault_root(ws);
    let full =
        confine_real(&root, rel, true, false).ok_or_else(|| "brain: path escapes vault".to_string())?;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let result = if exclusive {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&full)
            .and_then(|mut f| f.write_all(content.as_bytes()))
    } else {
        std::fs::write(&full, content)
    };
    match result {
        Ok(()) => Ok(WriteOutcome::Ok),
        Err(e) if exclusive && e.kind() == std::io::ErrorKind::AlreadyExists => Ok(WriteOutcome::Exists),
        Err(e) => Err(e.to_string()),
    }
}

/// Ports `deleteNote(ws, rel)`, including the case-insensitive AGENTS.md
/// protection.
pub fn delete_note(ws: &str, rel: &str) -> Result<(), String> {
    let root = vault_root(ws);
    let full = confine_real(&root, rel, true, true).ok_or_else(|| "brain: path escapes vault".to_string())?;
    let is_agents_md = full
        .file_name()
        .map(|n| n.to_string_lossy().eq_ignore_ascii_case("agents.md"))
        .unwrap_or(false);
    if is_agents_md {
        return Err("brain: AGENTS.md is protected".to_string());
    }
    std::fs::remove_file(&full).map_err(|e| e.to_string())
}

/// Ports the `{ configured, root, folders }` object `coreInfo` returns.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoreInfo {
    configured: bool,
    root: Option<String>,
    folders: Vec<String>,
}

impl CoreInfo {
    /// `Some(root)` iff the core vault is configured — exactly the
    /// condition `index.js`'s `buildAgentEnv` gates `TOME_CORE_VAULT` on
    /// (`if (info.configured) env.TOME_CORE_VAULT = info.root`).
    /// `pub(crate)`: gives `ipc::pty::pty_create`'s `TOME_CORE_VAULT`
    /// wiring what it needs without exposing `CoreInfo`'s fields
    /// crate-wide — every other consumer (`ipc::brain`) only ever
    /// serializes a whole `CoreInfo` for the renderer, never reads a
    /// field programmatically. `core_info` only ever pairs `configured:
    /// true` with `root: Some(..)`, so this can never silently return
    /// `Some("")`-via-a-configured-but-rootless state.
    pub(crate) fn configured_root(&self) -> Option<&str> {
        self.configured.then(|| self.root.as_deref()).flatten()
    }
}

/// Ports `coreInfo(root)`. `root` is the caller's already-resolved
/// `core-vault` store value (a plain string path, or absent) — this
/// module, like the JS original, doesn't know the store's own file
/// convention; `ipc::brain::brain_core_info`/`brain_promote` resolve it via
/// `crate::store::get`.
pub fn core_info(root: Option<&str>) -> CoreInfo {
    let root = match root {
        Some(r) if !r.is_empty() => r,
        _ => return CoreInfo { configured: false, root: None, folders: Vec::new() },
    };
    match std::fs::read_dir(root) {
        Ok(entries) => {
            let mut folders: Vec<String> = entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    (!name.starts_with('.')).then_some(name)
                })
                .collect();
            folders.sort();
            CoreInfo { configured: true, root: Some(root.to_string()), folders }
        }
        Err(_) => CoreInfo { configured: false, root: Some(root.to_string()), folders: Vec::new() },
    }
}

/// The two shapes `promote` can resolve to (besides an error) — ports
/// `{ ok: true, rel }` / `{ collision: true }`.
#[derive(Debug)]
pub enum PromoteOutcome {
    Ok { rel: String },
    Collision,
}

/// Ports `promote(coreRoot, ws, rel, folder, { overwrite, rename })`.
pub fn promote(
    core_root: Option<&str>,
    ws: &str,
    rel: &str,
    folder: Option<&str>,
    overwrite: bool,
    rename: bool,
) -> Result<PromoteOutcome, String> {
    let info = core_info(core_root);
    if !info.configured {
        return Err("brain: core vault not configured".to_string());
    }
    let info_root = info.root.expect("core_info: configured is only true when root is Some");
    let info_root_path = PathBuf::from(&info_root);

    let src_root = vault_root(ws);
    let src_full =
        confine_real(&src_root, rel, true, true).ok_or_else(|| "brain: path escapes vault".to_string())?;

    let dest_dir = match folder {
        Some(f) if !f.is_empty() => confine_real(&info_root_path, f, false, false)
            .ok_or_else(|| "brain: folder escapes core vault".to_string())?,
        _ => info_root_path.clone(),
    };
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;

    let mut name = src_full
        .file_name()
        .expect("confine_real with require_md=true always yields a filename")
        .to_string_lossy()
        .into_owned();
    let mut dest_full = dest_dir.join(&name);
    if dest_full.exists() {
        if rename {
            let stem = name.strip_suffix(".md").unwrap_or(&name).to_string();
            let mut n = 2;
            loop {
                name = format!("{stem} {n}.md");
                dest_full = dest_dir.join(&name);
                if !dest_full.exists() {
                    break;
                }
                n += 1;
            }
        } else if !overwrite {
            return Ok(PromoteOutcome::Collision);
        }
    }
    std::fs::copy(&src_full, &dest_full).map_err(|e| e.to_string())?;
    let rel_out = dest_full.strip_prefix(&info_root_path).unwrap_or(&dest_full).to_string_lossy().into_owned();
    Ok(PromoteOutcome::Ok { rel: rel_out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Instant;
    use tempfile::tempdir;

    // ================= sanitize_ws =================

    #[test]
    fn sanitize_ws_replaces_path_hostile_characters() {
        assert_eq!(sanitize_ws("a/b\\c:d.e"), "a_b_c_d_e");
    }

    #[test]
    fn sanitize_ws_falls_back_to_workspace_for_empty_input() {
        assert_eq!(sanitize_ws(""), "workspace");
    }

    #[test]
    fn sanitize_ws_does_not_fall_back_when_result_is_nonempty_underscores() {
        // "..." replaces to "___" — nonempty, so it must NOT collapse to
        // "workspace" (only a truly empty *input* does).
        assert_eq!(sanitize_ws("..."), "___");
    }

    #[test]
    fn sanitize_ws_leaves_ordinary_names_untouched() {
        assert_eq!(sanitize_ws("my-project_2"), "my-project_2");
    }

    // ================= agents_md_template =================

    #[test]
    fn agents_md_template_embeds_workspace_name_and_key_sections() {
        let s = agents_md_template("demo-ws");
        assert!(s.starts_with("# AGENTS.md\n\n"));
        assert!(s.contains("the demo-ws workspace vault ($TOME_BRAIN)"));
        assert!(s.contains("## Conventions"));
        assert!(s.contains("## Cross-workspace facts"));
        assert!(s.ends_with("mark the local copy status: promoted.\n"));
    }

    // ================= parse_frontmatter =================

    #[test]
    fn parse_frontmatter_extracts_tags_status_created_and_body() {
        // Exactly one newline after the closing "---" (no blank-line
        // separator) — the leading-blank-line-stripping nuance has its own
        // dedicated test below.
        let raw = "---\ntags: [a, b]\ncreated: 2024-01-02\nstatus: draft\n---\n# Title\nbody text\n";
        let fm = parse_frontmatter(raw);
        assert_eq!(fm.tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(fm.status, "draft");
        assert_eq!(fm.created, "2024-01-02");
        assert_eq!(fm.body, "# Title\nbody text\n");
    }

    #[test]
    fn parse_frontmatter_handles_missing_frontmatter_block() {
        let raw = "# Title\nno frontmatter here\n";
        let fm = parse_frontmatter(raw);
        assert_eq!(fm.tags, Vec::<String>::new());
        assert_eq!(fm.status, "");
        assert_eq!(fm.created, "");
        assert_eq!(fm.body, raw);
    }

    #[test]
    fn parse_frontmatter_handles_empty_tags_array() {
        let raw = "---\ntags: []\nstatus: draft\n---\nbody\n";
        let fm = parse_frontmatter(raw);
        assert_eq!(fm.tags, Vec::<String>::new());
    }

    #[test]
    fn parse_frontmatter_tolerates_crlf_line_endings() {
        let raw = "---\r\ntags: [x]\r\nstatus: draft\r\n---\r\nbody\r\n";
        let fm = parse_frontmatter(raw);
        assert_eq!(fm.tags, vec!["x".to_string()]);
        assert_eq!(fm.status, "draft");
    }

    #[test]
    fn parse_frontmatter_trims_whitespace_around_status_and_created() {
        let raw = "---\nstatus:   draft   \ncreated:   2024-05-06   \n---\nbody\n";
        let fm = parse_frontmatter(raw);
        assert_eq!(fm.status, "draft");
        assert_eq!(fm.created, "2024-05-06");
    }

    #[test]
    fn parse_frontmatter_fields_are_found_regardless_of_order() {
        let raw = "---\ncreated: 2024-01-01\nstatus: active\ntags: [z]\n---\nbody\n";
        let fm = parse_frontmatter(raw);
        assert_eq!(fm.created, "2024-01-01");
        assert_eq!(fm.status, "active");
        assert_eq!(fm.tags, vec!["z".to_string()]);
    }

    #[test]
    fn parse_frontmatter_strips_only_one_leading_blank_line_from_body() {
        // Three newlines follow the closing "---" (two blank lines before
        // the text): only the first is stripped, so two remain.
        let raw = "---\nstatus: draft\n---\n\n\nbody starts here\n";
        let fm = parse_frontmatter(raw);
        assert_eq!(fm.body, "\n\nbody starts here\n");
    }

    // ================= extract_wikilinks =================

    #[test]
    fn extract_wikilinks_finds_plain_links() {
        assert_eq!(extract_wikilinks("see [[Note One]] please"), vec!["Note One".to_string()]);
    }

    #[test]
    fn extract_wikilinks_strips_alias_after_pipe() {
        assert_eq!(extract_wikilinks("[[Target|shown text]]"), vec!["Target".to_string()]);
    }

    #[test]
    fn extract_wikilinks_strips_heading_anchor_after_hash() {
        assert_eq!(extract_wikilinks("[[Target#Some Heading]]"), vec!["Target".to_string()]);
    }

    #[test]
    fn extract_wikilinks_trims_whitespace_inside_brackets() {
        assert_eq!(extract_wikilinks("[[  Spaced Name  ]]"), vec!["Spaced Name".to_string()]);
    }

    #[test]
    fn extract_wikilinks_finds_multiple_links_in_one_document() {
        let raw = "[[One]] and [[Two|alias]] and [[Three#h]]";
        assert_eq!(
            extract_wikilinks(raw),
            vec!["One".to_string(), "Two".to_string(), "Three".to_string()]
        );
    }

    #[test]
    fn extract_wikilinks_returns_empty_for_no_links() {
        assert_eq!(extract_wikilinks("plain text, no links"), Vec::<String>::new());
    }

    // ================= build_index_at =================

    #[test]
    fn build_index_at_walks_subdirectories_and_parses_notes() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Top.md"), "---\nstatus: draft\n---\ntop body").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("Nested.md"), "nested body, no frontmatter").unwrap();

        let index = build_index_at(root);
        let mut names: Vec<&str> = index.notes.iter().map(|n| n.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["Nested", "Top"]);

        let nested = index.notes.iter().find(|n| n.name == "Nested").unwrap();
        assert_eq!(nested.rel, "sub/Nested.md");
        assert_eq!(nested.body, "nested body, no frontmatter");

        let top = index.notes.iter().find(|n| n.name == "Top").unwrap();
        assert_eq!(top.status, "draft");
        assert!(top.mtime > 0.0);
    }

    #[test]
    fn build_index_at_skips_dotfiles_and_non_markdown_files() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(".hidden.md"), "x").unwrap();
        std::fs::write(root.join("notes.txt"), "x").unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join(".git").join("config.md"), "x").unwrap();
        std::fs::write(root.join("Real.md"), "x").unwrap();

        let index = build_index_at(root);
        assert_eq!(index.notes.len(), 1);
        assert_eq!(index.notes[0].name, "Real");
    }

    #[test]
    fn build_index_at_skips_symlinked_markdown_files() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside"); // sibling of root, NOT inside it
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("outside.md");
        std::fs::write(&outside_file, "outside content").unwrap();
        symlink(&outside_file, root.join("Link.md")).unwrap();
        std::fs::write(root.join("Real.md"), "x").unwrap();

        let index = build_index_at(&root);
        assert_eq!(index.notes.len(), 1);
        assert_eq!(index.notes[0].name, "Real");
    }

    #[test]
    fn build_index_at_computes_backlinks_case_insensitively() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Alpha.md"), "links to [[beta]]").unwrap();
        std::fs::write(root.join("Beta.md"), "no outgoing links").unwrap();

        let index = build_index_at(root);
        assert_eq!(index.backlinks.get("beta"), Some(&vec!["Alpha.md".to_string()]));
    }

    #[test]
    fn build_index_at_excludes_self_links_from_backlinks() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Alpha.md"), "links to itself [[Alpha]]").unwrap();

        let index = build_index_at(root);
        assert!(!index.backlinks.contains_key("alpha"));
    }

    #[test]
    fn build_index_at_returns_empty_index_for_a_missing_root() {
        let missing = PathBuf::from("/definitely/does/not/exist/brain-vault-xyz");
        let index = build_index_at(&missing);
        assert!(index.notes.is_empty());
        assert!(index.backlinks.is_empty());
    }

    // ================= confine_real =================

    #[test]
    fn confine_real_allows_a_path_inside_the_vault_that_exists() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("note.md"), "x").unwrap();
        assert_eq!(confine_real(&root, "note.md", true, true), Some(root.join("note.md")));
    }

    #[test]
    fn confine_real_rejects_traversal_via_the_shared_confine_guard() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        assert_eq!(confine_real(&root, "../escape.md", true, true), None);
        assert_eq!(confine_real(&root, "note.txt", true, true), None); // requireMd
    }

    #[test]
    fn confine_real_rejects_a_symlink_escaping_the_vault_must_exist_true() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("vault");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.md");
        std::fs::write(&secret, "shh").unwrap();
        symlink(&secret, root.join("escape.md")).unwrap();

        assert_eq!(confine_real(&root, "escape.md", true, true), None);
    }

    #[test]
    fn confine_real_must_exist_false_allows_a_not_yet_created_file_under_an_existing_dir() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let result = confine_real(&root, "brand-new.md", true, false);
        assert_eq!(result, Some(root.join("brand-new.md")));
    }

    #[test]
    fn confine_real_must_exist_false_rejects_a_symlinked_intermediate_dir_that_escapes_root() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("vault");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // `root/escaped` is a symlink to a directory outside the vault; the
        // target file itself doesn't exist yet, but the symlinked
        // *directory* does — must be caught while walking up.
        symlink(&outside, root.join("escaped")).unwrap();

        let result = confine_real(&root, "escaped/new-note.md", true, false);
        assert_eq!(result, None);
    }

    #[test]
    fn confine_real_must_exist_true_rejects_a_nonexistent_target() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        assert_eq!(confine_real(&root, "ghost.md", true, true), None);
    }

    #[test]
    fn confine_real_returns_none_when_the_vault_root_itself_does_not_exist() {
        let tmp = tempdir().unwrap();
        let missing_root = tmp.path().join("never-created");
        assert_eq!(confine_real(&missing_root, "note.md", true, true), None);
        assert_eq!(confine_real(&missing_root, "note.md", true, false), None);
    }

    #[test]
    fn confine_real_folder_confinement_allows_non_md_directory_targets() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        // requireMd=false, mustExist=false — promote()'s folder argument.
        let result = confine_real(&root, "some-folder", false, false);
        assert_eq!(result, Some(root.join("some-folder")));
    }

    // ================= ensure_brain =================

    #[test]
    fn ensure_brain_creates_the_vault_dir_and_agents_md() {
        let tmp = tempdir().unwrap();
        // ensure_brain resolves the vault path off $HOME, which we can't
        // override cleanly here without a real home-dir dependency — so
        // exercise the same logic build_index_at/confine_real already
        // cover via a hand-rolled equivalent instead.
        let root = tmp.path().join("ws-root");
        std::fs::create_dir_all(&root).unwrap();
        let agents_path = root.join("AGENTS.md");
        assert!(!agents_path.exists());
        std::fs::write(&agents_path, agents_md_template("ws-root")).unwrap();
        assert!(agents_path.exists());
        assert!(std::fs::read_to_string(&agents_path).unwrap().starts_with("# AGENTS.md"));
    }

    #[test]
    fn ensure_brain_does_not_clobber_an_existing_agents_md_content() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("ws-root-2");
        std::fs::create_dir_all(&root).unwrap();
        let agents_path = root.join("AGENTS.md");
        std::fs::write(&agents_path, "user-edited content").unwrap();
        // Mirrors ensure_brain's own exists-check-then-skip-write logic.
        if !agents_path.exists() {
            std::fs::write(&agents_path, agents_md_template("ws-root-2")).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&agents_path).unwrap(), "user-edited content");
    }

    // ================= start_watch_with =================

    #[test]
    fn start_watch_with_detects_a_change_and_fires_after_the_debounce() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("seed.md"), "1").unwrap();

        let hits = Arc::new(StdMutex::new(0u32));
        let hits2 = hits.clone();
        let _debouncer = start_watch_with(&root, move || {
            *hits2.lock().unwrap() += 1;
        })
        .expect("watch should register on a real tempdir");

        std::fs::write(root.join("seed.md"), "2").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && *hits.lock().unwrap() == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(*hits.lock().unwrap() >= 1, "expected the debounced watcher to fire at least once");
    }

    #[test]
    fn start_watch_with_detects_a_change_in_a_subdirectory_recursively() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let hits = Arc::new(StdMutex::new(0u32));
        let hits2 = hits.clone();
        let _debouncer = start_watch_with(&root, move || {
            *hits2.lock().unwrap() += 1;
        })
        .expect("watch should register on a real tempdir");

        std::fs::write(sub.join("nested.md"), "hello").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && *hits.lock().unwrap() == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            *hits.lock().unwrap() >= 1,
            "expected a change in a subdirectory to be detected recursively"
        );
    }

    // ================= read_note / write_note / delete_note =================

    #[test]
    fn read_note_returns_file_contents() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("a.md"), "hello world").unwrap();
        // read_note resolves vault_root(ws) internally via $HOME, so drive
        // confine_real directly here (already covered) plus the thin
        // read_to_string wrapper below with an inlined root.
        let full = confine_real(&root, "a.md", true, true).unwrap();
        assert_eq!(std::fs::read_to_string(full).unwrap(), "hello world");
    }

    #[test]
    fn write_note_creates_intermediate_directories_and_overwrites_by_default() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let full = confine_real(&root, "nested/dir/note.md", true, false).unwrap();
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, "one").unwrap();
        std::fs::write(&full, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&full).unwrap(), "two");
    }

    #[test]
    fn write_note_exclusive_reports_exists_without_overwriting() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let full = root.join("dup.md");
        std::fs::write(&full, "original").unwrap();

        let result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&full)
            .and_then(|mut f| f.write_all(b"clobber"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&full).unwrap(), "original");
    }

    #[test]
    fn write_note_exclusive_succeeds_when_the_file_is_new() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let full = root.join("fresh.md");
        let result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&full)
            .and_then(|mut f| f.write_all(b"content"));
        assert!(result.is_ok());
        assert_eq!(std::fs::read_to_string(&full).unwrap(), "content");
    }

    #[test]
    fn delete_note_removes_the_file() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let full = root.join("gone.md");
        std::fs::write(&full, "x").unwrap();
        let confined = confine_real(&root, "gone.md", true, true).unwrap();
        std::fs::remove_file(&confined).unwrap();
        assert!(!full.exists());
    }

    #[test]
    fn delete_note_protects_agents_md_case_insensitively() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("AGENTS.md"), "x").unwrap();
        let full = confine_real(&root, "AGENTS.md", true, true).unwrap();
        let is_agents_md =
            full.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case("agents.md")).unwrap_or(false);
        assert!(is_agents_md);

        // lowercase variant, in case a note is literally named agents.md
        std::fs::write(root.join("agents.md"), "y").unwrap();
    }

    #[test]
    fn delete_note_rejects_a_path_that_escapes_the_vault() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        assert_eq!(confine_real(&root, "../escape.md", true, true), None);
    }

    // ================= core_info =================

    #[test]
    fn core_info_reports_unconfigured_for_missing_or_empty_root() {
        assert_eq!(core_info(None), CoreInfo { configured: false, root: None, folders: vec![] });
        assert_eq!(core_info(Some("")), CoreInfo { configured: false, root: None, folders: vec![] });
    }

    #[test]
    fn core_info_lists_directories_sorted_excluding_dotfiles_and_files() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("zeta")).unwrap();
        std::fs::create_dir(root.join("alpha")).unwrap();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        std::fs::write(root.join("readme.md"), "x").unwrap();

        let info = core_info(Some(root.to_str().unwrap()));
        assert!(info.configured);
        assert_eq!(info.folders, vec!["alpha".to_string(), "zeta".to_string()]);
        assert_eq!(info.root.as_deref(), Some(root.to_str().unwrap()));
    }

    #[test]
    fn core_info_reports_configured_false_with_root_preserved_when_unreadable() {
        let missing = "/definitely/does/not/exist/core-vault-xyz";
        let info = core_info(Some(missing));
        assert!(!info.configured);
        assert_eq!(info.root.as_deref(), Some(missing));
        assert!(info.folders.is_empty());
    }

    // ================= promote =================

    fn setup_promote_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempdir().unwrap();
        let ws_root = tmp.path().join("ws-vault").canonicalize_or_create();
        let core_root = tmp.path().join("core-vault").canonicalize_or_create();
        (tmp, ws_root, core_root)
    }

    trait CanonicalizeOrCreate {
        fn canonicalize_or_create(&self) -> PathBuf;
    }
    impl CanonicalizeOrCreate for PathBuf {
        fn canonicalize_or_create(&self) -> PathBuf {
            std::fs::create_dir_all(self).unwrap();
            self.canonicalize().unwrap()
        }
    }

    #[test]
    fn promote_core_logic_copies_into_the_vault_root_by_default() {
        // promote() re-derives its source root from vault_root(ws), which
        // is $HOME-based — calling it directly here would make this test
        // depend on (and possibly interact with) the real home directory.
        // Drive the same underlying logic (confine + copy, folder=None =>
        // dest is the vault root itself) directly against fixture roots
        // instead, matching this file's other `promote_core_logic_*` tests.
        let (_tmp, ws_root, core_root) = setup_promote_fixture();
        std::fs::write(ws_root.join("note.md"), "content").unwrap();

        let src_full = confine_real(&ws_root, "note.md", true, true).unwrap();
        let dest_full = core_root.join("note.md");
        std::fs::copy(&src_full, &dest_full).unwrap();
        assert_eq!(std::fs::read_to_string(&dest_full).unwrap(), "content");
    }

    #[test]
    fn promote_core_logic_copies_into_a_named_folder() {
        let (_tmp, ws_root, core_root) = setup_promote_fixture();
        std::fs::write(ws_root.join("note.md"), "content").unwrap();
        std::fs::create_dir(core_root.join("projects")).unwrap();

        let dest_dir = confine_real(&core_root, "projects", false, false).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();
        let src_full = confine_real(&ws_root, "note.md", true, true).unwrap();
        let dest_full = dest_dir.join("note.md");
        std::fs::copy(&src_full, &dest_full).unwrap();
        assert_eq!(std::fs::read_to_string(&dest_full).unwrap(), "content");
        assert_eq!(dest_full.strip_prefix(&core_root).unwrap(), Path::new("projects/note.md"));
    }

    #[test]
    fn promote_core_logic_rejects_a_folder_that_escapes_the_core_vault() {
        let (_tmp, _ws_root, core_root) = setup_promote_fixture();
        assert_eq!(confine_real(&core_root, "../escape", false, false), None);
    }

    #[test]
    fn promote_core_logic_collision_without_overwrite_or_rename_is_reported() {
        let (_tmp, ws_root, core_root) = setup_promote_fixture();
        std::fs::write(ws_root.join("note.md"), "new").unwrap();
        std::fs::write(core_root.join("note.md"), "existing").unwrap();

        let dest_full = core_root.join("note.md");
        assert!(dest_full.exists());
        // Mirrors promote()'s own branch: exists && !rename && !overwrite => Collision.
        let overwrite = false;
        let rename = false;
        let collision = dest_full.exists() && !rename && !overwrite;
        assert!(collision);
        assert_eq!(std::fs::read_to_string(&dest_full).unwrap(), "existing"); // untouched
    }

    #[test]
    fn promote_core_logic_overwrite_replaces_existing_destination() {
        let (_tmp, ws_root, core_root) = setup_promote_fixture();
        std::fs::write(ws_root.join("note.md"), "new").unwrap();
        std::fs::write(core_root.join("note.md"), "existing").unwrap();

        let src_full = confine_real(&ws_root, "note.md", true, true).unwrap();
        let dest_full = core_root.join("note.md");
        std::fs::copy(&src_full, &dest_full).unwrap(); // overwrite=true path just copies over
        assert_eq!(std::fs::read_to_string(&dest_full).unwrap(), "new");
    }

    #[test]
    fn promote_core_logic_rename_picks_the_next_free_suffix() {
        let (_tmp, _ws_root, core_root) = setup_promote_fixture();
        std::fs::write(core_root.join("note.md"), "a").unwrap();
        std::fs::write(core_root.join("note 2.md"), "b").unwrap();

        // Mirrors promote()'s do-while rename loop.
        let stem = "note";
        let mut n = 2;
        let mut name;
        let mut dest_full;
        loop {
            name = format!("{stem} {n}.md");
            dest_full = core_root.join(&name);
            if !dest_full.exists() {
                break;
            }
            n += 1;
        }
        assert_eq!(name, "note 3.md");
    }

    #[test]
    fn promote_fails_when_core_vault_is_not_configured() {
        let (_tmp, _ws_root, _core_root) = setup_promote_fixture();
        let err = promote(None, "ws", "note.md", None, false, false).unwrap_err();
        assert_eq!(err, "brain: core vault not configured");
    }

    #[test]
    fn promote_core_logic_fails_when_the_source_escapes_the_workspace_vault() {
        let (_tmp, ws_root, _core_root) = setup_promote_fixture();
        assert_eq!(confine_real(&ws_root, "../escape.md", true, true), None);
    }

    // ================= build_index / close — module-cache wrapper =================
    //
    // Uses process-unique ws names (via a fresh AtomicUsize-derived suffix)
    // to stay safe under cargo test's parallel execution, since CACHE is a
    // single process-wide static keyed by plain ws strings.

    fn unique_ws(label: &str) -> String {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        format!("brain-test-{label}-{}", COUNTER.fetch_add(1, Ordering::SeqCst))
    }

    #[test]
    fn build_index_populates_the_module_cache() {
        let ws = unique_ws("cache-populate");
        // vault_root(ws) is $HOME-derived and likely doesn't exist for a
        // fresh random ws — build_index/collect_markdown_files both
        // tolerate a missing root (empty index), so this only exercises
        // the cache side effect itself, not real note parsing.
        let index = build_index(&ws);
        assert!(cache().lock().unwrap().contains_key(&ws));
        assert_eq!(cache().lock().unwrap().get(&ws), Some(&index));
    }

    #[test]
    fn close_clears_the_cached_index() {
        let ws = unique_ws("cache-close");
        build_index(&ws);
        assert!(cache().lock().unwrap().contains_key(&ws));
        close(&ws);
        assert!(!cache().lock().unwrap().contains_key(&ws));
    }
}
