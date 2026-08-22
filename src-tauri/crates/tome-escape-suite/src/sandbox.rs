//! Per-OS spawn helpers for the escape suite: the REAL production spawn
//! paths, driven at the highest possible seam.
//!
//! - **macOS**: [`MacFixture`] builds the SBPL profile with the production
//!   builder (`tome_flow::egress::seatbelt::seatbelt_profile` — the exact
//!   function `ipc::pty::pty_create` calls) and executes probes through
//!   `/usr/bin/sandbox-exec` with it, exactly as production wraps agent
//!   panes (`SandboxWrap::Prefix`). The config dir handed to the builder
//!   is deliberately a REAL path (no symlinked ancestors) — the caller
//!   invariant seatbelt.rs's canonical-path caveat documents; the symlink
//!   variant is exercised separately, live, by the attempts module.
//! - **Linux**: [`LinuxFixture`] mirrors
//!   `src/linux_sandbox_integration_tests.rs`'s `SandboxFixture`: the
//!   production `build_bwrap_argv` + `default_landlock_allow_set` +
//!   `PaneProxy::spawn` (with the loopback-bridge unix socket) + the real
//!   `tome-shim` sidecar. One fixture is built per attempt and every probe
//!   in that attempt re-uses the SAME proxy — so unlock/relock state
//!   carries across probes exactly as it does across a real pane's
//!   lifetime.
//!
//! Both helpers enforce TOME-007 discipline (env_clear + explicit env —
//! the sandboxed probe gets exactly what a real pane would, never the
//! harness's own ambient environment) and run every probe under a hard
//! timeout.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::process::{Child, Command};

use tome_flow::egress::linux::{
    build_bwrap_argv, default_landlock_allow_set, ensure_pane_socket_dir, GappedSpawnSpec,
};
use tome_flow::egress::proxy::{BlockedEvent, PaneProxy};
use tome_flow::egress::seatbelt::seatbelt_profile;

/// Hard cap for any single sandboxed probe. Individual curls carry their
/// own `--max-time` well under this; the cap is the backstop against a
/// hung shim/bwrap/sandbox-exec.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct RunOutput {
    pub exit: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

pub async fn run_to_completion(child: Child, timeout: Duration) -> RunOutput {
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => RunOutput {
            exit: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            timed_out: false,
        },
        Ok(Err(e)) => RunOutput {
            exit: None,
            stdout: String::new(),
            stderr: format!("wait on sandboxed process failed: {e}"),
            timed_out: true,
        },
        Err(_) => RunOutput {
            exit: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
        },
    }
}

/// Squash a probe's stderr into a single printable line.
pub fn one_line(s: &str) -> String {
    let squashed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if squashed.len() > 240 {
        format!("{}…", &squashed[..240])
    } else {
        squashed
    }
}

// ============================== macOS ==============================

/// A hermetic macOS fixture: a real (canonical — no symlinked ancestors)
/// scratch tree under `$HOME`, a config dir the seatbelt profile will
/// confine, a hermetic home for the Docker-socket deny paths, and the
/// pane's proxy port the profile's loopback carve-out names. `proxy_port:
/// None` spawns the fixture's OWN real `PaneProxy` (production order:
/// proxy first, profile names its port — F-01); `Some(port)` attaches the
/// caller's proxy (the proxy attempt).
pub struct MacFixture {
    pub config_dir: PathBuf,
    pub home: PathBuf,
    pub proxy_port: u16,
    /// The fixture's own proxy (dropped when the fixture drops — mirrors
    /// pane teardown). `None` when the caller attached its own.
    _proxy: Option<Arc<PaneProxy>>,
    /// Kept alive for the fixture's lifetime.
    _base: tempfile::TempDir,
}

