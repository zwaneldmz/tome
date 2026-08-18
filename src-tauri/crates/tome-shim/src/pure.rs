//! Small OS-agnostic helpers factored out of `linux.rs` purely so they can
//! carry their own `#[cfg(test)]` coverage that runs on every host (see
//! `main.rs`'s top doc comment for why that split matters for this crate
//! specifically: the mechanism they're used FROM is `#[cfg(target_os =
//! "linux")]` and untestable here). Neither function below references
//! `nix`/`libc` — both crates are Linux-only dependencies (see
//! `Cargo.toml`'s `[target.'cfg(target_os = "linux")'.dependencies]`), so
//! anything in this file that touched them would fail to compile on every
//! other target, defeating the point.
//!
//! Everything here is exercised by `linux.rs` at runtime on Linux only;
//! `#[allow(dead_code)]` at module level (rather than scattered over each
//! item) because on a non-Linux `cargo check`/`cargo test`, `linux.rs`
//! doesn't exist as a compiled module at all, so nothing calls these —
//! same rationale this workspace already uses in
//! `src-tauri/src/egress/mod.rs` and `src-tauri/src/pty_authority.rs` for
//! code whose only caller is a different slice/target.
#![allow(dead_code)]

/// Maps a `std::process::ExitStatus`'s `(code, signal)` pair — as read via
/// `ExitStatusExt` on the raw wait status `linux::run` gets back from
/// `Child::wait()` — to the single integer `tome-shim` itself exits with,
/// so the pty layer watching THIS process sees the same convention any
/// shell/init does: the child's own exit code when it exited normally, or
/// 128+signal when a signal killed it (the same convention `sh`/`bash`
/// use for `$?`, and what `portable-pty`'s own exit-code reporting
/// upstream already expects to parse). `code`/`signal` are mutually
/// exclusive in a real `ExitStatus` (a process exits via exactly one of
/// the two) but nothing here assumes that — `code` wins if somehow both
/// are `Some`, and the fallback constant covers the (should-never-happen)
/// case where the OS reports neither.
pub fn exit_code_from(code: Option<i32>, signal: Option<i32>) -> i32 {
    match code {
        Some(c) => c,
        None => 128_i32.saturating_add(signal.unwrap_or(0)),
    }
}

/// Formats one line of `/proc/self/{uid,gid}_map` content — `self_unshare`
/// (fallback-ladder step 2, `linux.rs`) writes exactly one such line to
/// each file, mapping `inside` (the id this process appears to have once
/// inside the fresh user namespace — always `0`, that is it looks like root
/// to itself, at every real call site) to `outside` (the real,
/// already-unprivileged host id this process actually runs as — `1`
/// meaning "a single id", never a range, since `tome-shim` only ever needs
/// to map its own one uid/gid, not delegate a sub-range the way a
/// multi-user container runtime would). See user_namespaces(7)'s
/// "/proc/[pid]/uid_map" section for the three-column format this
/// reproduces.
pub fn id_map_line(inside: u32, outside: u32) -> String {
    format!("{inside} {outside} 1\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- exit_code_from ----

    #[test]
    fn uses_the_exit_code_when_the_process_exited_normally() {
        assert_eq!(exit_code_from(Some(0), None), 0);
        assert_eq!(exit_code_from(Some(1), None), 1);
        assert_eq!(exit_code_from(Some(42), None), 42);
    }

    #[test]
    fn maps_a_terminating_signal_to_128_plus_the_signal_number() {
        // SIGTERM=15, SIGKILL=9 — the shell/init convention this mirrors.
        assert_eq!(exit_code_from(None, Some(15)), 143);
        assert_eq!(exit_code_from(None, Some(9)), 137);
    }

    #[test]
    fn prefers_the_exit_code_when_both_are_somehow_present() {
        assert_eq!(exit_code_from(Some(7), Some(9)), 7);
    }

    #[test]
    fn falls_back_to_128_when_neither_code_nor_signal_is_known() {
        assert_eq!(exit_code_from(None, None), 128);
    }

    // ---- id_map_line ----

    #[test]
    fn formats_a_single_id_mapping_line() {
        assert_eq!(id_map_line(0, 1000), "0 1000 1\n");
    }

    #[test]
    fn always_maps_exactly_one_id_never_a_range() {
        // The trailing column is the mapping LENGTH, not an id — pinned
        // literally so a future edit can't accidentally turn this into a
        // range map without a test noticing.
        assert!(id_map_line(0, 54321).ends_with(" 1\n"));
    }
}
