//! Filtered Docker Engine gateway — the opt-in "safe Docker" route for a
//! gapped pane. A gapped pane's only container-runtime access is a Unix
//! socket owned by THIS module (the gateway), which proxies the Docker
//! Engine API to the host's real daemon while stripping the host-escape
//! primitives a raw `docker.sock` grant would hand over (privileged,
//! `--network/--pid/--ipc/--uts/--userns host`, dangerous capabilities,
//! host bind mounts outside the allowed roots, device access, and
//! unconfined security profiles). The real daemon socket is NEVER exposed
//! to the pane: the seatbelt profile denies the known `docker.sock`
//! literals on macOS (see `seatbelt.rs`) and the Linux allow-list omits
//! `~/.docker` (see `linux.rs`); this gateway is the pane's only route in.
//!
//! ## Why a Unix socket, not a TCP port
//!
//! On macOS the seatbelt profile's `(allow default)` lets a gapped pane
//! connect to ANY Unix socket except the denied `docker.sock` literals — so
//! this gateway's socket (a different path, outside `app_data_dir`) is
//! reachable with NO profile change, and F-01 (loopback pinned to the
//! pane's proxy port) is never weakened. On Linux the socket is
//! bind-mounted into the bwrap namespace exactly like the loopback-bridge
//! proxy socket (`CONTAINER_PROXY_SOCK_PATH` in `linux.rs`).
//!
//! ## Reject, never silently strip
//!
//! The filter returns `Err(reason)` for any escape primitive and the
//! gateway answers the request with a `403` whose body names the exact
//! offence, plus a deny callback the integrator fans out to the UI
//! ("blocked toast") and the event log. It does NOT rewrite the request to
//! quietly drop the offending field — a dev who asked for `--privileged`
//! must see WHY it was refused, not have the flag vanish and the container
//! silently behave differently from what they asked.
//!
//! ## Transport notes (HTTP/1.1 over Unix sockets)
//!
//! The Docker Engine API is plain HTTP/1.1. Interactive attach/exec
//! (`docker run -it`, `docker exec -it`) use HTTP connection hijack — a
//! `Upgrade: tcp` request answered by a `101 UPGRADED` response followed by
//! a raw bidirectional stream — which the gateway passes through verbatim.
//! Everything else is forwarded with `Connection: close` forced on the
//! daemon leg so the response body can be streamed to EOF without parsing
//! chunked framing. BuildKit's h2c/gRPC control channel is deliberately NOT
//! proxied (it is not plain HTTP/1.1): the integrator sets
//! `DOCKER_BUILDKIT=0` in the pane env so the CLI falls back to the legacy
//! builder, which IS a plain HTTP POST the gateway can mediate.

// Same module-level rationale as the other egress submodules: everything
// here is exercised by `#[cfg(test)]` below, and the real callers
// (`ipc::egress`, `ipc::pty`) are separate files.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::AbortHandle;

/// Maximum in-memory request body the gateway will buffer to inspect a
/// container-create payload. A create body is a few KB; anything larger is
/// refused rather than buffered unboundedly.
const MAX_CREATE_BODY: usize = 4 * 1024 * 1024;

/// Capabilities that grant a container host-escape reach. Any one of these
/// in `HostConfig.CapAdd` is refused. The set is deliberately conservative:
/// a capability not listed here is left alone (the point is to deny the
/// escape primitives, not to audit every benign cap).
const DANGEROUS_CAPS: &[&str] = &[
    "SYS_ADMIN",
    "SYS_MODULE",
    "SYS_RAWIO",
    "SYS_PTRACE",
    "SYS_BOOT",
    "SYS_TIME",
    "NET_ADMIN",
    "NET_RAW",
    "NET_BROADCAST",
    "DAC_READ_SEARCH",
    "DAC_OVERRIDE",
    "MKNOD",
    "SYS_CHROOT",
    "SYSLOG",
    "SETFCAP",
    "SYS_PACCT",
    "SYS_TTY_CONFIG",
    "BLOCK_SUSPEND",
    "WAKE_ALARM",
    "LINUX_IMMUTABLE",
    "PERFMON",
    "BPF",
    "AUDIT_READ",
    "AUDIT_WRITE",
    "AUDIT_CONTROL",
    "MAC_ADMIN",
    "MAC_OVERRIDE",
    "SYS_RESOURCE",
    "SYS_NICE",
    "LEASE",
    "CHECKPOINT_RESTORE",
];

