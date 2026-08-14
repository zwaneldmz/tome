//! The real mechanism — everything in this file is `#[cfg(target_os =
//! "linux")]` and runs only inside a fresh (bwrap- or self-`unshare`d)
//! network namespace. See `main.rs`'s top doc comment for the
//! policy/mechanism split this file is the "mechanism" half of, and this
//! crate's task report for the verification boundary: this module
//! compiles and cross-checks clean (`cargo check --target
//! x86_64-unknown-linux-gnu`) but has never actually RUN, because the host
//! this slice was authored on is macOS, which has no network namespaces to
//! run it in. Nothing here should be read as "proven to work" — only "type
//! -checked against the real Linux ABI, and reviewed against the man
//! pages cited inline." The real proof is the `#[ignore]`d
//! `#[cfg(target_os = "linux")]` integration tests + GitHub Actions ubuntu
//! job a later slice adds, which actually execute this binary inside a
//! real namespace.
//!
//! ## Process shape: `tome-shim` is a tiny supervisor, not `exec`'d away
//!
//! The plan's "Linux sandbox" section describes this binary as running
//! "PID 1 of the wrap," shoveling bytes AND (eventually) exec'ing the
//! agent shell — which reads, at first glance, like a single process doing
//! both forever. It cannot literally be that: `execve` REPLACES the
//! calling process image (and kills every other thread in it), so a
//! process that has already `exec`'d away cannot still be running the
//! accept-loop threads that keep the loopback bridge alive for the rest of
//! the pane's lifetime — and the plan explicitly requires exactly that
//! ("Forward SIGTERM/SIGINT to the child; propagate exit status," which
//! presupposes a SEPARATE child to forward signals to and wait on).
//!
//! So [`run`] never calls `execve` on itself. It: brings `lo` up and binds
//! the namespace-internal TCP listener; spawns the bridge's accept-loop
//! thread (this process keeps running, unlike a self-`exec`); then uses
//! `std::process::Command` (fork+exec under the hood, not a raw hand-rolled
//! `fork()` — this process already has live threads by the time it spawns
//! the child, and raw `fork()` in a multithreaded process is exactly the
//! footgun `Command` exists to paper over) to start the REAL agent shell as
//! a CHILD process, dropping capabilities and arming `PR_SET_PDEATHSIG` in
//! that child via `pre_exec` — i.e. between the fork and the child's own
//! `execve`, exactly where the plan's ordering says. `tome-shim` itself
//! then supervises: forwards `SIGTERM`/`SIGINT` to the child, waits for it,
//! and exits with its translated exit status. This IS the "PID 1 of the
//! wrap" framing — `tome-shim` is the long-lived process the wrap's
//! lifetime is pinned to (its own death, or `bwrap`'s own `--die-with-
//! parent`, is what makes `PR_SET_PDEATHSIG(SIGKILL)` on the child mean
//! anything) — it just achieves that by supervising a child rather than by
//! becoming the agent itself.
//!
//! ## Fallback-ladder step 2 (`--self-unshare`)
//!
//! When invoked without bwrap having already prepared a namespace,
//! [`self_unshare`] does the `CLONE_NEWUSER|CLONE_NEWNET` unshare and
//! uid/gid mapping itself, before anything else in [`run`] executes (see
//! that function's own doc comment for why it must run before any other
//! thread in this process exists). Deliberately **not** attempted here:
//! reproducing bwrap's `--tmpfs <appConfigDir>` filesystem confinement
//! (hiding the store/auth/consents files from a self-`unshare`d pane) —
//! that needs Landlock file rules, left as a clearly-marked follow-up
//! (search this file for "TODO(landlock)"). The network-namespace egress
//! kill is the load-bearing control this slice delivers; the filesystem
//! hardening is real but strictly secondary (macOS's own seatbelt profile
//! is the only OTHER OS this ships on today, and it already denies
//! `file-read*`/`file-write*` on the same paths — a self-`unshare`d Linux
//! pane without Landlock is thus no WORSE than the macOS baseline was
//! before this phase, only not yet BETTER on this one axis).
//!
//! `ShimArgs::deny_write`/`ShimArgs::deny_read` (this rung's would-be
//! Landlock targets) are accepted by `args::parse_args` and threaded all
//! the way here — see that module's own doc comment for the cross-crate
//! wire-contract drift this closes: the argv builder that emits them
//! (`airgap::linux::build_self_unshare_argv`, a sibling crate) landed
//! before this parser had any arm for them, which made every rung-2 spawn
//! crash with `UnknownFlag` before this function ever ran at all. Now that
//! rung 2 can actually reach [`run`]/[`self_unshare`], the gap described in
//! the paragraph above stops being moot (a crashed rung never got far
//! enough to matter) and starts being live: [`run`] prints an explicit
//! stderr NOTE whenever either path is present, so the still-missing
//! file-confinement half of the contract is visible at the point a real
//! operator would actually see it (this process's own stderr), not only in
//! this doc comment.
//!
//! `ShimArgs::new_session` IS fully wired (unlike deny_write/deny_read):
//! [`run`]'s `pre_exec` closure calls `setsid(2)` on the child when set —
//! see `build_self_unshare_argv`'s own doc comment (sibling crate) for why
//! this rung, not bwrap, is the one that has to apply it.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;

