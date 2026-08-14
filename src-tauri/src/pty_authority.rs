//! The policy decisions a pty-spawn command must own instead of trusting
//! the renderer for (TOME-001): whether a pane is actually gapped, what
//! directory it starts in, and whether an ungapped spawn needs a fresh
//! re-auth ceremony first. Ports `src/main/lib/pty-authority.js`, pinned
//! by `test/pty-authority.test.js` (ported below as `#[cfg(test)] mod
//! tests`).
//!
//! All three used to come straight from the renderer with no main-side
//! check at all: `airgap: gapped` was passed through verbatim even while
//! the stored `airgap-default` preference wanted every pane gapped, and
//! `cwd` reached the spawn call unchanged, unlike every other
//! renderer-supplied path in this app (`confine::confined_real_path`,
//! `confine::confine`). Extracted so all three decisions are unit-testable
//! without a live Tauri app behind them; the future `ipc::pty::pty_create`
//! is the only intended caller of any of them.

// Every item below is exercised by its own #[cfg(test)] suite, but in a
// plain (non-test) build nothing calls any of it yet — same rationale as
// `agent_spawn.rs`'s module-level allow (see that module's top doc
// comment): the real caller (`ipc::pty::pty_create`) is a different
// slice's file and still a stub as of this slice landing.
#![allow(dead_code)]

use std::path::{Component, Path, PathBuf};

// ---- gapping ----

/// The renderer may ask for MORE isolation than policy requires (a
/// per-pane "run this one gapped" toggle) but can never ask for less: when
/// policy wants panes gapped by default, a renderer request of
/// `gapped: false` is overridden, not honored.
///
/// `renderer_gapped`/`policy_default` are plain `bool` rather than a port
/// of the JS original's tolerance for arbitrary falsy shapes
/// (`undefined`, `null`, `0`, `''`) — a Tauri command parameter typed
/// `bool`/`Option<bool>` can only ever be a real boolean (or absent) by
/// the time it reaches Rust code, so those extra JS shapes have no
/// analogue to port; the caller folds an absent renderer value to `false`
/// before calling in, same as it must already do for `policy_default`
/// (`index.js` computes that side as `(await readStore('airgap-default'))
/// !== false`).
pub fn resolve_gapping(renderer_gapped: bool, policy_default: bool) -> bool {
    renderer_gapped || policy_default
}

// ---- spawn cwd ----

/// `path.resolve(p)`'s single-argument behaviour: absolute-ify against the
/// process's current directory if `p` isn't already absolute, then
/// lexically collapse `.`/`..`/repeated separators (clamping `..` at the
/// root instead of erroring, same as Node) — never touches disk, never
/// requires `p` to exist. Duplicated from the equivalent private helper in
/// `confine.rs` rather than shared: that module belongs to a different
/// phase-1 slice, and this slice's file ownership (`agent_spawn.rs`,
/// `custom_agents.rs`, `pty_authority.rs`, `agent_env.rs` — all new files)
/// keeps this one self-contained rather than reaching into another
/// slice's internals. The JS originals make the identical trade: both
/// `pty-authority.js` and `confine.js` independently import `resolve`
/// from `node:path` rather than sharing a helper of their own.
fn resolve_absolute(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut root = PathBuf::new();
    let mut stack: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in abs.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => root.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                stack.pop();
            }
            Component::Normal(seg) => stack.push(seg),
        }
    }
    root.extend(stack);
    root
}

/// Port of `isInside(abs, base)`. The JS original guards a sibling-prefix
/// escape (`"<root>-evil"` must not pass for open root `"<root>"`) with a
/// manual `abs.startsWith(base + sep)` check, because JS string
/// `startsWith` has no concept of path-component boundaries. Rust's
/// `Path::starts_with` is already component-aware — "foo-evil" is not
/// treated as prefixed by "foo" — so no equivalent manual guard is needed
/// here; `sibling_prefix_of_a_root_is_not_inside_it` below pins that this
/// stdlib property actually holds, which is what makes dropping the
/// manual check safe.
fn is_inside(abs: &Path, base: &Path) -> bool {
    !base.as_os_str().is_empty() && (abs == base || abs.starts_with(base))
}

