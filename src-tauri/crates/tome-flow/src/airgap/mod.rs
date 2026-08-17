//! Airgap subsystem (Phase 3). Slice ownership within this directory:
//! `allowlist.rs` + `proxy.rs` = slice A1 (host-matching compiler, the
//! per-pane loopback CONNECT proxy, live tunnel tracking); `seatbelt.rs` +
//! THIS file = slice A3 (this slice). `seatbelt.rs` is the pure macOS SBPL
//! profile builder (see its own doc comment). This file is the pane-gapping
//! state machine, repo-allowlist consent bookkeeping, and unlock/relock
//! deadline tracking — ported from `src/main/airgap.js`'s module-level
//! state (`panes`, `repoConsents`, `appliedRepos`) and its
//! `unlockPane`/`relockPane`/`closePane`/`closeAll`/`readRepoAllowlist`/
//! `consentRepoAllowlist`/`revokeRepoAllowlist`/`reapplyRepoConsents`
//! exports, plus `test/airgap-proxy-lifecycle.test.js`'s pure-state
//! assertions and all of `test/repo-airgap.test.js`.
//!
//! ## What is deliberately NOT here, and why
//!
//! **No live proxy/tunnel objects, no Tauri emits.** The JS original's pane
//! record is `{ mode, expiresAt, timer, server, tunnels }` — a mix of pure
//! state (`mode`, `expiresAt`) and live I/O resources (`server`, an
//! `http.Server`; `tunnels`, live CONNECT sockets) it owns directly because
//! JS has no separate ownership boundary to put them behind. This port
//! splits that: [`AirgapState`] owns only the pure half (mode + deadline);
//! the live loopback listener and its tunnel registry belong to
//! `airgap::proxy` (slice A1) and are wired together by the integrator
//! (Task A4), which also owns pushing `airgap:state`/`airgap:blocked`
//! events onto the real Tauri event bus (the JS original's `onEvent`/
//! `pushState`). This keeps `AirgapState` framework-free — no
//! `tauri::AppHandle`, no `tauri::State`, nothing async — so every method
//! below is a plain synchronous call a `#[cfg(test)]` can exercise directly.
//! Confinement resolution (`confine::confined_real_path`, which DOES need a
//! live `tauri::State`) is threaded in the same way: as a closure the
//! caller supplies per call, mirroring the JS original's own
//! `setConfinedRealPath(fn)` injection but without a stored, boxed callback
//! — see [`AirgapState::read_repo_allowlist`]'s doc comment.
//!
//! **No `compileAllowlist`/`DEFAULT_ALLOW` (the wildcard hostname
//! matcher).** `test/airgap.test.js` (which despite its filename tests only
//! this compiler, imported from `lib/allowlist.js`) is NOT ported into this
//! file. That compiler's only consumer is the proxy's `hostAllowed(paneId,
//! host)` check, which — by the split above — lives with the proxy, not the
//! pane-state machine: `hostAllowed` in the original is `mode === 'open' ||
//! allowMatchers.some(...)`, a composition of ONE fact this module owns
//! (pane mode) with ONE fact `airgap::allowlist` (slice A1) owns (the
//! compiled matcher list). [`AirgapState::pane_mode`] exists precisely so
//! the integrator can perform that composition without this module needing
//! its own copy of the matcher compiler. `airgap/mod.rs`'s pre-existing doc
//! comment (before this slice's own work landed) already assigned
//! `allowlist.rs` to slice A1 for exactly this reason.
//!
//! **`parseRepoAllowlist`/`validateRepoAllowlist` are exposed here as
//! `pub fn`s with this module's own return shape, but RECONCILED (Task A4
//! integration) to delegate to `airgap::allowlist`'s implementation rather
//! than carry a second copy of the validation rules.** This module needed
//! them before `allowlist.rs` (slice A1) had landed — repo-consent
//! bookkeeping (`readRepoAllowlist`/`consentRepoAllowlist`) cannot report
//! `hosts`/`rejected` without them, and this slice's own gate could not
//! wait — so [`parse_repo_allowlist`]/[`validate_repo_allowlist`] below
//! started as a self-contained copy, pinned against every assertion in
//! `test/repo-airgap.test.js`. Once `allowlist.rs` landed as the real,
//! 1:1-ported single source of truth for those same rules, both functions
//! were reduced to thin adapters over it — same `pub` signatures (so every
//! existing caller, including this module's own 30+ pinned tests, needed
//! no changes), bodies now delegating. See the "repo allowlist parse +
//! validate" section further down for the reconciliation's own note.
//! `compileAllowlist`/`DEFAULT_ALLOW` never got a temporary copy here in
//! the first place, because nothing in *this* module's scope ever needs to
//! MATCH a host against a pattern — only validate a pattern's shape.
//!
//! **No real timers — deadlines are bookkeeping, not scheduling.** The JS
//! original's `unlockPane` both computes a deadline AND arms a real
//! `setTimeout` that calls `relockPane` when it fires; `vi.useFakeTimers()`
//! is how its test suite observes that without a real 15-minute wait.
//! [`AirgapState::unlock_pane`] only does the first half: it validates and
//! records the deadline, returning it so the integrator can arm a real
//! `tokio::time::sleep_until` (or equivalent) that calls
//! [`AirgapState::relock_pane`] when it fires — the closest Rust analogue
//! to the original's per-pane `setTimeout`. [`AirgapState::sweep_expired`]
//! is the other valid integration strategy (a periodic tick over every open
//! pane) and is also what lets this module's own tests pin the "deadline is
//! exclusive" boundary from `test/airgap-proxy-lifecycle.test.js`
//! deterministically, by passing an explicit `now_ms` instead of a fake
//! clock — see that test's port below.
//!
//! **No blocked-host-event 60s coalescing.** That logic (`logBlocked`/
//! `BLOCKED_COALESCE_MS`/`blockedPending` in the JS original) is driven by
//! the proxy's own request handler observing a blocked CONNECT/plain-HTTP
//! attempt — an event this module never produces, since it never runs a
//! server. Left to `airgap::proxy` (slice A1) or the integrator (Task A4).

// Every item below is exercised by its own #[cfg(test)] suite, but in a
// plain (non-test) build nothing calls any of it yet — same rationale as
// `pty_authority.rs`'s module-level allow (see that module's top doc
// comment): the real callers (`ipc::airgap::*`, `ipc::pty::pty_create`'s
// gapped-pane path) are different slices' files and still stubs as of this
// slice landing. One module-level allow here instead of scattering
// `#[allow(dead_code)]` over two dozen individual items; `cargo test` still
// compiles and exercises every one of them regardless.
#![allow(dead_code)]

pub mod allowlist;
pub mod linux;
pub mod proxy;
pub mod seatbelt;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use sha1::{Digest, Sha1};

// ---- unlock policy (src/main/airgap.js's ALLOWED_UNLOCK_MINUTES / DEFAULT_UNLOCK_MINUTES) ----

/// Main-owned menu of valid `airgap:unlock` durations, in minutes — exactly
/// what the renderer's own unlock UI offers. `unlock_pane` refuses any
/// value outside this set BEFORE mutating any state (TOME-019): a forged
/// IPC call with `minutes` outside this list — 0, negative, or absurdly
/// large — must not flip a pane to `Open` with a bogus or near-immediate
/// expiry.
///
/// The JS original's own test additionally rejects non-integer shapes
/// (`'15'` as a string, `NaN`, `Infinity`) that arrive over an untyped
/// `ipcMain.handle` payload. Those have no analogue here: `minutes: i64` in
/// [`AirgapState::unlock_pane`] makes them unrepresentable by the time a
/// Tauri command's argument deserialization would even call in — the same
/// type-level simplification `pty_authority.rs` and `confine.rs` document
/// for their own renderer-supplied parameters.
pub const ALLOWED_UNLOCK_MINUTES: [i64; 3] = [15, 30, 60];