use nix::sched::{unshare, CloneFlags};
use nix::sys::prctl;
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::unistd::{getgid, getuid, setsid};

use crate::args::ShimArgs;
use crate::pure::{exit_code_from, id_map_line};

// ---- exit codes for this binary's own failures (distinct from the
// exec'd agent's exit code, which is propagated via exit_code_from) ----
const EXIT_SELF_UNSHARE_FAILED: i32 = 3;
const EXIT_LOOPBACK_UP_FAILED: i32 = 4;
const EXIT_BIND_FAILED: i32 = 5;
const EXIT_SPAWN_FAILED: i32 = 6;
const EXIT_WAIT_FAILED: i32 = 7;

/// Entry point called from `main()` once [`crate::args::parse_args`] has
/// already validated the argv (see that module's doc comment for why
/// arg-parsing is kept separate and OS-agnostic). Never returns: every
/// path out of this function is `std::process::exit`.
pub fn run(args: ShimArgs) -> ! {
    if args.self_unshare {
        // MUST happen before the bridge thread below is spawned:
        // `unshare(CLONE_NEWUSER)` fails with EINVAL if the calling
        // process is already multithreaded (user_namespaces(7)), and this
        // is the very first thread this process ever has.
        if let Err(e) = self_unshare() {
            eprintln!("tome-shim: --self-unshare failed: {e}");
            std::process::exit(EXIT_SELF_UNSHARE_FAILED);
        }
    }

    // Whether bwrap already unshared the net namespace for us or we just
    // did it ourselves above, a fresh netns's loopback interface starts
    // administratively down — every path through here needs this.
    if let Err(e) = bring_loopback_up() {
        eprintln!("tome-shim: failed to bring lo up: {e}");
        std::process::exit(EXIT_LOOPBACK_UP_FAILED);
    }

    let listener = match TcpListener::bind(("127.0.0.1", args.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("tome-shim: failed to bind 127.0.0.1:{}: {e}", args.port);
            std::process::exit(EXIT_BIND_FAILED);
        }
    };
    spawn_bridge(listener, args.sock.clone());

    // `--deny-write`/`--deny-read` are accepted (see args.rs's own doc
    // comment on the cross-crate wire contract) but this rung has no
    // Landlock — or any other — filesystem-confinement mechanism yet (see
    // this module's top doc comment and self_unshare's TODO(landlock)).
    // Say so loudly, on THIS process's own stderr, every time either path
    // is actually present — not only in a source comment a future spawn's
    // operator will never read: the network-namespace egress kill below is
    // real; the config-dir-hidden / auth-file-hidden half of the posture
    // bwrap's rung 1 and macOS's seatbelt profile both provide is NOT, on
    // this rung, yet.
    if args.deny_write.is_some() || args.deny_read.is_some() {
        eprintln!(
            "tome-shim: NOTE this pane is running on the self-unshare fallback rung, which does \
             not yet enforce file confinement (Landlock is a TODO — see linux.rs's self_unshare \
             doc comment): the app config dir ({:?}) and auth file ({:?}) remain readable/writable \
             from inside this sandbox. Network egress IS confined by the fresh network namespace.",
            args.deny_write, args.deny_read
        );
    }

    let new_session = args.new_session;
    let mut command = Command::new(&args.argv[0]);
    command.args(&args.argv[1..]);
    // SAFETY: the closure only calls `setsid(2)`, `prctl(2)` (via nix), and
    // the raw `capset(2)` syscall — all single, allocation-free, lock-free
    // syscalls, so this stays within the async-signal-safe subset
    // `pre_exec` requires (it runs in the forked child, between fork and
    // exec, where the child may have inherited a parent heap/lock state
    // mid-operation from another thread that isn't there to finish it).
    // `new_session` is a plain `bool`, captured by copy — satisfies
    // `pre_exec`'s own `Send + Sync + 'static` bound on the closure without
    // needing any synchronization.
    unsafe {
        command.pre_exec(move || {
            if new_session {
                // Mirrors bwrap's own `--new-session` (setsid before exec)
                // — see `ShimArgs::new_session`'s doc comment for why THIS
                // process has to apply it on rung 2 instead of bwrap. Runs
                // on the CHILD (the eventual agent shell), not tome-shim
                // itself: tome-shim remains the long-lived supervisor
                // forwarding signals to whatever pid this child ends up as,
                // a relationship setsid() (session/process-group, not
                // parent/child) does not change.
                setsid().map_err(io::Error::from)?;
            }
            prctl::set_pdeathsig(Signal::SIGKILL).map_err(io::Error::from)?;
            drop_all_capabilities()?;
            Ok(())
        });
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tome-shim: failed to exec {:?}: {e}", args.argv);
            std::process::exit(EXIT_SPAWN_FAILED);
        }
    };

    CHILD_PID.store(child.id() as i32, Ordering::SeqCst);
    if let Err(e) = install_signal_forwarding() {
        // Not fatal — see install_signal_forwarding's own doc comment for
        // why a pane is still safe (just less prompt to tear down) without
        // this.
        eprintln!("tome-shim: signal forwarding not installed: {e}");
    }

    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tome-shim: waiting on the child failed: {e}");
            std::process::exit(EXIT_WAIT_FAILED);
        }
    };
    std::process::exit(exit_code_from(status.code(), status.signal()));
}