/// A pane's STARTING directory only — a shell is free to `cd` anywhere the
/// moment it's running, so unlike `confine::confined_real_path` this is
/// not a filesystem confinement boundary. What it closes is a compromised
/// (or merely buggy) renderer handing the spawn call an arbitrary `cwd`
/// outright: a path that doesn't exist (the spawn would fail outright), or
/// one with no relationship to the workspace at all. Accepted only when
/// `cwd` names an existing directory inside one of the open workspace
/// `roots` or inside the user's `home` subtree; anything else — outside
/// both, or just not there — falls back to `home`, the same default the
/// original already used for a missing `cwd`.
///
/// `cwd` is `Option<&str>` rather than a port of the JS original's
/// `typeof cwd !== 'string'` tolerance for non-string values (`undefined`,
/// `null`, `42`) — see this module's top doc comment and
/// `agent_spawn.rs`'s for the same type-level simplification. `roots`
/// being `&[]` covers both the JS test's "empty array" and "roots
/// altogether missing" cases: the JS original already folds both to `[]`
/// via `Array.isArray(roots) ? roots : []` before either one reaches this
/// logic, so there is no third state to distinguish.
pub fn resolve_spawn_cwd(cwd: Option<&str>, roots: &[PathBuf], home: &Path) -> PathBuf {
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else {
        return home.to_path_buf();
    };
    let abs = resolve_absolute(Path::new(cwd));
    if !roots.iter().any(|r| is_inside(&abs, r)) && !is_inside(&abs, home) {
        return home.to_path_buf();
    }
    match std::fs::metadata(&abs) {
        Ok(meta) if meta.is_dir() => abs,
        // Doesn't exist, isn't a directory, or unreadable — never hand
        // the spawn call a dead cwd.
        _ => home.to_path_buf(),
    }
}

// ---- unrestricted-spawn ceremony (TOME-001) ----