/// `src/main/airgap.js`'s `DEFAULT_UNLOCK_MINUTES` — not currently
/// user-configurable in either implementation; reported as `defaultMinutes`
/// in the `airgap:state` wire shape.
pub const DEFAULT_UNLOCK_MINUTES: i64 = 15;

// ---- pane gapping state ----

/// A pane's egress mode. Mirrors `airgap.js`'s `st.mode` string
/// (`'providers' | 'open'`) as a real enum; [`PaneMode::as_str`] produces
/// the identical two lowercase strings for the `airgap:state` wire shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMode {
    /// Only allowlisted hosts (the provider defaults, the user's own
    /// override file, and any consented repo hosts) are reachable through
    /// this pane's proxy. The default and resting mode.
    Providers,
    /// Temporarily unlocked: every host is reachable through this pane's
    /// proxy until `expires_at`. Only ever entered via
    /// [`AirgapState::unlock_pane`].
    Open,
}

impl PaneMode {
    fn as_str(self) -> &'static str {
        match self {
            PaneMode::Providers => "providers",
            PaneMode::Open => "open",
        }
    }
}

#[derive(Debug, Clone)]
struct PaneRecord {
    mode: PaneMode,
    /// Milliseconds since the Unix epoch — the same unit `Date.now()` +
    /// `minutes * 60_000` produces in the JS original. `None` in
    /// `Providers` mode, mirroring `expiresAt: null`.
    expires_at: Option<i64>,
}

// ---- repo allowlist consent ----

#[derive(Debug, Clone, PartialEq)]
struct RepoConsent {
    hash: String,
    hosts: Vec<String>,
}

/// One entry of `validateRepoAllowlist`'s `rejected` array — an
/// individually-rejected pattern from a repo's `.tome/airgap.json`, with a
/// human-readable reason. `pattern` is the original JSON value, not a
/// `String`: the JS original passes through whatever the entry actually
/// was (a number, `null`, an object — see the "rejects non-strings" test),
/// and this port keeps that fidelity rather than collapsing every
/// rejection to a stringified form.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RejectedPattern {
    pub pattern: serde_json::Value,
    pub reason: String,
}

/// Mirrors `readRepoAllowlist`'s two return shapes
/// (`{ state: 'absent' }` / `{ state: 'present', hash, hosts, rejected,
/// consented }`) as a tagged enum — `#[serde(tag = "state")]` serializes
/// exactly those two JSON shapes.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum RepoAllowlistReport {
    /// No `.tome/airgap.json`, or main could not resolve/read/parse it — a
    /// file main cannot honestly read is treated as a file main cannot
    /// apply, same as the JS original's every catch branch.
    Absent,
    Present {
        /// sha1 hex digest of the file's RAW TEXT (not the parsed array) —
        /// any edit, even whitespace-only, changes this hash and therefore
        /// invalidates a stored consent. See [`sha1_hex`].
        hash: String,
        /// Patterns that passed [`validate_repo_allowlist`].
        hosts: Vec<String>,
        /// Patterns that did not, each with a reason.
        rejected: Vec<RejectedPattern>,
        /// Whether a currently-stored consent for this root's hash matches
        /// THIS read's hash — i.e. whether the user has already agreed to
        /// exactly this file content.
        consented: bool,
    },
}

/// Mirrors `consentRepoAllowlist`'s two return shapes (`{ ok: true,
/// applied, rejected }` / `{ ok: false, error }`). Left un-`Serialize`
/// deliberately: unlike [`RepoAllowlistReport`], nothing in this slice's
/// scope hands this shape directly to a Tauri command's return type yet —
/// the future `ipc::airgap::airgap_consent_repo_allowlist` (not this
/// slice's file) is expected to pattern-match this and build its own
/// `{ ok, ... }` JSON, the same way every OTHER `ipc::airgap::*` stub
/// already builds its JSON by hand rather than serializing a shared type.
#[derive(Debug, Clone, PartialEq)]
pub enum ConsentOutcome {
    Ok {
        applied: Vec<String>,
        rejected: Vec<RejectedPattern>,
    },
    /// Always `"file changed"` today — the only failure mode
    /// `consentRepoAllowlist` has once `root`/`hash` are guaranteed strings
    /// by Rust's type system (the JS original's separate `'bad request'`
    /// branch for non-string `root`/`hash` has no analogue here; see this
    /// module's top doc comment on the same simplification pattern used
    /// throughout this codebase's port).
    Err(String),
}

// ---- `airgap:state` snapshot ----

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaneStateView {
    pub mode: &'static str,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepoStateEntry {
    pub root: String,
    pub hosts: usize,
}

/// Mirrors `getState()`'s return shape exactly (`{ panes, defaultMinutes,
/// repo }`) — the future `ipc::airgap::airgap_state` (not this slice's
/// file) additionally merges in `auth: authlock.authStatus()`, same as the
/// JS handler does on top of this same `getState()` call.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AirgapStateSnapshot {
    pub panes: HashMap<String, PaneStateView>,
    #[serde(rename = "defaultMinutes")]
    pub default_minutes: i64,
    pub repo: Vec<RepoStateEntry>,
}

// ---- the state machine itself ----

/// All of this module's mutable state behind ONE mutex, rather than one
/// mutex per map (`panes`/`repo_consents`/`applied_repos`/`consents_path`
/// are four separate module-level bindings in the JS original). A
/// security-sensitive state machine gains more from ruling out
/// lock-ordering bugs by construction than it would from the marginal
/// extra concurrency four separate locks would allow — none of these maps
/// are a hot path (pane unlock/relock/consent are all human-paced, rare
/// events, not per-request work).
struct Inner {
    panes: HashMap<String, PaneRecord>,
    /// root -> the consent last recorded for it (hash + the hosts that
    /// were `ok` at consent time). Persisted; survives restarts as long as
    /// the file's hash still matches (see [`AirgapState::reapply_repo_consents`]).
    repo_consents: HashMap<String, RepoConsent>,
    /// root -> hosts CURRENTLY folded into the effective allowlist —
    /// mirrors `appliedRepos`. Distinct from `repo_consents` for the same
    /// reason the original keeps two maps: a consent can exist for a root
    /// whose hosts are not (yet, or no longer) applied, e.g. mid-reapply.
    applied_repos: HashMap<String, Vec<String>>,
    /// Set once by [`AirgapState::load_repo_consents`] (mirrors the JS
    /// original's module-level `consentsFile`, set once at boot from
    /// `loadRepoConsents(userData)`). `None` means "never loaded" — every
    /// save is then a silent no-op, matching `saveRepoConsents`'s own
    /// `if (!consentsFile) return` guard.
    consents_path: Option<PathBuf>,
}

/// The air-gap pane-gapping state machine plus repo-allowlist consent
/// bookkeeping — Tauri-free (see this module's top doc comment), owns its
/// own interior locking so it is a plain value field on `AppState`, not
/// wrapped in another `Mutex` (the same shape `pty::Registry` already
/// uses).
pub struct AirgapState {
    inner: Mutex<Inner>,
    /// Re-entrancy guard for [`AirgapState::reapply_repo_consents`] —
    /// mirrors the JS original's module-level `reapplying` boolean, which
    /// exists because `ws:sync` can fire again (a second workspace-folder
    /// sync) while an earlier reapply is still resolving confinement for
    /// every consented root. A plain `AtomicBool` outside `inner`'s mutex
    /// rather than a field on `Inner`: it must be checked (and possibly
    /// short-circuit) WITHOUT holding `inner`'s lock, since a full reapply
    /// pass takes `inner`'s lock repeatedly across possibly-slow resolver
    /// calls (filesystem confinement, symlink resolution).
    reapplying: AtomicBool,
}

