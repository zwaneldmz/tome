//! Realpath-confinement for the ABSOLUTE managed paths `flow::runner` and
//! `flow::tools` build themselves by joining a trusted root with segments
//! that are already vetted (a generated run id, a sanitized log filename,
//! the literal `run.json`, a name `flow::model::unsafe_folder_name` already
//! accepted) — port of `src/main/lib/flow-confine.js`'s
//! `confineRealAbs`/`confineRealAbsSync`.
//!
//! Lexically joined paths are still not enough on their own: an ancestor
//! directory anywhere in the existing part of the path (`.tome`,
//! `.tome/flows`, a per-flow `runs/` folder from an earlier run) can itself
//! be a symlink, and every fs call that follows one silently operates
//! outside `root`.
//!
//! Mirrors `brain.rs`'s `confineReal` contract exactly — "validate real,
//! return lexical": `must_exist: true` canonicalizes the target itself and
//! checks containment; `must_exist: false` (a target that may not exist yet
//! — a run directory about to be created) walks up via `parent()` to the
//! nearest EXISTING ancestor and checks that instead. Either way the
//! LEXICAL `full` comes back on success, never the canonicalized one — a
//! symlinked tmp dir in a test (macOS's own `/tmp` -> `/private/tmp`) must
//! not rewrite a path a caller compares byte for byte against a plain
//! `join()`. Every failure returns `None`.
//!
//! Two copies of the same shape, not one generic core — mirroring the JS
//! original's own reasoning: `flow::runner` runs everything through Tokio's
//! async fs, but `flow::tools` is deliberately synchronous (its own module
//! doc comment: the conductor calls it un-awaited), and threading a
//! sync/async split through one function would cost more than the
//! duplication does.

use std::path::{Path, PathBuf};

/// `full` must already be lexically and STRICTLY inside `root` (root itself
/// does not count) before any fs call is worth making.
fn lexically_inside(root: &Path, full: &Path) -> bool {
    if root.as_os_str().is_empty() {
        return false;
    }
    full.strip_prefix(root)
        .is_ok_and(|rest| !rest.as_os_str().is_empty())
}

pub async fn confine_real_abs(root: &Path, full: &Path, must_exist: bool) -> Option<PathBuf> {
    if !lexically_inside(root, full) {
        return None;
    }
    let real_root = tokio::fs::canonicalize(root).await.ok()?;
    if must_exist {
        let real = tokio::fs::canonicalize(full).await.ok()?;
        return if real.starts_with(&real_root) {
            Some(full.to_path_buf())
        } else {
            None
        };
    }
    let mut dir = full.parent()?.to_path_buf();
    loop {
        if let Ok(real_dir) = tokio::fs::canonicalize(&dir).await {
            return if real_dir == real_root || real_dir.starts_with(&real_root) {
                Some(full.to_path_buf())
            } else {
                None
            };
        }
        let parent = dir.parent()?.to_path_buf();
        if parent == dir {
            return None; // reached the filesystem root without finding one that exists
        }
        dir = parent;
    }
}

