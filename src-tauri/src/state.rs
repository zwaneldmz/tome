use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::Notify;
use tokio::task::AbortHandle;

use crate::{authlock, conductor, egress, flow};

/// Shared app state, installed once via `.manage(AppState::new())` in
/// `lib.rs::run()` and reached from commands as `State<'_, AppState>`.
///
/// Kept to exactly the fields Phase 1 (this slice's quit handshake, plus the
/// domains later slices land in) needs. Later slices extend THEIR OWN
/// modules — `confine.rs`, `store.rs`, `egress::EgressState` (Phase 3),
/// etc. — rather than adding fields here, so this file stays a rare
/// merge-conflict site across parallel agents. Two deliberate exceptions
/// land in Phase 3 alongside `egress` itself: `proxies` and `auth` below are
/// flat fields rather than folded into a subsystem struct, because neither
/// has one to fold into by construction — `proxies` holds live per-pane
/// runtime handles a Tauri command spawns and owns directly (no
/// module-level singleton the way `egress::EgressState`'s pure state
/// machine is one), and `auth` is plain data (`authlock.rs`'s future `Auth`
/// shape) the same way `theme` below already is, not a subsystem with its
/// own API surface.
pub struct AppState {
    /// The composite gate `lock_gate::guard` reads directly — mirrors
    /// `index.js`'s `isLockedNow()` (`is_locked(configured, unlocked,
    /// shot_mode)`), computed once at boot (`lib.rs::run()`'s `.setup()`,
    /// once `authlock::AuthLock::load` resolves `configured`) and collapsed
    /// to `false` at every one-way login success point (`auth_login`,
    /// `auth_touchid`, `egress_setup` — mirroring `authlock.markUnlocked()`;
    /// setting `unlocked = true` always forces `is_locked(...) = false`
    /// regardless of `configured`/`shot_mode`, so a direct `false` write is
    /// equivalent to recomputing the full formula at each of those three
    /// call sites). Never flips back to `true` — this build has no
    /// re-lock-the-whole-app action, same as `authlock.js`'s `unlocked` is
    /// one-way for a process's lifetime.
    pub locked: RwLock<bool>,
    /// The RAW session flag `auth_status` reports as `unlocked` —
    /// `authlock.js`'s `isUnlocked()`/`markUnlocked()`. Deliberately a
    /// SEPARATE field from `locked` above: `locked` is the already-composed
    /// gate bool (needs no `configured`/`shot_mode` inputs at `guard()`'s
    /// call site), but `is_locked`'s formula is not invertible when
    /// `configured` is `false` (an unconfigured app is never locked
    /// regardless of this flag's true value), so `auth_status` cannot
    /// recover the honest raw bit from `locked` alone — see
    /// `ipc::auth::auth_status`. Starts `false`; flipped to `true` at the
    /// same three one-way call sites as `locked` above.
    pub auth_unlocked: RwLock<bool>,
    /// The renderer's open workspace folders, synced via the `ws_sync`
    /// command (Electron's `ws:sync`) — the confinement root set
    /// `confine::confined_real_path` will check paths against.
    pub open_folders: RwLock<Vec<PathBuf>>,
    /// Whether `open_folders` has received its first sync from the renderer
    /// this session, distinguishing "no folders reported yet" from
    /// "reported, and it's empty" — mirrors a distinction index.js's
    /// confinement code makes.
    pub folders_synced: RwLock<bool>,
    /// Resolved UI theme payload set via the `theme_set` command (Electron's
    /// `theme:set`, `{ pref, mode }`). `Value::Null` until first set.
    pub theme: RwLock<serde_json::Value>,
    /// Notified once by the `app_quit_ready` command (Electron's
    /// `app:quit-ready`). The main-window `CloseRequested` handler installed
    /// in `lib.rs::run()` awaits this, capped at 1.5s — see that handler's
    /// doc comment for the full handshake.
    pub quit_ready: Notify,
    /// Live PTY panes, keyed by pane id — backs the `pty_write`/
    /// `pty_resize`/`pty_kill` commands (`pty_create` itself is a later
    /// slice's work; see `pty.rs`'s module doc comment for the exact
    /// phase-2 split). Real from Phase 2 slice P1 on.
    pub pty: crate::pty::Registry,

    /// Popout window labels the renderer has cleared to close, armed by
    /// `ipc::popout::popout_close` and consulted (then cleared) by
    /// `lib.rs`'s `CloseRequested` handler — the direct port of
    /// `src/main/index.js`'s `popoutApproved` Set of BrowserWindow ids,
    /// keyed by Tauri window label instead (Tauri has no numeric window
    /// ids). A popout's close is vetoed until its label lands here; the
    /// entry is removed when the window actually finishes closing
    /// (`Destroyed`), mirroring the original's `child.on('closed', () =>
    /// popoutApproved.delete(child.id))`.
    pub popout_approved: Mutex<std::collections::HashSet<String>>,

