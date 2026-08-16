//! The two policy decisions `lsp.js` delegates to `lib/lsp-policy.js`
//! (TOME-003), ported 1:1 and pinned against `test/lsp-policy.test.js`'s
//! exact assertions below: which workspace root a renderer-supplied path
//! is allowed to resolve to ([`confine_to_root`]), and what environment a
//! language server gets launched with ([`resolve_server_env`]). Both used
//! to be more permissive — root resolution fell back to the opened file's
//! own directory when it wasn't inside any open folder, and that same
//! root's `node_modules/.bin` was prepended to the spawned server's PATH —
//! so a compromised renderer (or a prompt-injected path from the
//! conductor) could point an LSP process, and the binary that ran in its
//! place, at a directory of its choosing. The tests that matter are
//! exactly the refusals: an out-of-root path must never resolve to a
//! root, and no root can ever make it into the spawn PATH.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---- root confinement ----

/// Every LSP entry point (didOpen/didChange/didClose/hover/definition) is
/// driven by a path the renderer names, and the root this resolves to
/// becomes the spawned server's cwd (and, via [`resolve_server_env`],
/// steers what actually gets spawned). Out of root means refused, not
/// "root somewhere else" — mirrors `confineToRoot`'s own doc comment.
///
/// Ports `resolve(path)` + the prefix-match-and-pick-longest logic
/// lexically (no filesystem access, no symlink resolution — same
/// guarantee `path.resolve()` gives in the JS original). `Path::starts_with`
/// is component-wise, not a raw string prefix, which is what makes the
/// "same-prefix sibling" test below pass without this needing to manually
/// append a separator the way the JS original's `abs.startsWith(f + sep)`
/// does.
///
/// Ties (two open folders of equal length both matching — not exercised
/// by any real workspace today, since open folders are deduplicated
/// paths, but possible in principle) resolve to whichever came FIRST in
/// `folders`, matching the JS original's stable `.sort()` + `[0]`
/// (a stable descending sort keeps the earliest-appearing element among
/// ties at index 0) — not Rust's `Iterator::max_by_key`, which would keep
/// the LAST, so this is a hand-rolled fold rather than that adaptor.
pub fn confine_to_root(path: &str, folders: &[PathBuf]) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let abs = resolve_lexically(Path::new(path));
    let mut best: Option<&PathBuf> = None;
    for f in folders {
        if f.as_os_str().is_empty() || !abs.starts_with(f) {
            continue;
        }
        best = match best {
            Some(b) if f.as_os_str().len() <= b.as_os_str().len() => Some(b),
            _ => Some(f),
        };
    }
    best.cloned()
}

