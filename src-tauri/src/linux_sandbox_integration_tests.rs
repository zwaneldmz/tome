//! Phase 4/slice L3: the REAL enforcement proof — `#[ignore]`d integration
//! tests that spawn a genuine `bwrap`-wrapped `tome-shim` inside a real
//! Linux network namespace and curl from inside it. Everything else this
//! phase built (`egress::linux`'s argv assembly, `ipc::pty::pty_create`'s
//! wiring, `tome-shim` itself) is exercised only by pure unit tests on
//! whatever host happens to run `cargo test` — none of which can prove the
//! egress actually holds, since that requires a REAL `unshare(2)` call,
//! which this repo's own development host (macOS) cannot make. This file
//! is that proof, and it runs in exactly one place: the `linux-sandbox`
//! job in `.github/workflows/linux-sandbox.yml`, on a real `ubuntu-latest`
//! runner with `bubblewrap` installed via apt.
//!
//! **This module does not compile at all on macOS, or outside a `cargo
//! test` build at all** — see its `#[cfg(all(test, target_os = "linux"))]`
//! declaration in `lib.rs`: `target_os = "linux"` is what makes this
//! crate's native (`cargo check`/`cargo test`, both run on macOS) gates
//! never even parse this file, let alone compile or run it; `test` is
//! required too, separately, because this file depends on `tempfile` (a
//! `[dev-dependencies]`-only crate) — without it, a REAL Linux release
//! build (`cargo build`, no `--test`) would try to compile this module and
//! fail to resolve that dependency. Every `#[test]` below is ALSO
//! `#[ignore]`d, so even a genuine `cargo test` run on a real Linux box
//! skips them by default — `--ignored` (what the CI job passes) is
//! required to actually run this file's tests, matching the plan's own
//! "write these even though they only run in CI; gate them so `cargo
//! test` on macOS skips them cleanly" instruction to the letter (skipped
//! at compile time on macOS, skipped at collection time everywhere else
//! unless explicitly asked for).
//!
//! ## Honest scope limits (read before trusting a green run as more than it is)
//!
//! - **Rung 1 (bwrap) only.** Every test below calls
//!   `crate::egress::linux::build_bwrap_argv` directly, rather than going
//!   through `probe_sandbox_strategy()`'s own bwrap-vs-self-unshare
//!   decision — that DECISION is already fully unit-tested in
//!   `egress::linux`'s own `#[cfg(test)]` suite (runs on every host,
//!   including this crate's macOS gates); what ONLY a real Linux box can
//!   prove is whether the MECHANISM each rung names actually enforces
//!   anything, and bwrap is the mechanism this app prefers whenever it's
//!   available (true on every CI runner this job provisions). Rung 2
//!   (`tome-shim --self-unshare`) has no integration test here — a
//!   deliberately flagged gap, not an oversight; a reasonable follow-up,
//!   not required for THIS slice's own "the primary mechanism actually
//!   works" claim. Its own `TODO(landlock)` gap (no `--tmpfs` equivalent)
//!   is already documented in `egress/linux.rs`.
//! - **Never run, never observed to pass, before this commit.** Authored
//!   entirely on macOS. Treat a green run of THIS specific file, the
//!   first time CI actually executes it, as the actual news — not
//!   anything claimed in this repo's own commit history before that.
//! - **curl's own proxy env-var conventions are a real trap here, already
//!   hit once.** `SandboxFixture::http_proxy_env`'s `NO_PROXY` and the two
//!   "direct egress" tests' `--noproxy '*'` flags exist because curl
//!   (confirmed directly against a real curl binary, not assumed from
//!   memory) both auto-uses a configured proxy from environment even with
//!   NO `--proxy` flag on the command line, AND silently bypasses an
//!   EXPLICIT `--proxy` flag entirely for any target matching `NO_PROXY` —
//!   two independent behaviors that, combined with this file's mock
//!   upstreams all living on loopback addresses, would have made 5 of
//!   these 7 tests either skip the proxy mechanism they mean to exercise
//!   or fail outright on their very first real run, for reasons having
//!   nothing to do with `PaneProxy`/`tome-shim` themselves. Still unverified
//!   BY EXECUTION on a real Linux box — the general caveat above applies
//!   here just as much as everywhere else in this file — but the specific
//!   curl behaviors this reasoning rests on were reproduced directly on a
//!   real curl install while writing this fix, not inferred from a man
//!   page.
//!
//! ## What each test proves (mirrors the phase-4 task brief's curl matrix)
//!
//! 1. [`direct_curl_to_a_non_allowlisted_address_has_no_route`] — egress is
//!    deny-by-construction, not by application-level filtering: a DIRECT
//!    curl (no proxy at all) from inside the netns cannot reach anywhere,
//!    full stop.
//! 2. [`curl_via_proxy_to_an_allowlisted_host_succeeds`] — the loopback
//!    bridge itself works: `$HTTP_PROXY` (byte-identical env contract to
//!    macOS) resolves, through `tome-shim`, to the REAL host-side
//!    `PaneProxy`, tunneling (CONNECT) to an allowlisted upstream.
//! 3. [`curl_via_proxy_to_a_non_allowlisted_host_is_blocked`] — the SAME
//!    bridge refuses a non-allowlisted host with 403, and the host-side
//!    proxy's blocked-event callback actually fires.
//! 4. [`grandchild_process_is_equally_contained`] — containment is a
//!    property of the network namespace, inherited by every descendant
//!    process, not just tome-shim's immediate child.
//! 5. [`app_config_dir_is_hidden_by_the_bwrap_tmpfs`] — bwrap's `--tmpfs
//!    <appConfigDir>` genuinely replaces the directory (a file that exists
//!    on the host is invisible inside).
//! 6. [`relock_severs_a_live_tunnel_mid_transfer`] — `PaneProxy::relock`'s
//!    live-tunnel-kill (TOME-002 territory) actually reaches a connection
//!    that is, at that moment, running INSIDE the sandbox. The tunnel is
//!    unlocked open to a host NOT on the fixed allowlist and severed on a
//!    short, tight deadline specifically so this can only pass if relock's
//!    own abort fires promptly — not because the test's own drip upstream
//!    or curl's `--max-time` eventually gave up on their own regardless of
//!    relock (an earlier version of this test could pass exactly that way;
//!    see the test's own comments for the false-positive it used to be).
//! 7. [`killing_the_wrap_leaves_no_orphan_process`] — `PR_SET_PDEATHSIG` +
//!    bwrap's own `--die-with-parent` actually reap the whole process tree
//!    when the pane is killed, no leftover `pgrep`-visible process.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::process::{Child, Command};