// ---- fallback-ladder step 2: self-unshare ----

/// Unshares this process's own user + network namespaces and maps its
/// single real uid/gid into the fresh user namespace as uid/gid 0 — the
/// same "become root inside, mapped back to my real unprivileged id
/// outside" dance `unshare(1)`'s `--map-root-user` performs, done by hand
/// here since this IS the no-external-helper fallback path. A fresh,
/// otherwise-empty network namespace is deny-all egress by construction —
/// the load-bearing control this function provides (see this module's top
/// doc comment for what it deliberately does NOT also provide).
///
/// Must be called before any other thread exists in this process (see
/// [`run`]'s call site) — `unshare(CLONE_NEWUSER)` requires a
/// single-threaded caller (user_namespaces(7)).
fn self_unshare() -> io::Result<()> {
    unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNET).map_err(io::Error::from)?;

    let uid = getuid().as_raw();
    let gid = getgid().as_raw();

    // Write order matters: on kernels with the CVE-2014-8989 fix, an
    // unprivileged process's gid_map write is refused unless
    // /proc/self/setgroups was already set to "deny" first (uid_map has no
    // such ordering requirement, but writing it here too, before gid_map,
    // matches the conventional order every reference implementation of
    // this dance uses). See user_namespaces(7), "/proc/[pid]/setgroups".
    std::fs::write("/proc/self/setgroups", "deny")?;
    std::fs::write("/proc/self/uid_map", id_map_line(0, uid))?;
    std::fs::write("/proc/self/gid_map", id_map_line(0, gid))?;

    // TODO(landlock): file-confinement rules for the app config dir /
    // auth file (bwrap's `--tmpfs <appConfigDir>` equivalent) belong here
    // as a follow-up — see this module's top doc comment. Not attempted
    // this slice: the netns egress kill above is the property TOME-001
    // actually needs closed; a half-finished Landlock ruleset would be
    // worse than an honestly-absent one.
    Ok(())
}

// ---- bring `lo` up inside whatever netns this process is in ----

