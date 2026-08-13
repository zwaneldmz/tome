//! Confinement guards for renderer-supplied paths: the pure vault-relative
//! guard (`confine`, porting `src/main/lib/confine.js` — its only vitest
//! coverage lives in `test/brain.test.js`, since the function was
//! originally extracted out of `brain.js` "so the guard is testable
//! without module state") and the state-aware, symlink-safe guard used
//! elsewhere in `index.js` (`confined_real_path`, porting that file's
//! `confinedRealPath`/`isConfinedPath`/`isBrainPath` trio — grep it for
//! "TOCTOU").
//!
//! Neither of *this slice's own* modules (`fs.rs`, `git.rs`) calls
//! `confined_real_path`. That is not an oversight: `index.js`'s own
//! "file-open confinement" comment (immediately above its `openFolders`
//! declaration) is explicit that `fs:readFile`/`writeFile`/`mkdir`/
//! `createFile` "stay unvetted by design: the editor and tree are
//! user-driven; the trust boundary is documented in the review — renderer
//! compromise ≈ user-privileged file access." The same file confirms
//! `fs:readDir`/`fs:watch`/`fs:unwatch` and every `git:*` handler follow
//! the same rule — none of them call `confinedRealPath` either (verified
//! by reading each handler body, not just the comment). Only the
//! model-driven/OS-handoff paths — conductor's `open_file` tool,
//! `doc:read`, `shell:openPath` — are confined, and none of those are this
//! slice's files. `confined_real_path` lives here, real and tested, for
//! those call sites to use once they land.

// Every function below is exercised by its own #[cfg(test)] suite, but in
// a plain (non-test) build only `confine` and `confined_real_path` are
// meant to be reachable from outside this module — and neither has a
// caller yet: this slice's own fs.rs/git.rs don't confine (see their doc
// comments for why), and the future callers that will (doc.rs, shell.rs,
// brain.rs) are still Phase 1 stubs. One module-level allow here instead
// of scattering #[allow(dead_code)] over nine individual items; `cargo
// test` still compiles and exercises every one of them regardless.
#![allow(dead_code)]

use std::env;
use std::path::{Component, Path, PathBuf};

use tauri::State;

use crate::state::AppState;

// ---- lexical path resolution (no filesystem access) ----
//
// Node's `path.resolve(...)` joins/absolutizes/normalizes purely as string
// manipulation — it never touches disk and never requires the path to
// exist. Rust's `Path::canonicalize` is the filesystem-touching opposite
// (resolves symlinks, requires existence). Both `confine()` and
// `is_confined path` need the lexical, no-filesystem version first —
// `confined_real_path` does its own, separate, filesystem-touching
// resolution afterward via `std::fs::canonicalize` (see that function).

/// `path.resolve(p)`'s single-argument behaviour: absolute-ify against the
/// current working directory if `p` isn't already absolute, then
/// lexically collapse `.`/`..`/repeated separators (clamping `..` at the
/// root instead of erroring, same as Node).
fn resolve1(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        env::current_dir().unwrap_or_default().join(p)
    };
    normalize_lexically(&abs)
}

/// `path.resolve(root, rel)`'s two-argument behaviour: if `rel` is itself
/// absolute, `root` is discarded entirely (matching Node's right-to-left
/// resolution); otherwise `rel` is joined onto `root` and the result is
/// run through the same absolutize+normalize as `resolve1`.
fn path_resolve(root: &Path, rel: &Path) -> PathBuf {
    if rel.is_absolute() {
        resolve1(rel)
    } else {
        resolve1(&root.join(rel))
    }
}

/// Collapses `.`/`..`/repeated-separator components the way `path.resolve`
/// does after joining: a `..` pops the last real segment (never popping
/// past the root) instead of erroring, `.` segments vanish, repeated
/// separators collapse. Assumes `p` is already absolute — both call sites
/// above ensure that before calling in.
fn normalize_lexically(p: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    let mut stack: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => root.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                stack.pop();
            }
            Component::Normal(seg) => stack.push(seg),
        }
    }
    let mut out = root;
    out.extend(stack);
    out
}

