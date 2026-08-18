//! `tome-shim`: the in-sandbox helper that makes the Linux egress real
//! (Phase 4 — see this repo's rewrite plan, "Linux sandbox" section, and
//! `src-tauri/src/ipc/pty.rs`'s `resolve_gapped_spawn`, which this crate
//! replaces the fail-closed refusal for on Linux). macOS enforcement
//! (`sandbox-exec` + the seatbelt profile, Phase 3) needs no analogue of
//! this binary at all — it is Linux-only, full stop, including this
//! `main()`'s own dispatch below.
//!
//! ## The problem this binary solves
//!
//! A gapped pane runs inside a fresh network namespace (deny-all egress by
//! construction — the actual TOME-001 fix). But a fresh netns cannot reach
//! the host's real per-pane proxy at `127.0.0.1:<port>` — namespaces
//! isolate network stacks from each other, that's the whole point. The
//! ONE thing that legitimately crosses the boundary is a bind-mounted unix
//! domain socket (`egress::proxy::PaneProxy::spawn`'s `unix_socket_path`
//! seam, already landed in Phase 3 — see that module's "Linux seam" doc
//! section). `tome-shim` is what turns that one crossable socket back into
//! a normal-looking TCP proxy INSIDE the namespace, on the SAME port
//! number the host chose, so that `HTTP_PROXY=http://127.0.0.1:<port>`
//! (set once, by `agent_env.rs`, identically on every OS) is byte-for-byte
//! true regardless of which side of the netns boundary the agent process
//! actually runs on.
//!
//! ## Policy vs. mechanism (why this crate is split the way it is)
//!
//! This whole crate was authored on macOS, which has no network
//! namespaces, no `unshare(2)` isolation, nothing to actually RUN this
//! binary's Linux path against. What CAN be verified here:
//!
//! - `tome_shim::args`: pure argv parsing — no `nix`/`libc` types anywhere
//!   in it, so it compiles and its `#[cfg(test)]` suite runs on every host,
//!   including this one. Also importable from the MAIN `tome` package's own
//!   test suite (this crate's `[lib]` target exists for exactly that — see
//!   `lib.rs`'s doc comment), for the cross-crate contract test that feeds
//!   `egress::linux`'s real argv builders through this crate's real parser.
//! - `tome_shim::pure`: small OS-agnostic helpers pulled out of the Linux
//!   mechanism specifically so THEY can be unit-tested here too, even
//!   though their only real caller (`linux.rs`) cannot run here.
//! - `tome_shim::linux` (this file's `#[cfg(target_os = "linux")] use
//!   tome_shim::linux;` below): the actual `unshare`/ioctl/`capset`/
//!   `fork+exec`/signal mechanism. Type-checked via `cargo check --target
//!   x86_64-unknown-linux-gnu` (this crate's primary gate for anything in
//!   that module) but never executed on this host. See `linux.rs`'s own
//!   top doc comment for the exact verification boundary and what a later
//!   slice's CI-gated integration tests are expected to actually prove.
//!
//! Every design decision in `linux.rs` is documented against the man pages
//! it's built from, but "compiles against the real Linux ABI and matches
//! the cited man page" is a categorically weaker claim than "observed to
//! work" — this file, and every doc comment in `linux.rs`, is careful to
//! never claim the latter.

#[cfg(target_os = "linux")]
use tome_shim::{args, linux};

fn main() {
    #[cfg(target_os = "linux")]
    {
        let raw: Vec<String> = std::env::args().skip(1).collect();
        match args::parse_args(raw) {
            Ok(parsed) => linux::run(parsed),
            Err(e) => {
                eprintln!("tome-shim: {e}");
                std::process::exit(2);
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    eprintln!("tome-shim is Linux-only");
}