use crate::egress::linux::{
    build_bwrap_argv, build_self_unshare_argv, default_landlock_allow_set, ensure_pane_socket_dir,
    probe_userns_allowed, GappedSpawnSpec,
};
use crate::egress::proxy::{BlockedEvent, PaneProxy};

// ---- environment preconditions ----

fn require_bwrap() {
    let ok = std::process::Command::new("bwrap")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(
        ok,
        "bwrap not found on $PATH — this test requires a real Linux sandbox environment \
         (the linux-sandbox CI job installs it via apt; a local run needs `sudo apt install bubblewrap`)"
    );
}

/// bwrap must not merely be installed — it must be able to actually create a
/// user namespace (unprivileged userns), which some environments forbid.
/// Notably the fedora CI job runs in a docker container whose runner's kernel
/// userns policy bwrap cannot relax from inside the container, so these tests
/// SKIP there (returning `None` from `build_fixture`) rather than fail; the
/// ubuntu legs, which `sysctl` the restriction away, still run the full
/// matrix. Cached: the smoke test is a fork+exec, not worth repeating per test.
fn bwrap_userns_available() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::process::Command::new("bwrap")
            .args(["--unshare-user", "--dev-bind", "/", "/", "--", "/bin/true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// `TOME_SHIM_BIN` override, else the release build `scripts/build-sidecar.sh`
/// (and this repo's own CI job) produces, else a plain dev `cargo build -p
/// tome-shim` output — checked in that order so a CI job that already ran
/// the sidecar-staging step needs no extra plumbing.
fn resolve_tome_shim_bin() -> PathBuf {
    if let Ok(p) = std::env::var("TOME_SHIM_BIN") {
        return PathBuf::from(p);
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // .../src-tauri
    for candidate in ["target/release/tome-shim", "target/debug/tome-shim"] {
        let path = manifest_dir.join(candidate);
        if path.is_file() {
            return path;
        }
    }
    panic!(
        "tome-shim binary not found — build it first (`cargo build -p tome-shim [--release]`) \
         or set TOME_SHIM_BIN to its path"
    );
}

// ---- a tiny plain-HTTP test upstream (no TLS needed — see the module doc
// comment on why every real allowlisted host is normally HTTPS-only in
// production, but the CONNECT tunnel mechanics this proves are identical
// regardless of what's spoken over them once established) ----

/// Binds a plain-HTTP server on `127.0.0.1:0` that answers every request
/// with a fixed 200 body. Returns the bound port; the server task runs for
/// the rest of the process (aborted implicitly when the test process
/// exits — these are short-lived `#[ignore]`d integration tests, not a
/// long-running service).
async fn spawn_fixed_response_upstream(body: &'static str) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind test upstream");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await; // request not parsed — fixed response regardless
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    port
}

/// Binds a plain-HTTP server that "drips" a large declared body slowly (one
/// small chunk every 150ms, up to `max_chunks`) — used only by
/// [`relock_severs_a_live_tunnel_mid_transfer`] to keep a tunnel
/// observably ALIVE (still receiving bytes) for long enough that a
/// concurrently-issued `relock()` has a real, non-instantaneous transfer
/// to sever, rather than racing a transfer that may have already finished.
async fn spawn_drip_upstream(max_chunks: u32) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind drip upstream");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let head = "HTTP/1.1 200 OK\r\nContent-Length: 10000000\r\n\r\n";
                if sock.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for _ in 0..max_chunks {
                    if sock.write_all(b"0123456789").await.is_err() {
                        return; // peer gone — exactly what a severed tunnel looks like from here
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            });
        }
    });
    port
}