/// An ungapped pane is an unsandboxed shell/agent with the user's full
/// privileges and open egress — the exact authority a compromised
/// renderer must not be able to seize on its own. So the spawn command
/// requires a fresh second-factor re-auth before it spawns one, EVERY time
/// (the product decision was "re-auth per action", not a time-boxed
/// grant). This returns whether that ceremony applies to a given spawn:
/// only when the resolved pane is ungapped AND the app has an auth factor
/// configured to re-prove — an app with no passphrase set has no factor to
/// check, and gapped panes are already contained by the sandbox.
pub fn unrestricted_spawn_needs_reauth(effective_gapped: bool, auth_configured: bool) -> bool {
    !effective_gapped && auth_configured
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // ---- resolve_gapping ----

    #[test]
    fn renderer_requests_no_gap_but_policy_defaults_to_gapped_gapped_wins() {
        assert!(resolve_gapping(false, true));
    }

    #[test]
    fn renderer_requests_a_gap_even_though_policy_default_is_off_still_gapped() {
        assert!(resolve_gapping(true, false));
    }

    #[test]
    fn both_sides_agree_there_is_no_gap_ungapped() {
        assert!(!resolve_gapping(false, false));
    }

    #[test]
    fn both_sides_agree_there_is_a_gap_gapped() {
        assert!(resolve_gapping(true, true));
    }

    #[test]
    fn the_renderer_can_only_ever_add_isolation_never_remove_what_policy_wants() {
        // TOME-001 case: a compromised renderer sending gapped:false must
        // not escape a "gap by default" policy.
        assert!(resolve_gapping(false, true));
    }

    // ---- resolve_spawn_cwd ----

    struct Dirs {
        root: PathBuf,
        sibling: PathBuf,
        home: PathBuf,
        _root_guard: tempfile::TempDir,
        _sibling_guard: tempfile::TempDir,
        _home_guard: tempfile::TempDir,
    }

    fn make_dirs() -> Dirs {
        let root_guard = tempdir().unwrap();
        let sibling_guard = tempdir().unwrap();
        let home_guard = tempdir().unwrap();
        Dirs {
            root: root_guard.path().canonicalize().unwrap(),
            sibling: sibling_guard.path().canonicalize().unwrap(),
            home: home_guard.path().canonicalize().unwrap(),
            _root_guard: root_guard,
            _sibling_guard: sibling_guard,
            _home_guard: home_guard,
        }
    }

    #[test]
    fn accepts_a_directory_inside_an_open_workspace_root() {
        let d = make_dirs();
        let dir = d.root.join("sub");
        fs::create_dir(&dir).unwrap();
        assert_eq!(resolve_spawn_cwd(dir.to_str(), &[d.root.clone()], &d.home), dir);
    }

    #[test]
    fn accepts_the_root_itself() {
        let d = make_dirs();
        assert_eq!(resolve_spawn_cwd(d.root.to_str(), &[d.root.clone()], &d.home), d.root);
    }

    #[test]
    fn accepts_a_directory_inside_the_home_subtree_even_when_no_root_matches() {
        let d = make_dirs();
        let dir = d.home.join("projects").join("x");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(resolve_spawn_cwd(dir.to_str(), &[d.root.clone()], &d.home), dir);
    }

    #[test]
    fn accepts_home_itself() {
        let d = make_dirs();
        assert_eq!(resolve_spawn_cwd(d.home.to_str(), &[d.root.clone()], &d.home), d.home);
    }

    #[test]
    fn falls_back_to_home_for_a_directory_outside_every_root_and_outside_home() {
        let d = make_dirs();
        assert_eq!(resolve_spawn_cwd(d.sibling.to_str(), &[d.root.clone()], &d.home), d.home);
    }

    #[test]
    fn sibling_prefix_of_a_root_is_not_inside_it() {
        // "<root>-evil" must not pass for open root "<root>" — string-
        // prefix matching without a separator boundary would wrongly
        // allow this, the same trap confine.rs's confinement guards
        // avoid. Pins that Path::starts_with's component-awareness
        // actually holds (see `is_inside`'s doc comment).
        let d = make_dirs();
        let evil = PathBuf::from(format!("{}-evil", d.root.display()));
        fs::create_dir(&evil).unwrap();
        assert_eq!(resolve_spawn_cwd(evil.to_str(), &[d.root.clone()], &d.home), d.home);
    }

    #[test]
    fn falls_back_to_home_for_a_path_that_does_not_exist() {
        let d = make_dirs();
        let never = d.root.join("never-created");
        assert_eq!(resolve_spawn_cwd(never.to_str(), &[d.root.clone()], &d.home), d.home);
    }

    #[test]
    fn falls_back_to_home_for_an_existing_file_not_a_directory() {
        let d = make_dirs();
        let file = d.root.join("a.txt");
        fs::write(&file, "x").unwrap();
        assert_eq!(resolve_spawn_cwd(file.to_str(), &[d.root.clone()], &d.home), d.home);
    }

    #[test]
    fn falls_back_to_home_for_none_or_empty_cwd_without_panicking() {
        let d = make_dirs();
        assert_eq!(resolve_spawn_cwd(None, &[d.root.clone()], &d.home), d.home);
        assert_eq!(resolve_spawn_cwd(Some(""), &[d.root.clone()], &d.home), d.home);
    }

    #[test]
    fn falls_back_to_home_when_no_workspace_roots_are_open_yet() {
        let d = make_dirs();
        let dir = d.root.join("sub2");
        fs::create_dir(&dir).unwrap();
        assert_eq!(resolve_spawn_cwd(dir.to_str(), &[], &d.home), d.home);
    }

    #[test]
    fn picks_whichever_open_root_actually_contains_the_path_when_several_are_open() {
        let d = make_dirs();
        let other_guard = tempdir().unwrap();
        let other = other_guard.path().canonicalize().unwrap();
        let dir = other.join("sub");
        fs::create_dir(&dir).unwrap();
        assert_eq!(resolve_spawn_cwd(dir.to_str(), &[d.root.clone(), other.clone()], &d.home), dir);
    }

    // ---- unrestricted_spawn_needs_reauth ----

    #[test]
    fn ungapped_pane_with_a_configured_factor_needs_the_ceremony() {
        assert!(unrestricted_spawn_needs_reauth(false, true));
    }

    #[test]
    fn gapped_pane_never_asks_regardless_of_config() {
        assert!(!unrestricted_spawn_needs_reauth(true, true));
        assert!(!unrestricted_spawn_needs_reauth(true, false));
    }

    #[test]
    fn ungapped_but_no_factor_configured_nothing_to_prove() {
        assert!(!unrestricted_spawn_needs_reauth(false, false));
    }
}
