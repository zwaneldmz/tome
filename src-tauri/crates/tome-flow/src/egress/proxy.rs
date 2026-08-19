//! Per-pane loopback CONNECT/HTTP proxy — the egress's only route out.
//! Ports the proxy half of `src/main/egress.js` (`createPaneProxy`,
//! `unlockPane`/`relockPane`'s tunnel-teardown mechanics, `closePane`/
//! `closeAll`, the blocked-event coalescer) into a Tauri-free,
//! unit-testable primitive.
//!
//! ## Ownership split with the egress orchestration layer
//!
//! `egress.js` interleaves TWO concerns in one module: the socket-level
//! proxy mechanics (this file's job), and MULTI-pane orchestration policy
//! — the `panes`/`appliedRepos` maps, `ALLOWED_UNLOCK_MINUTES` validation,
//! the unlock auto-relock timer, repo consent, and the seatbelt profile.
//! That policy layer is a later slice's job (`mod.rs`, per its own doc
//! comment: "mod.rs+seatbelt.rs = slice A4 (integration)"). [`PaneProxy`]
//! here is the single-pane primitive that slice composes: one
//! socket-owning instance per pane, holding a bare open/providers MODE
//! rather than an expiry or its own timer — [`PaneProxy::unlock`] flips
//! the mode only; scheduling the auto-relock after N minutes (and
//! validating N against `ALLOWED_UNLOCK_MINUTES` in the first place) is
//! the integrator's job, done by holding the [`PaneProxy`] handle and
//! calling [`PaneProxy::relock`] from its own timer. This keeps the
//! socket/tunnel mechanics here fully testable without any policy
//! decisions mixed in.
//!
//! ## TOME-002 (connect-completion recheck)
//!
//! A CONNECT tunnel's destination is re-checked (pane still alive AND
//! host still allowed) at connect-COMPLETION time, not just at accept
//! time: the host is attacker-controlled and can stall the TCP handshake
//! for as long as it likes, during which `relock`/`shutdown` may already
//! have run. Without this, a tunnel that was only ever allowed because
//! the pane was in `Open` mode could finish handshaking AFTER a relock
//! already swept the tunnel registry, register itself afterward, and pipe
//! forever with its host never having been re-checked. See
//! [`connect_upstream_rechecked`] for the exact sequencing, and its
//! `#[cfg(test)]` callers for how the race is pinned deterministically
//! (a real connect completes far too fast to race against from a test).
//!
//! The recheck alone is not the whole story: `handle_connect` still has to
//! write the "200 Connection Established" reply (to the pane's own local
//! socket) and, if the client packed extra bytes into the same TCP segment
//! as its CONNECT request, forward those to the upstream — both real
//! `.await` points, both AFTER the recheck above has already passed, and
//! BEFORE the JS original's synchronous-event-loop equivalent of
//! registering the tunnel where `relock`/`shutdown` can find it. Naively
//! porting that ordering re-opens the exact TOCTOU the recheck exists to
//! close: a `relock`/`shutdown` landing in that gap would find nothing in
//! `state.tunnels` yet, and once the writes finally complete the tunnel
//! registers itself anyway — unrechecked, on a host that may no longer be
//! allowed. [`register_connect_tunnel`] is what actually closes this: it
//! re-runs the SAME recheck a second time, then spawns the task that does
//! those writes AND registers its `AbortHandle` in `state.tunnels`
//! synchronously (no intervening `.await`), so a concurrent
//! `relock`/`shutdown` can always find and abort this tunnel — even one
//! still mid-handshake — rather than only ever seeing it once every risky
//! write has already succeeded.
//!
//! ## Linux seam (built in Phase 3, wired in Phase 4/slice L3)
//!
//! [`PaneProxy::spawn`] optionally binds a Unix domain socket alongside
//! the TCP listener, serving the identical proxy logic over both — Phase
//! 4's `tome-shim` (a fresh network namespace's PID 1) shovels bytes from
//! a bind-mounted copy of that socket to a TCP listener bound *inside*
//! the namespace, since a namespaced process cannot reach the host's
//! `127.0.0.1:<port>` directly. Nothing here implements the shim itself —
//! that's `crates/tome-shim`'s own job; this file only makes sure the
//! same per-connection handler works over `UnixStream` as well as
//! `TcpStream` (see [`handle_connection`]'s generic bound). The real
//! caller supplying a `Some(unix_socket_path)` is
//! `ipc::egress::create_gapped_pane_proxy`, itself called from
//! `ipc::pty::pty_create`'s Linux gapped branch — see that function's own
//! doc comment for the fallback-ladder decision this socket only ever
//! gets bound for.
//!
//! ## Blocked-event signal
//!
//! `on_blocked` (supplied to [`PaneProxy::spawn`]) fires two kinds of
//! [`BlockedEvent`]: [`BlockedEvent::Attempt`] on every single refused
//! request/CONNECT, uncoalesced (mirrors `onEvent('blocked', ...)`'s live
//! push), and [`BlockedEvent::Coalesced`] on the same 60-second-window
//! cadence as `logBlocked`/`flushBlocked` (immediately on a window's first
//! attempt, and again with the total count if more attempts land before
//! the window closes — mirrors the persistent event log's "× N" coalescing
//! exactly, including "a lone attempt in a window logs only once"). This
//! module never touches Tauri or the persistent event log directly — the
//! integrator's callback is expected to fan `Coalesced` out to
//! `events::append` (kind `"egress:blocked"`) and, if it wants the
//! uncoalesced live signal too, `Attempt` out to its own emit.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::AbortHandle;
use tokio::time::Instant;

#[cfg(unix)]
use tokio::net::UnixListener;

use super::allowlist::{compile_allowlist, is_allowed, HostMatcher};

#[allow(dead_code)] // read by relock()/host_allowed(); never constructed directly outside spawn()
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Only hosts in the current allow set may tunnel/forward.
    Providers,
    /// Every host is allowed — the widened window `unlock` opens.
    Open,
}

/// Fired once per refused attempt ([`Attempt`](BlockedEvent::Attempt)) and
/// on the 60s coalescing cadence ([`Coalesced`](BlockedEvent::Coalesced))
/// — see the module doc comment's "Blocked-event signal" section. Neither
/// variant carries a pane id: [`PaneProxy`] is already a single pane's
/// handle, so a caller managing several panes supplies pane context by
/// closing over it in the callback passed to [`PaneProxy::spawn`], rather
/// than every event threading it through.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockedEvent {
    Attempt { host: String },
    Coalesced { host: String, count: u32 },
}

/// The only ports a Providers-mode pane may reach on an otherwise
/// allowlisted host (F-05, the security assessment's port-restriction
/// finding): the standard HTTP/HTTPS ports — which is all every shipped
/// allowlist entry uses, and all a provider API's own traffic should
/// ever need. Without this, `host_allowed`'s hostname-only match let a
/// gapped pane CONNECT to arbitrary ports on allowlisted hosts
/// (`api.anthropic.com:22` as an SSH relay, for example). A pane that
/// genuinely needs a non-standard port on an allowlisted host uses the
/// explicit, second-factor-gated unlock (`Mode::Open`, which imposes no
/// port restriction) — the same tradeoff the unlock already makes for
/// hostnames.
const PROVIDER_PORTS: &[u16] = &[80, 443];

