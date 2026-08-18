//! PTY core (Phase 2, slice P1): the spawn/read/batch/write/resize/kill
//! mechanism every pane uses. Ports the mechanism half of
//! `src/main/index.js`'s `pty:*` handlers — the 4ms/64KB output batcher
//! (`queuePtyData`/`flushPtyData`, lines 62-86) verbatim, and the
//! `pty:write`/`pty:resize`/`pty:kill` handlers (lines 788-797) for real.
//!
//! Deliberately NOT here (a different, "integration", slice's job — see the
//! phase 2 task brief): `pty:create`'s own command body, agent-kind
//! resolution (`buildAgentSpawnFrom`), gapping policy
//! (`resolveGapping`/`unrestrictedSpawnNeedsReauth`), cwd fallback
//! (`resolveSpawnCwd`), the login-shell PATH/secrets harvest
//! (`ensureLoginEnv`), or the seatbelt/bwrap wrap. [`TerminalOpts`] is
//! intentionally a "give me an already-resolved spawn spec" shape — no
//! policy fields — so that slice can build one without this module needing
//! to know anything about gapping, custom agents, or re-auth.
//!
//! ## Why explicit kill + explicit reap (unlike node-pty)
//!
//! `node-pty` SIGHUPs its child automatically when the master fd is closed,
//! so the Electron original could get away with `ptys.get(id)?.kill()` and
//! nothing else. `portable-pty` does not: dropping its `MasterPty` alone
//! does NOT make the child die, and a killed-but-unwaited child becomes a
//! zombie. [`Registry::kill`] is explicit about every step index.js got for
//! free: signal the child, drop the master, and reap it.
//!
//! ## Why one thread owns `Child`, and the registry only ever holds a
//! cloned killer
//!
//! `portable_pty::Child::kill`/`::wait` both take `&mut self`, so a single
//! `Child` cannot simultaneously be "kept in the registry for `pty:kill` to
//! signal on demand" AND "blocked in `.wait()` on a background thread,
//! however long that takes". `ChildKiller::clone_killer` is portable-pty's
//! own answer to exactly this split (see its doc comment: "Clone an object
//! that can be split out from the Child in order to send it signals
//! independently from a thread that may be blocked in `.wait`"). So: the
//! ORIGINAL `Child` moves into [`reader_loop`] at spawn time and is never
//! seen again outside it; the registry keeps only a cloned
//! `Box<dyn ChildKiller>` for [`Registry::kill`] to signal through.
//!
//! `reader_loop` reads the master until it observes a clean EOF —
//! portable-pty's own Unix `Read` impl already folds the EIO-on-hangup
//! quirk into `Ok(0)` (see that crate's `unix.rs`), so `Ok(0)` is the only
//! "this pty session is over" signal this module needs to recognize. That
//! happens once the slave side has no more open references ANYWHERE — that is
//! once the child (and anything that inherited its fds) has fully exited,
//! whether it exited on its own or was just killed. So `reader_loop` is
//! also the ONLY place that calls `child.wait()` and the ONLY place that
//! fires `on_exit` (`pty:exit`) — a natural exit and a killed exit both
//! funnel through identical cleanup, exactly like node-pty's single
//! `onExit` callback did in the Electron original (search index.js for
//! `p.onExit`: it always sends `{ id, exitCode }`, kill() does not send its
//! own separate event).
//!
//! `pty:data` streams out over the `Channel` every spawn is given (see
//! `batcher_loop`/`flush_buf`); `pty:exit` deliberately does NOT — it goes
//! through the `on_exit` callback [`Registry::spawn_raw`] takes instead, so
//! the production caller (`ipc::pty::pty_create`) can route it onto the
//! global Tauri event bus (`app.emit("pty:exit", ...)`) rather than the
//! Channel. That split matters: the already-committed renderer contract
//! (`tome-ipc.js`) wires `pty.create`'s Channel to `onData` subscribers
//! only, and `pty.onExit` to a separate `listen('pty:exit', cb)` — an exit
//! payload sent down the Channel would reach `onData` instead and never
//! reach `onExit` at all.
//!
//! ## The UTF-8 tail-carry the JS original never needed
//!
//! `queuePtyData` buffers a JS *string* (`buf.data += data`) — node-pty
//! decodes each raw read through Node's `StringDecoder`, which already
//! carries an incomplete trailing multi-byte sequence to the next chunk
//! internally. Reading raw bytes ourselves means this module has to do that
//! carrying itself: [`incomplete_utf8_tail_len`] finds how many trailing
//! bytes of a buffer are an as-yet-incomplete UTF-8 sequence (0..=3) so a
//! character split across two OS reads — and therefore, potentially, two
//! flush windows — is never corrupted into a replacement character. Only a
//! genuinely invalid sequence (not just incomplete) is flushed immediately
//! via lossy decoding; nothing is ever held back forever.
//!
//! `ipc::pty::pty_create` (a different, "integration", slice's file — see
//! its own doc comment) is `spawn_terminal`/`spawn_raw`'s real production
//! caller for the terminal branch this phase actually spawns. The blanket
//! allow below stays rather than shrinking to one attribute per item: a
//! handful of things here — `Registry::contains`/`size_of`, the
//! `#[cfg(test)]`-only introspection helpers — are still exercised by
//! nothing but this module's own tests, and narrowing this further isn't
//! this pass's concern; `cargo test` compiles and exercises every item here
//! regardless of which ones a non-test build currently reaches.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use tauri::ipc::Channel;

/// A per-pane output tap: called with each flushed (batched, UTF-8-decoded)
/// chunk on the batcher task, alongside the `pty:data` Channel send. Kept a
/// bare `Fn(&str)` so this module stays conductor-agnostic (matching
/// `TerminalOpts`'s "no policy fields" design) — `ipc::pty::pty_create`
/// installs one that captures an `Arc<Conductor>` + pane id and calls
/// `Conductor::record`, the scrollback ring `read_terminal` reads back.
pub type DataTap = std::sync::Arc<dyn Fn(&str) + Send + Sync>;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// `index.js`'s `PTY_FLUSH_MS` (line 64) — verbatim.
const PTY_FLUSH_MS: u64 = 4;
/// `index.js`'s `PTY_FLUSH_BYTES` (line 65) — verbatim. The JS original
/// compares this against a JS *string*'s `.length`; this port compares it
/// against a raw byte count instead (see the module doc comment's UTF-8
/// section for why bytes, not decoded text, are what this module buffers) —
/// a more literal "64KB" than the original's code-unit count ever was for
/// non-ASCII output, not a behavioral regression.
const PTY_FLUSH_BYTES: usize = 64 * 1024;

