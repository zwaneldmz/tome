//! `tome-shim` as a library — exists purely so this package's own argv
//! parser ([`args::parse_args`]) is importable from a DIFFERENT crate's
//! test suite (the main `tome` package, as a `[dev-dependencies]` path
//! dependency), for the cross-crate contract test `args.rs`'s own top doc
//! comment describes: feeding the REAL output of
//! `airgap::linux::build_bwrap_argv`/`build_self_unshare_argv` (the sibling
//! crate that builds this binary's argv) through the REAL [`args::parse_args`]
//! (this crate, the thing that actually has to accept that argv at spawn
//! time), so the two sides of this wire contract can never drift silently
//! again the way they did before this file existed (see `args.rs`'s doc
//! comment for the concrete incident).
//!
//! `src/main.rs` is the ACTUAL shipped sidecar — a thin binary that calls
//! straight into this same module tree (`tome_shim::args::parse_args`,
//! `tome_shim::linux::run`). Splitting lib+bin like this changes nothing
//! about what ships: `[[bin]] name = "tome-shim"` in `Cargo.toml` is still
//! the only artifact `scripts/build-sidecar.sh`/`tauri.conf.json`'s
//! `externalBin` ever stage or bundle — this file adds an import seam, not
//! a second binary.
//!
//! Module-level `#[cfg]`s mirror `main.rs`'s own (see that file's top doc
//! comment for the policy/mechanism split this crate is built around):
//! [`args`] and [`pure`] are OS-unconditional and compile on every host
//! this workspace builds on, including the macOS host that depends on this
//! crate purely for its own native `cargo test` run; [`linux`] is
//! `#[cfg(target_os = "linux")]`, so a macOS build of the DEPENDENT crate
//! (`tome`) never even parses it, let alone links `nix`/`libc`.

pub mod args;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod pure;