/// Same contract, synchronous — for `flow::tools` only (see module doc
/// comment). `flow::tools` itself has no caller yet in a plain
/// (non-test) build — see that module's own top-level `allow` — so this
/// warns as unused until the conductor slice lands; `confine_real_abs`
/// above has a real caller already (`flow::runner`) and needs no such
/// allow.
#[allow(dead_code)]
pub fn confine_real_abs_sync(root: &Path, full: &Path, must_exist: bool) -> Option<PathBuf> {
    if !lexically_inside(root, full) {
        return None;
    }
    let real_root = std::fs::canonicalize(root).ok()?;
    if must_exist {
        let real = std::fs::canonicalize(full).ok()?;
        return if real.starts_with(&real_root) {
            Some(full.to_path_buf())
        } else {
            None
        };
    }
    let mut dir = full.parent()?.to_path_buf();
    loop {
        if let Ok(real_dir) = std::fs::canonicalize(&dir) {
            return if real_dir == real_root || real_dir.starts_with(&real_root) {
                Some(full.to_path_buf())
            } else {
                None
            };
        }
        let parent = dir.parent()?.to_path_buf();
        if parent == dir {
            return None;
        }
        dir = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn tmp(prefix: &str) -> PathBuf {
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .unwrap()
            .keep();
        dir
    }

    // ---- confine_real_abs (async — flow::runner) ----

    #[tokio::test]
    async fn returns_the_lexical_path_unchanged_for_an_ordinary_path_inside_root() {
        let root = tmp("tome-confine-root-");
        let full = root.join("a").join("b");
        std::fs::create_dir_all(&full).unwrap();
        assert_eq!(confine_real_abs(&root, &full, true).await, Some(full));
    }

    #[tokio::test]
    async fn must_exist_true_follows_a_symlinked_file_outside_root_and_rejects_it() {
        let root = tmp("tome-confine-root-");
        let outside = tmp("tome-confine-outside-");
        let target = outside.join("secret.flow.json");
        std::fs::write(&target, "{}").unwrap();
        let link = root.join("evil.flow.json");
        symlink(&target, &link).unwrap();
        assert_eq!(confine_real_abs(&root, &link, true).await, None);
    }

    #[tokio::test]
    async fn must_exist_true_rejects_a_path_through_a_symlinked_ancestor_directory() {
        let root = tmp("tome-confine-root-");
        let outside = tmp("tome-confine-outside-");
        std::fs::write(outside.join("x.flow.json"), "{}").unwrap();
        symlink(&outside, root.join("linked")).unwrap();
        assert_eq!(
            confine_real_abs(&root, &root.join("linked").join("x.flow.json"), true).await,
            None
        );
    }

    #[tokio::test]
    async fn must_exist_false_walks_up_and_rejects_a_symlinked_ancestor() {
        let root = tmp("tome-confine-root-");
        let outside = tmp("tome-confine-outside-");
        symlink(&outside, root.join("flows")).unwrap();
        let not_yet_created = root.join("flows").join("myflow").join("runs").join("r1");
        assert_eq!(confine_real_abs(&root, &not_yet_created, false).await, None);
    }

    #[tokio::test]
    async fn must_exist_false_accepts_a_not_yet_created_path_with_a_real_ancestor() {
        let root = tmp("tome-confine-root-");
        let not_yet_created = root.join("flows").join("myflow").join("runs").join("r1");
        assert_eq!(
            confine_real_abs(&root, &not_yet_created, false).await,
            Some(not_yet_created)
        );
    }

    #[tokio::test]
    async fn rejects_a_path_not_even_lexically_inside_root_and_root_itself_does_not_count() {
        let root = tmp("tome-confine-root-");
        let outside = tmp("tome-confine-outside-");
        assert_eq!(
            confine_real_abs(&root, &outside.join("x"), true).await,
            None
        );
        assert_eq!(confine_real_abs(&root, &root, true).await, None);
    }

    #[tokio::test]
    async fn rejects_a_root_that_does_not_exist_at_all() {
        let root = tmp("tome-confine-root-");
        let ghost_root = root.join("never-created");
        assert_eq!(
            confine_real_abs(&ghost_root, &ghost_root.join("x"), false).await,
            None
        );
    }

    // ---- confine_real_abs_sync — same contract, synchronous ----

    #[test]
    fn sync_returns_the_lexical_path_unchanged_for_an_ordinary_path_inside_root() {
        let root = tmp("tome-confine-root-");
        let full = root.join("a.flow.json");
        assert_eq!(confine_real_abs_sync(&root, &full, false), Some(full));
    }

    #[test]
    fn sync_follows_a_symlinked_ancestor_directory_and_rejects_it() {
        let root = tmp("tome-confine-root-");
        let outside = tmp("tome-confine-outside-");
        symlink(&outside, root.join("flows")).unwrap();
        let full = root.join("flows").join("x.flow.json");
        assert_eq!(confine_real_abs_sync(&root, &full, false), None);
    }

    #[test]
    fn sync_follows_a_symlinked_file_and_rejects_it() {
        let root = tmp("tome-confine-root-");
        let outside = tmp("tome-confine-outside-");
        let target = outside.join("secret.flow.json");
        std::fs::write(&target, "{}").unwrap();
        let link = root.join("evil.flow.json");
        symlink(&target, &link).unwrap();
        assert_eq!(confine_real_abs_sync(&root, &link, true), None);
    }
}
