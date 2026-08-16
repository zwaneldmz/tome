//! Confined filesystem operations backing the `fs_read_dir`, `fs_read_file`,
//! `fs_write_file`, `fs_mkdir`, `fs_create_file`, `fs_watch`, `fs_unwatch`
//! commands. Ports `src/main/index.js`'s fs handlers (~lines 893-978)
//! verbatim, including a property that reads as a gap until you trace it
//! back to that file's "file-open confinement" comment: none of these
//! seven handlers call `confinedRealPath` in the original either. Only the
//! model-driven/OS-handoff paths (conductor's `open_file` tool,
//! `doc:read`, `shell:openPath` — none of them this slice's files) are
//! confined; fs:* is direct tree/editor traffic, "unvetted by design" per
//! that comment, so this module stays unconfined too. `crate::confine`
//! still exists, real and tested, for the call sites that actually need
//! it — see that module's doc comment.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

/// One `fs:readDir` entry — mirrors index.js's
/// `.map((d) => ({ name: d.name, dir: d.isDirectory() }))`.
#[derive(Serialize, Debug, PartialEq)]
struct Entry {
    name: String,
    dir: bool,
}

/// Formats an `io::Error` the way Node's fs errors read (`"CODE:
/// message, 'path'"`). Only one code is actually load-bearing today:
/// `src/renderer/tree.js`'s `createFileIn`/`createFolderIn` pattern-match
/// `err.message` for the substring `"EEXIST"` to tell "already exists"
/// apart from every other failure (see the comment above its
/// `mkdir`/`createFile` calls there). The rest are included so a failure
/// reads the same way the Electron original's did, not because anything
/// else currently parses them.
fn fmt_io_err(err: &std::io::Error, path: &str) -> String {
    let code = match err.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::AlreadyExists => "EEXIST",
        std::io::ErrorKind::PermissionDenied => "EACCES",
        std::io::ErrorKind::NotADirectory => "ENOTDIR",
        _ => return format!("{err}, '{path}'"),
    };
    format!("{code}: {err}, '{path}'")
}

/// `fs:readDir` — lists `dir` non-recursively, filtering out `.git` and
/// `.DS_Store`, directories first then lexicographic by name (index.js:
/// `.sort((a, b) => b.dir - a.dir || a.name.localeCompare(b.name))`). The
/// `localeCompare` half is approximated by folding ASCII case before
/// comparing (`['Banana.txt','apple.txt','Zebra.txt'].sort(localeCompare)`
/// interleaves case, e.g. `apple, Banana, Zebra` — plain ordinal `str`
/// ordering instead sorts every uppercase-initial name before every
/// lowercase one, a directly user-visible divergence on any real,
/// typically-mixed-case project tree). This is still not full locale
/// collation — accented characters, non-Latin scripts, and numeric
/// ("file2" vs "file10") ordering can still diverge from `localeCompare`'s
/// default-locale behavior — matching that exactly needs an ICU-backed
/// collation crate this slice isn't scoped to add. Ties after case-folding
/// (e.g. two entries differing only in case) fall back to ordinal
/// comparison so the order is deterministic rather than dependent on
/// whatever order the OS's `readdir` happened to return.
pub async fn read_dir(dir: &str) -> Result<Value, String> {
    let mut rd = tokio::fs::read_dir(dir)
        .await
        .map_err(|e| fmt_io_err(&e, dir))?;
    let mut out = Vec::new();
    while let Some(entry) = rd.next_entry().await.map_err(|e| fmt_io_err(&e, dir))? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" || name == ".DS_Store" {
            continue;
        }
        let is_dir = entry
            .file_type()
            .await
            .map_err(|e| fmt_io_err(&e, dir))?
            .is_dir();
        out.push(Entry { name, dir: is_dir });
    }
    out.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    serde_json::to_value(out).map_err(|e| e.to_string())
}