// ---- host-side PaneProxy + bwrap argv wiring shared by every test ----

struct SandboxFixture {
    proxy: Arc<PaneProxy>,
    argv: Vec<String>,
    config_dir: tempfile::TempDir,
    #[allow(dead_code)] // kept alive for the fixture's lifetime — dropping removes the tempdir
    runtime_dir: tempfile::TempDir,
    blocked: Arc<Mutex<Vec<BlockedEvent>>>,
}

/// Sets up everything a real gapped-pane spawn needs, using the SAME
/// production code (`egress::linux::build_bwrap_argv`,
/// `egress::proxy::PaneProxy::spawn`) `ipc::pty::pty_create` calls — the
/// whole point of testing from INSIDE this crate rather than as an
/// external black box (see `lib.rs`'s registration comment).
///
/// `initial_allowed`: the pane's starting allowlist — tests pass `vec!
/// ["127.0.0.1".to_string()]` (an exact-literal pattern, matched by
/// connecting to `127.0.0.1:<port>` directly, but NOT by "localhost" —
/// the same "same address, different presented name" trick `proxy.rs`'s
/// own unit tests use to distinguish allowed vs. blocked without needing a
/// second real upstream).
async fn build_fixture(
    pane_id: &str,
    initial_allowed: Vec<String>,
    inner_argv: Vec<String>,
) -> Option<SandboxFixture> {
    require_bwrap();
    if !bwrap_userns_available() {
        eprintln!("skipping: bwrap cannot create a user namespace on this host (unprivileged userns disabled)");
        return None;
    }
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let config_dir = tempfile::tempdir().expect("config tempdir");
    let sock_path = runtime_dir.path().join(format!("pane-{pane_id}.sock"));
    ensure_pane_socket_dir(runtime_dir.path()).expect("ensure pane socket dir");

    let blocked: Arc<Mutex<Vec<BlockedEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let blocked_for_cb = blocked.clone();
    let proxy = PaneProxy::spawn(initial_allowed, Some(sock_path.clone()), move |ev| {
        blocked_for_cb.lock().unwrap().push(ev);
    })
    .await
    .expect("spawn PaneProxy");
    let proxy = Arc::new(proxy);

    let spec = GappedSpawnSpec {
        pane_id: pane_id.to_string(),
        proxy_port: proxy.port(),
        host_socket_path: sock_path,
        app_config_dir: config_dir.path().to_path_buf(),
        shim_path: resolve_tome_shim_bin(),
        inner_argv,
        headless: true, // no interactive terminal need for any of these tests
        // Rung 1 (bwrap) ignores the Landlock allow-set — `--tmpfs` over
        // the config dir is bwrap's own file confinement. Empty is fine.
        allow_read: Vec::new(),
        allow_write: Vec::new(),
    };
    let argv = build_bwrap_argv(&spec);

    Some(SandboxFixture {
        proxy,
        argv,
        config_dir,
        runtime_dir,
        blocked,
    })
}

impl SandboxFixture {
    fn http_proxy_env(&self) -> Vec<(String, String)> {
        let proxy_url = format!("http://127.0.0.1:{}", self.proxy.port());
        vec![
            ("HTTP_PROXY".to_string(), proxy_url.clone()),
            ("HTTPS_PROXY".to_string(), proxy_url.clone()),
            ("http_proxy".to_string(), proxy_url.clone()),
            ("https_proxy".to_string(), proxy_url),
            // Deliberately EMPTY, not `agent_env.rs`'s real
            // "localhost,127.0.0.1" (what a real gapped pane actually
            // gets) — every mock upstream in this file binds on a loopback
            // address specifically so it can stand in for "an allowlisted
            // provider" from inside the sandbox, and curl (confirmed
            // directly, not assumed) treats a `NO_PROXY`/`no_proxy` entry
            // as an instruction to bypass proxying ENTIRELY for a matching
            // target — even one reached via an EXPLICIT `--proxy` flag,
            // not just curl's own env-based auto-detection. Copying
            // production's literal value here would make every proxy-
            // mechanism test below that targets "127.0.0.1" or "localhost"
            // (`curl_via_proxy_to_an_allowlisted_host_succeeds`,
            // `curl_via_proxy_to_a_non_allowlisted_host_is_blocked`,
            // `relock_severs_a_live_tunnel_mid_transfer`) silently skip the
            // proxy and attempt a DIRECT connection instead — which, from
            // inside this sandbox's own netns, reaches nothing at all (the
            // real upstream servers live in the HOST's namespace, only
            // reachable via the loopback bridge), making a broken proxy
            // mechanism indistinguishable from a merely-misconfigured test
            // environment. `NO_PROXY`'s VALUE is not itself under test
            // anywhere in this file (see the module doc comment's numbered
            // list) — only its ABSENCE-of-collision matters here.
            ("NO_PROXY".to_string(), String::new()),
            ("no_proxy".to_string(), String::new()),
        ]
    }