/// Brings the loopback interface up via the classic `SIOCGIFFLAGS`/
/// `SIOCSIFFLAGS` netdevice ioctl pair on a throwaway `AF_INET`/
/// `SOCK_DGRAM` socket — the same mechanism `ip link set lo up` uses
/// internally. Required both after bwrap's `--unshare-net` (a fresh netns
/// always starts with `lo` administratively down) and after
/// [`self_unshare`] (same reason) — `run` calls this unconditionally,
/// right after the optional self-unshare step.
///
/// Hand-rolled rather than pulled in as a dependency: `nix`'s own `ioctl`
/// feature (not enabled in this crate's `Cargo.toml`) only supplies
/// request-code-encoding macros, not a ready-made "bring an interface up"
/// call — this is one well-understood pair of raw syscalls, not enough
/// surface to justify a new dependency for.
fn bring_loopback_up() -> io::Result<()> {
    const IFNAME: &[u8] = b"lo\0";
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut ifr: libc::ifreq = std::mem::zeroed();
        for (dst, src) in ifr.ifr_name.iter_mut().zip(IFNAME.iter()) {
            *dst = *src as libc::c_char;
        }
        if libc::ioctl(fd, libc::SIOCGIFFLAGS, &mut ifr) < 0 {
            let err = io::Error::last_os_error();
            libc::close(fd);
            return Err(err);
        }
        ifr.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
        if libc::ioctl(fd, libc::SIOCSIFFLAGS, &mut ifr) < 0 {
            let err = io::Error::last_os_error();
            libc::close(fd);
            return Err(err);
        }
        libc::close(fd);
    }
    Ok(())
}

// ---- the loopback bridge itself ----

/// Spawns the accept loop as a background thread that outlives this
/// function: every inbound connection on the namespace-internal
/// `127.0.0.1:<port>` gets bridged to a fresh connection to the
/// bind-mounted host `PaneProxy` unix socket at `sock_path`, byte-shoveled
/// in both directions for as long as either side keeps the connection
/// open. This is what makes `HTTP_PROXY=http://127.0.0.1:<port>` (set by
/// the host, unchanged by entering the sandbox — see `agent_env.rs`)
/// resolve to something real from inside a network namespace that
/// otherwise cannot reach the host's real proxy listener at all — see
/// this repo's "Linux sandbox" plan section for the full loopback-bridge
/// rationale.
fn spawn_bridge(listener: TcpListener, sock_path: PathBuf) {
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(tcp) = conn else { continue }; // one bad accept must not kill the bridge
            let sock_path = sock_path.clone();
            thread::spawn(move || {
                // A failed connect (host proxy not up yet, or already torn
                // down) just drops this one TCP connection — the agent's
                // own HTTP client sees a reset/EOF, same as any other
                // proxy-unreachable case; the bridge itself stays up for
                // the next attempt.
                if let Ok(unix) = UnixStream::connect(&sock_path) {
                    shovel_bidirectional(tcp, unix);
                }
            });
        }
    });
}

/// Copies bytes in both directions between `tcp` (the namespace-internal
/// leg the agent's HTTP client talks to) and `unix` (the bind-mounted leg
/// reaching the host's real `PaneProxy`) until either side closes. Plain
/// `std::io::copy` over two threads — the design brief's own preference
/// (std over pulling in tokio just for this, to keep the sidecar tiny and
/// dep-light). Half-close propagates: the moment one direction hits EOF,
/// the write half of the OTHER stream is shut down too, so a client that
/// only half-closes (shuts down its write side, keeps reading) doesn't
/// leave this bridge connection dangling forever.
fn shovel_bidirectional(tcp: TcpStream, unix: UnixStream) {
    let Ok(tcp_reader) = tcp.try_clone() else { return };
    let Ok(unix_writer) = unix.try_clone() else { return };
    let mut tcp_writer = tcp;
    let mut unix_reader = unix;

    let forward = thread::spawn(move || {
        let mut tcp_reader = tcp_reader;
        let mut unix_writer = unix_writer;
        let _ = io::copy(&mut tcp_reader, &mut unix_writer);
        let _ = unix_writer.shutdown(std::net::Shutdown::Write);
    });

    let _ = io::copy(&mut unix_reader, &mut tcp_writer);
    let _ = tcp_writer.shutdown(std::net::Shutdown::Write);
    let _ = forward.join();
}

// ---- capability drop (pre_exec, between fork and the agent's execve) ----