/// `fs:readFile` — reads as UTF-8 text like Node's `readFile(p, 'utf8')`,
/// which is a *lossy* decode (invalid byte sequences become U+FFFD; it
/// never throws for encoding reasons) rather than a strict one. Uses
/// `read` + `String::from_utf8_lossy` rather than `read_to_string`, which
/// would reject invalid UTF-8 that Node happily "reads" as replacement
/// characters — a real behavioral difference this preserves on purpose.
pub async fn read_file(path: &str) -> Result<Value, String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| fmt_io_err(&e, path))?;
    Ok(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
}

/// `fs:writeFile`.
pub async fn write_file(path: &str, content: &str) -> Result<Value, String> {
    tokio::fs::write(path, content)
        .await
        .map_err(|e| fmt_io_err(&e, path))?;
    Ok(Value::Null)
}

/// `fs:mkdir` — recursive, same as `mkdir(p, { recursive: true })`. Node's
/// version resolves to the first directory path it had to create (or
/// `undefined` if the full path already existed); nothing in the renderer
/// reads that return value (`tree.js`/`panes.js`/`panels/flow.js` only
/// ever `await` the call or inspect a *rejection's* `.message`), so this
/// returns `null` unconditionally rather than tracking which ancestor was
/// first-created to reproduce it byte-for-byte.
pub async fn mkdir(path: &str) -> Result<Value, String> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| fmt_io_err(&e, path))?;
    Ok(Value::Null)
}

/// `fs:createFile` — exclusive create, same as Node's `{ flag: 'wx' }`
/// (`O_CREAT|O_EXCL`: fails rather than clobbering an existing file).
pub async fn create_file(path: &str) -> Result<Value, String> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|e| fmt_io_err(&e, path))?;
    Ok(Value::Null)
}

// ---- fs:watch / fs:unwatch ----
//
// index.js keys a `Map<path, { watcher, count, timer }>` at module scope.
// A real per-path notify `Debouncer` handle plus a refcount doesn't fit a
// plain `()` value type, so this module-level static is the replacement —
// same shape as the JS Map, just living outside AppState (whose former
// `watchers` placeholder field was dropped as dead).

struct WatchEntry {
    count: u32,
    _debouncer: Debouncer<RecommendedWatcher>,
}

static WATCHED: OnceLock<Mutex<HashMap<String, WatchEntry>>> = OnceLock::new();

fn watched() -> &'static Mutex<HashMap<String, WatchEntry>> {
    WATCHED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The same 120ms debounce index.js's `fs:watch` handler uses (its own
/// `setTimeout(..., 120)`, reset on every raw event for that path).
const WATCH_DEBOUNCE: Duration = Duration::from_millis(120);

/// Core of `fs:watch`, parameterized over the "something changed" callback
/// so it's unit-testable without a live `AppHandle` (which, like
/// `tauri::State`, this crate has no standalone constructor for — the
/// `tauri` dependency does not enable the `test` feature). `watch` below
/// is the real entry point, closing over `app.emit(...)`.
///
/// Refcounted like the original (the same path can be open in more than
/// one pane): a second `watch` on an already-watched path just bumps the
/// count and returns `true` without touching the filesystem again.
/// Registration failure (bad/gone path) returns `false`, mirroring the
/// original's try/catch around `fs.watch(p, cb)`.
fn watch_with<F>(path: String, mut on_change: F) -> bool
where
    F: FnMut() + Send + 'static,
{
    let mut map = watched().lock().unwrap();
    if let Some(entry) = map.get_mut(&path) {
        entry.count += 1;
        return true;
    }

    let handler = move |result: DebounceEventResult| {
        if matches!(result, Ok(events) if !events.is_empty()) {
            on_change();
        }
        // A watch-backend error (e.g. the watched file got removed out
        // from under it) is swallowed here exactly like index.js's
        // `watcher.on('error', () => {})` — never surfaced to the renderer.
    };
    let mut debouncer = match new_debouncer(WATCH_DEBOUNCE, handler) {
        Ok(d) => d,
        Err(_) => return false,
    };
    if debouncer
        .watcher()
        .watch(Path::new(&path), RecursiveMode::NonRecursive)
        .is_err()
    {
        return false;
    }
    map.insert(
        path,
        WatchEntry {
            count: 1,
            _debouncer: debouncer,
        },
    );
    true
}