/// Everything [`Registry::spawn_terminal`] needs to start a plain
/// login-shell pane, already resolved by the caller. No policy fields on
/// purpose — see the module doc comment.
pub struct TerminalOpts {
    /// The renderer-generated pane id — becomes the `id` field of every
    /// `pty:data`/`pty:exit` message this pane produces.
    pub id: String,
    /// Absolute path to the login shell, for example `index.js`'s
    /// `const SHELL = process.env.SHELL || '/bin/zsh'` (line 138 — not this
    /// module's job to read that env var; the caller resolves it).
    pub shell: String,
    /// Already-resolved starting directory (`resolveSpawnCwd`'s output — a
    /// different slice's function). Used as-is, no existence check: a bad
    /// cwd surfaces as a normal spawn failure, same as it would from
    /// `pty.spawn` in the original.
    pub cwd: PathBuf,
    /// The child's COMPLETE environment. Replaces whatever this process's
    /// own environment is — it is not merged with it (see
    /// `build_terminal_command`'s doc comment for why that matters). The
    /// caller (`buildAgentEnv`'s future port) owns allowlisting/secrets/
    /// `TERM`; this module applies exactly what it is given.
    pub env: Vec<(String, String)>,
    /// Initial size. `index.js`'s `pty.spawn` call hardcodes
    /// `cols: 80, rows: 24` regardless of the pane's real rendered size —
    /// callers should pass that same default to match; the renderer
    /// corrects it with a real `pty:resize` immediately after create.
    pub cols: u16,
    pub rows: u16,
}

/// One live pane's resources. Never `Clone`/`Copy` — moved out of the
/// registry wholesale by [`Registry::kill`], and by `reader_loop` on a
/// natural exit.
struct PaneHandle {
    /// Resize target, and dropped as part of `kill()`'s sequence. Only
    /// ever touched under the registry's own mutex.
    master: Box<dyn MasterPty + Send>,
    /// `MasterPty::take_writer` can only be called once per pty (that
    /// trait's own restriction), so it is taken once at spawn time and
    /// kept here rather than re-derived from `master` on every write.
    writer: Box<dyn Write + Send>,
    /// Cloned off the real `Child` at spawn time — see the module doc
    /// comment's "one thread owns `Child`" section for why the registry
    /// never holds the `Child` itself.
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// This spawn's identity token (`Registry::next_seq`, one fresh value
    /// per `spawn_raw` call). Lets that spawn's own `reader_loop` tell,
    /// once it finally reaps its child, whether the map's CURRENT entry
    /// for this pane id is still the one it inserted — see `spawn_raw`'s
    /// doc comment on duplicate ids for the ABA race this closes.
    seq: u64,
    /// Retained for a future slice's clean-shutdown path (for example the quit
    /// handshake enumerating every live pane and awaiting its teardown,
    /// mirroring index.js's `window-all-closed` doing
    /// `for (const p of ptys.values()) p.kill()`) — neither task is
    /// `.await`ed by anything in this slice (dropping a `JoinHandle` does
    /// not cancel the task; both run to completion regardless of whether
    /// anything ever joins them), hence the leading underscores.
    _reader_task: JoinHandle<()>,
    _batcher_task: JoinHandle<()>,
}

/// `HashMap<paneId, handle>` behind a mutex, per the phase 2 task brief.
/// The map itself lives behind an `Arc` (not just the `Mutex`) so
/// background tasks spawned by [`Registry::spawn_raw`] can hold their own
/// cheap clone of it — those tasks must outlive the `&self` borrow of
/// whichever `spawn_terminal` call started them.
pub struct Registry {
    inner: Arc<Mutex<HashMap<String, PaneHandle>>>,
    /// One fresh `u64` per [`Registry::spawn_raw`] call — see
    /// [`PaneHandle::seq`]'s doc comment. `Relaxed` ordering is enough: the
    /// counter's only job is producing values no two concurrent spawns
    /// share, not synchronizing anything else.
    next_seq: Arc<AtomicU64>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            next_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, PaneHandle>> {
        self.inner.lock().expect("pty::Registry mutex poisoned")
    }

    /// Pane count — test/introspection support only (a future slice's quit
    /// handshake is a plausible real caller; nothing in this slice needs
    /// it outside `#[cfg(test)]`).
    #[cfg(test)]
    fn contains(&self, id: &str) -> bool {
        self.entries().contains_key(id)
    }

    /// Current terminal size as the kernel has it — test support for
    /// [`Registry::resize`] (`get_size()` round-trips what `resize()` just
    /// set, proving the call actually reached the pty rather than merely
    /// returning `true`).
    #[cfg(test)]
    fn size_of(&self, id: &str) -> Option<(u16, u16)> {
        let entries = self.entries();
        let handle = entries.get(id)?;
        let size = handle.master.get_size().ok()?;
        Some((size.cols, size.rows))
    }

    /// `pty:write` (index.js line 788: `ptys.get(id)?.write(data)`) —
    /// silently a no-op for an unknown pane id, same as the original's
    /// optional chaining. Returns whether a pane was found, purely so
    /// tests can assert on it; the real `pty_write` command ignores it
    /// (the original never surfaced a result for this channel either — it
    /// is `ipcMain.on`, fire-and-forget).
    pub fn write(&self, id: &str, data: &str) -> bool {
        let mut entries = self.entries();
        let Some(handle) = entries.get_mut(id) else {
            return false;
        };
        let _ = handle.writer.write_all(data.as_bytes());
        let _ = handle.writer.flush();
        true
    }

    /// `pty:resize` (index.js line 790: `ptys.get(id)?.resize(cols, rows)`)
    /// — same no-op-on-unknown-id contract as `write`.
    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> bool {
        let entries = self.entries();
        let Some(handle) = entries.get(id) else {
            return false;
        };
        let _ = handle.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        true
    }