/// Mirrors the kernel's `struct __user_cap_header_struct` — glibc has
/// never wrapped `capset`/`capget` as ordinary libc functions (unlike
/// `prctl`), so both the header/data layout and the syscall invocation
/// below are hand-rolled against `capabilities(7)` directly, the same way
/// `libcap` itself ultimately does internally.
#[repr(C)]
struct CapUserHeader {
    version: u32,
    pid: i32,
}

/// Mirrors `struct __user_cap_data_struct`. `capset(2)` with
/// `_LINUX_CAPABILITY_VERSION_3` takes an array of TWO of these (the
/// low/high 32 bits of each 64-bit-wide capability set) — see
/// [`drop_all_capabilities`].
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapUserData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

/// Drops every capability from this process's effective/permitted/
/// inheritable sets — `capset(2)` with all-zero data, equivalent to what
/// `libcap`'s `cap_set_proc(cap_init())` does. Called from
/// [`run`]'s `pre_exec` closure, i.e. in the forked child, immediately
/// before it execs the real agent shell: `tome-shim` itself needed
/// `cap_net_admin` (from bwrap's `--cap-add cap_net_admin`) to bring `lo`
/// up and bind inside the netns, but the agent process that's about to
/// run has no legitimate use for it — least privilege, not a
/// TOME-001-load-bearing control on its own (the netns egress kill is
/// that control; this is belt-and-braces on top of it).
///
/// Scope note: this does NOT touch the capability BOUNDING set (that
/// needs one `prctl(PR_CAPBSET_DROP, cap)` call per capability bit, ~40
/// calls for every capability the kernel defines) — out of scope for this
/// slice, and not required for correctness here: a plain, non-file-
/// capability executable like `zsh` derives its capabilities purely from
/// the sets this function already empties, so the bounding set is inert
/// for this specific exec regardless.
///
/// # Safety / signal-safety
/// Must stay async-signal-safe (this runs inside `pre_exec` — see `run`'s
/// call site): one direct `syscall(2)` invocation, no allocation, no
/// locking.
fn drop_all_capabilities() -> io::Result<()> {
    let header = CapUserHeader { version: LINUX_CAPABILITY_VERSION_3, pid: 0 };
    let data = [CapUserData::default(); 2];
    let ret = unsafe { libc::syscall(libc::SYS_capset, &header as *const CapUserHeader, data.as_ptr()) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// ---- SIGTERM/SIGINT forwarding to the child ----

/// The child's pid, set once by [`run`] right after a successful spawn —
/// read only from [`forward_to_child`], the raw signal handler installed
/// by [`install_signal_forwarding`]. `0` (the initial value, and the only
/// value ever written back) means "no child to forward to yet"; pids are
/// always positive, so the handler's `pid > 0` guard is exact.
static CHILD_PID: AtomicI32 = AtomicI32::new(0);

/// The actual signal handler. Must stay async-signal-safe (POSIX's
/// signal-safety list): reads one atomic, then calls `kill(2)` directly —
/// nothing else. Installed for `SIGTERM`/`SIGINT` only (see
/// [`install_signal_forwarding`]): `tome-shim`'s own teardown (a
/// `pty:kill`-driven signal from the host, or an interactive Ctrl-C
/// reaching this process's controlling terminal) must reach the
/// sandboxed child promptly rather than only killing `tome-shim` itself
/// and leaving the child to die later, more slowly, off its own
/// `PR_SET_PDEATHSIG`.
extern "C" fn forward_to_child(sig: libc::c_int) {
    let pid = CHILD_PID.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid, sig);
        }
    }
}

/// Installs [`forward_to_child`] for `SIGTERM` and `SIGINT`. Failure is
/// reported but not fatal — the pane remains fail-closed either way (a
/// killed `tome-shim` still takes the sandboxed child down with it via
/// `PR_SET_PDEATHSIG`, just not as promptly as an explicit forwarded
/// signal would), so [`run`] logs and continues rather than refusing the
/// whole pane over a `sigaction(2)` failure that should never realistically
/// happen.
fn install_signal_forwarding() -> io::Result<()> {
    let action = SigAction::new(SigHandler::Handler(forward_to_child), SaFlags::empty(), SigSet::empty());
    unsafe {
        signal::sigaction(Signal::SIGTERM, &action).map_err(io::Error::from)?;
        signal::sigaction(Signal::SIGINT, &action).map_err(io::Error::from)?;
    }
    Ok(())
}