// ---- confine() — src/main/lib/confine.js, pinned by test/brain.test.js ----

/// Ports `src/main/lib/confine.js`'s `confine(root, rel, requireMd)`.
/// `root` is the vault (or similar) root; `rel` is untrusted input;
/// `requireMd` additionally demands a `.md` extension (notes only —
/// callers confining a directory argument pass `false`). Returns the
/// confined absolute path, or `None` if `rel` escapes `root` by any of: an
/// absolute path, a `..` segment (split on `\` as well as `/`, so a
/// Windows-style traversal string can't sneak past a POSIX host, matching
/// the original's `rel.split(/[\\/]/)`), or resolving to `root` itself —
/// containment is strict, the root is not a valid target (contrast
/// `is_confined_path` below, where an open workspace folder's root *is*
/// a valid target).
///
/// The JS original also rejects non-string `rel` (`null`/`undefined`/a
/// number/an array all hit its `typeof rel !== 'string'` guard). There is
/// no Rust analogue to port: `rel: &str` makes that whole input class
/// unrepresentable at the type level, so those cases are dropped rather
/// than translated.
pub fn confine(root: &Path, rel: &str, require_md: bool) -> Option<PathBuf> {
    if require_md && !rel.ends_with(".md") {
        return None;
    }
    if rel.starts_with('/') {
        return None;
    }
    if rel.split(['\\', '/']).any(|seg| seg == "..") {
        return None;
    }
    let full = path_resolve(root, Path::new(rel));
    if full.as_path() == root || !full.starts_with(root) {
        return None;
    }
    Some(full)
}

// ---- confined_real_path() — index.js's confinedRealPath, state-aware ----

fn brains_root() -> PathBuf {
    env::home_dir()
        .unwrap_or_default()
        .join("Tome")
        .join("Brains")
}

/// Ports `index.js`'s `isBrainPath`: `p` is strictly inside (not equal to)
/// the brain-vaults root, mirroring that function's `p.startsWith(BRAINS_
/// ROOT + sep)`. Assumes `p` is already absolute, the same precondition
/// its only caller (`is_confined`) upholds.
fn is_brain_path(p: &Path) -> bool {
    let root = brains_root();
    p != root.as_path() && p.starts_with(&root)
}

/// Pure core of `index.js`'s `isConfinedPath` — everything except reading
/// `AppState`'s `RwLock`s, so it's unit-testable without a live Tauri
/// `State` (which this crate has no way to construct standalone: the
/// `tauri` dependency in Cargo.toml does not enable the `test` feature,
/// and this slice does not touch Cargo.toml). `open_folders`/
/// `folders_synced` are exactly `AppState`'s fields of the same names,
/// passed by value/slice instead of read through a lock.
///
/// `false` until the renderer's first `ws_sync`; after that, `true` iff
/// `p` resolves (lexically only — no filesystem access, no symlink
/// following) inside an open workspace folder (the root itself included:
/// unlike `confine()` above, an exact match *is* confined) or inside a
/// brain vault.
fn is_confined(open_folders: &[PathBuf], folders_synced: bool, p: &Path) -> bool {
    if !folders_synced {
        return false;
    }
    if p.as_os_str().is_empty() {
        return false;
    }
    let abs = resolve1(p);
    open_folders.iter().any(|f| abs.starts_with(f)) || is_brain_path(&abs)
}

fn is_confined_path(state: &State<'_, AppState>, p: &Path) -> bool {
    let folders_synced = *state.folders_synced.read().unwrap();
    let open_folders = state.open_folders.read().unwrap();
    is_confined(&open_folders, folders_synced, p)
}

/// The reason half of `index.js`'s `confinementError(what)` — everything
/// but the `"${what}: "` prefix, since this module has no operation name
/// to prepend. A future caller (`doc:read`, `shell:openPath` — neither of
/// them this slice's files) is expected to format its own `"{channel}:
/// {reason}"` on an `Err` from `confined_real_path`, the way the original
/// calls `confinementError('doc:read')`/`confinementError('shell:openPath')`
/// itself after getting `null` back.
fn confinement_reason(folders_synced: bool) -> String {
    if folders_synced {
        "path is outside the open workspace folders".to_string()
    } else {
        "workspace folders have not been reported yet".to_string()
    }
}