/// Namespace modes whose `host` value puts the container in the host's
/// network/pid/ipc/uts/user namespaces — an escape by construction.
const HOST_ONLY_MODES: &[&str] = &["NetworkMode", "PidMode", "IpcMode", "UTSMode", "UsernsMode"];

/// The per-pane mount policy plus the escape-primitive check. `Default`
/// gives a closed policy (no mount roots), which is what a caller uses
/// before threading the real workspace roots in.
#[derive(Debug, Clone, Default)]
pub struct DockerPolicy {
    /// Absolute host source paths a bind/`Mounts` entry may read from —
    /// the open workspace roots plus (when wired) a scratch docker-data dir.
    pub allowed_mount_roots: Vec<PathBuf>,
}

/// One refused operation, carrying the human reason. The integrator maps
/// this to a UI toast + event-log entry; the gateway itself is UI-free.
#[derive(Debug, Clone, PartialEq)]
pub struct DockerDenied {
    pub reason: String,
}

impl DockerPolicy {
    /// Checks a container-create request body (Docker's
    /// `ContainerCreateRequest`) for escape primitives. `Ok(())` means the
    /// body may be forwarded; `Err(reason)` means the gateway must refuse it.
    ///
    /// Looks for the `HostConfig` field, and also tolerates the body being a
    /// bare `HostConfig` (some callers post one directly).
    pub fn check_create(&self, body: &Value) -> Result<(), DockerDenied> {
        if let Some(hc) = body.get("HostConfig") {
            self.check_host_config(hc)?;
        }
        // Defensive: a body that IS a HostConfig (no wrapper key) still gets
        // checked. Harmless when `body` is a full request that happens to
        // carry one of these keys at top level (none of the checked keys are
        // valid top-level `ContainerCreateRequest` fields, so no false
        // positives).
        self.check_host_config(body)?;
        Ok(())
    }