const BLOCKED_COALESCE: Duration = Duration::from_secs(60);

struct TunnelEntry {
    host: String,
    port: u16,
    abort: AbortHandle,
}

struct PendingBlock {
    count: u32,
    first_at: Instant,
    generation: u64,
}

struct ProxyState {
    allowed: RwLock<Vec<HostMatcher>>,
    mode: RwLock<Mode>,
    tunnels: Mutex<HashMap<u64, TunnelEntry>>,
    next_tunnel_id: AtomicU64,
    next_block_generation: AtomicU64,
    /// Flips true on `shutdown()` — the Rust analog of `!panes.get(id)`
    /// in `egress.js`'s TOME-002 recheck (see the module doc comment):
    /// `PaneProxy` doesn't disappear from a shared map the way a JS pane
    /// entry does, but this flag is the same "has this pane already been
    /// torn down" fact the recheck needs.
    closed: AtomicBool,
    on_blocked: Box<dyn Fn(BlockedEvent) + Send + Sync>,
    blocked_pending: Mutex<HashMap<String, PendingBlock>>,
    /// `BLOCKED_COALESCE` in production (`PaneProxy::spawn` always passes
    /// it); a field rather than `schedule_coalesced_log` reading the
    /// constant directly ONLY so `#[cfg(test)]` can shrink it — this
    /// crate's `tokio` dependency doesn't enable the `test-util` feature
    /// (this slice does not own `Cargo.toml`), so the coalescing tests use
    /// a short real window instead of `tokio::time::pause`/`advance`.
    coalesce_window: Duration,
    /// Ports a Providers-mode pane may reach on an allowlisted host —
    /// [`PROVIDER_PORTS`] in production; a field (not a constant read) so
    /// `#[cfg(test)]` can widen it to the kernel-assigned ports their
    /// echo/upstream fixtures actually bind to. `Mutex`, not a bare
    /// `Vec`: `ProxyState` is shared behind an `Arc`, and the tests
    /// mutate this list after spawn.
    allowed_ports: Mutex<Vec<u16>>,
    http_client: reqwest::Client,
}

impl ProxyState {
    fn new(
        initial_allowed: &[String],
        on_blocked: Box<dyn Fn(BlockedEvent) + Send + Sync>,
        coalesce_window: Duration,
        allowed_ports: Vec<u16>,
    ) -> Self {
        Self {
            allowed: RwLock::new(compile_allowlist(initial_allowed)),
            mode: RwLock::new(Mode::Providers),
            tunnels: Mutex::new(HashMap::new()),
            next_tunnel_id: AtomicU64::new(0),
            next_block_generation: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            on_blocked,
            blocked_pending: Mutex::new(HashMap::new()),
            coalesce_window,
            allowed_ports: Mutex::new(allowed_ports),
            // Direct connections only: this client makes the proxy's OWN
            // outbound requests (the absolute-URI HTTP leg) and must never
            // itself chain through an ambient HTTP_PROXY/HTTPS_PROXY the
            // host process happens to have set — that would defeat the
            // whole point of this being the pane's egress boundary.
            //
            // `.redirect(Policy::none())` is equally load-bearing and NOT
            // optional: reqwest's default policy transparently follows up
            // to 10 redirects internally, and `handle_plain` below checks
            // `host_allowed` only against the ORIGINAL request-target's
            // host, before ever handing the request to this client — a
            // redirect chased internally by reqwest would fetch (and
            // return to the gapped pane) whatever host a `Location` header
            // names, entirely unchecked. This is the egress's whole
            // purpose, so disabling redirect-chasing here is as
            // security-critical as `.no_proxy()` above. With it disabled,
            // a 3xx response comes back from `req.send()` as an ordinary
            // `Ok(resp)` (not an error) whose `Location` header
            // `handle_plain`'s existing generic response-forwarding loop
            // already writes straight back to the client unmodified —
            // exactly like the JS original's raw `http.request`/`.pipe()`
            // leg, which never auto-follows redirects either (a 3xx is
            // piped straight back to the pane's own client, whose own next
            // hop must reissue a fresh request through this same proxy,
            // subject to a fresh `host_allowed` check). See
            // `plain_http_leg_does_not_auto_follow_a_redirect_off_the_
            // allowlist` below for the pinned regression.
            http_client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client builds from static, always-valid config"),
        }
    }
}

/// A single pane's loopback proxy: the pane's only route to the network.
/// Binds `127.0.0.1:0` (kernel-assigned port) and, if a path is given, a
/// Unix domain socket too — see the module doc comment. Construct with
/// [`PaneProxy::spawn`]; call [`PaneProxy::shutdown`] (or just drop it —
/// `Drop` calls `shutdown` too) when the pane closes.
pub struct PaneProxy {
    port: u16,
    unix_path: Option<PathBuf>,
    state: Arc<ProxyState>,
    accept_tasks: Vec<AbortHandle>,
}