    /// Spawns the fixture's bwrap argv with `extra_env` layered on top of
    /// (never replacing) the proxy env every real gapped pane gets — plus
    /// the bare minimum PATH a shell/curl invocation needs, since
    /// `env_clear()` here mirrors `ipc::pty::build_pty_command`'s own
    /// TOME-007 discipline (a sandboxed process gets EXACTLY what this
    /// function sets, never this test process's own ambient environment).
    fn spawn(&self, extra_env: &[(&str, &str)]) -> Child {
        let mut cmd = Command::new(&self.argv[0]);
        cmd.args(&self.argv[1..]);
        cmd.env_clear();
        cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        for (k, v) in self.http_proxy_env() {
            cmd.env(k, v);
        }
        for (k, v) in extra_env {
            cmd.env(*k, *v);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        cmd.spawn().expect("spawn bwrap")
    }
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

async fn run_to_completion(child: Child, timeout: Duration) -> Output {
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => Output {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            timed_out: false,
        },
        Ok(Err(e)) => panic!("waiting on sandboxed process failed: {e}"),
        Err(_) => Output {
            code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
        },
    }
}

const TEST_TIMEOUT: Duration = Duration::from_secs(20);

// ==== 1. direct egress has no route at all ====

#[tokio::test]
#[ignore = "requires a real Linux netns + bwrap — see .github/workflows/linux-sandbox.yml"]
async fn direct_curl_to_a_non_allowlisted_address_has_no_route() {
    // 203.0.113.0/24 (TEST-NET-3, RFC 5737) is permanently reserved for
    // documentation and never routable on a real network — belt-and-
    // braces on top of the netns itself having no route anywhere, so even
    // a hypothetical netns-setup bug that leaked SOME connectivity still
    // could not reach this address for real.
    //
    // `--noproxy '*'` is load-bearing, not defensive decoration: this
    // fixture's `spawn()` unconditionally sets `http_proxy`/`HTTP_PROXY`
    // env vars for every test (real gapped panes always have them set —
    // see `http_proxy_env`), and curl (confirmed directly) auto-detects
    // and uses a configured proxy from environment even with NO `--proxy`
    // flag on the command line at all. Without this flag, "direct curl, no
    // proxy at all" would actually be lying about its own premise: curl
    // would silently route this request through the REAL proxy (which
    // isn't exempted for "203.0.113.1" — that address isn't on
    // `http_proxy_env`'s `NO_PROXY` list), get back a 403 from `handle_
    // plain`'s own allowlist check (203.0.113.1 isn't allowlisted either),
    // and — since plain curl treats any valid HTTP response, even a 403,
    // as SUCCESS — exit 0, which would make this test's own `assert_ne!
    // (out.code, Some(0), ...)` below fail outright on a real run. `--noproxy
    // '*'` (curl's own documented wildcard for "never use a proxy, for any
    // host, regardless of what's configured") is what actually guarantees
    // this specific request tests netns-level route absence rather than
    // proxy-level allowlist denial.
    let Some(fixture) = build_fixture(
        "direct-egress",
        vec!["127.0.0.1".to_string()],
        vec![
            "/usr/bin/curl".to_string(),
            "-sS".to_string(),
            "--noproxy".to_string(),
            "*".to_string(),
            "--max-time".to_string(),
            "5".to_string(),
            "-o".to_string(),
            "/dev/null".to_string(),
            "http://203.0.113.1/".to_string(),
        ],
    )
    .await
    else {
        return;
    };

    let child = fixture.spawn(&[]);
    let out = run_to_completion(child, TEST_TIMEOUT).await;

    assert!(
        !out.timed_out,
        "direct curl should fail immediately (no route), not hang"
    );
    assert_ne!(
        out.code,
        Some(0),
        "direct egress must fail — stderr: {}",
        out.stderr
    );
}

// ==== 2. proxy -> allowlisted host succeeds (CONNECT tunnel leg) ====

#[tokio::test]
#[ignore = "requires a real Linux netns + bwrap — see .github/workflows/linux-sandbox.yml"]
async fn curl_via_proxy_to_an_allowlisted_host_succeeds() {
    let upstream_port = spawn_fixed_response_upstream("allowlisted-ok").await;
    let Some(fixture) = build_fixture(
        "proxy-allow",
        vec!["127.0.0.1".to_string()],
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!(
                "curl -sS --max-time 8 --proxy \"$HTTP_PROXY\" --proxytunnel -o /dev/null -w '%{{http_code}}' http://127.0.0.1:{upstream_port}/"
            ),
        ],
    )
    .await else {
        return;
    };

    let child = fixture.spawn(&[]);
    let out = run_to_completion(child, TEST_TIMEOUT).await;