    fn check_host_config(&self, hc: &Value) -> Result<(), DockerDenied> {
        if hc
            .get("Privileged")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(DockerDenied {
                reason: "privileged containers are not allowed".to_string(),
            });
        }
        for mode in HOST_ONLY_MODES {
            if hc.get(*mode).and_then(Value::as_str) == Some("host") {
                return Err(DockerDenied {
                    reason: format!("{mode} host is not allowed"),
                });
            }
        }
        if let Some(caps) = hc.get("CapAdd").and_then(Value::as_array) {
            for cap in caps {
                if let Some(c) = cap.as_str() {
                    if DANGEROUS_CAPS.iter().any(|d| d.eq_ignore_ascii_case(c)) {
                        return Err(DockerDenied {
                            reason: format!("capability {c} is not allowed"),
                        });
                    }
                }
            }
        }
        if let Some(binds) = hc.get("Binds").and_then(Value::as_array) {
            for b in binds {
                if let Some(b) = b.as_str() {
                    self.check_bind(b)?;
                }
            }
        }
        if let Some(mounts) = hc.get("Mounts").and_then(Value::as_array) {
            for m in mounts {
                self.check_mount(m)?;
            }
        }
        if let Some(devices) = hc.get("Devices").and_then(Value::as_array) {
            if !devices.is_empty() {
                return Err(DockerDenied {
                    reason: "device access is not allowed".to_string(),
                });
            }
        }
        if let Some(opts) = hc.get("SecurityOpt").and_then(Value::as_array) {
            for o in opts {
                if let Some(o) = o.as_str() {
                    let lower = o.to_ascii_lowercase();
                    if lower == "seccomp=unconfined" || lower == "apparmor=unconfined" {
                        return Err(DockerDenied {
                            reason: format!("{o} is not allowed"),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// A `Binds` entry — `"src:dst[:mode]"`, source first.
    fn check_bind(&self, bind: &str) -> Result<(), DockerDenied> {
        let src = bind.split(':').next().unwrap_or("");
        self.check_host_source(src)
    }

    /// A `Mounts` entry. Only `bind` (and legacy `none`) mounts carry a host
    /// source path; `volume`/`tmpfs` mounts have no host path and are fine.
    fn check_mount(&self, mount: &Value) -> Result<(), DockerDenied> {
        let ty = mount.get("Type").and_then(Value::as_str).unwrap_or("");
        if ty == "bind" || ty == "none" {
            if let Some(src) = mount.get("Source").and_then(Value::as_str) {
                self.check_host_source(src)?;
            }
        }
        Ok(())
    }

    /// The host path a bind/mount wants to read from. Denied when it is the
    /// Docker socket or any container-runtime dir (DinD re-entry), when it
    /// is an absolute path outside the allowed roots, or a relative host
    /// path (named volumes — sources with no `/` — are the only non-absolute
    /// source we accept; a `./rel` host bind is conservatively refused).
    fn check_host_source(&self, src: &str) -> Result<(), DockerDenied> {
        if src.is_empty() {
            return Ok(());
        }
        let lower = src.to_ascii_lowercase();
        if lower.contains("docker.sock") || lower.contains(".docker") {
            return Err(DockerDenied {
                reason: format!("mount source {src} (container runtime) is not allowed"),
            });
        }
        let p = Path::new(src);
        if p.is_absolute() {
            let allowed = self.allowed_mount_roots.iter().any(|r| p.starts_with(r));
            if !allowed {
                return Err(DockerDenied {
                    reason: format!("host bind mount {src} is outside the allowed roots"),
                });
            }
        } else if src.contains('/') {
            return Err(DockerDenied {
                reason: format!("relative bind mount {src} is not allowed"),
            });
        }
        Ok(())
    }
}

/// Resolves the host Docker daemon socket path, in precedence order:
/// `DOCKER_HOST=unix://…` (if set and present), then the standard Docker
/// Desktop / default Linux socket, then the rootless runtime-dir socket.
/// Returns `None` when no candidate exists — the caller treats that as
/// "Docker not available" and spawns the pane without a gateway.
pub fn resolve_daemon_socket() -> Option<PathBuf> {
    if let Ok(host) = std::env::var("DOCKER_HOST") {
        if let Some(path) = host.strip_prefix("unix://") {
            let p = PathBuf::from(path);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let home = std::env::home_dir()?;
    let mut candidates: Vec<PathBuf> = vec![
        home.join(".docker/run/docker.sock"),
        PathBuf::from("/var/run/docker.sock"),
    ];
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        candidates.push(PathBuf::from(runtime).join("docker.sock"));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// The gateway socket path for a pane. Linux keeps it next to the loopback
/// bridge under `$XDG_RUNTIME_DIR/tome/` (so the same `0700` parent-dir
/// discipline the proxy socket relies on applies); macOS keeps it under
/// `~/Tome/docker/` (outside `app_data_dir`, so the seatbelt profile's
/// config-dir deny never covers it, and outside the `docker.sock` literals
/// the profile denies).
pub fn gateway_socket_path(pane_id: &str) -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime)
            .join("tome")
            .join(format!("{pane_id}-docker.sock"));
    }
    let home = std::env::home_dir().unwrap_or_default();
    home.join("Tome")
        .join("docker")
        .join(format!("{pane_id}.sock"))
}

/// A minimal HTTP head (request or response): the start line plus headers.
struct Head {
    line: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn is_status(&self, status: u16) -> bool {
        self.line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            == Some(status)
    }
}

async fn read_head<R: AsyncBufRead + Unpin>(reader: &mut R) -> Option<Head> {
    let mut line = String::new();
    if reader.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let line = line.trim_end_matches(['\r', '\n']).to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).await.ok()? == 0 {
            return None;
        }
        let h = h.trim_end_matches(['\r', '\n']);
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Some(Head { line, headers })
}

/// Whether a request method+target needs create-payload filtering.
fn needs_filter(method: &str, target: &str) -> bool {
    let m = method.to_ascii_uppercase();
    if m != "POST" && m != "PUT" {
        return false;
    }
    let path = target.split('?').next().unwrap_or("");
    path.ends_with("/containers/create")
        || path.ends_with("/services/create")
        || (path.contains("/containers/") && path.ends_with("/update"))
}

struct GatewayState {
    daemon: PathBuf,
    policy: DockerPolicy,
    on_deny: Box<dyn Fn(DockerDenied) + Send + Sync>,
}

/// A single pane's filtered Docker gateway: binds a Unix socket, proxies
/// the Engine API to the real daemon, and refuses escape primitives.
pub struct DockerGateway {
    state: Arc<GatewayState>,
    socket_path: PathBuf,
    accept_task: AbortHandle,
}

impl DockerGateway {
    /// Binds `socket_path` and starts serving. Stale sockets from a crashed
    /// prior run are removed first. The caller owns creating the parent
    /// directory (and its `0700` lockdown) before this runs.
    pub async fn spawn(
        socket_path: PathBuf,
        daemon: PathBuf,
        policy: DockerPolicy,
        on_deny: impl Fn(DockerDenied) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        let state = Arc::new(GatewayState {
            daemon,
            policy,
            on_deny: Box::new(on_deny),
        });
        let accept_state = state.clone();
        let accept_task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        if std::env::var("TOME_DOCKER_DEBUG").is_ok() {
                            eprintln!("[docker-gw] accepted connection");
                        }
                        let st = accept_state.clone();
                        tokio::spawn(async move { handle_connection(stream, st).await });
                    }
                    Err(_) => continue,
                }
            }
        });
        Ok(Self {
            state,
            socket_path,
            accept_task: accept_task.abort_handle(),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn shutdown(&self) {
        self.accept_task.abort();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for DockerGateway {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn handle_connection(stream: UnixStream, state: Arc<GatewayState>) {
    let mut reader = BufReader::new(stream);
    let Some(head) = read_head(&mut reader).await else {
        if std::env::var("TOME_DOCKER_DEBUG").is_ok() {
            eprintln!("[docker-gw] read_head returned None (client sent nothing/closed)");
        }
        return;
    };
    if std::env::var("TOME_DOCKER_DEBUG").is_ok() {
        eprintln!("[docker-gw] request line: {}", head.line);
    }
    let (method, target) = {
        let mut parts = head.line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("").to_string();
        (method, target)
    };

    let content_length: usize = head
        .header("content-length")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = Vec::new();
    if content_length > 0 {
        if content_length > MAX_CREATE_BODY {
            let _ = deny_client(&mut reader, "request body too large").await;
            return;
        }
        body.resize(content_length, 0);
        if reader.read_exact(&mut body).await.is_err() {
            return;
        }
    }
    // Any bytes BufReader already buffered past the body belong to the raw
    // stream (hijack); keep them for the copy below.
    let pending = reader.buffer().to_vec();
    let mut client = reader.into_inner();

    // Filter create payloads before anything reaches the daemon.
    if needs_filter(&method, &target) && !body.is_empty() {
        if let Ok(value) = serde_json::from_slice::<Value>(&body) {
            if let Err(denied) = state.policy.check_create(&value) {
                (state.on_deny)(denied.clone());
                let resp = format!(
                    "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    json!({ "message": denied.reason }).to_string().len(),
                    json!({ "message": denied.reason }),
                );
                let _ = client.write_all(resp.as_bytes()).await;
                let _ = client.shutdown().await;
                return;
            }
        }
    }

    let Ok(mut daemon) = UnixStream::connect(&state.daemon).await else {
        let _ = client
            .write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await;
        let _ = client.shutdown().await;
        return;
    };

    let is_upgrade = head.header("upgrade").is_some();
    // Forward the request. Non-upgrade legs force `Connection: close` on the
    // daemon side so the response body can be streamed to EOF.
    let _ = daemon.write_all(head.line.as_bytes()).await;
    let _ = daemon.write_all(b"\r\n").await;
    for (k, v) in &head.headers {
        if k.eq_ignore_ascii_case("connection") || k.eq_ignore_ascii_case("proxy-connection") {
            continue;
        }
        let _ = daemon.write_all(format!("{k}: {v}\r\n").as_bytes()).await;
    }
    if !is_upgrade {
        let _ = daemon.write_all(b"Connection: close\r\n").await;
    }
    let _ = daemon.write_all(b"\r\n").await;
    if !body.is_empty() {
        let _ = daemon.write_all(&body).await;
    }

    let mut daemon_reader = BufReader::new(daemon);
    let Some(resp) = read_head(&mut daemon_reader).await else {
        let _ = client.shutdown().await;
        return;
    };

    // Write the response head back to the client, minus hop-by-hop framing.
    let mut resp_head = String::new();
    resp_head.push_str(&resp.line);
    resp_head.push_str("\r\n");
    for (k, v) in &resp.headers {
        if k.eq_ignore_ascii_case("connection")
            || k.eq_ignore_ascii_case("keep-alive")
            || k.eq_ignore_ascii_case("transfer-encoding")
        {
            continue;
        }
        resp_head.push_str(&format!("{k}: {v}\r\n"));
    }
    if resp.is_status(101) {
        resp_head.push_str("Connection: Upgrade\r\nUpgrade: tcp\r\n");
    } else {
        resp_head.push_str("Connection: close\r\n");
    }
    resp_head.push_str("\r\n");
    if client.write_all(resp_head.as_bytes()).await.is_err() {
        return;
    }

    let resp_pending = daemon_reader.buffer().to_vec();
    let resp_content_length: Option<usize> = resp
        .header("content-length")
        .and_then(|v| v.trim().parse().ok());
    let mut daemon = daemon_reader.into_inner();

    if resp.is_status(101) || is_upgrade {
        // Hijacked stream: raw bidirectional copy until either side closes.
        let mut client_buf = pending;
        let mut daemon_buf = resp_pending;
        copy_bidirectional_with_buffers(&mut client, &mut daemon, &mut client_buf, &mut daemon_buf)
            .await;
    } else {
        forward_body(&mut daemon, &mut client, resp_pending, resp_content_length).await;
        let _ = client.shutdown().await;
    }
}

/// Forwards the response body from `daemon` to `client`. `pending` holds
/// bytes already buffered past the response head. When the response carries
/// a `Content-Length`, exactly that many body bytes are forwarded and the
/// call returns (the daemon may keep the connection alive); otherwise the
/// body is streamed until EOF (the request forced `Connection: close`, so
/// EOF is the body terminator for chunked/raw-stream responses).
async fn forward_body(
    daemon: &mut UnixStream,
    client: &mut UnixStream,
    pending: Vec<u8>,
    content_length: Option<usize>,
) {
    if !pending.is_empty() && client.write_all(&pending).await.is_err() {
        return;
    }
    match content_length {
        Some(total) => {
            let mut remaining = total.saturating_sub(pending.len());
            let mut buf = [0u8; 8192];
            while remaining > 0 {
                let want = remaining.min(buf.len());
                match daemon.read(&mut buf[..want]).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        remaining -= n;
                        if client.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
        None => {
            let mut buf = [0u8; 8192];
            loop {
                match daemon.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if client.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

async fn deny_client<S>(client: &mut S, msg: &str) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let body = json!({ "message": msg }).to_string();
    let resp = format!(
        "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    client.write_all(resp.as_bytes()).await?;
    client.shutdown().await
}

async fn copy_bidirectional_with_buffers(
    a: &mut UnixStream,
    b: &mut UnixStream,
    a_pending: &mut Vec<u8>,
    b_pending: &mut Vec<u8>,
) {
    let mut a_owned = std::mem::take(a_pending);
    let mut b_owned = std::mem::take(b_pending);

    let a_r = a;
    let b_r = b;
    loop {
        tokio::select! {
            res = read_some(a_r, &mut a_owned) => {
                match res {
                    Some(bytes) => { if b_r.write_all(&bytes).await.is_err() { break; } }
                    None => { let _ = b_r.shutdown().await; break; }
                }
            }
            res = read_some(b_r, &mut b_owned) => {
                match res {
                    Some(bytes) => { if a_r.write_all(&bytes).await.is_err() { break; } }
                    None => { let _ = a_r.shutdown().await; break; }
                }
            }
        }
    }
}

async fn read_some(stream: &mut UnixStream, pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    if !pending.is_empty() {
        return Some(std::mem::take(pending));
    }
    let mut buf = [0u8; 8192];
    match stream.read(&mut buf).await {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(buf[..n].to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> DockerPolicy {
        DockerPolicy {
            allowed_mount_roots: vec![PathBuf::from("/Users/test/workspace")],
        }
    }

    fn check(p: &DockerPolicy, host_config: Value) -> Result<(), DockerDenied> {
        p.check_create(&json!({ "HostConfig": host_config }))
    }

    fn assert_denied(p: &DockerPolicy, host_config: Value, needle: &str) {
        let err = check(p, host_config).unwrap_err();
        assert!(
            err.reason.contains(needle),
            "expected reason containing {needle:?}, got {err:?}"
        );
    }

    // ---- escape primitives: one test per rule ----

    #[test]
    fn allows_a_plain_run_with_named_volume() {
        check(
            &policy(),
            json!({ "Binds": ["myvol:/data"], "NetworkMode": "bridge" }),
        )
        .unwrap();
    }

    #[test]
    fn denies_privileged() {
        assert_denied(&policy(), json!({ "Privileged": true }), "privileged");
    }

    #[test]
    fn allows_privileged_explicitly_false() {
        check(&policy(), json!({ "Privileged": false })).unwrap();
    }

    #[test]
    fn denies_each_host_namespace_mode() {
        for mode in ["NetworkMode", "PidMode", "IpcMode", "UTSMode", "UsernsMode"] {
            let mut hc = serde_json::Map::new();
            hc.insert(mode.to_string(), json!("host"));
            assert_denied(&policy(), Value::Object(hc), "host");
        }
    }

    #[test]
    fn allows_a_non_host_network_mode() {
        check(&policy(), json!({ "NetworkMode": "bridge" })).unwrap();
    }

    #[test]
    fn denies_dangerous_capabilities() {
        assert_denied(&policy(), json!({ "CapAdd": ["SYS_ADMIN"] }), "SYS_ADMIN");
        assert_denied(&policy(), json!({ "CapAdd": ["NET_ADMIN"] }), "NET_ADMIN");
        // case-insensitive
        assert_denied(&policy(), json!({ "CapAdd": ["sys_admin"] }), "sys_admin");
    }

    #[test]
    fn allows_benign_capabilities() {
        check(&policy(), json!({ "CapAdd": ["CHOWN", "SETUID"] })).unwrap();
    }

    #[test]
    fn denies_bind_mount_outside_allowed_roots() {
        assert_denied(
            &policy(),
            json!({ "Binds": ["/etc/passwd:/etc/passwd:ro"] }),
            "outside the allowed roots",
        );
    }

    #[test]
    fn denies_bind_mount_of_root() {
        assert_denied(&policy(), json!({ "Binds": ["/:/host"] }), "outside");
    }

    #[test]
    fn denies_bind_mount_of_ssh_dir() {
        assert_denied(
            &policy(),
            json!({ "Binds": ["/Users/test/.ssh:/root/.ssh"] }),
            "outside",
        );
    }

    #[test]
    fn allows_bind_mount_inside_workspace() {
        check(
            &policy(),
            json!({ "Binds": ["/Users/test/workspace/app:/app"] }),
        )
        .unwrap();
    }

    #[test]
    fn denies_bind_mount_of_the_docker_socket() {
        assert_denied(
            &policy(),
            json!({ "Binds": ["/var/run/docker.sock:/var/run/docker.sock"] }),
            "container runtime",
        );
        assert_denied(
            &policy(),
            json!({ "Binds": ["/Users/test/.docker/run/docker.sock:/var/run/docker.sock"] }),
            "container runtime",
        );
    }

    #[test]
    fn denies_relative_host_bind_mount() {
        assert_denied(&policy(), json!({ "Binds": ["./data:/data"] }), "relative");
    }

    #[test]
    fn denies_mounts_bind_source_outside_roots() {
        assert_denied(
            &policy(),
            json!({ "Mounts": [{ "Type": "bind", "Source": "/etc", "Target": "/etc" }] }),
            "outside",
        );
    }

    #[test]
    fn allows_volume_and_tmpfs_mounts() {
        check(
            &policy(),
            json!({ "Mounts": [
                { "Type": "volume", "Source": "myvol", "Target": "/data" },
                { "Type": "tmpfs", "Target": "/tmp" }
            ] }),
        )
        .unwrap();
    }

    #[test]
    fn denies_any_device() {
        assert_denied(
            &policy(),
            json!({ "Devices": [{ "PathOnHost": "/dev/sda" }] }),
            "device",
        );
    }

    #[test]
    fn denies_unconfined_security_opt() {
        assert_denied(
            &policy(),
            json!({ "SecurityOpt": ["seccomp=unconfined"] }),
            "unconfined",
        );
        assert_denied(
            &policy(),
            json!({ "SecurityOpt": ["apparmor=unconfined"] }),
            "unconfined",
        );
    }

    #[test]
    fn allows_normal_security_opts() {
        check(&policy(), json!({ "SecurityOpt": ["no-new-privileges"] })).unwrap();
    }

    #[test]
    fn checks_a_body_that_is_a_bare_host_config() {
        assert_denied(&policy(), json!({ "Privileged": true }), "privileged");
    }

    #[test]
    fn empty_roots_deny_all_absolute_binds() {
        let closed = DockerPolicy::default();
        assert_denied(
            &closed,
            json!({ "Binds": ["/Users/test/workspace/app:/app"] }),
            "outside",
        );
    }

    #[test]
    fn sibling_prefix_is_not_inside_a_root() {
        let p = DockerPolicy {
            allowed_mount_roots: vec![PathBuf::from("/Users/test/workspace")],
        };
        // Path::starts_with is component-aware: "workspace-evil" must not match.
        assert_denied(
            &p,
            json!({ "Binds": ["/Users/test/workspace-evil:/app"] }),
            "outside",
        );
    }

    // ---- needs_filter ----

    #[test]
    fn filters_container_and_service_create_and_update() {
        assert!(needs_filter("POST", "/v1.49/containers/create"));
        assert!(needs_filter("POST", "/containers/create"));
        assert!(needs_filter("POST", "/services/create"));
        assert!(needs_filter("POST", "/containers/abc123/update"));
        assert!(needs_filter("PUT", "/containers/abc123/update"));
    }

    #[test]
    fn does_not_filter_get_or_other_endpoints() {
        assert!(!needs_filter("GET", "/containers/json"));
        assert!(!needs_filter("POST", "/containers/abc123/start"));
        assert!(!needs_filter("POST", "/images/create"));
        assert!(!needs_filter("GET", "/containers/create"));
    }

    // ---- gateway transport against a fake daemon socket ----

    async fn spawn_fake_daemon() -> (
        PathBuf,
        tokio::sync::mpsc::UnboundedReceiver<String>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut r = BufReader::new(stream);
                    let Some(head) = read_head(&mut r).await else {
                        return;
                    };
                    let cl: usize = head
                        .header("content-length")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let mut body = vec![0u8; cl];
                    if cl > 0 {
                        let _ = r.read_exact(&mut body).await;
                    }
                    let _ = tx.send(format!(
                        "{} {} {}",
                        head.line.split_whitespace().next().unwrap_or(""),
                        head.line.split_whitespace().nth(1).unwrap_or(""),
                        String::from_utf8_lossy(&body)
                    ));
                    let mut s = r.into_inner();
                    let resp =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                    let _ = s.write_all(resp).await;
                    let _ = s.shutdown().await;
                });
            }
        });
        (path, rx, dir)
    }

    #[tokio::test]
    async fn gateway_forwards_an_allowed_create_to_the_daemon() {
        let (daemon, mut rx, _dir) = spawn_fake_daemon().await;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("gw.sock");
        let gw = DockerGateway::spawn(sock.clone(), daemon, policy(), |_| panic!("must not deny"))
            .await
            .unwrap();

        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let body = json!({ "HostConfig": { "Binds": ["myvol:/data"] } }).to_string();
        let req = format!(
            "POST /v1.49/containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "got {text}");

        let forwarded = rx.recv().await.unwrap();
        assert!(
            forwarded.starts_with("POST /v1.49/containers/create "),
            "{forwarded}"
        );
        assert!(forwarded.contains("myvol"), "{forwarded}");
        gw.shutdown();
    }

    #[tokio::test]
    async fn gateway_refuses_privileged_without_touching_the_daemon() {
        let (daemon, mut rx, _dir) = spawn_fake_daemon().await;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("gw.sock");
        let (deny_tx, mut deny_rx) = tokio::sync::mpsc::unbounded_channel();
        let gw = DockerGateway::spawn(sock.clone(), daemon, policy(), move |d| {
            let _ = deny_tx.send(d);
        })
        .await
        .unwrap();

        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let body = json!({ "HostConfig": { "Privileged": true } }).to_string();
        let req = format!(
            "POST /containers/create HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(text.starts_with("HTTP/1.1 403"), "got {text}");
        assert!(text.contains("privileged"), "got {text}");

        let denied = deny_rx.recv().await.unwrap();
        assert!(denied.reason.contains("privileged"));
        // The fake daemon must never have seen the request.
        assert!(rx.is_closed() || rx.try_recv().is_err());
        gw.shutdown();
    }

    #[tokio::test]
    async fn gateway_passes_a_non_create_request_through_unfiltered() {
        let (daemon, mut rx, _dir) = spawn_fake_daemon().await;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("gw.sock");
        let gw = DockerGateway::spawn(sock.clone(), daemon, DockerPolicy::default(), |_| {
            panic!("must not deny")
        })
        .await
        .unwrap();

        let mut stream = UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"GET /v1.49/containers/json HTTP/1.1\r\nHost: docker\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp).starts_with("HTTP/1.1 200 OK"));

        let forwarded = rx.recv().await.unwrap();
        assert!(
            forwarded.starts_with("GET /v1.49/containers/json "),
            "{forwarded}"
        );
        gw.shutdown();
    }
}