/// Lexical equivalent of Node's `path.resolve(p)`: joins onto the current
/// directory when relative, then collapses `.`/`..` components without
/// touching the filesystem (a `..` at the root simply has nowhere to pop
/// to, same as `path.resolve`'s own root-clamping behavior).
fn resolve_lexically(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---- executable resolution ----

/// Servers are launched by bare command name ([`super::SERVERS`]) and
/// resolved by the OS via PATH lookup. This used to prepend
/// `<root>/node_modules/.bin` so a project-pinned server won over a global
/// install — but `root` can be a renderer-chosen (if confined, see
/// [`confine_to_root`]) workspace folder, so that prefix let a compromised
/// renderer plant its own binary at `<root>/node_modules/.bin/<cmd>` and
/// have it run in place of the real language server. The trusted policy is
/// simply the given base environment, unmodified, returned as an
/// independent copy — never a per-workspace override. `root` is accepted
/// (not just available to a caller as a separate value) so the signature
/// itself documents that it must NOT feed the result, and a test can pin
/// that varying it never changes the output.
#[allow(unused_variables)] // root: signature-only, see doc comment above
pub fn resolve_server_env(
    root: &Path,
    base_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    base_env.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> PathBuf {
        PathBuf::from("/workspace/proj")
    }
    fn other_ws() -> PathBuf {
        PathBuf::from("/workspace/other")
    }

    // ================= confine_to_root =================

    #[test]
    fn confine_to_root_accepts_a_path_inside_the_open_folder_returning_that_folder_as_root() {
        let folders = vec![ws()];
        assert_eq!(
            confine_to_root("/workspace/proj/src/index.ts", &folders),
            Some(ws())
        );
        assert_eq!(confine_to_root("/workspace/proj", &folders), Some(ws())); // the folder itself
    }

    #[test]
    fn confine_to_root_rejects_a_path_outside_every_open_folder() {
        let folders = vec![ws()];
        assert_eq!(confine_to_root("/etc/passwd", &folders), None);
        assert_eq!(confine_to_root("/Users/evil/file.ts", &folders), None);
    }

    #[test]
    fn confine_to_root_rejects_when_no_folders_are_open_at_all() {
        assert_eq!(confine_to_root("/workspace/proj/a.ts", &[]), None);
    }

    #[test]
    fn confine_to_root_does_not_fall_back_to_the_opened_files_own_directory() {
        // The old rootFor() returned dirname(path) for anything unmatched,
        // which rooted a language server — and, via the old PATH prefix, ran
        // a binary — at a directory a compromised renderer chose outright.
        // confine_to_root must refuse instead of picking a substitute root.
        let folders = vec![ws()];
        assert_eq!(confine_to_root("/tmp/evil-project/file.ts", &folders), None);
    }

    #[test]
    fn confine_to_root_picks_the_most_specific_matching_folder_when_open_folders_nest() {
        let nested = ws().join("packages").join("sub");
        let file = nested.join("index.ts");
        let folders = vec![ws(), nested.clone()];
        assert_eq!(
            confine_to_root(file.to_str().unwrap(), &folders),
            Some(nested)
        );
    }

    #[test]
    fn confine_to_root_does_not_treat_a_same_prefix_sibling_folder_as_inside_the_workspace() {
        // "/workspace/proj-evil" must not pass for open folder "/workspace/proj".
        let sibling_file = PathBuf::from("/workspace/proj-evil/file.ts");
        let folders = vec![ws()];
        assert_eq!(
            confine_to_root(sibling_file.to_str().unwrap(), &folders),
            None
        );
    }

    #[test]
    fn confine_to_root_ignores_folders_that_are_not_the_one_the_path_is_actually_under() {
        assert_eq!(confine_to_root("/workspace/proj/a.ts", &[other_ws()]), None);
        assert_eq!(
            confine_to_root("/workspace/proj/a.ts", &[other_ws(), ws()]),
            Some(ws())
        );
    }

    #[test]
    fn confine_to_root_rejects_an_empty_path_without_panicking() {
        // The JS suite also pins null/undefined/non-string inputs; those
        // have no meaningful Rust equivalent (the type system already
        // rejects them at the IPC deserialization boundary, before this
        // function is ever called), so only the empty-string case — which
        // IS a real, valid `String` value — carries over.
        assert_eq!(confine_to_root("", &[ws()]), None);
    }

    // ================= resolve_server_env =================

    fn base_env() -> HashMap<String, String> {
        HashMap::from([
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("HOME".to_string(), "/Users/tester".to_string()),
        ])
    }

    #[test]
    fn resolve_server_env_returns_the_base_environment_untouched() {
        let base = base_env();
        assert_eq!(resolve_server_env(&ws(), &base), base);
        assert_eq!(
            resolve_server_env(&ws(), &base).get("PATH"),
            base.get("PATH")
        );
    }

    #[test]
    fn resolve_server_env_never_prepends_the_workspace_roots_node_modules_bin() {
        // The removed behavior built `${root}/node_modules/.bin:${PATH}` — a
        // compromised renderer could point `root` at a directory holding its
        // own "typescript-language-server" and have that run in the real
        // server's place. Pin that no root, however chosen, makes it back
        // into PATH.
        let base = base_env();
        for root in [ws(), other_ws(), PathBuf::from("/tmp/evil-project")] {
            let env = resolve_server_env(&root, &base);
            assert_eq!(env.get("PATH"), base.get("PATH"));
            assert!(!env["PATH"].contains("node_modules"));
            assert!(!env["PATH"].contains(root.to_str().unwrap()));
        }
    }

    #[test]
    fn resolve_server_env_returns_an_independent_copy_not_the_same_map() {
        let base = base_env();
        let mut env = resolve_server_env(&ws(), &base);
        env.insert("PATH".to_string(), "/mutated".to_string());
        assert_eq!(base.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
        // original untouched
    }
}
