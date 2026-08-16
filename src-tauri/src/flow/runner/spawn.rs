//! The injected-spawn seam `flow::runner` drives every headless node
//! through, plus the one production backend that actually spawns an OS
//! process. Split out from `runner/mod.rs` so the scheduling logic never
//! has to know whether it is running a real `claude -p` or a test double —
//! mirrors `flow-runner.js`'s own `let spawn = childSpawn` module-level
//! injection point (`init({ spawn })`), swapped for a real `Fn` value here
//! since Rust has no bare module-level `let` to reassign.
//!
//! `#[cfg(test)]` fakes in `runner/mod.rs`'s own test module build
//! [`Spawned`] values with no real process behind them at all (mirroring
//! `flow-runner.test.js`'s `fakeChild()`) for the SIGTERM/SIGKILL
//! escalation-timing tests; every other ported test injects [`spawn_process`]
//! itself (or a thin wrapper over it) so process-group kill, closed stdin,
//! and real log capture are exercised against a REAL `/bin/sh` process —
//! never a real agent CLI.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::AsyncRead;
use tokio::process::Command;

/// What the runner asks the spawn backend to start. Fixed choices the JS
/// original also hardcodes at its one call site — `stdio: ['ignore', 'pipe',
/// 'pipe']`, `detached: true` — are NOT part of this request: they are
/// [`spawn_process`]'s own unconditional behaviour (see its doc comment),
/// not something any caller should vary.
pub struct SpawnRequest {
    pub cmd: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

/// A process's exit, in the two shapes `run.nodes[].exit`/the log's trailer
/// line need to distinguish: a normal exit code, or death by signal (no
/// code at all) — mirrors Node's `child.on('close', (code, signal) => ...)`.
pub struct ExitOutcome {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// A live spawned node, abstracted just enough for the runner to drive it
/// without knowing whether it's real. `stdout`/`stderr` are `None` for a
/// fake double with nothing to pipe into the node's log (mirrors
/// `fakeChild()` having no stream properties at all — JS's own
/// `child.stdout?.pipe(...)` already tolerates that).
pub struct Spawned {
    pub pid: i32,
    pub stdout: Option<Box<dyn AsyncRead + Unpin + Send>>,
    pub stderr: Option<Box<dyn AsyncRead + Unpin + Send>>,
    /// Signals THIS pid only (never the process group — that's
    /// `signal_node`'s own `signal_pid(-pid, ..)` call in `runner/mod.rs`,
    /// tried first). Mirrors `child.kill(sig)`, the JS original's fallback
    /// when `process.kill(-pid, sig)` throws.
    pub kill: Arc<dyn Fn(i32) -> io::Result<()> + Send + Sync>,
    /// Resolves once the process has exited. A production `Spawned` wires
    /// this to a background task awaiting the real `Child`; a test double's
    /// sender is held by the test, fired manually (mirrors
    /// `fakeChild().fire('close', code, signal)`).
    pub exit: tokio::sync::oneshot::Receiver<ExitOutcome>,
}

/// Rust's `Command::spawn()` fails SYNCHRONOUSLY for a missing binary
/// (ENOENT surfaces immediately as an `Err`, unlike Node's `child_process.spawn`,
/// which always returns a live-looking `ChildProcess` and defers the same
/// failure to an async `'error'` event). This collapses the JS original's
/// two separate failure paths — `launch()`'s own `try { spawn(...) } catch`
/// AND `child.on('error', ...)` — into the one Rust spawn genuinely has;
/// `runner/mod.rs`'s `launch` treats `Failed` exactly like JS treats either.
pub enum SpawnOutcome {
    Started(Spawned),
    Failed(io::Error),
}

// ---- raw signal(2) — no new Cargo dependency ----
//
// `nix`/`libc` are already transitively resolved (this workspace's other
// dependencies pull them in — see Cargo.lock), but this slice does not own
// `Cargo.toml` (out of scope per this task's file ownership), so this is a
// direct, minimal FFI declaration instead of a new direct dependency on
// either crate. `kill(2)` with a NEGATIVE pid addresses the whole process
// GROUP — the exact `process.kill(-pid, sig)` semantics `signal_node` in
// `runner/mod.rs` needs. Unix-only, matching this app's two shipping
// targets (macOS + Linux) — see the rewrite plan's locked decisions.
#[cfg(unix)]
mod sys {
    use std::io;

    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    pub const SIGTERM: i32 = 15;
    pub const SIGKILL: i32 = 9;