    /// Egress pane-gapping state machine, repo-allowlist consent
    /// bookkeeping, and unlock/relock deadline tracking —
    /// `egress::EgressState` (Phase 3, Task A3; see that module's doc
    /// comment for the exact scope split with `egress::proxy`/
    /// `egress::allowlist`, still empty files as of this field landing).
    /// Owns its own interior locking (the same shape `pty` above already
    /// uses), so it is a plain value field here, not wrapped in another
    /// `Mutex`.
    pub egress: egress::EgressState,

    /// Live per-pane proxy handles, keyed by pane id — Task A4's real wiring
    /// of `egress::proxy::PaneProxy` (a listening loopback CONNECT/HTTP
    /// server plus its live-tunnel registry, mirroring `src/main/egress.js`'s
    /// `st.server`/`st.tunnels`), replacing the placeholder `()` value this
    /// field started with. `Arc`-wrapped (not a bare owned value) so
    /// `ipc::egress::egress_unlock`'s auto-relock timer (a `tokio::spawn`
    /// task that must outlive the command invocation that scheduled it) can
    /// hold its own handle without keeping this map's mutex locked for the
    /// whole unlock window. Deliberately NOT folded into `egress:
    /// EgressState` above — see that module's top doc comment on why the
    /// live proxy/tunnel objects stay out of the pure pane-gapping state
    /// machine. A pane's `EgressState` record (mode/expiry, for the
    /// `egress:state` UI snapshot) and its entry here (the actual live
    /// enforcement — `PaneProxy` has its own independent mode) are TWO
    /// representations the integrator keeps in sync at exactly two
    /// mutation points (`egress_unlock`, `egress_relock`/pane close) —
    /// documented as a judgment call in this slice's task report.
    pub proxies: Mutex<HashMap<String, Arc<egress::proxy::PaneProxy>>>,

    /// One scheduled auto-relock task per currently-`Open` pane, keyed by
    /// pane id — the Rust analog of `egress.js`'s per-pane `st.timer`
    /// (`setTimeout(() => relockPane(paneId), minutes * 60_000)`), which
    /// `unlockPane` `clearTimeout`s and replaces on every fresh unlock.
    /// Neither `egress::EgressState` (framework-free, no timers of its own
    /// — see that module's doc comment) nor `egress::proxy::PaneProxy`
    /// (deliberately policy-free — see that module's doc comment) owns a
    /// timer handle, so the integrator holds it here: `egress_unlock`
    /// aborts any existing entry for the pane before inserting the new
    /// task's `AbortHandle`; `egress_relock` and pane-close both abort and
    /// remove it directly (an immediate relock must not let a
    /// since-superseded timer fire later and relock a pane that has since
    /// been closed and possibly reused).
    ///
    /// The `u64` paired with each `AbortHandle` is the generation
    /// [`relock_timer_generation`](Self::relock_timer_generation) minted
    /// for that specific timer — `AbortHandle::abort()` alone is NOT a
    /// reliable way to guarantee a superseded timer never fires: tokio
    /// cancellation only takes effect at a task's NEXT suspension point,
    /// so a timer whose sleep had already elapsed (and whose continuation
    /// had already resumed running) by the time a fresh unlock's
    /// `.abort()` call lands keeps going regardless. Every scheduled
    /// timer's continuation re-checks, immediately before calling
    /// `relock_now`, that ITS OWN captured generation is still the one
    /// stored here for its pane — see `ipc::egress::schedule_unlock`'s doc
    /// comment for the exact race this closes.
    pub relock_timers: Mutex<HashMap<String, (u64, AbortHandle)>>,

    /// Monotonic counter minting the generation token above — bumped once
    /// per `ipc::egress::schedule_unlock` call, regardless of pane, so
    /// every scheduled timer gets a value strictly greater than any prior
    /// timer's (for ANY pane — a single shared counter is simpler than a
    /// per-pane one and is just as correct, since each timer only ever
    /// compares its own value against the CURRENT entry for its OWN pane
    /// id).
    pub relock_timer_generation: std::sync::atomic::AtomicU64,