impl PaneProxy {
    /// Binds the proxy and starts accepting connections. `initial_allowed`
    /// is the pane's starting hostname pattern set (raw pattern strings —
    /// compiled internally via `allowlist::compile_allowlist`); pass the
    /// shipped `DEFAULT_ALLOW` plus whatever repo hosts are already
    /// consented, same as `createPaneProxy`'s caller supplies via the
    /// module-level `allowMatchers` today. `unix_socket_path`, when
    /// `Some`, ALSO serves the identical proxy over a Unix domain socket
    /// at that path (stale sockets from a crashed prior run are removed
    /// first) — see the module doc comment's Linux-seam section.
    pub async fn spawn(
        initial_allowed: Vec<String>,
        unix_socket_path: Option<PathBuf>,
        on_blocked: impl Fn(BlockedEvent) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();

        let state = Arc::new(ProxyState::new(
            &initial_allowed,
            Box::new(on_blocked),
            BLOCKED_COALESCE,
            PROVIDER_PORTS.to_vec(),
        ));

        let mut accept_tasks = Vec::with_capacity(2);
        let tcp_state = state.clone();
        let tcp_task = tokio::spawn(accept_loop(listener, tcp_state));
        accept_tasks.push(tcp_task.abort_handle());

        let mut bound_unix_path = None;
        if let Some(path) = &unix_socket_path {
            #[cfg(unix)]
            {
                let _ = std::fs::remove_file(path); // stale socket from a crashed prior run
                let unix_listener = UnixListener::bind(path)?;
                let unix_state = state.clone();
                let unix_task = tokio::spawn(accept_loop_unix(unix_listener, unix_state));
                accept_tasks.push(unix_task.abort_handle());
                bound_unix_path = Some(path.clone());
            }
            #[cfg(not(unix))]
            {
                let _ = path; // unix sockets are unix-only; parameter kept for API parity
            }
        }

        Ok(Self {
            port,
            unix_path: bound_unix_path,
            state,
            accept_tasks,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn unix_path(&self) -> Option<&PathBuf> {
        self.unix_path.as_ref()
    }

    pub fn mode(&self) -> Mode {
        *self.state.mode.read().unwrap()
    }

    /// Replaces the pane's compiled allow set — mirrors egress.js's
    /// `recompile()`. Takes effect for every connection accepted from now
    /// on (including the TOME-002 recheck of tunnels already mid-connect).
    pub fn set_allowed(&self, patterns: Vec<String>) {
        *self.state.allowed.write().unwrap() = compile_allowlist(&patterns);
    }

    /// Replaces the Providers-mode port allow-set (F-05). Production only
    /// ever leaves the [`PROVIDER_PORTS`] default `spawn` installs; this
    /// setter exists so a future policy change (for example consenting a
    /// repo host that lives on a non-standard port) and the test suites'
    /// kernel-assigned echo/upstream fixtures can widen the set without a
    /// second spawn path. Takes effect for every connection accepted from
    /// now on, including the TOME-002 recheck and [`relock`](Self::relock)'s
    /// tunnel-retain sweep.
    pub fn set_allowed_ports(&self, ports: Vec<u16>) {
        *self.state.allowed_ports.lock().unwrap() = ports;
    }

    /// Widens this pane's egress to any host until [`relock`](Self::relock)
    /// is called — mirrors `unlockPane`'s mode transition only. See the
    /// module doc comment: minutes validation and the auto-relock timer
    /// are the caller's job.
    pub fn unlock(&self) {
        *self.state.mode.write().unwrap() = Mode::Open;
    }

    /// Narrows back to providers-only AND kills every live tunnel whose
    /// host+port isn't allowed on its own merits under the CURRENT allow
    /// set — mirrors `relockPane`. A tunnel that's independently
    /// allowlisted (or repo-consented) survives, matching what the UI
    /// promises: relock narrows egress, it doesn't kill legitimate
    /// in-flight traffic.
    pub fn relock(&self) {
        *self.state.mode.write().unwrap() = Mode::Providers;
        let allowed = self.state.allowed.read().unwrap();
        let mut tunnels = self.state.tunnels.lock().unwrap();
        tunnels.retain(|_, entry| {
            let keep = is_allowed(&allowed, &entry.host)
                && self
                    .state
                    .allowed_ports
                    .lock()
                    .unwrap()
                    .contains(&entry.port);
            if !keep {
                entry.abort.abort();
            }
            keep
        });
    }

    /// Number of tunnels currently believed live. Filters out entries
    /// whose task has already finished on its own (natural peer close) —
    /// self-removal from the registry happens inside the tunnel task after
    /// its copy loop ends, which is spawned fractionally before the
    /// registry insert; filtering on `AbortHandle::is_finished` here makes
    /// this accurate regardless of that ordering rather than relying on
    /// the self-removal race resolving in a particular direction.
    pub fn live_tunnel_count(&self) -> usize {
        self.state
            .tunnels
            .lock()
            .unwrap()
            .values()
            .filter(|e| !e.abort.is_finished())
            .count()
    }

    /// Stops accepting new connections and kills every live tunnel —
    /// mirrors `closePane` (idempotent, same as `closePane`/`closeAll`
    /// calling it twice on quit).
    pub fn shutdown(&self) {
        self.state.closed.store(true, Ordering::SeqCst);
        for task in &self.accept_tasks {
            task.abort();
        }
        let mut tunnels = self.state.tunnels.lock().unwrap();
        for (_, entry) in tunnels.drain() {
            entry.abort.abort();
        }
        #[cfg(unix)]
        if let Some(path) = &self.unix_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for PaneProxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn accept_loop(listener: TcpListener, state: Arc<ProxyState>) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let st = state.clone();
                tokio::spawn(async move { handle_connection(stream, st).await });
            }
            // A single failed accept (for example, transient EMFILE) must not take
            // the whole pane offline.
            Err(_) => continue,
        }
    }
}

#[cfg(unix)]
async fn accept_loop_unix(listener: UnixListener, state: Arc<ProxyState>) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let st = state.clone();
                tokio::spawn(async move { handle_connection(stream, st).await });
            }
            Err(_) => continue,
        }
    }
}

// ---- request-head parsing ----