/// Pure core of `confined_real_path` — same testability rationale as
/// `is_confined` above. `std::fs::canonicalize` (this function's only
/// filesystem access) is Rust's `realpath(3)` wrapper, the same syscall
/// Node's `fs.promises.realpath` (the original's `realpath` import)
/// resolves through, so both requiring the path to exist and both fully
/// resolving symlinks match exactly.
fn confined_real_path_core(
    open_folders: &[PathBuf],
    folders_synced: bool,
    path: &Path,
) -> Result<PathBuf, String> {
    if !is_confined(open_folders, folders_synced, path) {
        return Err(confinement_reason(folders_synced));
    }
    let real = std::fs::canonicalize(path).map_err(|_| confinement_reason(folders_synced))?;
    if is_confined(open_folders, folders_synced, &real) {
        Ok(real)
    } else {
        Err(confinement_reason(folders_synced))
    }
}

/// Resolves `path` to its real (symlink-free) form and checks it against
/// `state.open_folders` — the confinement boundary every fs/git/doc/shell
/// command must pass through before touching disk on behalf of the
/// renderer, porting index.js's `confinedRealPath` (grep it for "TOCTOU":
/// it re-resolves on every call rather than trusting a cached realpath,
/// because a symlink can change between calls).
///
/// Two-step, exactly as the original: (1) lexically resolve `path` and
/// check it's inside a confined root at all — no filesystem access yet,
/// just a cheap reject; (2) `realpath` it (the step that requires the
/// path to exist) and re-check the *resolved* path against the same
/// roots, so a symlink whose target lands outside a confined root is
/// still refused even though its own name looked fine.
///
/// Phase 1 stub signature, unchanged (see module docs above for why this
/// slice's own commands don't call it yet): callers already thread
/// `state`/`path` through exactly this way.
pub fn confined_real_path(state: &State<'_, AppState>, path: &Path) -> Result<PathBuf, String> {
    let folders_synced = *state.folders_synced.read().unwrap();
    let open_folders = state.open_folders.read().unwrap();
    confined_real_path_core(&open_folders, folders_synced, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    fn root() -> PathBuf {
        PathBuf::from("/vaults/foo")
    }

    // ---- confine() — ported 1:1 from test/brain.test.js ----

    #[test]
    fn confine_allows_normal_relative_paths_inside_the_vault() {
        let r = root();
        assert_eq!(confine(&r, "note.md", true), Some(r.join("note.md")));
        assert_eq!(
            confine(&r, "sub/dir/note.md", true),
            Some(r.join("sub").join("dir").join("note.md"))
        );
        assert_eq!(confine(&r, "sub folder", false), Some(r.join("sub folder")));
    }

    #[test]
    fn confine_blocks_dotdot_traversal() {
        let r = root();
        assert_eq!(confine(&r, "../outside.md", true), None);
        assert_eq!(confine(&r, "sub/../../outside.md", true), None);
        assert_eq!(confine(&r, "..", false), None);
        // backslash separators count too (win-style input on any platform)
        assert_eq!(confine(&r, "..\\outside.md", true), None);
    }

    #[test]
    fn confine_blocks_absolute_paths() {
        let r = root();
        assert_eq!(confine(&r, "/etc/passwd.md", true), None);
        assert_eq!(confine(&r, "/vaults/foo/note.md", true), None);
    }

    #[test]
    fn confine_blocks_sibling_prefix_escapes() {
        let r = root();
        assert_eq!(confine(&r, "../foobar/x.md", true), None);
        // and the resolved-path check alone must not pass a sibling prefix
        assert_eq!(
            confine(&PathBuf::from("/vaults/foo"), "foo2.md", true),
            Some(r.join("foo2.md"))
        );
    }

    #[test]
    fn confine_require_md_demands_extension() {
        let r = root();
        assert_eq!(confine(&r, "note.txt", true), None);
        assert_eq!(confine(&r, "note", true), None);
        assert_eq!(confine(&r, "note.txt", false), Some(r.join("note.txt")));
        assert_eq!(confine(&r, "folder", false), Some(r.join("folder")));
    }

    #[test]
    fn confine_rejects_the_vault_root_itself() {
        let r = root();
        assert_eq!(confine(&r, ".", false), None);
        assert_eq!(confine(&r, "", false), None);
    }

    // ---- is_confined — the open_folders/brain-root decision, no I/O ----

    #[test]
    fn is_confined_denies_until_synced() {
        let folders = vec![PathBuf::from("/work/proj")];
        assert!(!is_confined(&folders, false, Path::new("/work/proj/f.txt")));
    }

    #[test]
    fn is_confined_allows_root_and_children() {
        let folders = vec![PathBuf::from("/work/proj")];
        assert!(is_confined(&folders, true, Path::new("/work/proj")));
        assert!(is_confined(
            &folders,
            true,
            Path::new("/work/proj/src/main.rs")
        ));
    }

    #[test]
    fn is_confined_denies_a_sibling_prefix_and_unrelated_paths() {
        let folders = vec![PathBuf::from("/work/proj")];
        // "/work/projected" is NOT inside "/work/proj" — a naive string
        // prefix check without a separator boundary would wrongly allow
        // this; component-wise Path::starts_with must not.
        assert!(!is_confined(
            &folders,
            true,
            Path::new("/work/projected/f.txt")
        ));
        assert!(!is_confined(&folders, true, Path::new("/work/other")));
    }

    #[test]
    fn is_confined_allows_the_brain_vault_but_not_its_own_root() {
        let folders: Vec<PathBuf> = vec![];
        let inside = brains_root().join("myws").join("note.md");
        assert!(is_confined(&folders, true, &inside));
        assert!(!is_confined(&folders, true, &brains_root()));
    }

    #[test]
    fn is_confined_denies_empty_path() {
        let folders = vec![PathBuf::from("/work/proj")];
        assert!(!is_confined(&folders, true, Path::new("")));
    }

    // ---- confined_real_path_core — the realpath-backed double-check ----

    #[test]
    fn confined_real_path_allows_a_real_file_inside_root() {
        let tmp = tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap(); // macOS: /tmp -> /private/tmp
        let inside = base.join("workspace");
        fs::create_dir_all(&inside).unwrap();
        let file = inside.join("note.txt");
        fs::write(&file, "hi").unwrap();

        let folders = vec![inside];
        let result = confined_real_path_core(&folders, true, &file);
        assert_eq!(result, Ok(file));
    }

    #[test]
    fn confined_real_path_rejects_a_symlink_that_escapes_the_root() {
        let tmp = tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let inside = base.join("workspace");
        let outside = base.join("secret");
        fs::create_dir_all(&inside).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let secret_file = outside.join("secret.txt");
        fs::write(&secret_file, "shh").unwrap();
        let link = inside.join("escape.txt");
        symlink(&secret_file, &link).unwrap();

        let folders = vec![inside];
        let result = confined_real_path_core(&folders, true, &link);
        assert!(
            result.is_err(),
            "a symlink whose target resolves outside the confined root must be refused"
        );
    }

    #[test]
    fn confined_real_path_rejects_a_lexically_outside_path_without_touching_disk() {
        let folders = vec![PathBuf::from("/work/proj")];
        let result = confined_real_path_core(&folders, true, Path::new("/etc/passwd"));
        assert_eq!(result, Err(confinement_reason(true)));
    }

    #[test]
    fn confined_real_path_rejects_a_nonexistent_path() {
        let tmp = tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let folders = vec![base.clone()];
        let result = confined_real_path_core(&folders, true, &base.join("does-not-exist"));
        assert!(result.is_err());
    }

    #[test]
    fn confined_real_path_reports_not_synced_vs_outside_distinctly() {
        let folders = vec![PathBuf::from("/work/proj")];
        assert_eq!(
            confined_real_path_core(&folders, false, Path::new("/work/proj/f.txt")),
            Err(confinement_reason(false))
        );
        assert_ne!(confinement_reason(true), confinement_reason(false));
    }
}