/// `fs:watch`. Emits `fs:changed` with the *original watched path*, not
/// whatever sub-path notify actually saw change — index.js's callback
/// ignores `fs.watch`'s own `(eventType, filename)` arguments and always
/// sends back the closed-over `p` it was registered with; this closes
/// over the same string, matching `src/renderer/tome-ipc.js`'s
/// `onChanged: (cb) => on('fs:changed', cb)` (plain-string payload, no
/// wrapper object).
pub fn watch(app: AppHandle, path: String) -> bool {
    let emit_path = path.clone();
    watch_with(path, move || {
        let _ = app.emit("fs:changed", emit_path.clone());
    })
}

/// `fs:unwatch` — decrements the refcount, dropping (and thereby
/// stopping, per `Debouncer`'s `Drop` impl) the watcher only once it
/// reaches zero. A path that was never (or no longer) watched is a no-op,
/// same as the original.
pub fn unwatch(path: &str) {
    let mut map = watched().lock().unwrap();
    let Some(entry) = map.get_mut(path) else {
        return;
    };
    entry.count -= 1;
    if entry.count == 0 {
        map.remove(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Instant;
    use tempfile::tempdir;

    #[tokio::test]
    async fn read_dir_filters_dotfiles_and_sorts_dirs_first_then_name() {
        let tmp = tempdir().unwrap();
        for name in [".git", ".DS_Store", "b.txt", "a.txt", "zdir", "adir"] {
            let p = tmp.path().join(name);
            if name.ends_with("dir") {
                tokio::fs::create_dir(&p).await.unwrap();
            } else {
                tokio::fs::write(&p, b"x").await.unwrap();
            }
        }
        let v = read_dir(tmp.path().to_str().unwrap()).await.unwrap();
        let names: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["adir", "zdir", "a.txt", "b.txt"]);
        assert_eq!(v[0]["dir"], true);
        assert_eq!(v[2]["dir"], false);
    }

    #[tokio::test]
    async fn read_dir_sorts_mixed_case_names_like_localecompare_not_ordinal() {
        // Same fixture as JS's `['Banana.txt','apple.txt','Zebra.txt','mango.txt']
        // .sort((a, b) => a.localeCompare(b))` => apple, Banana, mango, Zebra.
        // Plain ordinal `str::cmp` would instead produce `Banana, Zebra, apple,
        // mango` (every uppercase-initial name first) — this pins the
        // case-folded behavior a real mixed-case project tree relies on.
        let tmp = tempdir().unwrap();
        for name in ["Banana.txt", "apple.txt", "Zebra.txt", "mango.txt"] {
            tokio::fs::write(tmp.path().join(name), b"x").await.unwrap();
        }
        let v = read_dir(tmp.path().to_str().unwrap()).await.unwrap();
        let names: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["apple.txt", "Banana.txt", "mango.txt", "Zebra.txt"]
        );
    }

    #[tokio::test]
    async fn read_dir_reports_enoent_for_a_missing_directory() {
        let err = read_dir("/definitely/does/not/exist/anywhere")
            .await
            .unwrap_err();
        assert!(err.contains("ENOENT"), "expected ENOENT in: {err}");
    }

    #[tokio::test]
    async fn read_file_reads_utf8_text() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("f.txt");
        tokio::fs::write(&file, "hello \u{1F600}").await.unwrap();
        let v = read_file(file.to_str().unwrap()).await.unwrap();
        assert_eq!(v, Value::String("hello \u{1F600}".to_string()));
    }

    #[tokio::test]
    async fn read_file_lossy_decodes_invalid_utf8_instead_of_erroring() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("bin.dat");
        tokio::fs::write(&file, [0x68, 0x69, 0xff, 0xfe])
            .await
            .unwrap();
        let v = read_file(file.to_str().unwrap()).await.unwrap();
        let s = v.as_str().unwrap();
        assert!(s.starts_with("hi"));
        assert!(s.contains('\u{FFFD}'));
    }

    #[tokio::test]
    async fn write_file_writes_and_overwrites() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("f.txt");
        write_file(file.to_str().unwrap(), "one").await.unwrap();
        write_file(file.to_str().unwrap(), "two").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "two");
    }

    #[tokio::test]
    async fn mkdir_creates_nested_dirs_recursively() {
        let tmp = tempdir().unwrap();
        let nested = tmp.path().join("a").join("b").join("c");
        mkdir(nested.to_str().unwrap()).await.unwrap();
        assert!(nested.is_dir());
    }

    #[tokio::test]
    async fn mkdir_is_idempotent_on_an_existing_directory() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("already-there");
        tokio::fs::create_dir(&dir).await.unwrap();
        // must NOT error — tree.js relies on mkdir being safe to call on an
        // already-there directory parent.
        mkdir(dir.to_str().unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn mkdir_reports_eexist_when_the_exact_target_is_an_existing_file() {
        let tmp = tempdir().unwrap();
        let file_path = tmp.path().join("blocker");
        tokio::fs::write(&file_path, b"x").await.unwrap();
        let err = mkdir(file_path.to_str().unwrap()).await.unwrap_err();
        assert!(
            err.contains("EEXIST"),
            "renderer's tree.js branches on the substring EEXIST — got: {err}"
        );
    }

    #[tokio::test]
    async fn create_file_creates_an_empty_file() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("new.txt");
        create_file(file.to_str().unwrap()).await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "");
    }

    #[tokio::test]
    async fn create_file_reports_eexist_on_a_second_call() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("new.txt");
        create_file(file.to_str().unwrap()).await.unwrap();
        let err = create_file(file.to_str().unwrap()).await.unwrap_err();
        assert!(
            err.contains("EEXIST"),
            "renderer's tree.js branches on the substring EEXIST — got: {err}"
        );
    }

    #[tokio::test]
    async fn create_file_does_not_clobber_existing_content() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("keep.txt");
        tokio::fs::write(&file, "precious").await.unwrap();
        assert!(create_file(file.to_str().unwrap()).await.is_err());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "precious");
    }

    #[test]
    fn watch_with_refcounts_and_unwatch_only_removes_at_zero() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let path = dir.to_str().unwrap().to_string();

        assert!(watch_with(path.clone(), || {}));
        assert!(watch_with(path.clone(), || {})); // second watch bumps refcount
        assert_eq!(watched().lock().unwrap().get(&path).unwrap().count, 2);

        unwatch(&path);
        assert_eq!(watched().lock().unwrap().get(&path).unwrap().count, 1);
        assert!(watched().lock().unwrap().contains_key(&path));

        unwatch(&path);
        assert!(!watched().lock().unwrap().contains_key(&path));

        // unwatching an already-gone path is a no-op, not a panic
        unwatch(&path);
    }

    #[test]
    fn watch_with_returns_false_for_an_unwatchable_path() {
        let path = "/definitely/does/not/exist/at/all/xyz".to_string();
        assert!(!watch_with(path, || {}));
    }

    #[test]
    fn watch_with_detects_a_real_change_and_invokes_the_callback() {
        let tmp = tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let file = base.join("f.txt");
        std::fs::write(&file, "1").unwrap();
        let path = file.to_str().unwrap().to_string();

        let hits = Arc::new(StdMutex::new(0u32));
        let hits2 = hits.clone();
        assert!(watch_with(path.clone(), move || {
            *hits2.lock().unwrap() += 1;
        }));

        std::fs::write(&file, "2").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && *hits.lock().unwrap() == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            *hits.lock().unwrap() >= 1,
            "expected the debounced watcher to report at least one change"
        );

        unwatch(&path);
    }
}