    /// `pid` negative addresses the process GROUP; positive addresses one
    /// process — the same single primitive Node's `process.kill` wraps.
    pub fn signal_pid(pid: i32, sig: i32) -> io::Result<()> {
        let ret = unsafe { kill(pid, sig) };
        if ret == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
#[cfg(unix)]
pub use sys::{signal_pid, SIGKILL, SIGTERM};

/// Production spawn backend: a real OS process, made its own process-group
/// LEADER (`.process_group(0)`, the same `setpgid`-on-exec Node's
/// `detached: true` performs) so `signal_node`'s process-group signal
/// reaches every grandchild the node's own CLI spawns, not just the CLI
/// itself. Stdin is `/dev/null` — a one-shot headless CLI has nothing to
/// say on stdin, and a default inherited/piped stdin nobody writes to or
/// ends is exactly what makes `claude -p <prompt>` hang forever reading a
/// pipe as extra context. `argv` reaches `execvp` untouched (no shell
/// anywhere), which is what makes a composed brief safe as a single
/// element regardless of its content.
pub fn spawn_process(req: SpawnRequest) -> SpawnOutcome {
    let mut cmd = Command::new(&req.cmd);
    cmd.args(&req.args)
        .current_dir(&req.cwd)
        .env_clear()
        .envs(req.env.iter().cloned())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return SpawnOutcome::Failed(e),
    };
    let pid = child.id().unwrap_or(0) as i32;
    let stdout = child
        .stdout
        .take()
        .map(|s| Box::new(s) as Box<dyn AsyncRead + Unpin + Send>);
    let stderr = child
        .stderr
        .take()
        .map(|s| Box::new(s) as Box<dyn AsyncRead + Unpin + Send>);

    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let outcome = match child.wait().await {
            Ok(status) => exit_outcome_of(status),
            Err(_) => ExitOutcome {
                code: None,
                signal: None,
            },
        };
        let _ = tx.send(outcome);
    });

    let kill: Arc<dyn Fn(i32) -> io::Result<()> + Send + Sync> =
        Arc::new(move |sig| signal_pid(pid, sig));
    SpawnOutcome::Started(Spawned {
        pid,
        stdout,
        stderr,
        kill,
        exit: rx,
    })
}

#[cfg(unix)]
fn exit_outcome_of(status: std::process::ExitStatus) -> ExitOutcome {
    use std::os::unix::process::ExitStatusExt;
    ExitOutcome {
        code: status.code(),
        signal: status.signal(),
    }
}
#[cfg(not(unix))]
fn exit_outcome_of(status: std::process::ExitStatus) -> ExitOutcome {
    ExitOutcome {
        code: status.code(),
        signal: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_process_reports_enoent_for_a_missing_binary() {
        let outcome = spawn_process(SpawnRequest {
            cmd: "/definitely/not/a/real/binary-xyz".to_string(),
            args: vec![],
            cwd: std::env::temp_dir(),
            env: vec![],
        });
        match outcome {
            SpawnOutcome::Failed(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            SpawnOutcome::Started(_) => panic!("expected Failed for a nonexistent binary"),
        }
    }

    #[tokio::test]
    async fn spawn_process_never_hangs_the_child_on_stdin() {
        // `cat` with no args echoes stdin until EOF. If stdin were a pipe
        // nobody writes to or ends, this would hang past the timeout;
        // /dev/null gives it an immediate EOF.
        let outcome = spawn_process(SpawnRequest {
            cmd: "/bin/cat".to_string(),
            args: vec![],
            cwd: std::env::temp_dir(),
            env: vec![],
        });
        let SpawnOutcome::Started(spawned) = outcome else {
            panic!("expected Started")
        };
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), spawned.exit)
            .await
            .expect("cat must exit promptly once stdin is closed")
            .expect("exit sender must not be dropped");
        assert_eq!(exit.code, Some(0));
    }

    #[tokio::test]
    async fn spawn_process_leads_its_own_process_group_so_a_grandchild_can_be_reached() {
        // Spawn a shell that backgrounds a ticking loop (the "grandchild"
        // an agent CLI's own tool calls would be) and then sleeps. Killing
        // the NEGATIVE pid (the process group) must take the grandchild
        // down too — the whole reason .process_group(0) matters.
        let ticks =
            std::env::temp_dir().join(format!("tome-spawn-pg-test-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&ticks);
        let script = format!(
            "sh -c 'while :; do echo t >> {p}; sleep 0.02; done' & exec sleep 30",
            p = ticks.display()
        );
        let outcome = spawn_process(SpawnRequest {
            cmd: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script],
            cwd: std::env::temp_dir(),
            env: vec![],
        });
        let SpawnOutcome::Started(spawned) = outcome else {
            panic!("expected Started")
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::fs::read_to_string(&ticks)
            .unwrap_or_default()
            .is_empty()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "grandchild never started ticking"
            );
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }

        signal_pid(-spawned.pid, SIGKILL).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), spawned.exit).await;

        let after_kill = std::fs::read_to_string(&ticks).unwrap_or_default().len();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let settled = std::fs::read_to_string(&ticks).unwrap_or_default().len();
        // A tiny scheduling slack is fine; a grandchild that outlived the
        // kill would keep appending far more than that.
        assert!(
            settled - after_kill < 10,
            "grandchild kept ticking after the process group was killed"
        );
        let _ = std::fs::remove_file(&ticks);
    }

    #[test]
    fn signal_pid_on_a_pid_that_does_not_exist_returns_an_error_rather_than_panicking() {
        // The exact fallback trigger signal_node relies on: a bogus
        // process-group pid must fail cleanly (ESRCH), not panic.
        assert!(signal_pid(-0x40000000, SIGTERM).is_err());
    }
}