    /// `pty:kill` (index.js lines 791-797:
    /// `flushPtyData(id); ptys.get(id)?.kill(); ptys.delete(id); ...`),
    /// adapted for portable-pty's explicit-kill/explicit-reap model — see
    /// the module doc comment. Sequence: remove from the registry first
    /// (so no further `write`/`resize` can reach a pane that is being
    /// killed, and so `reader_loop`'s own natural-exit cleanup — if it
    /// runs around the same time — finds nothing left to do), then, off
    /// the async runtime (`spawn_blocking`, since `ChildKiller::kill` can
    /// block for a grace period — see its impl for
    /// `std::process::Child` — before falling back to `SIGKILL`): signal
    /// the child, then drop the master.
    ///
    /// Does NOT itself wait for the reap or for `pty:exit` to be sent —
    /// `reader_loop`, already running since spawn time, does that
    /// independently once it observes the now-signalled child's EOF (see
    /// the module doc comment). `tome-ipc.js`'s `kill()` is fire-and-forget
    /// (`fire('pty_kill', ...)`, never awaited by the renderer either), so
    /// there is no caller this needs to block for. Returns `false` — a
    /// safe no-op, not a panic — for an unknown or already-gone pane id
    /// (double-kill, or a kill that lost the race to a natural exit).
    pub async fn kill(&self, id: &str) -> bool {
        let Some(handle) = self.entries().remove(id) else {
            return false;
        };
        let PaneHandle {
            master,
            writer,
            killer,
            ..
        } = handle;
        let _ = tokio::task::spawn_blocking(move || {
            let mut killer = killer;
            let _ = killer.kill();
            drop(master);
            drop(writer);
        })
        .await;
        true
    }

    /// Builds the login-shell command line per `index.js`'s terminal
    /// branch (`kind === 'terminal'` -> `agentCmd` is `null` ->
    /// `spawnArgs = ['-l']`, line 753) and delegates to [`Self::spawn_raw`].
    ///
    /// `on_exit`: see [`Self::spawn_raw`]'s doc comment — fires with this
    /// pane's `pty:exit` exactly once, after its process has actually been
    /// reaped.
    pub async fn spawn_terminal(
        &self,
        opts: TerminalOpts,
        channel: Channel<Value>,
        tap: Option<DataTap>,
        on_exit: impl FnOnce(i64) + Send + 'static,
    ) -> Result<(), String> {
        let id = opts.id.clone();
        let size = PtySize {
            rows: opts.rows,
            cols: opts.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let cmd = build_terminal_command(&opts);
        self.spawn_raw(id, cmd, size, channel, tap, on_exit).await
    }

    /// The mechanism every spawn path shares — `spawn_terminal` above is a
    /// thin `CommandBuilder` builder on top of this; a later phase's agent
    /// path (once the egress port lifts the phase 2 restriction — see this
    /// slice's task brief) is expected to call this directly with its own
    /// `CommandBuilder` (sandbox wrap argv and all) rather than duplicating
    /// any of it. `pub(crate)` rather than a private fn for exactly that
    /// reason; `pub` rather than `pub(crate)` would be no safer since
    /// nothing outside this crate can reach a `Registry` at all.
    ///
    /// `on_exit` fires exactly once, from `reader_loop`, once this pane's
    /// process has actually been reaped (see that function's doc comment)
    /// — the production caller (`ipc::pty::pty_create`) passes a closure
    /// that does `app.emit("pty:exit", ...)`. A plain callback rather than
    /// a concrete `AppHandle` parameter, so this module stays decoupled
    /// from Tauri specifics (matching `TerminalOpts`'s "no policy fields"
    /// design) and so this module's own tests can observe an exit without
    /// needing Tauri's `test` cargo feature, which this crate's
    /// `Cargo.toml` — out of this slice's scope to edit — does not enable
    /// (see `events.rs`'s "Testing boundary note" for the identical
    /// constraint elsewhere in this crate).
    ///
    /// ## Duplicate ids
    ///
    /// Nothing upstream of this function guarantees `id` isn't already
    /// live in the registry — this app's own threat model is a renderer
    /// that cannot be trusted to always pair every `pty:create` with a
    /// `pty:kill` first. A bare `self.entries().insert(id, ...)` here
    /// would silently DROP any existing `PaneHandle` at that key without
    /// ever signaling its process — the module doc comment's "one thread
    /// owns `Child`" section is explicit that dropping
    /// `master`/`writer`/`killer` alone does NOT make a `portable-pty`
    /// child die, so that dropped process would keep running, orphaned,
    /// with no reference left anywhere to ever signal it again. Worse,
    /// that orphan's own `reader_loop` — already running since ITS spawn —
    /// would eventually observe its EOF and run its own cleanup, which
    /// (absent [`PaneHandle::seq`]'s check) would then unconditionally
    /// evict whatever is CURRENTLY at `id`: by then, the new, live pane
    /// this call just inserted. So: any existing entry is evicted and
    /// properly torn down (signal + drop, exactly like [`Self::kill`])
    /// BEFORE the new one is inserted, and every `PaneHandle` carries a
    /// fresh [`PaneHandle::seq`] so a stale `reader_loop`'s eventual
    /// cleanup can recognize it no longer owns the slot and leave it
    /// alone.
    pub(crate) async fn spawn_raw(
        &self,
        id: String,
        cmd: CommandBuilder,
        size: PtySize,
        channel: Channel<Value>,
        tap: Option<DataTap>,
        on_exit: impl FnOnce(i64) + Send + 'static,
    ) -> Result<(), String> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size).map_err(|e| e.to_string())?;
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        // The parent must not keep its own copy of the slave open past
        // spawn: portable-pty dup()s the slave into the child's stdio
        // during spawn_command, and as long as OUR copy stays open too,
        // the kernel never sees "zero readers" on hangup — the master's
        // read() would never return EOF even after the child exits, and
        // reader_loop's whole exit-detection strategy depends on that EOF
        // (see the module doc comment).
        drop(pair.slave);