/// Skip reason when the macOS environment cannot run a decisive probe.
pub fn mac_preflight() -> Option<String> {
    if !Path::new("/usr/bin/sandbox-exec").exists() {
        return Some("/usr/bin/sandbox-exec not present — seatbelt probes cannot run".into());
    }
    if !Path::new("/bin/sh").exists() || !Path::new("/usr/bin/curl").exists() {
        return Some("/bin/sh or /usr/bin/curl missing — probe tools unavailable".into());
    }
    let Some(home) = std::env::home_dir() else {
        return Some("no home directory — cannot build a real-path fixture".into());
    };
    // The seatbelt canonical-path caveat: subpath rules match the path
    // sandbox-exec canonicalizes to. A fixture rooted under a symlinked
    // HOME would silently fail to confine, so refuse to run rather than
    // produce false PASSes — production has the same invariant on
    // Tauri's app_data_dir.
    match std::fs::canonicalize(&home) {
        Ok(real) if real == home => None,
        Ok(real) => Some(format!(
            "HOME {} resolves to {} through a symlink — seatbelt subpath rules would not \
             confine this fixture (the canonical-path caveat). Production's app_data_dir has \
             no symlinked ancestors, so it is unaffected.",
            home.display(),
            real.display()
        )),
        Err(e) => Some(format!("cannot canonicalize HOME: {e}")),
    }
}

/// Builds the macOS fixture. `proxy_port: None` spawns the fixture's own
/// real `PaneProxy` and names its kernel-assigned port in the profile —
/// exactly what `ipc::pty::pty_create` does (proxy first, profile second;
/// sandbox-exec rejects port 0 in the remote-ip filter, so a no-proxy
/// fixture must still have a real port).
pub async fn build_mac_fixture(proxy_port: Option<u16>) -> MacFixture {
    let home = std::env::home_dir().expect("mac_preflight checked home_dir");
    let base = tempfile::TempDir::new_in(&home).expect("create scratch base under HOME");
    let config_dir = base.path().join("app-config");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let hermetic_home = base.path().join("home");
    std::fs::create_dir_all(&hermetic_home).expect("create hermetic home");
    // Fixture contract (the caller invariant, enforced here so a broken
    // fixture can never yield a false PASS): the config dir must be a
    // real path — identical to its canonicalization.
    assert_eq!(
        std::fs::canonicalize(&config_dir).expect("canonicalize config dir"),
        config_dir,
        "fixture config dir must have no symlinked ancestors (canonical-path caveat)"
    );
    let (port, own_proxy) = match proxy_port {
        Some(port) => (port, None),
        None => {
            let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, |_| {})
                .await
                .expect("spawn fixture PaneProxy");
            let port = proxy.port();
            (port, Some(Arc::new(proxy)))
        }
    };
    MacFixture {
        config_dir,
        home: hermetic_home,
        proxy_port: port,
        _proxy: own_proxy,
        _base: base,
    }
}

impl MacFixture {
    /// The production profile — built by the SAME function
    /// `ipc::pty::pty_create` uses, from this fixture's config dir, home,
    /// and proxy port.
    pub fn profile(&self) -> String {
        seatbelt_profile(&self.config_dir, self.proxy_port, &self.home)
    }

    /// The `sandbox-exec -p <profile>` command ready to run `argv` — the
    /// exact production wrap (`SandboxWrap::Prefix`).
    pub fn command(&self, argv: &[String], env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new("/usr/bin/sandbox-exec");
        cmd.arg("-p").arg(self.profile());
        cmd.args(argv);
        cmd.env_clear();
        cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        cmd.env("HOME", &self.home);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        cmd
    }

    /// Runs `argv` inside a real `sandbox-exec -p <profile>` to completion.
    pub async fn run(&self, argv: &[String], env: &[(&str, &str)]) -> RunOutput {
        let child = self.command(argv, env).spawn().expect("spawn sandbox-exec");
        run_to_completion(child, TEST_TIMEOUT).await
    }