    /// The unlock-authentication state — scrypt hash + salt, optional TOTP
    /// secret/active flag, mirroring `src/main/authlock.js`'s module-level
    /// `auth` variable (`{ salt, hash, totp? }`), now `authlock::AuthLock`
    /// (this phase's other slice, landed). `None` only for the brief window
    /// before `lib.rs::run()`'s `.setup()` resolves `app_data_dir` and calls
    /// `AuthLock::load` — every `#[tauri::command]` runs strictly after
    /// `.setup()` returns (Tauri does not start serving IPC before then), so
    /// no command should ever actually observe `None` here; callers still
    /// treat it as "unconfigured" defensively rather than panicking, same
    /// discipline as `watchers`/`proxies` above tolerating an empty map
    /// before their own first real write. The companion `unlocked` session
    /// flag lives on `AppState` directly (`locked`/`auth_unlocked` above),
    /// per `authlock.rs`'s own doc comment on that split.
    pub auth: Mutex<Option<authlock::AuthLock>>,

    /// The live background-flow-run registry — `flow::runner::Runner`
    /// (Phase 5b), mirroring `flow-runner.js`'s module-level `const runs =
    /// new Map()`. `Arc`-wrapped (the same reasoning as `proxies` above,
    /// just one level up): `runs:start`'s own background scheduling loop,
    /// per-node exit-await tasks, and kill-escalation timers all outlive
    /// the single command invocation that spawned them, and each needs its
    /// own strong reference to the SAME registry — a bare `Runner` field
    /// only ever reachable via `State<'_, AppState>`'s short-lived borrow
    /// cannot be captured into a `tokio::spawn`'d `'static` future the way
    /// an `Arc` clone can.
    pub flow: std::sync::Arc<flow::Runner>,

    /// The assistant tool loop's live session state — `conductor::Conductor`
    /// (Phase 5b), mirroring `conductor.js`'s module-level `meta`/`scrolls`/
    /// `readConsent`/`panes`/`allowRun`/`inflight` `let`s. A plain value
    /// field, not `Arc`-wrapped: unlike `flow` above, nothing here is read
    /// from a `tokio::spawn`'d background task outliving a single command
    /// invocation — the tool loop runs inline inside `chat_send`'s own
    /// async fn (see `conductor::chat::run_chat`'s doc comment) — so a bare
    /// value reachable through the ordinary `State<'_, AppState>` borrow
    /// each command already gets is enough, the same shape `pty`/`egress`
    /// above already use for the identical reason.
    /// `Arc` (not a bare field like the others) so the pty output batcher's
    /// `'static` data-tap closure can hold its own strong reference to feed
    /// `record()` — the per-chunk scrollback tap `read_terminal` reads back.
    pub conductor: std::sync::Arc<conductor::Conductor>,

    /// Mentor-mode comprehension-gate registry — `mentor::Mentor`
    /// (backend half of the `gate_question`/`mentor_answer` loop; see that
    /// module's doc comment). A plain value field that owns its own interior
    /// locking, the same shape `pty`/`egress` above already use — the gate is
    /// only ever touched from within a command's own `State<'_, AppState>`
    /// borrow, never from a `tokio::spawn`'d background task outliving that
    /// borrow, so no `Arc` wrapper is needed.
    pub mentor: crate::mentor::Mentor,

    /// The in-app scheduler's one 30-second tick loop —
    /// `lib.rs::spawn_schedule_ticker` spawns it once at boot and stores its
    /// `AbortHandle` here so `lib.rs`'s quit handshake can cancel it, the
    /// same `Mutex<Option<AbortHandle>>` shape [`relock_timers`](Self::relock_timers)
    /// uses for its own per-pane timers — a single slot rather than a map
    /// here, since this crate ever spawns exactly one such ticker per
    /// process, never one per pane. `None` until `spawn_schedule_ticker`
    /// runs, and again after the quit handler's `Option::take` cancels it —
    /// a second quit-path call finds nothing left to abort.
    pub schedule_ticker: Mutex<Option<AbortHandle>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            locked: RwLock::new(false),
            auth_unlocked: RwLock::new(false),
            open_folders: RwLock::new(Vec::new()),
            folders_synced: RwLock::new(false),
            theme: RwLock::new(serde_json::Value::Null),
            quit_ready: Notify::new(),
            pty: crate::pty::Registry::new(),
            popout_approved: Mutex::new(std::collections::HashSet::new()),
            egress: egress::EgressState::new(),
            proxies: Mutex::new(HashMap::new()),
            relock_timers: Mutex::new(HashMap::new()),
            relock_timer_generation: std::sync::atomic::AtomicU64::new(0),
            auth: Mutex::new(None),
            flow: std::sync::Arc::new(flow::Runner::new()),
            conductor: std::sync::Arc::new(conductor::Conductor::new()),
            mentor: crate::mentor::Mentor::new(),
            schedule_ticker: Mutex::new(None),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