        let reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => return Err(fail_spawned_child(child, e.to_string())),
        };
        let writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => return Err(fail_spawned_child(child, e.to_string())),
        };
        let killer = child.clone_killer();
        let master = pair.master;
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);

        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        // See reader_loop's doc comment: closed only after this pane's
        // entry actually lands in the map below, so an ultra-fast child
        // (an `echo`-class process can exit before this async fn even
        // reaches its own `insert` call) can never win the race and remove
        // an entry that isn't there yet.
        let (registered_tx, registered_rx) = tokio::sync::oneshot::channel::<()>();
        // Ordering guarantee `pty:data`/`pty:exit` need but run on separate
        // tasks (batcher_task vs. reader_task) so nothing else provides for
        // free — see reader_loop's doc comment on `batcher_done`.
        let (batcher_done_tx, batcher_done_rx) = tokio::sync::oneshot::channel::<()>();

        let reader_task = tokio::task::spawn_blocking({
            let id = id.clone();
            let registry = self.inner.clone();
            move || {
                reader_loop(
                    reader,
                    tx,
                    child,
                    id,
                    seq,
                    registry,
                    registered_rx,
                    batcher_done_rx,
                    on_exit,
                )
            }
        });
        let batcher_task =
            tokio::spawn(batcher_loop(rx, id.clone(), channel, tap, batcher_done_tx));

        {
            let mut entries = self.entries();
            if let Some(old) = entries.remove(&id) {
                // A duplicate id raced ahead of a `pty:kill` — see this
                // fn's doc comment above. Tear the OLD process down
                // exactly like `Self::kill` does, off the async runtime
                // since `ChildKiller::kill` can block for a grace period.
                // Not awaited: the new pane below must not wait on the old
                // one's kill, and the old pane's own `reader_loop` (still
                // running since ITS spawn) reaps it independently once it
                // observes EOF, same as any other kill.
                let PaneHandle {
                    master,
                    writer,
                    killer,
                    ..
                } = old;
                tokio::task::spawn_blocking(move || {
                    let mut killer = killer;
                    let _ = killer.kill();
                    drop(master);
                    drop(writer);
                });
            }
            entries.insert(
                id,
                PaneHandle {
                    master,
                    writer,
                    killer,
                    seq,
                    _reader_task: reader_task,
                    _batcher_task: batcher_task,
                },
            );
        }
        let _ = registered_tx.send(());
        Ok(())
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Best-effort cleanup for the rare case a child spawned fine but a step
/// right after it (cloning the reader, taking the writer) failed — kills
/// and reaps it here, synchronously, so a fd-exhaustion-class failure
/// doesn't leave a zombie with nothing left holding a `Child` to reap it.
/// Blocking this async fn's own thread briefly for `.wait()` is an
/// accepted trade-off in this path specifically: it should not occur in
/// practice, and is not worth a `spawn_blocking` hop for.
fn fail_spawned_child(mut child: Box<dyn Child + Send + Sync>, msg: String) -> String {
    let _ = child.kill();
    let _ = child.wait();
    msg
}

/// Pure builder for `spawn_terminal`'s `CommandBuilder` — separated out so
/// the exact argv/cwd/env shape is unit-testable without spawning anything.
fn build_terminal_command(opts: &TerminalOpts) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(&opts.shell);
    cmd.arg("-l");
    cmd.cwd(&opts.cwd);
    // CommandBuilder::new() seeds itself from THIS PROCESS's own
    // environment (portable-pty's `get_base_env()`) — that must never
    // reach a pty child unfiltered (the TOME-007 least-privilege rule
    // `buildAgentBaseEnv`/`buildAgentEnv` enforce; neither of them this
    // slice's files, but whatever allowlisted env they hand this module
    // must land in the child exactly, not merged on top of Tome's own
    // process env). `env_clear()` wipes that inherited seed; every pair in
    // `opts.env` is then the ONLY thing the child ends up with.
    cmd.env_clear();
    for (k, v) in &opts.env {
        cmd.env(k, v);
    }
    cmd
}

/// Runs on a `spawn_blocking` thread for the pane's whole lifetime. See the
/// module doc comment's "one thread owns `Child`" section for the full
/// rationale; in short: blocks in `reader.read()` forwarding raw bytes to
/// the batcher until the pty session ends (for ANY reason), then is the
/// sole place that reaps the child and emits `pty:exit`.
///
/// `registered`: resolves once `spawn_raw` has actually inserted this
/// pane's `PaneHandle` into the registry. Waited on (blocking — this
/// thread is not async) AFTER the read loop ends but BEFORE touching the
/// registry, so this can never remove/miss an entry that `spawn_raw`
/// hasn't inserted yet (see `spawn_raw`'s doc comment on the same
/// channel).
///
/// `batcher_done`: resolves once `batcher_loop` has sent its own final
/// flush (if any) and returned. Waited on AFTER dropping `tx` (which is
/// what lets the batcher reach that point at all) and BEFORE sending
/// `pty:exit` — without this, `pty:exit` and the pane's last `pty:data`
/// race on two independent tasks with no inherent ordering, unlike the
/// Electron original where `flushPtyData(id)` and the `pty:exit` send sit
/// in the same synchronous callback (`p.onExit`) and JS's single-threaded
/// event loop settles the ordering for free. A fast-exiting process (for example
/// `printf hi`) makes this a real, not theoretical, race: reader_loop can
/// reach this point before the batcher's own `PTY_FLUSH_MS` timer has even
/// fired.
#[allow(clippy::too_many_arguments)]
fn reader_loop(
    mut reader: Box<dyn Read + Send>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    mut child: Box<dyn Child + Send + Sync>,
    id: String,
    seq: u64,
    registry: Arc<Mutex<HashMap<String, PaneHandle>>>,
    registered: tokio::sync::oneshot::Receiver<()>,
    batcher_done: tokio::sync::oneshot::Receiver<()>,
    on_exit: impl FnOnce(i64) + Send + 'static,
) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF — slave side has no more open references anywhere
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break; // batcher task already gone — nothing left to feed
                }
            }
            Err(_) => break,
        }
    }
    drop(tx); // lets the batcher's recv() return None and run its final flush
    let exit_code = child.wait().ok().map(|s| s.exit_code() as i64).unwrap_or(0);
    let _ = registered.blocking_recv();
    // Idempotent AND identity-checked: if `Registry::kill` already removed
    // this pane, or a duplicate-id spawn's own eviction (see `spawn_raw`'s
    // doc comment) already removed-and-replaced it with a NEWER one under
    // the same id, the map's current entry for `id` either doesn't exist
    // or carries a different `seq` — either way, leave it alone. Only
    // remove when the map still holds exactly the entry THIS spawn
    // inserted (the common case: a natural exit with nothing else racing
    // it) — never blindly whatever is CURRENTLY there.
    {
        let mut entries = registry.lock().expect("pty::Registry mutex poisoned");
        if entries.get(&id).map(|h| h.seq) == Some(seq) {
            entries.remove(&id);
        }
    }
    let _ = batcher_done.blocking_recv();
    on_exit(exit_code);
}