    assert!(
        !out.timed_out,
        "proxied curl to an allowlisted host should not hang"
    );
    assert_eq!(
        out.code,
        Some(0),
        "curl itself must succeed — stderr: {}",
        out.stderr
    );
    assert_eq!(
        out.stdout.trim(),
        "200",
        "expected HTTP 200 from the allowlisted upstream, stderr: {}",
        out.stderr
    );
}

// ==== 3. proxy -> non-allowlisted host is blocked (plain-HTTP leg + on_blocked) ====

#[tokio::test]
#[ignore = "requires a real Linux netns + bwrap — see .github/workflows/linux-sandbox.yml"]
async fn curl_via_proxy_to_a_non_allowlisted_host_is_blocked() {
    let upstream_port = spawn_fixed_response_upstream("should-never-be-seen").await;
    // Only "127.0.0.1" is allowlisted; "localhost" resolves to the SAME
    // loopback upstream but is not itself a literal allowlist match.
    let Some(fixture) = build_fixture(
        "proxy-block",
        vec!["127.0.0.1".to_string()],
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!(
                "curl -sS --max-time 8 --proxy \"$HTTP_PROXY\" -o /dev/null -w '%{{http_code}}' http://localhost:{upstream_port}/"
            ),
        ],
    )
    .await else {
        return;
    };

    let child = fixture.spawn(&[]);
    let out = run_to_completion(child, TEST_TIMEOUT).await;

    assert!(
        !out.timed_out,
        "a blocked request should get a fast 403, not hang"
    );
    assert_eq!(
        out.stdout.trim(),
        "403",
        "expected HTTP 403 for a non-allowlisted host, stderr: {}",
        out.stderr
    );

    // The host-side PaneProxy's on_blocked callback — the exact mechanism
    // `ipc::egress::create_gapped_pane_proxy` wires to `events::append`
    // (kind `egress:blocked`) in production — must have actually fired.
    let blocked = fixture.blocked.lock().unwrap();
    assert!(
        blocked
            .iter()
            .any(|e| matches!(e, BlockedEvent::Attempt { host } if host == "localhost")),
        "expected a BlockedEvent::Attempt for host \"localhost\", got {blocked:?}"
    );
}

// ==== 4. grandchild descendants are equally contained ====

#[tokio::test]
#[ignore = "requires a real Linux netns + bwrap — see .github/workflows/linux-sandbox.yml"]
async fn grandchild_process_is_equally_contained() {
    // sh -c 'sh -c "curl ..."' — curl runs as a GRANDCHILD of the process
    // tome-shim itself execs, proving containment is inherited network-
    // namespace membership, not something only the immediate child gets.
    // `--noproxy '*'` here is load-bearing for the same reason it is in
    // `direct_curl_to_a_non_allowlisted_address_has_no_route` (see that
    // test's own comment): a bare curl with no `--proxy` flag still
    // auto-detects this fixture's ambient `http_proxy` env var, which
    // would otherwise route this "direct egress" probe through the real
    // proxy instead of actually testing netns-level containment.
    let Some(fixture) = build_fixture(
        "grandchild-egress",
        vec!["127.0.0.1".to_string()],
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sh -c 'curl -sS --noproxy \"*\" --max-time 5 -o /dev/null http://203.0.113.1/'"
                .to_string(),
        ],
    )
    .await
    else {
        return;
    };

    let child = fixture.spawn(&[]);
    let out = run_to_completion(child, TEST_TIMEOUT).await;

    assert!(
        !out.timed_out,
        "a contained grandchild's direct egress should fail fast, not hang"
    );
    assert_ne!(
        out.code,
        Some(0),
        "grandchild direct egress must fail — stderr: {}",
        out.stderr
    );
}

// ==== 5. app config dir is hidden by bwrap's --tmpfs ====