impl AirgapState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                panes: HashMap::new(),
                repo_consents: HashMap::new(),
                applied_repos: HashMap::new(),
                consents_path: None,
            }),
            reapplying: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .expect("airgap::AirgapState mutex poisoned")
    }

    // ---- pane lifecycle ----

    /// Registers a freshly-created pane in `Providers` mode with no expiry
    /// — the state half of `createPaneProxy`'s `panes.set(paneId, { mode:
    /// 'providers', expiresAt: null, ... })`. The integrator calls this
    /// once its real loopback proxy (`airgap::proxy`, slice A1) is actually
    /// listening; this module has no proxy of its own to wait on.
    ///
    /// Like `Map.set`, re-registering an already-live id overwrites the
    /// existing record — the original has the same behavior (`panes.set`
    /// unconditionally), and pane ids are renderer-generated fresh per
    /// spawn, so this should never observe a real collision in practice.
    pub fn register_pane(&self, id: &str) {
        self.lock().panes.insert(
            id.to_string(),
            PaneRecord {
                mode: PaneMode::Providers,
                expires_at: None,
            },
        );
    }

    /// `closePane` — drops the pane's state entry. No-op (returns `false`)
    /// for an unknown id, same as the original's `if (!st) return`
    /// (`test/airgap-proxy-lifecycle.test.js`: "closePane on an unknown id
    /// is a no-op"). The integrator is responsible for the matching
    /// `server.close()` + live-tunnel teardown on the real proxy handle —
    /// this call only ever needs to happen once regardless of order, so
    /// callers may tear down the proxy before or after calling this.
    pub fn close_pane(&self, id: &str) -> bool {
        self.lock().panes.remove(id).is_some()
    }

    /// `closeAll` — drops every pane's state entry, returning the ids that
    /// were present (so the integrator knows exactly which real proxy
    /// handles to tear down). Idempotent: a second call finds nothing left
    /// and returns an empty `Vec`, matching the original's own
    /// idempotence test (both `will-quit` and `window-all-closed` call
    /// `closeAll`, and either order may call it twice).
    pub fn close_all(&self) -> Vec<String> {
        let mut inner = self.lock();
        let ids: Vec<String> = inner.panes.keys().cloned().collect();
        inner.panes.clear();
        ids
    }

    /// The one fact the proxy-side `hostAllowed(paneId, host)` check needs
    /// from this module: `None` for an unknown pane, otherwise its current
    /// mode. The integrator composes `pane_mode(id) == Some(PaneMode::Open)
    /// || <compiled matcher>.matches(host)` — see this module's top doc
    /// comment for why the matcher half is not this module's job.
    pub fn pane_mode(&self, id: &str) -> Option<PaneMode> {
        self.lock().panes.get(id).map(|r| r.mode)
    }

    /// Mode + expiry together, for tests and for building an
    /// [`AirgapStateSnapshot`] entry by hand when only one pane is needed.
    pub fn pane_state(&self, id: &str) -> Option<(PaneMode, Option<i64>)> {
        self.lock().panes.get(id).map(|r| (r.mode, r.expires_at))
    }

    // ---- unlock / relock ----

    /// `unlockPane` — validates `minutes` against
    /// [`ALLOWED_UNLOCK_MINUTES`] and, for an existing pane, flips it to
    /// `Open` with `expires_at = now_ms + minutes * 60_000`, returning that
    /// deadline. Returns `None` — WITHOUT mutating anything — for an
    /// invalid `minutes` or an unknown pane id, exactly mirroring the JS
    /// original's `unlockPane`, which validates before touching `st` at
    /// all (TOME-019: a forged `minutes` must never partially apply).
    ///
    /// Unlike the JS original, this does not itself arm a timer. The
    /// returned deadline is what the integrator schedules a real relock
    /// against (`tokio::time::sleep_until`, converting this epoch-ms value
    /// to an `Instant`) — see this module's top doc comment.
    pub fn unlock_pane(&self, id: &str, minutes: i64, now_ms: i64) -> Option<i64> {
        if !ALLOWED_UNLOCK_MINUTES.contains(&minutes) {
            return None;
        }
        let mut inner = self.lock();
        let record = inner.panes.get_mut(id)?;
        let expires_at = now_ms + minutes * 60_000;
        record.mode = PaneMode::Open;
        record.expires_at = Some(expires_at);
        Some(expires_at)
    }

    /// `relockPane`'s pure half — flips an existing pane back to
    /// `Providers` and clears its expiry, returning whether a pane was
    /// found. No-op for an unknown id, mirroring `if (!st) return`.
    ///
    /// The original's tunnel-teardown loop (destroying every live CONNECT
    /// tunnel that was only ever allowed because mode was `'open'`,
    /// TOME-002) is NOT here — it needs live tunnel handles this module
    /// never holds (see the top doc comment). The integrator calls this
    /// first to get the authoritative new mode, then walks the pane's real
    /// tunnels itself, keeping only those `pane_mode` no longer needs to
    /// explain (i.e. ones independently allowed by the compiled matcher).
    pub fn relock_pane(&self, id: &str) -> bool {
        let mut inner = self.lock();
        let Some(record) = inner.panes.get_mut(id) else {
            return false;
        };
        record.mode = PaneMode::Providers;
        record.expires_at = None;
        true
    }

    /// Relocks every `Open` pane whose deadline has passed as of `now_ms`
    /// (`now_ms >= expires_at`, an INCLUSIVE-at-the-deadline comparison —
    /// see `test/airgap-proxy-lifecycle.test.js`'s "not yet — deadline is
    /// exclusive" assertion, ported below), returning the ids actually
    /// relocked. A pane already in `Providers` mode is left alone
    /// regardless of `now_ms`.
    ///
    /// One of two valid ways for the integrator to drive real relocking
    /// (see this module's top doc comment); a periodic tick calling this
    /// is simpler to reason about than N precise per-pane timers, at the
    /// cost of up to one tick's worth of latency past the real deadline.
    pub fn sweep_expired(&self, now_ms: i64) -> Vec<String> {
        let mut inner = self.lock();
        let mut relocked = Vec::new();
        for (id, record) in inner.panes.iter_mut() {
            if record.mode == PaneMode::Open {
                if let Some(expires_at) = record.expires_at {
                    if now_ms >= expires_at {
                        record.mode = PaneMode::Providers;
                        record.expires_at = None;
                        relocked.push(id.clone());
                    }
                }
            }
        }
        relocked
    }

    /// `getState()`, minus the `auth` field the real `airgap:state` handler
    /// merges in from a different subsystem entirely (see
    /// [`AirgapStateSnapshot`]'s doc comment).
    pub fn state_snapshot(&self) -> AirgapStateSnapshot {
        let inner = self.lock();
        let panes = inner
            .panes
            .iter()
            .map(|(id, r)| {
                (
                    id.clone(),
                    PaneStateView {
                        mode: r.mode.as_str(),
                        expires_at: r.expires_at,
                    },
                )
            })
            .collect();
        let repo = inner
            .applied_repos
            .iter()
            .map(|(root, hosts)| RepoStateEntry {
                root: root.clone(),
                hosts: hosts.len(),
            })
            .collect();
        AirgapStateSnapshot {
            panes,
            default_minutes: DEFAULT_UNLOCK_MINUTES,
            repo,
        }
    }

    // ---- repo allowlist consent ----

    /// Flattened hosts from every currently-applied repo consent — the
    /// piece `state_snapshot`'s `RepoStateEntry.hosts` (a COUNT, for the UI)
    /// deliberately does not expose. Added for Task A4's integration: the
    /// proxy's live allow set (`airgap::proxy::PaneProxy::set_allowed`) is
    /// `DEFAULT_ALLOW ++ effective_repo_hosts()`, mirroring `airgap.js`'s
    /// `recompile()` (`[...(userAllow || DEFAULT_ALLOW), ...[...appliedRepos.values()].flat()]`,
    /// minus the user-override half — see this slice's task report for why
    /// `userAllow`/`loadAllowlist` has no port yet). Order is
    /// HashMap-iteration order (unspecified) — callers only ever fold this
    /// into a hostname allow SET, where order is not observable.
    pub fn effective_repo_hosts(&self) -> Vec<String> {
        self.lock()
            .applied_repos
            .values()
            .flatten()
            .cloned()
            .collect()
    }

    /// `readRepoAllowlist(root)` — reports what main WOULD apply for
    /// `${root}/.tome/airgap.json`, without applying anything. `resolve` is
    /// this call's confinement resolver (mirrors the JS original's
    /// injected, module-level `confinedRealPath` — see this module's top
    /// doc comment for why it is a per-call closure here rather than a
    /// stored one): given the raw, unconfirmed candidate path, it returns
    /// the real, symlink-resolved path IF `root` is one of the open
    /// workspace folders and the file actually exists there, or `None`
    /// otherwise. A production caller passes `|p| confine::confined_real_
    /// path(&state, p).ok()`; that function is already synchronous (unlike
    /// the JS original's `async confinedRealPath`), so no async boundary
    /// needs to cross into this module for it.
    ///
    /// Every failure mode — empty `root`, a refusing resolver, an unreadable
    /// file, or a file that fails to parse as `{ "allow": [...] }` —
    /// collapses to [`RepoAllowlistReport::Absent`], matching the original's
    /// blanket "a file main cannot honestly read is a file main cannot
    /// apply." `root` being `&str` already rules out the JS original's
    /// separate `typeof root !== 'string'` branch (see this module's top
    /// doc comment on this simplification pattern).
    pub fn read_repo_allowlist(
        &self,
        root: &str,
        resolve: impl Fn(&Path) -> Option<PathBuf>,
    ) -> RepoAllowlistReport {
        if root.is_empty() {
            return RepoAllowlistReport::Absent;
        }
        // Plain concatenation, not `Path::join`, matching the JS original's
        // template literal (`` `${root}/.tome/airgap.json` ``) exactly —
        // including its quirk of a doubled separator if `root` already ends
        // in one, which the resolver either tolerates or refuses the same
        // way it would for the JS version.
        let candidate = PathBuf::from(format!("{root}/.tome/airgap.json"));
        let Some(real) = resolve(&candidate) else {
            return RepoAllowlistReport::Absent;
        };
        let Ok(text) = std::fs::read_to_string(&real) else {
            return RepoAllowlistReport::Absent;
        };
        let Some(raw_hosts) = parse_repo_allowlist(&text) else {
            return RepoAllowlistReport::Absent;
        };
        let hash = sha1_hex(&text);
        let (hosts, rejected) = validate_repo_allowlist(&raw_hosts);
        let consented = self
            .lock()
            .repo_consents
            .get(root)
            .map(|c| c.hash == hash)
            .unwrap_or(false);
        RepoAllowlistReport::Present {
            hash,
            hosts,
            rejected,
            consented,
        }
    }

    /// `consentRepoAllowlist(root, hash)` — TOCTOU-safe: re-reads and
    /// re-hashes the file NOW via [`AirgapState::read_repo_allowlist`], and
    /// only records consent (and folds `hosts` into `applied_repos`,
    /// widening the effective allowlist) if the freshly-computed hash
    /// matches `presented_hash` exactly. The caller never supplies hosts —
    /// only proof (the hash) that it saw a specific file content; the hosts
    /// that get applied are always what THIS call just parsed and
    /// validated, never anything the caller passed in.
    ///
    /// Persists the updated consent map best-effort on success (mirrors
    /// `saveRepoConsents().catch(() => {})` — persistence failure must not
    /// undo an in-memory consent the user just granted). No-op path
    /// ([`ConsentOutcome::Err`]) does not touch disk.
    pub fn consent_repo_allowlist(
        &self,
        root: &str,
        presented_hash: &str,
        resolve: impl Fn(&Path) -> Option<PathBuf>,
    ) -> ConsentOutcome {
        let report = self.read_repo_allowlist(root, &resolve);
        let RepoAllowlistReport::Present {
            hash,
            hosts,
            rejected,
            ..
        } = report
        else {
            return ConsentOutcome::Err("file changed".to_string());
        };
        if hash != presented_hash {
            return ConsentOutcome::Err("file changed".to_string());
        }
        {
            let mut inner = self.lock();
            inner.repo_consents.insert(
                root.to_string(),
                RepoConsent {
                    hash,
                    hosts: hosts.clone(),
                },
            );
            inner.applied_repos.insert(root.to_string(), hosts.clone());
        }
        let _ = self.save_repo_consents();
        ConsentOutcome::Ok {
            applied: hosts,
            rejected,
        }
    }

    /// `revokeRepoAllowlist(root)` — drops both the stored consent and the
    /// applied hosts for `root`, unconditionally (the original always
    /// returns `{ ok: true }`, even for a root with no consent to revoke).
    /// Persists best-effort, same as [`AirgapState::consent_repo_allowlist`].
    pub fn revoke_repo_allowlist(&self, root: &str) {
        {
            let mut inner = self.lock();
            inner.repo_consents.remove(root);
            inner.applied_repos.remove(root);
        }
        let _ = self.save_repo_consents();
    }

    /// `reapplyRepoConsents()` — re-validates every STORED consent against
    /// the live file it pins: a root whose file still hashes the same gets
    /// its hosts re-applied (this is what makes stored consents survive a
    /// restart); a root whose file changed or vanished has its consent
    /// dropped outright (re-prompt-on-change, and revoke-by-delete, both
    /// become true this way). Persists once at the end, only if anything
    /// actually changed — matching the original's own `if (changed) await
    /// saveRepoConsents()`.
    ///
    /// Returns `false` without doing anything if a reapply is already in
    /// flight (mirrors the original's `if (reapplying) return` guard,
    /// which exists because a second `ws:sync` can arrive mid-reapply);
    /// `true` once this call actually ran to completion.
    pub fn reapply_repo_consents(&self, resolve: impl Fn(&Path) -> Option<PathBuf>) -> bool {
        if self.reapplying.swap(true, Ordering::SeqCst) {
            return false;
        }
        // Snapshot first, then release the lock before calling into
        // `read_repo_allowlist` (which takes the SAME lock internally, for
        // its own `consented` check) — `std::sync::Mutex` is not
        // re-entrant, so holding it across that call would deadlock.
        let snapshot: Vec<(String, RepoConsent)> = self
            .lock()
            .repo_consents
            .iter()
            .map(|(root, c)| (root.clone(), c.clone()))
            .collect();
        let mut changed = false;
        for (root, consent) in snapshot {
            match self.read_repo_allowlist(&root, &resolve) {
                RepoAllowlistReport::Present { hash, .. } if hash == consent.hash => {
                    self.lock().applied_repos.insert(root, consent.hosts);
                }
                _ => {
                    let mut inner = self.lock();
                    inner.repo_consents.remove(&root);
                    inner.applied_repos.remove(&root);
                    changed = true;
                }
            }
        }
        if changed {
            let _ = self.save_repo_consents();
        }
        self.reapplying.store(false, Ordering::SeqCst);
        true
    }

    // ---- repo consent persistence ----

    /// `loadRepoConsents(userData)` — records `path` for future saves (this
    /// call's `userData`-derived path is remembered exactly the way the JS
    /// original's module-level `consentsFile` is), then loads whatever
    /// consents are there. Any failure — missing file, malformed JSON, a
    /// shape that doesn't match `{ root: { hash: string, hosts: string[] }
    /// }` — leaves the in-memory consent map exactly as it was before the
    /// call (empty, if this is the boot-time call), matching the original's
    /// bare `catch {}`: "missing/corrupt consent file = no consents — the
    /// safe default." `path` is still recorded even on a load failure, same
    /// as the original (a missing file at boot is normal — the very next
    /// consent still needs somewhere to save to).
    pub fn load_repo_consents(&self, path: &Path) {
        let mut inner = self.lock();
        inner.consents_path = Some(path.to_path_buf());
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text)
        else {
            return;
        };
        for (root, v) in map {
            let hash = v.get("hash").and_then(|h| h.as_str()).map(str::to_string);
            let hosts = v.get("hosts").and_then(|h| h.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            });
            if let (Some(hash), Some(hosts)) = (hash, hosts) {
                inner
                    .repo_consents
                    .insert(root, RepoConsent { hash, hosts });
            }
        }
    }

    /// `saveRepoConsents()` — writes the full consent map as one JSON
    /// object (`{ root: { hash, hosts } }`, matching `Object.fromEntries
    /// (repoConsents)`) to the path [`AirgapState::load_repo_consents`]
    /// recorded, then chmods it `0600` on Unix — "the consent file proves
    /// user intent, so it must not be world-readable even outside the
    /// sandbox," the same discipline `authlock.js`'s auth file uses.
    /// Silent no-op if no path has ever been recorded (mirrors `if
    /// (!consentsFile) return`). Not `pub`: the JS original does not export
    /// this either — every mutator above calls it internally, best-effort,
    /// after its own in-memory change.
    fn save_repo_consents(&self) -> std::io::Result<()> {
        let (path, json) = {
            let inner = self.lock();
            let Some(path) = inner.consents_path.clone() else {
                return Ok(());
            };
            let map: serde_json::Map<String, serde_json::Value> = inner
                .repo_consents
                .iter()
                .map(|(root, c)| {
                    (
                        root.clone(),
                        serde_json::json!({ "hash": c.hash, "hosts": c.hosts }),
                    )
                })
                .collect();
            let json = serde_json::to_string(&serde_json::Value::Object(map))?;
            (path, json)
        };
        std::fs::write(&path, json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

impl Default for AirgapState {
    fn default() -> Self {
        Self::new()
    }
}

// ---- sha1 (repo-consent hashing) ----

/// sha1 hex digest of `text` — used to fingerprint a repo's `.tome/
/// airgap.json` RAW TEXT (not its parsed form) for consent pinning. `pub`
/// so a future caller (e.g. a UI-facing diagnostic, or `ipc::airgap`'s real
/// implementation) never needs to reimplement this one line.
pub fn sha1_hex(text: &str) -> String {
    let digest = Sha1::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- repo allowlist parse + validate ----
//
// RECONCILED (Task A4 integration — flagged by both this slice's OWN top
// doc comment and by `allowlist.rs`'s, each naming the other as the
// eventual single source of truth once both landed): these two functions
// now DELEGATE to `airgap::allowlist::{parse_repo_allowlist,
// validate_repo_allowlist}` — the real, single implementation of the
// validation rules — rather than carrying a second, independently-written
// copy of the same checks. Signatures and both functions' own `pub`
// visibility are UNCHANGED so every caller (including this module's own
// 30+ `#[cfg(test)]` assertions ported from `test/repo-airgap.test.js`,
// written directly against these two names) keeps working without any
// edits; only the bodies changed, from "re-implement" to "delegate + adapt
// the return shape". The two implementations were independently pinned
// against the identical JS test suite and were already behaviorally
// identical (bar one inconsequential unit difference — see the diff this
// reconciliation removes: `pattern.len()` (bytes) vs. `pattern.chars()
// .count()` (chars) for the 253-character-limit check, which only differ
// for non-ASCII patterns of exactly boundary length, a case no real
// hostname and no pinned test exercises), so this is a pure de-duplication,
// not a behavior change.

/// `parseRepoAllowlist(text)` — see `airgap::allowlist::parse_repo_allowlist`
/// for the real implementation. `Option`, not that function's `Result`:
/// every caller in this module immediately collapses either error case to
/// [`RepoAllowlistReport::Absent`] anyway, so there is no case here that
/// needs the underlying error message.
pub fn parse_repo_allowlist(text: &str) -> Option<Vec<serde_json::Value>> {
    allowlist::parse_repo_allowlist(text).ok()
}

/// `validateRepoAllowlist(patterns)` — see
/// `airgap::allowlist::validate_repo_allowlist` for the real
/// implementation and its own doc comment for the exact positional breadth
/// rule. Adapts that function's `ValidationResult { ok, rejected }` (whose
/// `rejected: Vec<allowlist::RejectedPattern>` is a plain, non-`Serialize`
/// struct — `allowlist.rs` has no caller that hands one directly to a Tauri
/// command's return type) into this module's own `(Vec<String>,
/// Vec<RejectedPattern>)` shape, where THIS module's [`RejectedPattern`]
/// derives `Serialize` for exactly that reason (`ipc::airgap`'s handlers
/// serialize it directly — see that type's own doc comment).
pub fn validate_repo_allowlist(
    patterns: &[serde_json::Value],
) -> (Vec<String>, Vec<RejectedPattern>) {
    let result = allowlist::validate_repo_allowlist(patterns);
    let rejected = result
        .rejected
        .into_iter()
        .map(|r| RejectedPattern {
            pattern: r.pattern,
            reason: r.reason,
        })
        .collect();
    (result.ok, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    // ==== pane gapping state machine ====

    // ---- register / pane_mode / pane_state ----

    #[test]
    fn register_pane_starts_in_providers_mode_with_no_expiry() {
        let state = AirgapState::new();
        state.register_pane("pty-1");
        assert_eq!(state.pane_mode("pty-1"), Some(PaneMode::Providers));
        assert_eq!(state.pane_state("pty-1"), Some((PaneMode::Providers, None)));
    }

    #[test]
    fn unknown_pane_reports_none_everywhere() {
        let state = AirgapState::new();
        assert_eq!(state.pane_mode("ghost"), None);
        assert_eq!(state.pane_state("ghost"), None);
    }

    #[test]
    fn re_registering_an_id_overwrites_the_existing_record() {
        let state = AirgapState::new();
        state.register_pane("pty-1");
        state.unlock_pane("pty-1", 15, 0);
        assert_eq!(state.pane_mode("pty-1"), Some(PaneMode::Open));
        state.register_pane("pty-1");
        assert_eq!(state.pane_state("pty-1"), Some((PaneMode::Providers, None)));
    }

    // ---- close_pane / close_all — test/airgap-proxy-lifecycle.test.js "pane proxy lifecycle" ----

    #[test]
    fn close_pane_drops_the_state_entry() {
        let state = AirgapState::new();
        state.register_pane("pty-1");
        assert!(state.pane_state("pty-1").is_some());
        assert!(state.close_pane("pty-1"));
        assert_eq!(state.pane_state("pty-1"), None);
    }

    #[test]
    fn close_pane_on_an_unknown_id_is_a_no_op() {
        let state = AirgapState::new();
        assert!(!state.close_pane("never-existed"));
    }

    #[test]
    fn close_all_reaps_every_pane() {
        let state = AirgapState::new();
        state.register_pane("pty-a");
        state.register_pane("pty-b");
        let mut ids = state.close_all();
        ids.sort();
        assert_eq!(ids, vec!["pty-a".to_string(), "pty-b".to_string()]);
        assert_eq!(state.pane_state("pty-a"), None);
        assert_eq!(state.pane_state("pty-b"), None);
    }

    #[test]
    fn close_all_is_idempotent() {
        let state = AirgapState::new();
        state.register_pane("pty-a");
        assert_eq!(state.close_all().len(), 1);
        assert_eq!(state.close_all().len(), 0); // will-quit AND window-all-closed both call it
    }

    // ---- unlockPane minutes validation (TOME-019) ----

    #[test]
    fn allowed_unlock_minutes_matches_the_menu_the_ui_offers() {
        assert_eq!(ALLOWED_UNLOCK_MINUTES, [15, 30, 60]);
    }

    #[test]
    fn rejects_minutes_outside_the_allowed_set_without_mutating_pane_state() {
        // '15' (string) / NaN / Infinity from the JS suite have no Rust
        // analogue — see ALLOWED_UNLOCK_MINUTES's doc comment.
        for bad in [0, -1, 999] {
            let state = AirgapState::new();
            state.register_pane("pty-1");
            assert_eq!(state.unlock_pane("pty-1", bad, 0), None, "minutes={bad}");
            assert_eq!(state.pane_state("pty-1"), Some((PaneMode::Providers, None)));
        }
    }

    #[test]
    fn unlock_pane_on_an_unknown_id_returns_none() {
        let state = AirgapState::new();
        assert_eq!(state.unlock_pane("ghost", 15, 0), None);
    }

    #[test]
    fn accepts_each_allowed_value_opens_the_pane_and_relocks_by_the_deadline() {
        for minutes in ALLOWED_UNLOCK_MINUTES {
            let state = AirgapState::new();
            state.register_pane("pty-1");
            let now = 1_700_000_000_000_i64;
            let deadline = state.unlock_pane("pty-1", minutes, now);
            let expected_deadline = now + minutes * 60_000;
            assert_eq!(deadline, Some(expected_deadline));
            assert_eq!(
                state.pane_state("pty-1"),
                Some((PaneMode::Open, Some(expected_deadline)))
            );

            // Not yet — deadline is exclusive.
            assert_eq!(
                state.sweep_expired(expected_deadline - 1),
                Vec::<String>::new()
            );
            assert_eq!(state.pane_mode("pty-1"), Some(PaneMode::Open));

            // Relocked itself, exactly at the deadline.
            assert_eq!(
                state.sweep_expired(expected_deadline),
                vec!["pty-1".to_string()]
            );
            assert_eq!(state.pane_state("pty-1"), Some((PaneMode::Providers, None)));
        }
    }

    // ---- relock_pane / sweep_expired ----

    #[test]
    fn relock_pane_on_an_unknown_id_returns_false_and_is_a_no_op() {
        let state = AirgapState::new();
        assert!(!state.relock_pane("ghost"));
    }

    #[test]
    fn relock_pane_is_immediate_unlike_sweep_expired() {
        let state = AirgapState::new();
        state.register_pane("pty-1");
        state.unlock_pane("pty-1", 15, 0);
        assert!(state.relock_pane("pty-1"));
        assert_eq!(state.pane_state("pty-1"), Some((PaneMode::Providers, None)));
    }

    #[test]
    fn sweep_expired_leaves_providers_mode_panes_alone() {
        let state = AirgapState::new();
        state.register_pane("pty-1"); // never unlocked
        assert_eq!(state.sweep_expired(i64::MAX), Vec::<String>::new());
        assert_eq!(state.pane_mode("pty-1"), Some(PaneMode::Providers));
    }

    #[test]
    fn sweep_expired_only_relocks_panes_whose_deadline_has_actually_passed() {
        let state = AirgapState::new();
        state.register_pane("a");
        state.register_pane("b");
        state.unlock_pane("a", 15, 0); // deadline 900_000
        state.unlock_pane("b", 60, 0); // deadline 3_600_000
        assert_eq!(state.sweep_expired(1_000_000), vec!["a".to_string()]);
        assert_eq!(state.pane_mode("a"), Some(PaneMode::Providers));
        assert_eq!(state.pane_mode("b"), Some(PaneMode::Open)); // not due yet
    }

    // ---- state_snapshot ----

    #[test]
    fn state_snapshot_serializes_with_camelcase_expires_at_and_default_minutes() {
        let state = AirgapState::new();
        state.register_pane("pty-1");
        state.unlock_pane("pty-1", 15, 1_000);
        let value = serde_json::to_value(state.state_snapshot()).unwrap();
        assert_eq!(value["panes"]["pty-1"]["mode"], json!("open"));
        assert_eq!(
            value["panes"]["pty-1"]["expiresAt"],
            json!(1_000 + 15 * 60_000)
        );
        assert_eq!(value["defaultMinutes"], json!(15));
        assert_eq!(value["repo"], json!([]));
    }

    #[test]
    fn state_snapshot_reports_a_null_expiry_for_a_providers_mode_pane() {
        let state = AirgapState::new();
        state.register_pane("pty-1");
        let value = serde_json::to_value(state.state_snapshot()).unwrap();
        assert_eq!(value["panes"]["pty-1"]["mode"], json!("providers"));
        assert_eq!(value["panes"]["pty-1"]["expiresAt"], json!(null));
    }

    // ==== sha1_hex ====

    #[test]
    fn sha1_hex_matches_known_answer_vectors() {
        // Cross-checked against Node's `createHash('sha1')` for the same
        // inputs — see this slice's task report for the exact command.
        assert_eq!(
            sha1_hex("hello world"),
            "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed"
        );
        assert_eq!(sha1_hex(""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            sha1_hex(r#"{"allow":["api.example.com"]}"#),
            "6bec1ff4dd1d4e0a0247cc99d420276819c6c6e0"
        );
    }

    // ==== parse_repo_allowlist ====

    #[test]
    fn parse_repo_allowlist_extracts_the_allow_array_as_is() {
        assert_eq!(
            parse_repo_allowlist(r#"{"allow":["a.com","b.com"]}"#),
            Some(vec![json!("a.com"), json!("b.com")])
        );
    }

    #[test]
    fn parse_repo_allowlist_preserves_mixed_type_entries_for_validate_to_reject_individually() {
        assert_eq!(
            parse_repo_allowlist(r#"{"allow":["a.com", 42, null]}"#),
            Some(vec![json!("a.com"), json!(42), json!(null)])
        );
    }

    #[test]
    fn parse_repo_allowlist_rejects_missing_or_non_array_allow_or_bad_json() {
        assert_eq!(parse_repo_allowlist(r#"{"allow":"not-an-array"}"#), None);
        assert_eq!(parse_repo_allowlist(r#"{}"#), None);
        assert_eq!(parse_repo_allowlist("not json"), None);
        assert_eq!(parse_repo_allowlist(r#"[1,2,3]"#), None);
    }

    // ==== validate_repo_allowlist — ported from test/repo-airgap.test.js ====

    fn ok_of(patterns: &[serde_json::Value]) -> Vec<String> {
        validate_repo_allowlist(patterns).0
    }
    fn rejected_of(patterns: &[serde_json::Value]) -> Vec<RejectedPattern> {
        validate_repo_allowlist(patterns).1
    }

    #[test]
    fn accepts_valid_hostname_patterns() {
        for p in [
            "api.example.com",
            "*.example.com",
            "bedrock-runtime.*.amazonaws.com",
            "deep.sub.domain.example.co.uk",
            "API.EXAMPLE.COM",
        ] {
            assert_eq!(ok_of(&[json!(p)]), vec![p.to_string()], "pattern: {p}");
            assert_eq!(rejected_of(&[json!(p)]), Vec::new(), "pattern: {p}");
        }
    }

    #[test]
    fn keeps_valid_entries_when_mixed_with_invalid_ones() {
        let (ok, rejected) = validate_repo_allowlist(&[
            json!("api.example.com"),
            json!("*"),
            json!("*.example.com"),
        ]);
        assert_eq!(
            ok,
            vec!["api.example.com".to_string(), "*.example.com".to_string()]
        );
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].pattern, json!("*"));
    }

    #[test]
    fn rejects_shape_violations_one_at_a_time() {
        for p in [
            "*",
            "*.com",
            "*.*",
            "localhost",
            "https://x.com",
            "x.com/path",
            "user@x.com",
            "has space.com",
            "tab\there.com",
            "",
            "api.example.com ",
        ] {
            assert_eq!(ok_of(&[json!(p)]), Vec::<String>::new(), "pattern: {p:?}");
            assert_eq!(rejected_of(&[json!(p)]).len(), 1, "pattern: {p:?}");
        }
    }

    #[test]
    fn rejects_non_strings() {
        let patterns = [
            json!(42),
            json!(null),
            json!(null),
            json!({}),
            json!(["x.com"]),
        ];
        let (ok, rejected) = validate_repo_allowlist(&patterns);
        assert_eq!(ok, Vec::<String>::new());
        assert_eq!(rejected.len(), 5);
        for r in &rejected {
            assert_eq!(r.reason, "not a string");
        }
    }

    #[test]
    fn rejects_over_long_patterns() {
        let long = format!("{}.com", "a".repeat(250));
        assert!(long.len() > 253);
        assert_eq!(ok_of(&[json!(long)]), Vec::<String>::new());
        assert_eq!(rejected_of(&[json!(long)]).len(), 1);
    }

    #[test]
    fn rejects_partial_wildcards() {
        assert_eq!(ok_of(&[json!("*api.example.com")]), Vec::<String>::new());
        assert_eq!(ok_of(&[json!("api*.example.com")]), Vec::<String>::new());
    }

    #[test]
    fn every_rejection_carries_a_human_reason() {
        let (_, rejected) = validate_repo_allowlist(&[
            json!("*"),
            json!("localhost"),
            json!(42),
            json!("https://x.com"),
        ]);
        assert_eq!(rejected.len(), 4);
        for r in &rejected {
            assert!(!r.reason.is_empty());
        }
    }

    // ---- breadth boundary (pinned as-designed, not as ideal — see lib/allowlist.js) ----

    #[test]
    fn accepts_interior_double_wildcard() {
        assert_eq!(
            ok_of(&[json!("*.*.example.com")]),
            vec!["*.*.example.com".to_string()]
        );
    }

    #[test]
    fn accepts_a_three_label_leading_wildcard_even_over_a_known_suffix() {
        // KNOWN boundary: no public-suffix list, so *.co.uk is the same
        // breadth class as *.example.com even though co.uk is a suffix.
        assert_eq!(ok_of(&[json!("*.co.uk")]), vec!["*.co.uk".to_string()]);
    }

    #[test]
    fn accepts_an_interior_wildcard() {
        assert_eq!(ok_of(&[json!("a.*.com")]), vec!["a.*.com".to_string()]);
    }

    #[test]
    fn accepts_uppercase_leading_wildcard_pattern_unchanged() {
        // Only the ACCEPT half is ported — the matching-is-case-insensitive
        // half of the original test exercises `compileAllowlist`, out of
        // this module's scope (see the top doc comment).
        assert_eq!(
            ok_of(&[json!("*.EXAMPLE.COM")]),
            vec!["*.EXAMPLE.COM".to_string()]
        );
    }

    // ==== repo consent flow: read / consent / revoke / reapply ====

    fn write_repo_allowlist(root: &Path, text: &str) -> PathBuf {
        let tome_dir = root.join(".tome");
        fs::create_dir_all(&tome_dir).unwrap();
        let file = tome_dir.join("airgap.json");
        fs::write(&file, text).unwrap();
        file
    }

    #[test]
    fn read_repo_allowlist_reports_absent_for_an_empty_root() {
        let state = AirgapState::new();
        assert_eq!(
            state.read_repo_allowlist("", |_| None),
            RepoAllowlistReport::Absent
        );
    }

    #[test]
    fn read_repo_allowlist_reports_absent_when_the_resolver_refuses() {
        // Mirrors confinedRealPath returning null: root outside the open
        // workspace folders, or before ws:sync has run at all.
        let state = AirgapState::new();
        assert_eq!(
            state.read_repo_allowlist("/some/repo", |_| None),
            RepoAllowlistReport::Absent
        );
    }

    #[test]
    fn read_repo_allowlist_reports_absent_for_malformed_json() {
        let dir = tempdir().unwrap();
        let file = write_repo_allowlist(dir.path(), "not json");
        let state = AirgapState::new();
        let root = dir.path().to_str().unwrap();
        assert_eq!(
            state.read_repo_allowlist(root, |_| Some(file.clone())),
            RepoAllowlistReport::Absent
        );
    }

    #[test]
    fn read_repo_allowlist_reports_present_with_hash_ok_and_rejected() {
        let dir = tempdir().unwrap();
        let text = r#"{"allow":["api.example.com","*"]}"#;
        let file = write_repo_allowlist(dir.path(), text);
        let state = AirgapState::new();
        let root = dir.path().to_str().unwrap();
        let report = state.read_repo_allowlist(root, |_| Some(file.clone()));
        match report {
            RepoAllowlistReport::Present {
                hash,
                hosts,
                rejected,
                consented,
            } => {
                assert_eq!(hash, sha1_hex(text));
                assert_eq!(hosts, vec!["api.example.com".to_string()]);
                assert_eq!(rejected.len(), 1);
                assert!(!consented);
            }
            other => panic!("expected Present, got {other:?}"),
        }
    }

    #[test]
    fn consent_then_read_reports_consented_true_until_the_file_changes() {
        let dir = tempdir().unwrap();
        let file = write_repo_allowlist(dir.path(), r#"{"allow":["api.example.com"]}"#);
        let state = AirgapState::new();
        let root = dir.path().to_str().unwrap();
        let resolve = |_p: &Path| Some(file.clone());

        let RepoAllowlistReport::Present { hash, .. } = state.read_repo_allowlist(root, resolve)
        else {
            panic!("expected Present")
        };
        let outcome = state.consent_repo_allowlist(root, &hash, resolve);
        assert!(matches!(outcome, ConsentOutcome::Ok { .. }));

        let RepoAllowlistReport::Present { consented, .. } =
            state.read_repo_allowlist(root, resolve)
        else {
            panic!("expected Present")
        };
        assert!(consented);

        // TOCTOU: the file changes underneath the stored consent.
        fs::write(&file, r#"{"allow":["other.example.com"]}"#).unwrap();
        let RepoAllowlistReport::Present { consented, .. } =
            state.read_repo_allowlist(root, resolve)
        else {
            panic!("expected Present")
        };
        assert!(!consented);
    }

    #[test]
    fn consent_rejects_a_stale_presented_hash() {
        let dir = tempdir().unwrap();
        let file = write_repo_allowlist(dir.path(), r#"{"allow":["api.example.com"]}"#);
        let state = AirgapState::new();
        let root = dir.path().to_str().unwrap();
        let outcome =
            state.consent_repo_allowlist(root, "not-the-real-hash", |_p| Some(file.clone()));
        assert_eq!(outcome, ConsentOutcome::Err("file changed".to_string()));
    }

    #[test]
    fn consent_never_applies_hosts_the_caller_supplied_only_what_it_reads_itself() {
        let dir = tempdir().unwrap();
        let file = write_repo_allowlist(dir.path(), r#"{"allow":["api.example.com"]}"#);
        let state = AirgapState::new();
        let root = dir.path().to_str().unwrap();
        let resolve = |_p: &Path| Some(file.clone());
        let RepoAllowlistReport::Present { hash, .. } = state.read_repo_allowlist(root, resolve)
        else {
            panic!()
        };
        let ConsentOutcome::Ok { applied, .. } = state.consent_repo_allowlist(root, &hash, resolve)
        else {
            panic!()
        };
        assert_eq!(applied, vec!["api.example.com".to_string()]);
        let snap = state.state_snapshot();
        assert_eq!(
            snap.repo,
            vec![RepoStateEntry {
                root: root.to_string(),
                hosts: 1
            }]
        );
    }

    // ---- effective_repo_hosts (Task A4 addition — proxy allow-set wiring) ----

    #[test]
    fn effective_repo_hosts_is_empty_with_no_consents() {
        let state = AirgapState::new();
        assert_eq!(state.effective_repo_hosts(), Vec::<String>::new());
    }

    #[test]
    fn effective_repo_hosts_flattens_every_applied_repos_hosts() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        let file_a = write_repo_allowlist(
            dir_a.path(),
            r#"{"allow":["a.example.com","b.example.com"]}"#,
        );
        let file_b = write_repo_allowlist(dir_b.path(), r#"{"allow":["c.example.com"]}"#);
        let root_a = dir_a.path().to_str().unwrap().to_string();
        let root_b = dir_b.path().to_str().unwrap().to_string();
        let state = AirgapState::new();
        let resolve = |p: &Path| {
            if p.starts_with(&root_a) {
                Some(file_a.clone())
            } else {
                Some(file_b.clone())
            }
        };
        let RepoAllowlistReport::Present { hash: hash_a, .. } =
            state.read_repo_allowlist(&root_a, resolve)
        else {
            panic!()
        };
        let RepoAllowlistReport::Present { hash: hash_b, .. } =
            state.read_repo_allowlist(&root_b, resolve)
        else {
            panic!()
        };
        state.consent_repo_allowlist(&root_a, &hash_a, resolve);
        state.consent_repo_allowlist(&root_b, &hash_b, resolve);

        let mut hosts = state.effective_repo_hosts();
        hosts.sort();
        assert_eq!(
            hosts,
            vec![
                "a.example.com".to_string(),
                "b.example.com".to_string(),
                "c.example.com".to_string()
            ]
        );
    }

    #[test]
    fn effective_repo_hosts_drops_a_roots_hosts_once_revoked() {
        let dir = tempdir().unwrap();
        let file = write_repo_allowlist(dir.path(), r#"{"allow":["api.example.com"]}"#);
        let root = dir.path().to_str().unwrap();
        let resolve = |_p: &Path| Some(file.clone());
        let state = AirgapState::new();
        let RepoAllowlistReport::Present { hash, .. } = state.read_repo_allowlist(root, resolve)
        else {
            panic!()
        };
        state.consent_repo_allowlist(root, &hash, resolve);
        assert_eq!(
            state.effective_repo_hosts(),
            vec!["api.example.com".to_string()]
        );
        state.revoke_repo_allowlist(root);
        assert_eq!(state.effective_repo_hosts(), Vec::<String>::new());
    }

    #[test]
    fn revoke_removes_a_stored_consent_and_its_applied_hosts() {
        let dir = tempdir().unwrap();
        let file = write_repo_allowlist(dir.path(), r#"{"allow":["api.example.com"]}"#);
        let state = AirgapState::new();
        let root = dir.path().to_str().unwrap();
        let resolve = |_p: &Path| Some(file.clone());
        let RepoAllowlistReport::Present { hash, .. } = state.read_repo_allowlist(root, resolve)
        else {
            panic!()
        };
        state.consent_repo_allowlist(root, &hash, resolve);
        assert_eq!(state.state_snapshot().repo.len(), 1);

        state.revoke_repo_allowlist(root);
        assert_eq!(state.state_snapshot().repo.len(), 0);
        let RepoAllowlistReport::Present { consented, .. } =
            state.read_repo_allowlist(root, resolve)
        else {
            panic!()
        };
        assert!(!consented);
    }

    #[test]
    fn revoke_of_a_root_with_no_consent_does_not_panic() {
        let state = AirgapState::new();
        state.revoke_repo_allowlist("/never/consented");
    }

    #[test]
    fn reapply_keeps_an_unchanged_consent_and_drops_a_changed_one() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        let file_a = write_repo_allowlist(dir_a.path(), r#"{"allow":["a.example.com"]}"#);
        let file_b = write_repo_allowlist(dir_b.path(), r#"{"allow":["b.example.com"]}"#);
        let root_a = dir_a.path().to_str().unwrap().to_string();
        let root_b = dir_b.path().to_str().unwrap().to_string();

        let state = AirgapState::new();
        // No `move`: capturing `file_a`/`file_b`/`root_a` by shared
        // reference (all three outlive every use below) rather than by
        // value keeps this closure `Copy`, so it can be passed BY VALUE at
        // each call site below without a `&` at every call — same as every
        // other single-file `resolve` closure in this test module.
        let resolve = |p: &Path| {
            if p.starts_with(&root_a) {
                Some(file_a.clone())
            } else {
                Some(file_b.clone())
            }
        };
        let RepoAllowlistReport::Present { hash: hash_a, .. } =
            state.read_repo_allowlist(&root_a, resolve)
        else {
            panic!()
        };
        let RepoAllowlistReport::Present { hash: hash_b, .. } =
            state.read_repo_allowlist(&root_b, resolve)
        else {
            panic!()
        };
        state.consent_repo_allowlist(&root_a, &hash_a, resolve);
        state.consent_repo_allowlist(&root_b, &hash_b, resolve);
        assert_eq!(state.state_snapshot().repo.len(), 2);

        // root_b's file changes on disk after consent was granted.
        fs::write(&file_b, r#"{"allow":["changed.example.com"]}"#).unwrap();

        assert!(state.reapply_repo_consents(resolve));
        let repo = state.state_snapshot().repo;
        assert_eq!(repo.len(), 1);
        assert_eq!(repo[0].root, root_a);
    }

    #[test]
    fn reapply_with_no_consents_is_a_harmless_no_op() {
        let state = AirgapState::new();
        assert!(state.reapply_repo_consents(|_| None));
        assert_eq!(state.state_snapshot().repo, Vec::new());
    }

    // ==== persistence: save/load round trip + 0600 ====

    #[test]
    fn save_and_load_repo_consents_round_trip_and_the_file_is_0600() {
        let scratch = tempdir().unwrap();
        let repo_dir = tempdir().unwrap();
        let consents_path = scratch.path().join("airgap-repo-consents.json");
        let file = write_repo_allowlist(repo_dir.path(), r#"{"allow":["api.example.com"]}"#);
        let root = repo_dir.path().to_str().unwrap();
        let resolve = |_p: &Path| Some(file.clone());

        let state = AirgapState::new();
        state.load_repo_consents(&consents_path); // no file yet — starts empty, records the path
        let RepoAllowlistReport::Present { hash, .. } = state.read_repo_allowlist(root, resolve)
        else {
            panic!()
        };
        state.consent_repo_allowlist(root, &hash, resolve);
        assert!(
            consents_path.exists(),
            "consent_repo_allowlist must persist on success"
        );

        // A fresh AirgapState loading the same file sees the same consent.
        let reloaded = AirgapState::new();
        reloaded.load_repo_consents(&consents_path);
        let RepoAllowlistReport::Present { consented, .. } =
            reloaded.read_repo_allowlist(root, resolve)
        else {
            panic!()
        };
        assert!(consented);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&consents_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn load_repo_consents_on_a_missing_file_leaves_state_empty_but_still_records_the_path_for_future_saves(
    ) {
        let scratch = tempdir().unwrap();
        let consents_path = scratch.path().join("does-not-exist.json");
        let state = AirgapState::new();
        state.load_repo_consents(&consents_path);
        assert_eq!(state.state_snapshot().repo, Vec::new());

        // The path was still recorded — the next consent can save.
        let repo_dir = tempdir().unwrap();
        let file = write_repo_allowlist(repo_dir.path(), r#"{"allow":["api.example.com"]}"#);
        let root = repo_dir.path().to_str().unwrap();
        let resolve = |_p: &Path| Some(file.clone());
        let RepoAllowlistReport::Present { hash, .. } = state.read_repo_allowlist(root, resolve)
        else {
            panic!()
        };
        state.consent_repo_allowlist(root, &hash, resolve);
        assert!(consents_path.exists());
    }

    #[test]
    fn save_before_any_load_is_a_silent_no_op_not_an_error() {
        // consent_repo_allowlist swallows the save's Result already; this
        // pins that no path anywhere on disk gets written when
        // `consents_path` was never set.
        let repo_dir = tempdir().unwrap();
        let file = write_repo_allowlist(repo_dir.path(), r#"{"allow":["api.example.com"]}"#);
        let root = repo_dir.path().to_str().unwrap();
        let resolve = |_p: &Path| Some(file.clone());
        let state = AirgapState::new(); // load_repo_consents never called
        let RepoAllowlistReport::Present { hash, .. } = state.read_repo_allowlist(root, resolve)
        else {
            panic!()
        };
        let outcome = state.consent_repo_allowlist(root, &hash, resolve);
        assert!(matches!(outcome, ConsentOutcome::Ok { .. }));
        // In-memory consent still recorded even though nothing was saved.
        assert_eq!(state.state_snapshot().repo.len(), 1);
    }
}