    /// The same argv WITHOUT the sandbox — host-side baselines that prove
    /// a probe would have succeeded had egress been open (decisiveness).
    pub async fn host_run(&self, argv: &[String]) -> RunOutput {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        cmd.env_clear();
        cmd.env(
            "PATH",
            "/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin",
        );
        cmd.env("HOME", &self.home);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        let child = cmd.spawn().expect("spawn host-side baseline");
        run_to_completion(child, TEST_TIMEOUT).await
    }
}

/// Runs a caller-supplied profile (not a fixture's) through real
/// `sandbox-exec` — used by the symlink-caveat attempt, which must build
/// profiles from deliberately-misspelled paths.
pub async fn mac_run_profile(profile: &str, argv: &[String], env: &[(&str, &str)]) -> RunOutput {
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(profile);
    cmd.args(argv);
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    cmd.env("HOME", "/tmp");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    let child = cmd.spawn().expect("spawn sandbox-exec");
    run_to_completion(child, TEST_TIMEOUT).await
}

// ============================== Linux ==============================

/// The Linux fixture: production `build_bwrap_argv` fed by production
/// `default_landlock_allow_set` + a real `PaneProxy` on the loopback
/// bridge's unix socket + the real `tome-shim` — the same wiring
/// `linux_sandbox_integration_tests.rs::SandboxFixture` uses. One fixture
/// per attempt; every probe re-uses the SAME proxy so mode/unlock/relock
/// state carries across probes like it does across a real pane's life.
pub struct LinuxFixture {
    pub proxy: Arc<PaneProxy>,
    pub config_dir: PathBuf,
    /// The sandboxed node's writable workspace root (the allow-list's
    /// write target) — carried for fixture parity with the integration
    /// tests; future attempts (write-confinement probes) use it.
    #[allow(dead_code)]
    pub workspace: PathBuf,
    pub home: PathBuf,
    pub runtime_dir: PathBuf,
    pub blocked: Arc<Mutex<Vec<BlockedEvent>>>,
    sock_path: PathBuf,
    shim_path: PathBuf,
    allow_read: Vec<PathBuf>,
    allow_write: Vec<PathBuf>,
    _guards: Vec<tempfile::TempDir>,
}

/// Skip reason when the Linux environment cannot run a decisive probe.
pub fn linux_preflight() -> Option<String> {
    let ok = std::process::Command::new("bwrap")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return Some(
            "bwrap not on $PATH — the linux-sandbox CI job installs it via apt; locally: \
             `sudo apt install bubblewrap`"
                .into(),
        );
    }
    if !bwrap_userns_available() {
        return Some(
            "bwrap cannot create a user namespace on this host (unprivileged userns disabled \
             by sysctl/AppArmor) — see .github/workflows/linux-sandbox.yml's sysctl step"
                .into(),
        );
    }
    if resolve_tome_shim_bin().is_none() {
        return Some(
            "tome-shim binary not found — build it (`cargo build -p tome-shim [--release]`) or \
             set TOME_SHIM_BIN"
                .into(),
        );
    }
    if !Path::new("/bin/sh").exists() || !Path::new("/usr/bin/curl").exists() {
        return Some("/bin/sh or /usr/bin/curl missing — probe tools unavailable".into());
    }
    None
}

/// bwrap must be able to actually create a user namespace — mirrors the
/// integration tests' own smoke test (and their skip on the fedora job).
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