struct RequestHead {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl RequestHead {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Strips ASCII control characters from a raw request-line token.
/// `read_line`'s framing already guarantees no embedded real newline can
/// survive into a token (a `\n` always ends the line first), but a bare,
/// unpaired `\r` can still land mid-token if a client sends one — and
/// `target`/`host` get interpolated verbatim into hand-assembled response
/// BODIES later (`handle_connect`/`handle_plain`'s "is blocked" messages).
/// Embedding a stray control character there could never inject a header
/// or split the response (it lands after the blank line, in the body,
/// same as if it just weren't there), but stripping it at the source
/// means nobody reading a call site has to re-derive that argument.
/// No-op for every real request: legitimate methods/targets never contain
/// control characters.
fn strip_control_chars(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Reads a request line + headers (up to the blank line) off any buffered
/// async reader. Returns `None` on EOF or a malformed head — the caller's
/// only recourse in either case is to drop the connection, mirroring
/// Node's `server.on('clientError', (err, socket) => socket.destroy())`.
async fn read_request_head<R: AsyncBufRead + Unpin>(reader: &mut R) -> Option<RequestHead> {
    let mut line = String::new();
    if reader.read_line(&mut line).await.ok()? == 0 {
        return None; // EOF before a request line ever arrived
    }
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let mut parts = trimmed.splitn(3, ' ');
    let method = strip_control_chars(parts.next()?);
    let target = strip_control_chars(parts.next()?);
    parts.next()?; // HTTP version — parsed to shape-check, not otherwise used
    if method.is_empty() || target.is_empty() {
        return None;
    }

    let mut headers = Vec::new();
    loop {
        let mut hline = String::new();
        if reader.read_line(&mut hline).await.ok()? == 0 {
            return None; // EOF mid-headers
        }
        let hline = hline.trim_end_matches(['\r', '\n']);
        if hline.is_empty() {
            break;
        }
        let (name, value) = hline.split_once(':')?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    Some(RequestHead {
        method,
        target,
        headers,
    })
}

/// Generic over the accepted stream type so the SAME logic serves TCP and
/// Unix connections — see the module doc comment's Linux-seam section.
async fn handle_connection<S>(stream: S, state: Arc<ProxyState>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(stream);
    let Some(head) = read_request_head(&mut reader).await else {
        return;
    };
    // Any bytes the BufReader already pulled off the wire past the blank
    // line (Node's "head" bytes handed to the 'connect' event, for example, the
    // first flight of a TLS ClientHello arriving in the same packet as the
    // CONNECT request) must be replayed before any raw copy begins —
    // `into_inner()` would otherwise silently discard them.
    let pending = reader.buffer().to_vec();
    let stream = reader.into_inner();

    if head.method.eq_ignore_ascii_case("CONNECT") {
        handle_connect(stream, pending, &head, state).await;
    } else {
        handle_plain(stream, pending, &head, state).await;
    }
}

fn host_allowed(state: &ProxyState, host: &str, port: u16) -> bool {
    if *state.mode.read().unwrap() == Mode::Open {
        return true;
    }
    // F-05: Providers mode matches host AND port — an allowlisted host is
    // only reachable on the standard HTTP/HTTPS ports. Open mode (the
    // second-factor-gated unlock) drops the port check.
    state.allowed_ports.lock().unwrap().contains(&port)
        && is_allowed(&state.allowed.read().unwrap(), host)
}

fn note_blocked(state: &Arc<ProxyState>, host: &str) {
    (state.on_blocked)(BlockedEvent::Attempt {
        host: host.to_string(),
    });
    schedule_coalesced_log(state, host);
}

/// Port of `logBlocked`/`flushBlocked`'s 60s coalescing: the first attempt
/// in a window fires [`BlockedEvent::Coalesced`] immediately (`count: 1`);
/// later attempts inside the SAME window just bump an in-memory counter
/// (the flush task already spawned below targets the fixed deadline
/// `first_at + BLOCKED_COALESCE`, so — unlike JS's `setTimeout`, which the
/// original manually re-arms on every attempt — nothing needs rescheduling
/// here, only the counter changes); when the window closes, one trailing
/// `Coalesced` fires with the total count, but ONLY if more than one
/// attempt actually landed (a lone attempt was already covered by the
/// immediate fire, matching `flushBlocked`'s `if (!p || p.count < 2)
/// return`). A `generation` counter on each window guards a flush task
/// against firing for a window that's since been replaced (the "window
/// expired without a flush yet — log fresh" case JS handles with
/// `clearTimeout`).
fn schedule_coalesced_log(state: &Arc<ProxyState>, host: &str) {
    let now = Instant::now();
    let window = state.coalesce_window;
    let mut pending = state.blocked_pending.lock().unwrap();
    if let Some(p) = pending.get_mut(host) {
        if now.duration_since(p.first_at) < window {
            p.count += 1;
            return; // still inside the window; the spawned flush task owns the eventual log
        }
        // Window logically expired even if the flush task hasn't run yet
        // — fall through and start a fresh window below.
    }
    let generation = state.next_block_generation.fetch_add(1, Ordering::Relaxed);
    pending.insert(
        host.to_string(),
        PendingBlock {
            count: 1,
            first_at: now,
            generation,
        },
    );
    drop(pending);

    (state.on_blocked)(BlockedEvent::Coalesced {
        host: host.to_string(),
        count: 1,
    });

    let state = state.clone();
    let host = host.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(window).await;
        let mut pending = state.blocked_pending.lock().unwrap();
        let is_current_window = matches!(pending.get(&host), Some(p) if p.generation == generation);
        if !is_current_window {
            return; // a newer window already replaced this one; it owns the flush
        }
        let count = pending.remove(&host).unwrap().count;
        drop(pending);
        if count >= 2 {
            (state.on_blocked)(BlockedEvent::Coalesced { host, count });
        }
    });
}

// ---- CONNECT leg ----

/// Parses a CONNECT request-target ("host:port" authority form). Mirrors
/// egress.js's `req.url.lastIndexOf(':')` split — the LAST colon (not the
/// first) is what keeps a multi-colon target from being misread; NOTE this
/// deliberately matches the JS original's actual wire contract, which is
/// only unambiguous for UNBRACKETED hosts (an IPv6 literal must arrive
/// without brackets, for example `::1:9999`, for the split to land on the real
/// port separator — a bracketed `[::1]:9999` would (also matching the JS
/// original) extract host `"[::1]"` including the brackets, which fails to
/// resolve). Defaults to port 443 both when there is no colon at all and
/// when the colon exists but its suffix isn't a valid port number.
fn parse_connect_target(target: &str) -> (String, u16) {
    match target.rfind(':') {
        Some(idx) if idx > 0 => {
            let host = target[..idx].to_string();
            let port = target[idx + 1..].parse().unwrap_or(443);
            (host, port)
        }
        _ => (target.to_string(), 443),
    }
}

/// Attempts the upstream TCP connect for a CONNECT tunnel, re-checking
/// pane-alive AND host-allowed at connect-COMPLETION time before handing
/// the stream back as usable (TOME-002 — see the module doc comment). This
/// is the FIRST of two rechecks `handle_connect` performs — the second,
/// immediately before the tunnel is actually registered and piped, is
/// [`register_connect_tunnel`]'s job, since real `.await` points (writing
/// the "200" reply and any `pending` bytes) still separate this recheck
/// from that registration. `after_connect` fires exactly once,
/// synchronously, right after the connect succeeds and before the recheck
/// runs: production callers (`handle_connect`) pass a no-op; `#[cfg(test)]`
/// callers use it to deterministically simulate "the allow set (or pane)
/// changed while the connect was in flight" without depending on real
/// network timing, which a loopback connect completes far too fast to race
/// against honestly. Returns `None` on a connect error OR a failed
/// recheck; either way the caller must send no reply (see
/// `handle_connect`).
async fn connect_upstream_rechecked(
    state: &Arc<ProxyState>,
    host: &str,
    port: u16,
    after_connect: impl FnOnce(),
) -> Option<TcpStream> {
    let upstream = TcpStream::connect((host, port)).await.ok()?;
    after_connect();
    if state.closed.load(Ordering::SeqCst) || !host_allowed(state, host, port) {
        return None;
    }
    Some(upstream)
}

async fn handle_connect<S>(
    mut client: S,
    pending: Vec<u8>,
    head: &RequestHead,
    state: Arc<ProxyState>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (host, port) = parse_connect_target(&head.target);

    if !host_allowed(&state, &host, port) {
        note_blocked(&state, &host);
        let _ = client
            .write_all(
                format!("HTTP/1.1 403 Forbidden\r\n\r\negress: {host} is blocked\r\n").as_bytes(),
            )
            .await;
        let _ = client.shutdown().await;
        return;
    }

    // No reply on either failure branch below — a connect error and a
    // failed TOME-002 recheck both look, from the client's perspective,
    // like the connection simply dropped (matches egress.js: neither
    // `up.on('error', ...)` nor the post-connect recheck failure writes
    // anything before destroying the sockets).
    let Some(upstream) = connect_upstream_rechecked(&state, &host, port, || {}).await else {
        return;
    };

    // See `register_connect_tunnel`'s doc comment: production never wants
    // an artificial stall between the recheck above and the tunnel
    // becoming trackable, so this is always zero — only `#[cfg(test)]`
    // injects a real delay, to deterministically pin that a
    // relock/shutdown landing while the handshake writes are still in
    // flight actually finds and kills this tunnel.
    register_connect_tunnel(
        &state,
        host,
        port,
        client,
        upstream,
        pending,
        Duration::ZERO,
    )
    .await;
}

/// The second half of TOME-002: re-runs the SAME pane/host recheck
/// [`connect_upstream_rechecked`] already did once, then — only if it
/// still passes — spawns the task that writes the "200 Connection
/// Established" reply, forwards any `pending` bytes, and pipes the tunnel,
/// registering that task's `AbortHandle` in `state.tunnels` synchronously
/// (no `.await` between spawning and inserting) so `PaneProxy::relock`/
/// `PaneProxy::shutdown` can always find and abort this tunnel — even one
/// still mid-handshake — rather than only ever seeing it once every risky
/// write has already completed. See the module doc comment's TOME-002
/// section for why re-checking once, at connect-completion time, is not
/// enough on its own once the reply/forward writes themselves are real
/// `.await` points that can still be in flight when a relock/shutdown
/// runs.
///
/// `handshake_delay` is `Duration::ZERO` in production (`handle_connect`'s
/// only call site) — a deliberate `#[cfg(test)]`-only seam, the same
/// pattern `ProxyState::coalesce_window` already uses, so a test can force
/// the handshake writes to still be pending when it calls `relock`/
/// `shutdown`, without depending on real (and here, unreliably tiny —
/// `pending` is capped by `BufReader`'s ~8KB default and a fresh socket's
/// send buffer is typically far larger) TCP backpressure to stall them
/// honestly.
///
/// Returns whether the tunnel was actually registered (`false` on a
/// failed recheck) — production ignores it (mirrors `egress.js`'s
/// recheck-failure branch, which replies with nothing either); tests use
/// it to assert the recheck's own pass/fail behavior directly.
async fn register_connect_tunnel<S>(
    state: &Arc<ProxyState>,
    host: String,
    port: u16,
    client: S,
    upstream: TcpStream,
    pending: Vec<u8>,
    handshake_delay: Duration,
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if state.closed.load(Ordering::SeqCst) || !host_allowed(state, &host, port) {
        return false;
    }

    let id = state.next_tunnel_id.fetch_add(1, Ordering::SeqCst);
    let task_state = state.clone();
    let join = tokio::spawn(async move {
        let mut client = client;
        let mut upstream = upstream;
        if handshake_delay > Duration::ZERO {
            tokio::time::sleep(handshake_delay).await;
        }
        if client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_err()
        {
            task_state.tunnels.lock().unwrap().remove(&id);
            return;
        }
        if !pending.is_empty() && upstream.write_all(&pending).await.is_err() {
            task_state.tunnels.lock().unwrap().remove(&id);
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        task_state.tunnels.lock().unwrap().remove(&id);
    });
    state.tunnels.lock().unwrap().insert(
        id,
        TunnelEntry {
            host,
            port,
            abort: join.abort_handle(),
        },
    );
    true
}

// ---- absolute-URI HTTP leg ----

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| h.eq_ignore_ascii_case(name))
}

/// Forwards a plain (non-CONNECT) proxy request — request-target must be
/// an absolute URI, for example `GET http://host/path HTTP/1.1` — via `reqwest`.
/// Scoped deviations from a literal `egress.js` port, both because this
/// leg is the rare path (every shipped allowlist host is HTTPS-only, which
/// always arrives via the CONNECT leg above; this leg exists mainly for
/// completeness and plain-HTTP test upstreams) and because hand-rolling a
/// streaming HTTP/1.1 response writer isn't worth it for that rare path:
///
/// - Body forwarding is Content-Length-based only; chunked request bodies
///   aren't supported (there's no direct reqwest equivalent of piping
///   whatever bytes arrive verbatim the way `req.pipe(up)` does).
/// - The upstream response is buffered fully (`Response::bytes()`) rather
///   than streamed, and re-emitted with a freshly computed Content-Length
///   rather than whatever framing the upstream used. Long-lived SSE
///   responses are a non-issue in practice (they arrive over the CONNECT
///   leg, which streams via raw `copy_bidirectional` with no buffering at
///   all), so this only affects the rare plain-HTTP request.
/// - Hop-by-hop headers are stripped on BOTH directions (`egress.js` only
///   strips them request-bound; since this port already reconstructs the
///   response head by hand rather than reusing Node's `res.writeHead`, and
///   already computes its own Content-Length, stripping them symmetrically
///   is strictly safer and avoids protocol confusion — a deliberate
///   improvement over a literal port, not a fidelity gap).
async fn handle_plain<S>(
    mut client: S,
    pending: Vec<u8>,
    head: &RequestHead,
    state: Arc<ProxyState>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let Some(url) = reqwest::Url::parse(&head.target).ok() else {
        let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        let _ = client.shutdown().await;
        return;
    };
    let Some(host) = url.host_str().map(str::to_string) else {
        let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        let _ = client.shutdown().await;
        return;
    };
    // The URL's effective port: an explicit `:PORT` when present, else the
    // scheme's default (80 for http, 443 for https) — F-05's port check
    // covers this leg too.
    let port = url.port_or_known_default().unwrap_or(80);

    if !host_allowed(&state, &host, port) {
        note_blocked(&state, &host);
        let body = format!("egress: {host} is blocked (providers-only mode)\n");
        let resp = format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = client.write_all(resp.as_bytes()).await;
        let _ = client.shutdown().await;
        return;
    }

    let content_length: usize = head
        .header("content-length")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = pending;
    if body.len() < content_length {
        let mut rest = vec![0u8; content_length - body.len()];
        if client.read_exact(&mut rest).await.is_err() {
            let _ = client.shutdown().await;
            return;
        }
        body.extend_from_slice(&rest);
    } else {
        body.truncate(content_length);
    }

    let method =
        reqwest::Method::from_bytes(head.method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut req = state.http_client.request(method, url);
    for (name, value) in &head.headers {
        if is_hop_by_hop(name)
            || name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("host")
        {
            continue;
        }
        req = req.header(name.as_str(), value.as_str());
    }
    if !body.is_empty() {
        req = req.body(body);
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let mut out = format!(
                "HTTP/1.1 {} {}\r\n",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            );
            for (name, value) in resp.headers() {
                if is_hop_by_hop(name.as_str())
                    || name.as_str().eq_ignore_ascii_case("content-length")
                {
                    continue;
                }
                if let Ok(v) = value.to_str() {
                    out.push_str(name.as_str());
                    out.push_str(": ");
                    out.push_str(v);
                    out.push_str("\r\n");
                }
            }
            let body = resp.bytes().await.unwrap_or_default();
            out.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
            let _ = client.write_all(out.as_bytes()).await;
            let _ = client.write_all(&body).await;
        }
        Err(_) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
        }
    }
    let _ = client.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // ---- test helpers ----

