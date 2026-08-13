use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};

use tokio::sync::Notify;

/// Shared app state, installed once via `.manage(AppState::new())` in
/// `lib.rs::run()` and reached from commands as `State<'_, AppState>`.
///
/// Kept to exactly the fields Phase 1 (this slice's quit handshake, plus the
/// domains later slices land in) needs. Later slices extend THEIR OWN
/// modules — `confine.rs`, `store.rs`, `airgap` (Phase 3), etc. — rather
/// than adding fields here, so this file stays a rare merge-conflict site
/// across parallel agents.
pub struct AppState {
    /// Mirrors `src/main/authlock.js`'s locked/unlocked flag. Always
    /// `false` until the Phase 3 airgap+auth slice ports `initAuth` and
    /// actually flips it; `lock_gate::guard` does not consult it yet either
    /// (see that module's doc comment).
    pub locked: RwLock<bool>,
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
    /// Live filesystem watcher handles keyed by watched path, backing the
    /// `fs_watch`/`fs_unwatch` commands. Placeholder value type — becomes
    /// the real notify/notify-debouncer-mini handle when `fs.rs` grows a
    /// body; kept here now only so that slice never needs to touch this
    /// struct.
    pub watchers: Mutex<HashMap<String, ()>>,
    /// Live PTY panes, keyed by pane id — backs the `pty_write`/
    /// `pty_resize`/`pty_kill` commands (`pty_create` itself is a later
    /// slice's work; see `pty.rs`'s module doc comment for the exact
    /// phase-2 split). Real from Phase 2 slice P1 on, unlike `watchers`
    /// above.
    pub pty: crate::pty::Registry,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            locked: RwLock::new(false),
            open_folders: RwLock::new(Vec::new()),
            folders_synced: RwLock::new(false),
            theme: RwLock::new(serde_json::Value::Null),
            quit_ready: Notify::new(),
            watchers: Mutex::new(HashMap::new()),
            pty: crate::pty::Registry::new(),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