/// The 4ms/64KB batcher — `queuePtyData`/`flushPtyData` (index.js
/// lines 62-86), ported verbatim except for buffering raw bytes instead of
/// a decoded string (see the module doc comment's UTF-8 section). Runs as
/// a genuine async task (not `spawn_blocking`): the flush window is a
/// timer, which needs an executor to drive it.
///
/// Semantics pinned from the original: a flush window opens on the FIRST
/// byte of a new batch and is never extended by anything that arrives
/// before it closes (`if (!buf.timer) buf.timer = setTimeout(...)` — no
/// `clearTimeout`/reset anywhere) — bounded latency, not a debounce.
/// Crossing `PTY_FLUSH_BYTES` flushes immediately regardless of the timer.
///
/// `done`: fired unconditionally right before this fn returns (every path
/// out of the loop below falls through to it) — see `reader_loop`'s doc
/// comment on its `batcher_done` parameter for why `pty:exit` needs to
/// wait on it.
async fn batcher_loop(
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    id: String,
    channel: Channel<Value>,
    tap: Option<DataTap>,
    done: tokio::sync::oneshot::Sender<()>,
) {
    let mut buf: Vec<u8> = Vec::new();
    'outer: loop {
        // No timer pending: block for the first chunk of a new batch, or
        // for the reader ending (channel closed) between batches.
        let Some(chunk) = rx.recv().await else {
            break 'outer;
        };
        buf.extend_from_slice(&chunk);
        if buf.len() >= PTY_FLUSH_BYTES {
            flush_buf(&id, &channel, tap.as_ref(), &mut buf, false);
            continue 'outer;
        }
        // A flush window is now open, PTY_FLUSH_MS from THIS first chunk —
        // pinned so re-polling it below (once per further chunk) does not
        // restart it.
        let deadline = tokio::time::sleep(Duration::from_millis(PTY_FLUSH_MS));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => {
                    flush_buf(&id, &channel, tap.as_ref(), &mut buf, false);
                    continue 'outer;
                }
                next = rx.recv() => {
                    match next {
                        Some(c) => {
                            buf.extend_from_slice(&c);
                            if buf.len() >= PTY_FLUSH_BYTES {
                                flush_buf(&id, &channel, tap.as_ref(), &mut buf, false);
                                continue 'outer;
                            }
                            // still under threshold — loop back and keep
                            // waiting on the SAME deadline
                        }
                        None => {
                            // reader_loop ended: nothing left to wait for.
                            // Final flush — unlike every flush above, this
                            // one must not hold back an incomplete UTF-8
                            // tail forever, since there is no "next flush"
                            // coming (see flush_buf/drain_ready).
                            flush_buf(&id, &channel, tap.as_ref(), &mut buf, true);
                            break 'outer;
                        }
                    }
                }
            }
        }
    }
    let _ = done.send(());
}

/// Sends whatever [`drain_ready`] says is ready, if anything — mirrors
/// `flushPtyData`'s `if (buf.data) win?.webContents.send(...)` guard
/// (never emits an empty `pty:data`). `tap` (when present) sees the exact
/// same decoded chunk, mirroring `conductor.js`'s `p.onData = data => {
/// conductor.record(id, data); queuePtyData(id, data) }` — one tap call per
/// `pty:data` send, same text.
fn flush_buf(
    id: &str,
    channel: &Channel<Value>,
    tap: Option<&DataTap>,
    buf: &mut Vec<u8>,
    is_final: bool,
) {
    if let Some(data) = drain_ready(buf, is_final) {
        if let Some(tap) = tap {
            tap(&data);
        }
        let _ = channel.send(json!({"id": id, "data": data}));
    }
}

/// Pure core of a flush: splits `buf` at the UTF-8-safe boundary (see
/// [`incomplete_utf8_tail_len`]), lossily decodes the ready prefix, removes
/// those bytes from `buf` (leaving any incomplete trailing sequence in
/// place for the next call to extend), and returns the decoded text — or
/// `None` if nothing was ready to send (an empty buffer, or a buffer that
/// so far is entirely an incomplete trailing sequence).
///
/// `is_final` (the pty has closed — there is no "next call" that could
/// complete a truncated sequence) skips the hold-back entirely: the whole
/// buffer is flushed, with a genuinely truncated tail replaced by U+FFFD
/// rather than silently dropped.
fn drain_ready(buf: &mut Vec<u8>, is_final: bool) -> Option<String> {
    let tail = if is_final {
        0
    } else {
        incomplete_utf8_tail_len(buf)
    };
    let ready_len = buf.len() - tail;
    if ready_len == 0 {
        return None;
    }
    let data = String::from_utf8_lossy(&buf[..ready_len]).into_owned();
    buf.drain(..ready_len);
    Some(data)
}