/// `TOME_SHIM_BIN` override, else the release build `scripts/
/// build-sidecar.sh` produces, else a dev build — same order as the
/// integration tests' resolver.
fn resolve_tome_shim_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TOME_SHIM_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // .../crates/tome-escape-suite
    for candidate in [
        "../../target/release/tome-shim",
        "../../target/debug/tome-shim",
    ] {
        let path = manifest_dir.join(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Builds the production rung-1 (bwrap) fixture — the SAME code path
/// `ipc::pty::pty_create` walks on Linux. Returns `None` when the
/// environment can't support a decisive run (skip, never a false pass).
pub async fn build_linux_fixture(initial_allowed: Vec<String>) -> Option<LinuxFixture> {
    if linux_preflight().is_some() {
        return None;
    }
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let config_dir = tempfile::tempdir().expect("config tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let home_dir = tempfile::tempdir().expect("home tempdir");
    let sock_path = runtime_dir.path().join("pane-escape-suite.sock");
    ensure_pane_socket_dir(runtime_dir.path()).expect("ensure pane socket dir");

    let blocked: Arc<Mutex<Vec<BlockedEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let blocked_for_cb = blocked.clone();
    let proxy = PaneProxy::spawn(initial_allowed, Some(sock_path.clone()), move |ev| {
        blocked_for_cb.lock().unwrap().push(ev);
    })
    .await
    .expect("spawn PaneProxy");
    let proxy = Arc::new(proxy);

    let (allow_read, allow_write) = default_landlock_allow_set(
        workspace.path(),
        home_dir.path(),
        None,
        &[PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
    );

    Some(LinuxFixture {
        proxy,
        config_dir: config_dir.path().to_path_buf(),
        workspace: workspace.path().to_path_buf(),
        home: home_dir.path().to_path_buf(),
        runtime_dir: runtime_dir.path().to_path_buf(),
        blocked,
        sock_path,
        shim_path: resolve_tome_shim_bin().expect("linux_preflight checked the shim"),
        allow_read,
        allow_write,
        _guards: vec![runtime_dir, config_dir, workspace, home_dir],
    })
}

impl LinuxFixture {
    /// The proxy env every real gapped pane gets (byte-identical to
    /// `agent_env::compose_agent_env`'s HTTP_PROXY/HTTPS_PROXY contract),
    /// with `NO_PROXY` empty — same reasoning as the integration tests:
    /// a populated NO_PROXY would make curl silently bypass the proxy and
    /// turn mechanism tests into direct-connect tests.
    pub fn http_proxy_env(&self) -> Vec<(String, String)> {
        let proxy_url = format!("http://127.0.0.1:{}", self.proxy.port());
        vec![
            ("HTTP_PROXY".to_string(), proxy_url.clone()),
            ("HTTPS_PROXY".to_string(), proxy_url.clone()),
            ("http_proxy".to_string(), proxy_url.clone()),
            ("https_proxy".to_string(), proxy_url),
            ("NO_PROXY".to_string(), String::new()),
            ("no_proxy".to_string(), String::new()),
        ]
    }

    /// Assembles the production bwrap argv for `inner` re-using this
    /// fixture's proxy/socket/allow-set — the identical assembly
    /// `build_bwrap_argv` performs at spawn time.
    fn argv_for(&self, inner: Vec<String>) -> Vec<String> {
        let spec = GappedSpawnSpec {
            pane_id: "escape-suite".to_string(),
            proxy_port: self.proxy.port(),
            host_socket_path: self.sock_path.clone(),
            docker_gateway_socket: None,
            app_config_dir: self.config_dir.clone(),
            shim_path: self.shim_path.clone(),
            inner_argv: inner,
            headless: true,
            allow_read: self.allow_read.clone(),
            allow_write: self.allow_write.clone(),
        };
        build_bwrap_argv(&spec)
    }

    /// The `bwrap <argv>` command ready to run `inner` inside the real
    /// wrap, with the proxy env + `extra_env` (env_clear + explicit set,
    /// TOME-007).
    pub fn command(&self, inner: Vec<String>, extra_env: &[(&str, &str)]) -> Command {
        let argv = self.argv_for(inner);
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
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
        cmd
    }

    /// Runs `inner` inside the real wrap to completion.
    pub async fn run_inner(&self, inner: Vec<String>, extra_env: &[(&str, &str)]) -> RunOutput {
        let child = self.command(inner, extra_env).spawn().expect("spawn bwrap");
        run_to_completion(child, TEST_TIMEOUT).await
    }
}