#[tokio::test]
#[ignore = "requires a real Linux netns + bwrap — see .github/workflows/linux-sandbox.yml"]
async fn app_config_dir_is_hidden_by_the_bwrap_tmpfs() {
    let Some(fixture) = build_fixture(
        "tmpfs-hide",
        vec!["127.0.0.1".to_string()],
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "test -e \"$0\" && echo PRESENT || echo ABSENT".to_string(),
            "__unused_argv0_placeholder__".to_string(),
        ],
    )
    .await
    else {
        return;
    };

    // Write the marker AFTER build_fixture (which creates config_dir) but
    // BEFORE spawning — the auth file bwrap's own profile (and this
    // fixture's argv) targets, present on the HOST, must still exist on
    // disk right up until the sandboxed process actually checks for it.
    let marker = fixture.config_dir.path().join("egress-auth.json");
    std::fs::write(&marker, r#"{"salt":"deadbeef","hash":"deadbeef"}"#)
        .expect("write marker file on host");
    assert!(
        marker.exists(),
        "test precondition: the marker file must exist on the HOST"
    );

    // Rebuild the inner command now that the real marker path is known
    // (tempdir paths aren't known until build_fixture runs) — a second,
    // tiny bwrap argv reusing the SAME fixture's proxy/config dir, rather
    // than plumbing the path through build_fixture's own signature.
    let spec = GappedSpawnSpec {
        pane_id: "tmpfs-hide-2".to_string(),
        proxy_port: fixture.proxy.port(),
        host_socket_path: fixture.runtime_dir.path().join("pane-tmpfs-hide.sock"),
        app_config_dir: fixture.config_dir.path().to_path_buf(),
        shim_path: resolve_tome_shim_bin(),
        inner_argv: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!(
                "test -e {} && echo PRESENT || echo ABSENT",
                marker.display()
            ),
        ],
        headless: true,
        allow_read: Vec::new(),
        allow_write: Vec::new(),
    };
    let argv = build_bwrap_argv(&spec);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    let child = cmd.spawn().expect("spawn bwrap");
    let out = run_to_completion(child, TEST_TIMEOUT).await;

    assert!(!out.timed_out);
    assert_eq!(
        out.stdout.trim(),
        "ABSENT",
        "the app config dir's marker file must be invisible under bwrap's --tmpfs, stderr: {}",
        out.stderr
    );
}

// ==== 6. relock severs a live tunnel mid-transfer ====

#[tokio::test]
#[ignore = "requires a real Linux netns + bwrap — see .github/workflows/linux-sandbox.yml"]
async fn relock_severs_a_live_tunnel_mid_transfer() {
    // This tunnel's target is deliberately "localhost", NOT "127.0.0.1" —
    // see `fixture.proxy.unlock()` below for why that distinction is the
    // entire point of this test (a prior version of this test used
    // "127.0.0.1" here, which is ALSO the fixed allowlist entry a few
    // lines down, making relock's own is_allowed check a permanent no-op
    // for it — a regression this test now specifically guards against; see
    // proxy.rs's `relock_kills_an_open_mode_only_tunnel_but_spares_an_
    // allowlisted_one` unit test for the identical "same address, two
    // presented names" pattern this integration test mirrors for real).
    let upstream_port = spawn_drip_upstream(40).await; // ~6s of drip if never interrupted
    let Some(fixture) = build_fixture(
        "relock-tunnel",
        vec!["127.0.0.1".to_string()], // fixed allowlist — "localhost" is deliberately NOT on it
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!(
                "curl -sS --max-time 15 --proxy \"$HTTP_PROXY\" --proxytunnel -o /dev/null -w '%{{size_download}}' http://localhost:{upstream_port}/"
            ),
        ],
    )
    .await else {
        return;
    };

    // Mode::Open admits this tunnel despite "localhost" not being on the
    // fixed allowlist above — exactly the condition PaneProxy::relock has
    // to detect and sever (`is_allowed(&allowed, &entry.host)` must come
    // back false for "localhost" once relock narrows back to Providers).
    // Without this call, mode never leaves Providers, and a fixed
    // "127.0.0.1"-only allowlist would refuse the CONNECT outright before
    // any tunnel ever registers — there would be nothing for relock to
    // sever in the first place.
    fixture.proxy.unlock();

    let child = fixture.spawn(&[]);

    // Give curl time to connect, tunnel, and receive several drip chunks —
    // proving there is a genuinely LIVE transfer in flight, not racing a
    // connection that hasn't started yet.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        fixture.proxy.live_tunnel_count(),
        1,
        "expected exactly one live tunnel right before relock"
    );

    fixture.proxy.relock();
    // relock() now has real work to do: mode flips back to Providers, and
    // "localhost" fails is_allowed against the fixed ["127.0.0.1"] set —
    // entry.abort.abort() must fire for this specific tunnel.

    // A SHORT, deliberately tight timeout — NOT the file's usual
    // TEST_TIMEOUT (20s) — is the point, not an arbitrary tightening: the
    // drip upstream keeps sending for ~5.2s more from here even if nothing
    // severs it (40 chunks * 150ms, minus the 800ms already elapsed), and
    // curl's own --max-time is 15s. Either of those would ALSO eventually
    // make curl exit non-zero (a truncated Content-Length once the drip's
    // own generator loop ends, or curl's own client-side timeout) —
    // entirely independent of relock. A bare "curl eventually exited
    // non-zero, and the registry is empty" check (this test's own
    // previous shape) is satisfied just as well by a COMPLETELY BROKEN
    // relock() as by a working one, since the natural drip end and the
    // tunnel's own on-EOF self-removal from the registry produce the exact
    // same two observations with no relock() involvement at all. Bounding
    // the wait to well under both of those durations means only a PROMPT
    // sever — relock() actually aborting the tunnel — can satisfy
    // `!out.timed_out` below; a broken relock() now fails LOUDLY (a timeout
    // assertion) rather than passing by coincidence.
    const POST_RELOCK_TIMEOUT: Duration = Duration::from_secs(3);
    let out = run_to_completion(child, POST_RELOCK_TIMEOUT).await;
    assert!(
        !out.timed_out,
        "curl was still running {POST_RELOCK_TIMEOUT:?} after relock() returned — the tunnel was not \
         severed promptly, which means relock() did not actually abort it (or this test regressed back \
         to targeting an address relock() has no reason to touch)"
    );
    assert_ne!(
        out.code,
        Some(0),
        "curl must report a transfer error once its tunnel is severed mid-flight, stdout={:?} stderr={}",
        out.stdout,
        out.stderr
    );
    assert_eq!(
        fixture.proxy.live_tunnel_count(),
        0,
        "relock must leave no live tunnels behind"
    );
}

