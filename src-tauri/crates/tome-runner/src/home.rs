//! `$HOME`-relative path resolution — this crate has no `dirs`/
//! `directories` dependency (out of this slice's grant; see `Cargo.toml`'s
//! own note), so every well-known directory this binary reads or writes is
//! a plain join against `$HOME`, resolved here once and threaded through
//! explicitly rather than re-read ad hoc at each call site.
//!
//! Also home to [`lexical_resolve`] — `path.resolve(p)`'s single-argument,
//! no-symlinks-followed behavior, duplicated from the main crate's
//! `flow_env.rs::lexical_resolve` (out of this slice's file surface, and
//! not `pub` even if it were reachable) for the same reason that module's
//! own doc comment gives for ITS copy: small, self-contained, and cheaper
//! to duplicate once than to widen a file this slice does not own just to
//! export one helper.

use std::path::{Path, PathBuf};

/// Pure half of [`home_dir`]: is this already-read `$HOME` value actually
/// usable? Split out so the interesting logic (empty string treated as
/// unset, matching a shell's own `${HOME:-}`) has a `#[cfg(test)]` that
/// never mutates real process environment — `login_env.rs`'s own doc
/// comment explains why a test that calls `std::env::set_var` on a
/// variable every parallel test shares would be flaky by construction, not
/// a meaningful check; the same reasoning applies to `$HOME` here.
fn usable_home(candidate: Option<std::ffi::OsString>) -> Option<PathBuf> {
    candidate
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// `$HOME`, or `None` if unset/empty. A `systemd --user` unit always has
/// `$HOME` set (systemd populates it from the passwd entry before any
/// unit's own `Environment=`/`EnvironmentFile=` ever runs), so every real
/// caller in this binary treats a missing `$HOME` as a hard configuration
/// error — never silently defaulted to `/` or the current directory.
pub fn home_dir() -> Option<PathBuf> {
    usable_home(std::env::var_os("HOME"))
}

/// `~/.config/tome-runner` — server-owner-writable configuration: the
/// egress allowlist ([`crate::egress_config`]) and the `env` file the
/// systemd `EnvironmentFile=` directive reads provider credentials from
/// (see `docs/remote-runner.md`). This binary only ever reads under this
/// directory, never writes to it.
pub fn config_dir(home: &Path) -> PathBuf {
    home.join(".config").join("tome-runner")
}

/// `~/.local/state/tome-runner` — this binary's own runtime state (today:
/// just `events.jsonl`, see [`crate::events`]). `$XDG_STATE_HOME`'s
/// documented default location for "logs and history a program doesn't
/// want to lose, but that isn't config a person hand-edits."
pub fn state_dir(home: &Path) -> PathBuf {
    home.join(".local").join("state").join("tome-runner")
}

/// `path.resolve(p)`'s single-argument behavior: absolutize against the
/// current directory when `p` is relative, then collapse `.`/`..`
/// components lexically — no filesystem access, no symlink ever followed
/// (that is `flow::confine::confine_real_abs`'s job, applied deeper inside
/// `tome_flow::flow::runner::start_run` itself; see this crate's
/// `runner_env.rs` for where the two meet).
pub fn lexical_resolve(p: &Path) -> PathBuf {
    use std::path::Component;
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- usable_home ----

    #[test]
    fn usable_home_accepts_a_non_empty_value() {
        assert_eq!(
            usable_home(Some("/home/tester".into())),
            Some(PathBuf::from("/home/tester"))
        );
    }

    #[test]
    fn usable_home_treats_unset_and_empty_the_same_way() {
        assert_eq!(usable_home(None), None);
        assert_eq!(usable_home(Some("".into())), None);
    }

    // ---- config_dir / state_dir ----

    #[test]
    fn config_dir_joins_the_fixed_suffix() {
        assert_eq!(
            config_dir(Path::new("/home/tester")),
            PathBuf::from("/home/tester/.config/tome-runner")
        );
    }

    #[test]
    fn state_dir_joins_the_fixed_suffix() {
        assert_eq!(
            state_dir(Path::new("/home/tester")),
            PathBuf::from("/home/tester/.local/state/tome-runner")
        );
    }

    // ---- lexical_resolve ----

    #[test]
    fn lexical_resolve_normalizes_dot_and_dotdot_without_touching_disk() {
        assert_eq!(
            lexical_resolve(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(lexical_resolve(Path::new("/a/./b")), PathBuf::from("/a/b"));
    }

    #[test]
    fn lexical_resolve_absolutizes_a_relative_path_against_the_cwd() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            lexical_resolve(Path::new("myflow.flow.json")),
            cwd.join("myflow.flow.json")
        );
    }

    #[test]
    fn lexical_resolve_lets_dotdot_climb_above_the_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let expected = cwd.parent().unwrap_or(&cwd).join("sibling");
        assert_eq!(lexical_resolve(Path::new("../sibling")), expected);
    }

    #[test]
    fn lexical_resolve_leaves_an_already_absolute_path_unchanged_modulo_normalization() {
        assert_eq!(
            lexical_resolve(Path::new("/already/absolute")),
            PathBuf::from("/already/absolute")
        );
    }
}