    /// A bare TCP echo server on `addr` — bytes in, bytes out — so a live
    /// tunnel can be proven live (and a dead one proven dead) by
    /// round-tripping a payload. Mirrors `egress-proxy-lifecycle.test.js`'s
    /// `echoServer` helper.
    async fn spawn_echo_server(addr: &str) -> (u16, AbortHandle) {
        let listener = TcpListener::bind((addr, 0))
            .await
            .expect("bind echo server");
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = sock.read(&mut buf).await {
                        if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        (port, task.abort_handle())
    }

    /// A minimal fake HTTP upstream for the absolute-URI leg: reads
    /// whatever the client sends (never parsed) and always answers a
    /// canned 200.
    async fn spawn_fake_http_upstream() -> (u16, AbortHandle) {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind fake http upstream");
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let body = b"ok";
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.write_all(body).await;
                });
            }
        });
        (port, task.abort_handle())
    }

    /// Sends a raw CONNECT request through the proxy and reads back just
    /// the status line + headers (up to the blank line), returning the
    /// still-open stream (for tunnels expected to succeed) and the parsed
    /// status code. Mirrors `egress-proxy-lifecycle.test.js`'s
    /// `openTunnel` helper, generalized to also observe the status of a
    /// refused CONNECT.
    async fn connect_via_proxy(proxy_port: u16, target: &str) -> (TcpStream, u16) {
        let mut stream = TcpStream::connect(("127.0.0.1", proxy_port))
            .await
            .expect("connect to proxy");
        let req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
        stream
            .write_all(req.as_bytes())
            .await
            .expect("write CONNECT");
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            stream
                .read_exact(&mut byte)
                .await
                .expect("read CONNECT response");
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&head);
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (stream, status)
    }

    fn collector() -> (
        Arc<StdMutex<Vec<BlockedEvent>>>,
        impl Fn(BlockedEvent) + Send + Sync + 'static,
    ) {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let handle = events.clone();
        (events, move |e: BlockedEvent| {
            handle.lock().unwrap().push(e)
        })
    }

    async fn yield_a_few_times() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    /// A bare `Arc<ProxyState>` with no bound socket at all — the
    /// coalescing tests below exercise `note_blocked`/`schedule_coalesced_log`
    /// directly and don't need a live proxy, only a short `coalesce_window`
    /// (see `ProxyState`'s doc comment on that field for why it's
    /// test-shrinkable rather than the tests using paused tokio time).
    fn test_state(
        on_blocked: impl Fn(BlockedEvent) + Send + Sync + 'static,
        window: Duration,
    ) -> Arc<ProxyState> {
        Arc::new(ProxyState::new(
            &[],
            Box::new(on_blocked),
            window,
            PROVIDER_PORTS.to_vec(),
        ))
    }

    /// Admits a kernel-assigned test port into a proxy's Providers-mode
    /// port allow-set — production admits only [`PROVIDER_PORTS`], but the
    /// tests' echo/upstream fixtures bind to whatever port the kernel
    /// hands out, which is never 80/443 (both privileged). F-05's port
    /// check is what makes this helper necessary at all: without it every
    /// Providers-mode test below would be refused for port reasons instead
    /// of testing the host logic it means to pin.
    fn admit_port(proxy: &PaneProxy, port: u16) {
        proxy.state.allowed_ports.lock().unwrap().push(port);
    }

    // ---- CONNECT leg: allow / block ----

    #[tokio::test]
    async fn allowlisted_connect_tunnel_gets_200_and_echoes_bytes() {
        let (echo_port, _echo) = spawn_echo_server("127.0.0.1").await;
        let (events, cb) = collector();
        let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, cb)
            .await
            .unwrap();
        admit_port(&proxy, echo_port);

        let (mut stream, status) =
            connect_via_proxy(proxy.port(), &format!("127.0.0.1:{echo_port}")).await;
        assert_eq!(status, 200);

        stream.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");

        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn providers_mode_refuses_an_allowlisted_host_on_a_non_provider_port() {
        // F-05: hostname match alone must not be enough — the port must
        // also be one of PROVIDER_PORTS, so an allowlisted host can't be
        // used as a relay to arbitrary services on itself.
        let (echo_port, _echo) = spawn_echo_server("127.0.0.1").await;
        let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, |_| {})
            .await
            .unwrap();
        // Deliberately NOT admitting echo_port: the host is allowlisted,
        // the port is not.

        let (_stream, status) =
            connect_via_proxy(proxy.port(), &format!("127.0.0.1:{echo_port}")).await;
        assert_eq!(status, 403);
    }

    #[tokio::test]
    async fn open_mode_allows_an_allowlisted_host_on_any_port() {
        // The other half of F-05: the second-factor-gated unlock widens
        // ports as well as hosts.
        let (echo_port, _echo) = spawn_echo_server("127.0.0.1").await;
        let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, |_| {})
            .await
            .unwrap();
        proxy.unlock();

        let (_stream, status) =
            connect_via_proxy(proxy.port(), &format!("127.0.0.1:{echo_port}")).await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn non_allowlisted_connect_is_403_and_fires_the_attempt_callback() {
        let (events, cb) = collector();
        let proxy = PaneProxy::spawn(vec![], None, cb).await.unwrap();

        let (_stream, status) = connect_via_proxy(proxy.port(), "example.com:443").await;
        assert_eq!(status, 403);

        let logged = events.lock().unwrap();
        assert!(logged.iter().any(|e| *e
            == BlockedEvent::Attempt {
                host: "example.com".to_string()
            }));
    }

    #[test]
    fn strip_control_chars_removes_a_bare_stray_cr_but_leaves_normal_text_alone() {
        assert_eq!(strip_control_chars("evil.com\rBAD"), "evil.comBAD");
        assert_eq!(
            strip_control_chars("api.anthropic.com"),
            "api.anthropic.com"
        );
    }

    #[tokio::test]
    async fn a_target_with_an_embedded_bare_cr_degrades_to_a_clean_403_not_a_garbled_response() {
        let proxy = PaneProxy::spawn(vec![], None, |_| {}).await.unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap();
        // A bare `\r` (no paired `\n`) mid-target: read_line's framing
        // can't strip it as line-trailing whitespace since it isn't at the
        // end of the line, so it must be handled at the token level (see
        // `strip_control_chars`).
        stream
            .write_all(b"CONNECT evil.com\rBAD:443 HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(
            text.starts_with("HTTP/1.1 403"),
            "unexpected response: {text}"
        );
        // The stray CR must not have survived into the body: a raw
        // mid-body \r would not inject a header (it lands after the blank
        // line), but it should still be gone at the source.
        assert!(
            !text.contains("evil.com\rBAD"),
            "control character leaked into response: {text}"
        );
        assert!(text.contains("evil.comBAD"));
    }

    #[tokio::test]
    async fn set_allowed_recompiles_the_matcher_set_for_subsequent_connects() {
        let (echo_port, _echo) = spawn_echo_server("127.0.0.1").await;
        let proxy = PaneProxy::spawn(vec![], None, |_| {}).await.unwrap();
        admit_port(&proxy, echo_port);

        let (_s, status) = connect_via_proxy(proxy.port(), &format!("127.0.0.1:{echo_port}")).await;
        assert_eq!(status, 403);

        proxy.set_allowed(vec!["127.0.0.1".to_string()]);

        let (_s2, status2) =
            connect_via_proxy(proxy.port(), &format!("127.0.0.1:{echo_port}")).await;
        assert_eq!(status2, 200);
    }

    // ---- relock / shutdown tunnel teardown ----

    #[tokio::test]
    async fn relock_kills_an_open_mode_only_tunnel_but_spares_an_allowlisted_one() {
        let (echo_port, _echo) = spawn_echo_server("127.0.0.1").await;
        let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, |_| {})
            .await
            .unwrap();
        admit_port(&proxy, echo_port);
        proxy.unlock(); // mode Open: any host may tunnel right now

        let (mut allowed_sock, s1) =
            connect_via_proxy(proxy.port(), &format!("127.0.0.1:{echo_port}")).await;
        assert_eq!(s1, 200);
        // "localhost" resolves to the SAME loopback echo server but is not
        // itself a literal match for the "127.0.0.1" pattern — admitted
        // only because mode is currently Open, exactly the case relock
        // must sever.
        let (mut open_only_sock, s2) =
            connect_via_proxy(proxy.port(), &format!("localhost:{echo_port}")).await;
        assert_eq!(s2, 200);

        for sock in [&mut allowed_sock, &mut open_only_sock] {
            sock.write_all(b"pre").await.unwrap();
            let mut buf = [0u8; 3];
            sock.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"pre");
        }

        assert_eq!(proxy.live_tunnel_count(), 2);
        proxy.relock();
        assert_eq!(proxy.mode(), Mode::Providers);
        tokio::time::sleep(Duration::from_millis(50)).await; // let the abort land

        assert_eq!(proxy.live_tunnel_count(), 1);

        // Open-mode-only tunnel: cannot exchange another byte.
        let mut probe = [0u8; 1];
        let r = open_only_sock.read(&mut probe).await;
        assert!(matches!(r, Ok(0)) || r.is_err());

        // Provider-allowlisted tunnel: relock leaves it running.
        allowed_sock.write_all(b"post").await.unwrap();
        let mut buf2 = [0u8; 4];
        allowed_sock.read_exact(&mut buf2).await.unwrap();
        assert_eq!(&buf2, b"post");
    }

    #[tokio::test]
    async fn shutdown_kills_live_tunnels_and_stops_accepting_new_connections() {
        let (echo_port, _echo) = spawn_echo_server("127.0.0.1").await;
        let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, |_| {})
            .await
            .unwrap();
        admit_port(&proxy, echo_port);
        let (mut sock, status) =
            connect_via_proxy(proxy.port(), &format!("127.0.0.1:{echo_port}")).await;
        assert_eq!(status, 200);

        let port = proxy.port();
        proxy.shutdown();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut probe = [0u8; 1];
        let r = sock.read(&mut probe).await;
        assert!(matches!(r, Ok(0)) || r.is_err());

        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let proxy = PaneProxy::spawn(vec![], None, |_| {}).await.unwrap();
        proxy.shutdown();
        proxy.shutdown(); // must not panic
    }

    // ---- TOME-002 ----

    #[tokio::test]
    async fn tome_002_recheck_refuses_a_tunnel_deallowed_while_the_connect_was_in_flight() {
        let (echo_port, _echo) = spawn_echo_server("127.0.0.1").await;
        let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, |_| {})
            .await
            .unwrap();
        admit_port(&proxy, echo_port);

        let result = connect_upstream_rechecked(&proxy.state, "127.0.0.1", echo_port, || {
            // Simulates the allow set changing during the connect's flight
            // — by the time this closure runs, the connect already
            // succeeded; the recheck immediately after must still catch it.
            proxy.set_allowed(vec![]);
        })
        .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn tome_002_recheck_refuses_a_tunnel_whose_pane_closed_while_the_connect_was_in_flight() {
        let (echo_port, _echo) = spawn_echo_server("127.0.0.1").await;
        let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, |_| {})
            .await
            .unwrap();
        admit_port(&proxy, echo_port);

        let result = connect_upstream_rechecked(&proxy.state, "127.0.0.1", echo_port, || {
            proxy.state.closed.store(true, Ordering::SeqCst);
        })
        .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn tome_002_a_relock_landing_during_the_post_recheck_handshake_still_kills_the_tunnel() {
        // Regression test for the registration-after-risky-writes gap:
        // `register_connect_tunnel` must make this tunnel killable via
        // `state.tunnels` BEFORE its handshake writes (the "200" reply,
        // any `pending` bytes) run — not only once they've already
        // completed. `handshake_delay` (test-only — see
        // `register_connect_tunnel`'s doc comment) stands in for an
        // attacker stalling those writes, without this test needing to
        // race real TCP backpressure honestly (a fresh connection's small
        // writes essentially never block on realistic OS socket-buffer
        // defaults, so a real stall isn't something a test can lean on —
        // the same reasoning `connect_upstream_rechecked`'s own
        // `after_connect` injection hook is built on).
        let (echo_port, _echo) = spawn_echo_server("127.0.0.1").await;
        let proxy = PaneProxy::spawn(vec![], None, |_| {}).await.unwrap();
        proxy.unlock(); // Open mode only — exactly the case relock must sever

        let client_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let client_port = client_listener.local_addr().unwrap().port();
        let client_leg = TcpStream::connect(("127.0.0.1", client_port))
            .await
            .unwrap();
        let (mut observer, _) = client_listener.accept().await.unwrap();
        let upstream = TcpStream::connect(("127.0.0.1", echo_port)).await.unwrap();

        let registered = register_connect_tunnel(
            &proxy.state,
            "attacker.example".to_string(),
            echo_port,
            client_leg,
            upstream,
            Vec::new(),
            Duration::from_millis(150),
        )
        .await;
        assert!(registered, "the recheck (mode still Open) must pass");
        assert_eq!(
            proxy.live_tunnel_count(),
            1,
            "the tunnel must already be tracked before its delayed handshake write runs"
        );

        // Narrows back to providers-only (empty allow set): this tunnel
        // was only ever admitted because mode was Open, so relock must
        // kill it — even though its handshake write hasn't even started
        // yet.
        proxy.relock();

        let mut buf = [0u8; 64];
        let read = tokio::time::timeout(Duration::from_millis(1000), observer.read(&mut buf))
            .await
            .expect("must not hang");
        assert_eq!(
            read.unwrap_or(0),
            0,
            "an aborted tunnel must never deliver the 200 response"
        );
        assert_eq!(proxy.live_tunnel_count(), 0);
    }

    // ---- blocked-event coalescing ----

    const TEST_COALESCE_WINDOW: Duration = Duration::from_millis(120);
    // Comfortably past TEST_COALESCE_WINDOW so a real-clock flush has
    // landed, while staying well under a second per test.
    const PAST_WINDOW: Duration = Duration::from_millis(400);

    #[tokio::test]
    async fn blocked_events_coalesce_over_the_window() {
        let (events, cb) = collector();
        let state = test_state(cb, TEST_COALESCE_WINDOW);

        note_blocked(&state, "evil.example.com"); // first — logs immediately, count 1
        note_blocked(&state, "evil.example.com"); // coalesced
        note_blocked(&state, "evil.example.com"); // coalesced

        tokio::time::sleep(PAST_WINDOW).await;
        yield_a_few_times().await;

        let coalesced: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, BlockedEvent::Coalesced { .. }))
            .cloned()
            .collect();
        assert_eq!(
            coalesced,
            vec![
                BlockedEvent::Coalesced {
                    host: "evil.example.com".to_string(),
                    count: 1
                },
                BlockedEvent::Coalesced {
                    host: "evil.example.com".to_string(),
                    count: 3
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_lone_attempt_in_a_window_is_not_logged_a_second_time_at_flush() {
        let (events, cb) = collector();
        let state = test_state(cb, TEST_COALESCE_WINDOW);

        note_blocked(&state, "lonely.example.com");
        tokio::time::sleep(PAST_WINDOW).await;
        yield_a_few_times().await;

        let coalesced: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, BlockedEvent::Coalesced { .. }))
            .cloned()
            .collect();
        assert_eq!(
            coalesced,
            vec![BlockedEvent::Coalesced {
                host: "lonely.example.com".to_string(),
                count: 1
            }]
        );
    }

    #[tokio::test]
    async fn a_fresh_attempt_after_the_window_closes_starts_a_new_immediate_log() {
        let (events, cb) = collector();
        let state = test_state(cb, TEST_COALESCE_WINDOW);

        note_blocked(&state, "host.example.com");
        tokio::time::sleep(PAST_WINDOW).await;
        yield_a_few_times().await;
        note_blocked(&state, "host.example.com"); // new window: fresh immediate log
        tokio::time::sleep(PAST_WINDOW).await;
        yield_a_few_times().await;

        let coalesced: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, BlockedEvent::Coalesced { .. }))
            .cloned()
            .collect();
        assert_eq!(
            coalesced,
            vec![
                BlockedEvent::Coalesced {
                    host: "host.example.com".to_string(),
                    count: 1
                },
                BlockedEvent::Coalesced {
                    host: "host.example.com".to_string(),
                    count: 1
                },
            ]
        );
    }

    // ---- absolute-URI HTTP leg ----

    #[tokio::test]
    async fn plain_http_leg_forwards_an_allowlisted_absolute_uri_request() {
        let (upstream_port, _up) = spawn_fake_http_upstream().await;
        let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, |_| {})
            .await
            .unwrap();
        admit_port(&proxy, upstream_port);

        let mut stream = TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap();
        let req =
            format!("GET http://127.0.0.1:{upstream_port}/ HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();

        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(
            text.starts_with("HTTP/1.1 200"),
            "unexpected response: {text}"
        );
        assert!(text.ends_with("ok"), "unexpected response: {text}");
    }

    #[tokio::test]
    async fn plain_http_leg_blocks_a_non_allowlisted_host() {
        let (events, cb) = collector();
        let proxy = PaneProxy::spawn(vec![], None, cb).await.unwrap();

        let mut stream = TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap();
        let req = "GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n";
        stream.write_all(req.as_bytes()).await.unwrap();

        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(
            text.starts_with("HTTP/1.1 403"),
            "unexpected response: {text}"
        );

        assert!(events.lock().unwrap().iter().any(|e| *e
            == BlockedEvent::Attempt {
                host: "example.com".to_string()
            }));
    }

    #[tokio::test]
    async fn plain_http_leg_rejects_a_malformed_target_with_400() {
        let proxy = PaneProxy::spawn(vec![], None, |_| {}).await.unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap();
        // Not an absolute URI (no scheme/host) — a plain-origin-form target
        // is only valid when a client is talking to an actual origin
        // server, never to a proxy.
        stream
            .write_all(b"GET /just-a-path HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp).starts_with("HTTP/1.1 400"));
    }

    #[tokio::test]
    async fn plain_http_leg_does_not_auto_follow_a_redirect_off_the_allowlist() {
        // Regression test for the reqwest-default-redirect-policy bypass:
        // an ALLOWLISTED host ("redirector") responds 302 pointing at a
        // host that is NOT allowlisted ("evil", reached only via a
        // different hostname string — "localhost" vs. the allowlisted
        // literal "127.0.0.1", same trick
        // `relock_kills_an_open_mode_only_tunnel_but_spares_an_allowlisted_one`
        // uses). The proxy must hand the 302 (with its Location header)
        // straight back to the client, UNFOLLOWED — exactly like the JS
        // original's raw `http.request`/`.pipe()` leg. Before the
        // `.redirect(Policy::none())` fix, reqwest's default client chased
        // the redirect internally and returned the evil host's body
        // verbatim: a full allowlist bypass for any allowlisted host with
        // (or tricked into serving) an open redirect.
        let evil_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind evil upstream");
        let evil_port = evil_listener.local_addr().unwrap().port();
        let evil_task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = evil_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let body = b"EVIL_BODY_FROM_NON_ALLOWLISTED_HOST";
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.write_all(body).await;
                });
            }
        });

        let redirector_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind redirector upstream");
        let redirector_port = redirector_listener.local_addr().unwrap().port();
        let redirector_task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = redirector_listener.accept().await else {
                    break;
                };
                let location = format!("http://localhost:{evil_port}/");
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });

        // Only "127.0.0.1" (the redirector's own hostname) is allowlisted.
        let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, |_| {})
            .await
            .unwrap();
        admit_port(&proxy, redirector_port);

        let mut stream = TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap();
        let req =
            format!("GET http://127.0.0.1:{redirector_port}/ HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();

        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);

        assert!(
            text.starts_with("HTTP/1.1 302"),
            "redirect must be forwarded unfollowed, got: {text}"
        );
        assert!(
            text.contains(&format!("location: http://localhost:{evil_port}/"))
                || text.contains(&format!("Location: http://localhost:{evil_port}/")),
            "Location header must survive verbatim: {text}"
        );
        assert!(
            !text.contains("EVIL_BODY_FROM_NON_ALLOWLISTED_HOST"),
            "the evil host must never be fetched: {text}"
        );

        evil_task.abort();
        redirector_task.abort();
    }

    // ---- Linux seam: identical service over a Unix domain socket ----

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_leg_serves_the_identical_connect_proxy() {
        let (echo_port, _echo) = spawn_echo_server("127.0.0.1").await;
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("pane.sock");
        let proxy = PaneProxy::spawn(
            vec!["127.0.0.1".to_string()],
            Some(sock_path.clone()),
            |_| {},
        )
        .await
        .unwrap();
        admit_port(&proxy, echo_port);
        assert_eq!(proxy.unix_path(), Some(&sock_path));

        let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let req = format!("CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: x\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();

        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            stream.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));

        stream.write_all(b"via-unix").await.unwrap();
        let mut echoed = [0u8; 8];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"via-unix");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_removes_the_unix_socket_file() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("pane.sock");
        let proxy = PaneProxy::spawn(vec![], Some(sock_path.clone()), |_| {})
            .await
            .unwrap();
        assert!(sock_path.exists());
        proxy.shutdown();
        assert!(!sock_path.exists());
    }
}