/// Number of trailing bytes of `buf` (0..=3) that form the start of a
/// multi-byte UTF-8 sequence which is not yet complete — bytes that must
/// be held back and prepended to the next read rather than decoded now.
/// Bytes before that point are always safe to decode immediately: a
/// genuinely invalid sequence (as opposed to merely incomplete) can never
/// become valid no matter how many more bytes arrive, so it is left for
/// the caller's lossy decode rather than held back forever — only a
/// truncated sequence that is STILL the very tail of the buffer, with
/// nothing invalid or complete following it, is ever held back.
fn incomplete_utf8_tail_len(buf: &[u8]) -> usize {
    let len = buf.len();
    let mut lead_at = len;
    let mut continuations = 0;
    // Walk back over continuation bytes (0b10xxxxxx), at most 3 — the
    // longest UTF-8 sequence is 4 bytes (1 lead + 3 continuations).
    while lead_at > 0 && continuations < 3 && buf[lead_at - 1] & 0b1100_0000 == 0b1000_0000 {
        lead_at -= 1;
        continuations += 1;
    }
    if lead_at == 0 {
        // The whole (short) buffer is continuation bytes with no lead byte
        // in sight — nothing to identify as "incomplete" (there is no lead
        // byte to complete); hold back what little there is rather than
        // guess. In practice this can only be genuinely invalid input (any
        // lead byte a real hold-back left behind is still present at the
        // front — see drain_ready), so held-back bytes here get flushed as
        // soon as a later call sees something after them that isn't itself
        // a continuation byte (proven in this fn's tests).
        return continuations;
    }
    let lead = buf[lead_at - 1];
    let seq_len = if lead & 0b1000_0000 == 0 {
        1
    } else if lead & 0b1110_0000 == 0b1100_0000 {
        2
    } else if lead & 0b1111_0000 == 0b1110_0000 {
        3
    } else if lead & 0b1111_1000 == 0b1111_0000 {
        4
    } else {
        // Not a valid lead byte at all (a stray continuation byte with no
        // lead ahead of it, or a byte pattern UTF-8 never uses) —
        // genuinely invalid, not incomplete; nothing to hold back for it.
        return 0;
    };
    let have = len - (lead_at - 1);
    if have < seq_len {
        have
    } else {
        0 // sequence is already complete — nothing to hold back
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tauri::ipc::InvokeResponseBody;

    // ================= incomplete_utf8_tail_len =================

    #[test]
    fn ascii_never_holds_anything_back() {
        assert_eq!(incomplete_utf8_tail_len(b""), 0);
        assert_eq!(incomplete_utf8_tail_len(b"hello"), 0);
    }

    #[test]
    fn complete_multibyte_sequences_hold_nothing_back() {
        assert_eq!(incomplete_utf8_tail_len("é".as_bytes()), 0); // 2-byte
        assert_eq!(incomplete_utf8_tail_len("€".as_bytes()), 0); // 3-byte
        assert_eq!(incomplete_utf8_tail_len("😀".as_bytes()), 0); // 4-byte
        assert_eq!(incomplete_utf8_tail_len("hi€".as_bytes()), 0);
    }

    #[test]
    fn two_byte_sequence_split_after_the_lead_byte_holds_back_one() {
        let bytes = "é".as_bytes(); // [0xC3, 0xA9]
        assert_eq!(incomplete_utf8_tail_len(&bytes[..1]), 1);
    }

    #[test]
    fn three_byte_sequence_splits_hold_back_correctly() {
        let bytes = "€".as_bytes(); // [0xE2, 0x82, 0xAC]
        assert_eq!(incomplete_utf8_tail_len(&bytes[..1]), 1);
        assert_eq!(incomplete_utf8_tail_len(&bytes[..2]), 2);
    }

    #[test]
    fn four_byte_sequence_splits_hold_back_correctly() {
        let bytes = "😀".as_bytes(); // [0xF0, 0x9F, 0x98, 0x80]
        assert_eq!(incomplete_utf8_tail_len(&bytes[..1]), 1);
        assert_eq!(incomplete_utf8_tail_len(&bytes[..2]), 2);
        assert_eq!(incomplete_utf8_tail_len(&bytes[..3]), 3);
    }

    #[test]
    fn a_preceding_complete_char_does_not_confuse_the_trailing_split() {
        // "hi" + the first byte of "é" only.
        let mut v = b"hi".to_vec();
        v.push("é".as_bytes()[0]);
        assert_eq!(incomplete_utf8_tail_len(&v), 1);
    }

    #[test]
    fn a_lone_invalid_byte_is_not_held_back() {
        // 0xFF is not a valid UTF-8 lead byte under any interpretation.
        assert_eq!(incomplete_utf8_tail_len(&[0xFF]), 0);
        assert_eq!(incomplete_utf8_tail_len(&[b'h', b'i', 0xFF]), 0);
    }

    #[test]
    fn a_short_all_continuation_buffer_with_no_lead_byte_is_conservatively_held() {
        // Exactly 3 bytes, all continuation-shaped, nothing before them —
        // there is no lead byte within reach to say whether this is
        // "incomplete" or just broken, so this is held back conservatively
        // rather than guessed at (the `lead_at == 0` fallback).
        assert_eq!(incomplete_utf8_tail_len(&[0x80, 0x80, 0x80]), 3);
    }

    #[test]
    fn a_longer_run_of_stray_continuation_bytes_flushes_immediately() {
        // Once the walk-back's continuation cap (3) is hit, the byte it
        // lands on (itself another continuation byte, not a real lead) is
        // recognized as genuinely invalid rather than "incomplete" —
        // proving a `cat` of binary garbage can't stall output forever
        // waiting for bytes that no lead byte is promising are coming.
        let all_continuation = [0x80u8; 10];
        assert_eq!(incomplete_utf8_tail_len(&all_continuation), 0);
        // And the same holds once something un-continuation-shaped follows.
        let mut v = all_continuation.to_vec();
        v.push(b'h');
        assert_eq!(incomplete_utf8_tail_len(&v), 0);
    }

    // ================= drain_ready =================

    #[test]
    fn drain_ready_returns_none_for_empty_buffer() {
        let mut buf = Vec::new();
        assert_eq!(drain_ready(&mut buf, false), None);
    }

    #[test]
    fn drain_ready_flushes_all_of_a_fully_valid_buffer() {
        let mut buf = b"hello".to_vec();
        assert_eq!(drain_ready(&mut buf, false), Some("hello".to_string()));
        assert!(buf.is_empty());
    }

    #[test]
    fn drain_ready_holds_back_an_incomplete_tail_across_calls() {
        let full = "hi😀".as_bytes();
        // Simulate a read that ends mid-emoji: "hi" + first 2 bytes of 😀.
        let mut buf = full[..4].to_vec();
        let out = drain_ready(&mut buf, false);
        assert_eq!(out, Some("hi".to_string()));
        assert_eq!(buf, &full[2..4]); // the 2 held-back lead bytes remain

        // Next read supplies the rest of the sequence.
        buf.extend_from_slice(&full[4..]);
        let out2 = drain_ready(&mut buf, false);
        assert_eq!(out2, Some("😀".to_string()));
        assert!(buf.is_empty());
    }

    #[test]
    fn drain_ready_never_emits_an_empty_string_while_a_tail_is_incomplete() {
        let mut buf = "é".as_bytes()[..1].to_vec(); // just the lead byte
        assert_eq!(drain_ready(&mut buf, false), None);
        assert_eq!(buf.len(), 1); // still held, not silently dropped
    }

    #[test]
    fn drain_ready_final_flush_forces_out_a_truncated_tail_lossily() {
        let mut buf = "é".as_bytes()[..1].to_vec(); // incomplete forever now
        let out = drain_ready(&mut buf, true).unwrap();
        assert!(
            out.contains('\u{FFFD}'),
            "expected a replacement character, got {out:?}"
        );
        assert!(buf.is_empty());
    }

    // ================= build_terminal_command =================

    fn opts(env: Vec<(&str, &str)>) -> TerminalOpts {
        TerminalOpts {
            id: "pane-1".to_string(),
            shell: "/bin/sh".to_string(),
            cwd: PathBuf::from("/tmp"),
            env: env
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn terminal_command_is_a_bare_login_shell() {
        let cmd = build_terminal_command(&opts(vec![]));
        let argv: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv, vec!["/bin/sh".to_string(), "-l".to_string()]);
    }

    #[test]
    fn terminal_command_uses_the_given_cwd() {
        let cmd = build_terminal_command(&opts(vec![]));
        assert_eq!(cmd.get_cwd().unwrap().to_string_lossy(), "/tmp");
    }

    #[test]
    fn terminal_command_env_is_exactly_what_was_given_not_merged_with_this_process() {
        // Seed a known value into THIS process's env first, so the leak-check
        // below stays meaningful even on a minimal CI container (for example fedora)
        // that sets neither USER nor LOGNAME.
        std::env::set_var("USER", "tome-test-user");
        let cmd = build_terminal_command(&opts(vec![("PATH", "/usr/bin"), ("HOME", "/home/x")]));
        assert_eq!(cmd.get_env("PATH").unwrap().to_string_lossy(), "/usr/bin");
        assert_eq!(cmd.get_env("HOME").unwrap().to_string_lossy(), "/home/x");
        // TOME-007 property: something that is almost certainly set in
        // THIS test process's real environment, but was not in opts.env,
        // must not leak into the child's env. CommandBuilder::new() seeds
        // itself from this process's env unconditionally — this is exactly
        // what env_clear() must undo.
        assert!(
            std::env::var("USER").is_ok() || std::env::var("LOGNAME").is_ok(),
            "test precondition: expected USER or LOGNAME to be set in the test process"
        );
        assert!(cmd.get_env("USER").is_none() || std::env::var("USER").is_err());
        assert!(cmd.get_env("LOGNAME").is_none() || std::env::var("LOGNAME").is_err());
    }

    // ================= Registry: unknown-id no-ops =================

    #[tokio::test]
    async fn write_resize_kill_on_an_unknown_id_are_safe_no_ops() {
        let reg = Registry::new();
        assert!(!reg.write("nope", "hi"));
        assert!(!reg.resize("nope", 10, 10));
        assert!(!reg.kill("nope").await);
    }

    // ================= real PTY integration =================

    fn recording_channel() -> (Channel<Value>, mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = mpsc::unbounded_channel::<Value>();
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Json(s) = body {
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    let _ = tx.send(v);
                }
            }
            Ok(())
        });
        (channel, rx)
    }

    async fn recv_within(rx: &mut mpsc::UnboundedReceiver<Value>, secs: u64) -> Value {
        tokio::time::timeout(Duration::from_secs(secs), rx.recv())
            .await
            .expect("timed out waiting for a pty message")
            .expect("channel closed with no message")
    }

    /// `on_exit` sink for tests: `pty:exit` no longer travels over the
    /// `Channel` at all — production sends it via `app.emit`, through the
    /// closure `ipc::pty::pty_create` passes as `on_exit` (see
    /// `spawn_raw`'s doc comment for why a plain callback rather than a
    /// real `AppHandle`, which these tests have no way to construct).
    /// `FnOnce` fires exactly once, so a `oneshot` channel is the natural
    /// receiver.
    fn recording_exit() -> (
        impl FnOnce(i64) + Send + 'static,
        tokio::sync::oneshot::Receiver<i64>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel::<i64>();
        (
            move |code| {
                let _ = tx.send(code);
            },
            rx,
        )
    }

    async fn recv_exit(rx: tokio::sync::oneshot::Receiver<i64>, secs: u64) -> i64 {
        tokio::time::timeout(Duration::from_secs(secs), rx)
            .await
            .expect("timed out waiting for on_exit")
            .expect("on_exit sender dropped without firing")
    }

    fn sh_command(script: &str) -> CommandBuilder {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(script);
        cmd.env_clear();
        cmd.env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/bin:/usr/bin".to_string()),
        );
        cmd
    }

    fn size80x24() -> PtySize {
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// The required real-PTY integration test: spawn `sh -c 'printf hi'`,
    /// collect its output through the real reader/batcher path, and assert
    /// the exit event fires.
    #[tokio::test]
    async fn spawns_a_real_process_streams_its_output_and_reports_exit() {
        let reg = Registry::new();
        let (channel, mut rx) = recording_channel();
        let (on_exit, exit_rx) = recording_exit();
        reg.spawn_raw(
            "t1".to_string(),
            sh_command("printf hi"),
            size80x24(),
            channel,
            None,
            on_exit,
        )
        .await
        .expect("spawn_raw failed");

        let data_msg = recv_within(&mut rx, 5).await;
        assert_eq!(data_msg["id"], "t1");
        assert_eq!(data_msg["data"], "hi");

        // pty:exit no longer travels over the data Channel — it fires
        // through on_exit instead (production: app.emit("pty:exit", ...),
        // matching the renderer's separate onData/onExit wiring).
        let exit_code = recv_exit(exit_rx, 5).await;
        assert_eq!(exit_code, 0);

        // reader_loop's own cleanup must have run by the time it called
        // on_exit (registry removal happens before on_exit — see its
        // body), so the pane must already be gone.
        assert!(!reg.contains("t1"));
    }

    /// The scrollback tap (`ipc::pty::pty_create` installs one calling
    /// `Conductor::record`) sees exactly the same decoded chunks the
    /// `pty:data` Channel does — the feed `read_terminal` reads back.
    #[tokio::test]
    async fn the_data_tap_receives_the_same_output_the_channel_does() {
        let reg = Registry::new();
        let (channel, mut rx) = recording_channel();
        let (on_exit, exit_rx) = recording_exit();
        let seen = Arc::new(Mutex::new(String::new()));
        let tap: DataTap = {
            let seen = seen.clone();
            Arc::new(move |data: &str| seen.lock().unwrap().push_str(data))
        };
        reg.spawn_raw(
            "tap1".to_string(),
            sh_command("printf hi"),
            size80x24(),
            channel,
            Some(tap),
            on_exit,
        )
        .await
        .expect("spawn_raw failed");

        let data_msg = recv_within(&mut rx, 5).await;
        assert_eq!(data_msg["data"], "hi");
        let _ = recv_exit(exit_rx, 5).await;
        assert_eq!(
            *seen.lock().unwrap(),
            "hi",
            "tap must observe the same bytes the channel did"
        );
    }

    #[tokio::test]
    async fn nonzero_exit_code_is_reported() {
        let reg = Registry::new();
        let (channel, _rx) = recording_channel();
        let (on_exit, exit_rx) = recording_exit();
        reg.spawn_raw(
            "t2".to_string(),
            sh_command("exit 7"),
            size80x24(),
            channel,
            None,
            on_exit,
        )
        .await
        .expect("spawn_raw failed");

        let exit_code = recv_exit(exit_rx, 5).await;
        assert_eq!(exit_code, 7);
    }

    #[tokio::test]
    async fn write_delivers_bytes_to_the_child() {
        let reg = Registry::new();
        let (channel, mut rx) = recording_channel();
        let (on_exit, _exit_rx) = recording_exit();
        let mut cat = CommandBuilder::new("/bin/cat");
        cat.env_clear();
        reg.spawn_raw("t3".to_string(), cat, size80x24(), channel, None, on_exit)
            .await
            .expect("spawn_raw failed");

        assert!(reg.write("t3", "hello\n"));

        // Look at a handful of messages (pty echo may split "hello" from
        // cat's own copy across more than one flush) for the substring
        // rather than demanding an exact first message.
        let mut seen = String::new();
        for _ in 0..20 {
            let msg = recv_within(&mut rx, 5).await;
            if let Some(d) = msg.get("data").and_then(|d| d.as_str()) {
                seen.push_str(d);
            }
            if seen.contains("hello") {
                break;
            }
        }
        assert!(
            seen.contains("hello"),
            "expected to see written data echoed back, got {seen:?}"
        );

        assert!(reg.kill("t3").await);
    }

    #[tokio::test]
    async fn resize_changes_the_ptys_reported_size() {
        let reg = Registry::new();
        let (channel, _rx) = recording_channel();
        let (on_exit, _exit_rx) = recording_exit();
        reg.spawn_raw(
            "t4".to_string(),
            sh_command("sleep 5"),
            size80x24(),
            channel,
            None,
            on_exit,
        )
        .await
        .expect("spawn_raw failed");

        assert_eq!(reg.size_of("t4"), Some((80, 24)));
        assert!(reg.resize("t4", 120, 40));
        assert_eq!(reg.size_of("t4"), Some((120, 40)));

        assert!(reg.kill("t4").await);
    }

    #[tokio::test]
    async fn kill_ends_a_long_running_pane_well_before_it_would_exit_on_its_own() {
        let reg = Registry::new();
        let (channel, mut rx) = recording_channel();
        let (on_exit, exit_rx) = recording_exit();
        reg.spawn_raw(
            "t5".to_string(),
            sh_command("sleep 30"),
            size80x24(),
            channel,
            None,
            on_exit,
        )
        .await
        .expect("spawn_raw failed");

        let started = Instant::now();
        assert!(reg.kill("t5").await);
        let _exit_code = recv_exit(exit_rx, 10).await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "kill should end the pane in well under its 30s sleep, took {:?}",
            started.elapsed()
        );

        // A second kill on an already-gone pane is a safe no-op, not a
        // hang or a panic. `on_exit` is an `FnOnce` fired exactly once by
        // the one `reader_loop` this spawn started, so a second `pty:exit`
        // for this pane isn't even expressible, let alone something to
        // assert against here — drain any trailing `pty:data` noise a
        // killed shell's job control can still print instead.
        assert!(!reg.kill("t5").await);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        while let Ok(Some(_msg)) = tokio::time::timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            rx.recv(),
        )
        .await
        {
            // just draining trailing pty:data
        }
    }

    /// The duplicate-id case `spawn_raw`'s doc comment describes: a second
    /// `pty:create` for an id that is still live, with no `pty:kill` in
    /// between (this app's own threat model is a renderer that cannot be
    /// trusted to always pair the two). Must not orphan the first
    /// process, and must not let the first pane's own (later) cleanup
    /// evict the second, live one out of the registry.
    #[tokio::test]
    async fn a_duplicate_id_kills_the_old_process_instead_of_orphaning_it() {
        let reg = Registry::new();
        let (channel_a, _rx_a) = recording_channel();
        let (on_exit_a, exit_rx_a) = recording_exit();
        reg.spawn_raw(
            "dup".to_string(),
            sh_command("sleep 30"),
            size80x24(),
            channel_a,
            None,
            on_exit_a,
        )
        .await
        .expect("first spawn_raw failed");
        assert!(reg.contains("dup"));

        let (channel_b, _rx_b) = recording_channel();
        let (on_exit_b, exit_rx_b) = recording_exit();
        reg.spawn_raw(
            "dup".to_string(),
            sh_command("sleep 30"),
            size80x24(),
            channel_b,
            None,
            on_exit_b,
        )
        .await
        .expect("second spawn_raw failed");

        // The OLD process must actually have been killed, not orphaned —
        // proven by its own on_exit firing well before its 30s sleep would
        // ever end on its own.
        let started = Instant::now();
        let _ = recv_exit(exit_rx_a, 10).await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the superseded old process should have been killed promptly, took {:?}",
            started.elapsed()
        );

        // The registry slot must now belong to the NEW pane, fully live
        // and controllable — proving the old pane's own cleanup (which
        // runs after the on_exit awaited above) did not evict the new
        // entry out from under it once it caught up (the ABA race
        // `PaneHandle::seq` closes).
        assert!(reg.contains("dup"));
        assert_eq!(reg.size_of("dup"), Some((80, 24)));
        assert!(reg.kill("dup").await);
        let _ = recv_exit(exit_rx_b, 10).await;
        assert!(!reg.contains("dup"));
    }

    // ================= batcher: coalescing / size-triggered flush =================

    /// Spawns `batcher_loop` with a throwaway `done` signal — these tests
    /// synchronize on `task.await` (the task ending) directly, not on the
    /// ordering `done` exists for (that's `reader_loop`'s concern, covered
    /// by the real-PTY tests above).
    fn spawn_batcher(
        rx: mpsc::UnboundedReceiver<Vec<u8>>,
        id: &str,
        channel: Channel<Value>,
    ) -> JoinHandle<()> {
        let (done_tx, _done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(batcher_loop(rx, id.to_string(), channel, None, done_tx))
    }

    #[tokio::test]
    async fn small_chunks_arriving_together_coalesce_into_one_flush() {
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (channel, mut out) = recording_channel();
        let task = spawn_batcher(rx, "b1", channel);

        tx.send(b"hel".to_vec()).unwrap();
        tx.send(b"lo".to_vec()).unwrap();

        let msg = recv_within(&mut out, 2).await;
        assert_eq!(
            msg["data"], "hello",
            "both chunks should coalesce into a single flush"
        );

        // Nothing else should follow immediately — proves it was one flush,
        // not two.
        assert!(tokio::time::timeout(Duration::from_millis(50), out.recv())
            .await
            .is_err());

        drop(tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn crossing_the_byte_threshold_flushes_without_waiting_for_more_input() {
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (channel, mut out) = recording_channel();
        let task = spawn_batcher(rx, "b2", channel);

        let big = vec![b'x'; PTY_FLUSH_BYTES];
        tx.send(big.clone()).unwrap();

        // Should already be flushed well before the 4ms timer would have
        // fired on its own — no second chunk needed to trigger it.
        let msg = recv_within(&mut out, 2).await;
        assert_eq!(msg["data"].as_str().unwrap().len(), PTY_FLUSH_BYTES);

        drop(tx);
        let _ = task.await;
    }

    #[tokio::test]
    async fn final_flush_on_reader_close_delivers_a_pending_incomplete_tail() {
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (channel, mut out) = recording_channel();
        let task = spawn_batcher(rx, "b3", channel);

        // Only the lead byte of "é" — would normally be held back forever.
        tx.send("é".as_bytes()[..1].to_vec()).unwrap();
        drop(tx); // reader "closing" with an incomplete tail still buffered

        let msg = recv_within(&mut out, 2).await;
        assert!(msg["data"].as_str().unwrap().contains('\u{FFFD}'));

        let _ = task.await;
    }
}