// ==== 7. killing the wrap leaves no orphan process ====

#[tokio::test]
#[ignore = "requires a real Linux netns + bwrap — see .github/workflows/linux-sandbox.yml"]
async fn killing_the_wrap_leaves_no_orphan_process() {
    // A distinctive, unlikely-to-collide literal so `pgrep -f` can find it
    // specifically (and ONLY it) both before and after the kill.
    const MARKER: &str = "tome-orphan-check-sentinel-274b3a";
    // `exec -a` is a bashism — Ubuntu's /bin/sh is dash, whose `exec` has
    // no `-a` ("exec: -a: not found", exit 127), which is exactly what
    // this test's first real CI run died of: the sandboxed process never
    // became the renamed sleep, so the pgrep precondition below could
    // never hold. bash is present on every Linux CI runner (and is what
    // the sentinel rename needs), so name it explicitly.
    let Some(fixture) = build_fixture(
        "orphan-check",
        vec!["127.0.0.1".to_string()],
        vec![
            "/bin/bash".to_string(),
            "-c".to_string(),
            format!("exec -a {MARKER} sleep 300"),
        ],
    )
    .await
    else {
        return;
    };

    let mut child = fixture.spawn(&[]);

    // Let the process tree actually start (bwrap -> tome-shim -> sh -> the
    // renamed `sleep`) before asserting anything about it.
    for _ in 0..20 {
        if pgrep_matches(MARKER) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        pgrep_matches(MARKER),
        "test precondition: the sandboxed sleep never showed up under pgrep"
    );

    // Kill the TOP-LEVEL process the test itself spawned (bwrap) — the
    // exact same thing `crate::pty::Registry::kill` does to whatever
    // portable-pty directly spawned for a gapped Linux pane (argv[0] =
    // "bwrap" — see `ipc::pty::build_pty_command`'s `SandboxWrap::Full`
    // handling). Everything below it must go with it via
    // `PR_SET_PDEATHSIG(SIGKILL)` (tome-shim's own child) and bwrap's own
    // `--die-with-parent`.
    child.kill().await.expect("kill the wrap");
    let _ = child.wait().await;

    let mut still_present = pgrep_matches(MARKER);
    for _ in 0..25 {
        if !still_present {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        still_present = pgrep_matches(MARKER);
    }
    assert!(
        !still_present,
        "no process matching {MARKER:?} may survive the wrap being killed — orphan detected"
    );
}

fn pgrep_matches(pattern: &str) -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ==== F-02: rung-2 (self-unshare) Landlock file confinement ====
//
// The three assertions docs/LINUX-LANDLOCK-DESIGN.md's "Testing" section
// names: the auth file unreadable, the config dir unwritable, and the
// workspace + /tmp still writable (no over-restriction). These run the
// rung-2 mechanism DIRECTLY (`tome-shim --self-unshare`, no bwrap), so they
// exercise the fallback ladder's own enforcement rather than bwrap's
// `--tmpfs`.
//
// Skip-vs-fail policy, both documented in `egress::linux`:
// - `probe_userns_allowed()` is a heuristic — Ubuntu 23.10+ AppArmor can
//   still deny the actual `unshare()` — so a spawn that dies with
//   tome-shim's EXIT_SELF_UNSHARE_FAILED (3) skips with a note instead of
//   failing the assertion.
// - On a kernel without Landlock (or with it disabled), the shim prints its
//   "landlock file confinement" NOTE and continues egress-only; the tests
//   skip on that NOTE too — the assertions below can only meaningfully run
//   where Landlock actually applies.

/// Builds and runs one `tome-shim --self-unshare` sandbox whose inner
/// command is `sh -c <script>`. `workspace` is granted read+write by the
/// allow-set; `config_dir` is the EXCLUDED root (never in either set —
/// `default_landlock_allow_set` never adds it). Returns `(exit_code,
/// stdout, stderr)`.
async fn run_rung2(
    config_dir: &std::path::Path,
    workspace: &std::path::Path,
    script: String,
) -> (Option<i32>, String, String) {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    let (allow_read, allow_write) = default_landlock_allow_set(
        workspace,
        &home,
        None,
        &[PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
    );
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    ensure_pane_socket_dir(runtime_dir.path()).expect("ensure pane socket dir");

    let spec = GappedSpawnSpec {
        pane_id: "rung2-landlock".to_string(),
        proxy_port: 18080, // bound in the fresh netns; nothing connects to it in these tests
        host_socket_path: runtime_dir.path().join("pane-rung2-landlock.sock"),
        app_config_dir: config_dir.to_path_buf(),
        shim_path: resolve_tome_shim_bin(),
        inner_argv: vec!["/bin/sh".to_string(), "-c".to_string(), script],
        headless: true,
        allow_read,
        allow_write,
    };
    let argv = build_self_unshare_argv(&spec);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin");
    if let Ok(h) = std::env::var("HOME") {
        cmd.env("HOME", h);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    let child = cmd.spawn().expect("spawn tome-shim --self-unshare");
    let out = run_to_completion(child, TEST_TIMEOUT).await;
    (out.code, out.stdout, out.stderr)
}

/// True when a rung-2 run must be treated as "cannot test here" rather
/// than an assertion failure — the two documented skip conditions above.
fn rung2_skip_conditions(code: Option<i32>, stderr: &str) -> Option<String> {
    if !probe_userns_allowed() {
        return Some("unprivileged user namespaces unavailable (probe)".to_string());
    }
    if code == Some(3) {
        // tome-shim's EXIT_SELF_UNSHARE_FAILED — the AppArmor-style case
        // the probe heuristic cannot see.
        return Some(format!(
            "tome-shim --self-unshare failed at runtime (probe said yes, kernel said no): {stderr}"
        ));
    }
    if stderr.contains("landlock file confinement") {
        // The shim's NOTE — Landlock unavailable/disabled on this kernel,
        // the sandbox ran egress-only and the assertions can't apply.
        return Some(format!(
            "Landlock unavailable on this kernel (shim NOTE present): {stderr}"
        ));
    }
    None
}

#[tokio::test]
#[ignore = "requires a real Linux userns + Landlock — see .github/workflows/linux-sandbox.yml"]
async fn rung2_cannot_read_the_auth_file() {
    let config = tempfile::tempdir().expect("config tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let marker = config.path().join("egress-auth.json");
    std::fs::write(&marker, r#"{"salt":"deadbeef","hash":"deadbeef"}"#)
        .expect("write marker file on host");

    let script = format!(
        "if cat '{}' >/dev/null 2>&1; then echo READABLE; else echo DENIED; fi",
        marker.display()
    );
    let (code, stdout, stderr) = run_rung2(config.path(), workspace.path(), script).await;
    if let Some(reason) = rung2_skip_conditions(code, &stderr) {
        eprintln!("SKIP rung2_cannot_read_the_auth_file: {reason}");
        return;
    }
    assert_ne!(code, None, "rung-2 run timed out: {stderr}");
    assert_eq!(
        stdout.trim(),
        "DENIED",
        "a rung-2 pane must not read egress-auth.json, stderr: {stderr}"
    );
}

#[tokio::test]
#[ignore = "requires a real Linux userns + Landlock — see .github/workflows/linux-sandbox.yml"]
async fn rung2_cannot_write_the_config_dir() {
    let config = tempfile::tempdir().expect("config tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let target = config.path().join("pwned");

    let script = format!(
        "if touch '{}' 2>/dev/null; then echo WROTE; else echo DENIED; fi",
        target.display()
    );
    let (code, stdout, stderr) = run_rung2(config.path(), workspace.path(), script).await;
    if let Some(reason) = rung2_skip_conditions(code, &stderr) {
        eprintln!("SKIP rung2_cannot_write_the_config_dir: {reason}");
        return;
    }
    assert_ne!(code, None, "rung-2 run timed out: {stderr}");
    assert_eq!(
        stdout.trim(),
        "DENIED",
        "a rung-2 pane must not write under the app config dir, stderr: {stderr}"
    );
    assert!(
        !target.exists(),
        "the denied write must not have landed on disk"
    );
}

#[tokio::test]
#[ignore = "requires a real Linux userns + Landlock — see .github/workflows/linux-sandbox.yml"]
async fn rung2_can_still_write_the_workspace_and_tmp() {
    // The no-over-restriction half of the design's test plan: the
    // whitelist must not break the agent's legitimate write paths.
    let config = tempfile::tempdir().expect("config tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let ws_file = workspace.path().join("ok");

    let script = format!(
        "touch '{}' && touch /tmp/tome-rung2-ok-$$ && echo WROTE",
        ws_file.display()
    );
    let (code, stdout, stderr) = run_rung2(config.path(), workspace.path(), script).await;
    if let Some(reason) = rung2_skip_conditions(code, &stderr) {
        eprintln!("SKIP rung2_can_still_write_the_workspace_and_tmp: {reason}");
        return;
    }
    assert_ne!(code, None, "rung-2 run timed out: {stderr}");
    assert_eq!(
        stdout.trim(),
        "WROTE",
        "workspace and /tmp writes must still succeed under the allow-set, stderr: {stderr}"
    );
    assert!(
        ws_file.exists(),
        "the workspace write must have landed on disk"
    );
}
