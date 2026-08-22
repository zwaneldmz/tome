//! The P5.3 escape-attempt list. Every attempt is an assert-it-fails test
//! against a REAL contained pane — macOS `sandbox-exec` under the
//! production seatbelt profile, Linux `bwrap` + `tome-shim` under the
//! production argv — mapping to a THREATMODEL line, and printing PASS/FAIL
//! with the mechanism observed. A FAIL here means the thing escaped.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};
use tokio::process::Child;

use tome_flow::egress::allowlist::{
    compile_allowlist, is_allowed, parse_repo_allowlist, validate_repo_allowlist, DEFAULT_ALLOW,
};
use tome_flow::egress::proxy::{BlockedEvent, PaneProxy};
use tome_flow::egress::seatbelt::seatbelt_profile;

use crate::sandbox::{
    build_linux_fixture, build_mac_fixture, linux_preflight, mac_preflight, mac_run_profile,
    one_line, LinuxFixture, MacFixture, RunOutput,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    Skip,
    /// Asserted by named in-crate unit tests rather than by this harness
    /// (the seam lives in `pub(crate)` ipc code a separate crate cannot
    /// reach) — runs in the same CI gates via `cargo test`.
    Delegated,
}

pub struct Attempt {
    pub name: &'static str,
    pub threat: &'static str,
    pub outcome: Outcome,
    pub detail: Vec<String>,
}

impl Attempt {
    fn new(
        name: &'static str,
        threat: &'static str,
        outcome: Outcome,
        detail: Vec<String>,
    ) -> Self {
        Self {
            name,
            threat,
            outcome,
            detail,
        }
    }
    fn pass(name: &'static str, threat: &'static str, detail: Vec<String>) -> Self {
        Self::new(name, threat, Outcome::Pass, detail)
    }
    fn fail(name: &'static str, threat: &'static str, detail: Vec<String>) -> Self {
        Self::new(name, threat, Outcome::Fail, detail)
    }
    fn skip(name: &'static str, threat: &'static str, why: String) -> Self {
        Self::new(name, threat, Outcome::Skip, vec![why])
    }
}

fn finish(name: &'static str, threat: &'static str, ok: bool, detail: Vec<String>) -> Attempt {
    if ok {
        Attempt::pass(name, threat, detail)
    } else {
        Attempt::fail(name, threat, detail)
    }
}

/// Skip-early helpers: preflight runs once, its reason is carried into
/// the skip detail verbatim (a failed preflight must never panic).
fn require_mac(name: &'static str, threat: &'static str) -> Result<(), Attempt> {
    match mac_preflight() {
        Some(reason) => Err(Attempt::skip(name, threat, reason)),
        None => Ok(()),
    }
}

fn require_linux(name: &'static str, threat: &'static str) -> Result<(), Attempt> {
    match linux_preflight() {
        Some(reason) => Err(Attempt::skip(name, threat, reason)),
        None => Ok(()),
    }
}

// ---- shared probe fragments -------------------------------------------------

fn curl_direct(url: &str, max_time: &str) -> Vec<String> {
    [
        "/usr/bin/curl",
        "-sS",
        "--noproxy",
        "*",
        "--max-time",
        max_time,
        "-o",
        "/dev/null",
        "-w",
        "%{size_download}",
        url,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn curl_unix_socket(sock: &Path) -> Vec<String> {
    [
        "/usr/bin/curl",
        "-sS",
        "--max-time",
        "5",
        "--unix-socket",
        &sock.display().to_string(),
        "http://localhost/",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn ssh_probe(target: &str) -> Vec<String> {
    [
        "/usr/bin/ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=6",
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/dev/null",
        "-o",
        "LogLevel=ERROR",
        &format!("escape-user@{target}"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// `for f in "$@"; do cat ... || echo DENIED; done` — POSIX-sh, works on
/// bash (macOS /bin/sh) and dash (Ubuntu /bin/sh).
fn read_probe_inner(files: &[PathBuf]) -> Vec<String> {
    let script =
        "for f in \"$@\"; do if cat \"$f\" >/dev/null 2>/dev/null; then echo \"READABLE:$f\"; \
                  else echo \"DENIED:$f\"; fi; done"
            .to_string();
    let mut inner = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        script,
        "sh".to_string(),
    ];
    inner.extend(files.iter().map(|p| p.display().to_string()));
    inner
}

/// `[ -r ]`-based read probe for the Linux tmpfs check — distinguishes
/// "host file visible" (READABLE) from "hidden by the mount" (HIDDEN).
fn linux_read_probe_inner(files: &[PathBuf]) -> Vec<String> {
    let script = "for f in \"$@\"; do if [ -r \"$f\" ]; then echo \"READABLE:$f\"; \
                  else echo \"HIDDEN:$f\"; fi; done"
        .to_string();
    let mut inner = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        script,
        "sh".to_string(),
    ];
    inner.extend(files.iter().map(|p| p.display().to_string()));
    inner
}

fn write_probe_inner(target: &Path) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "if printf EVIL > \"$1\" 2>/dev/null; then echo WROTE; else echo DENIED; fi".to_string(),
        "sh".to_string(),
        target.display().to_string(),
    ]
}

// ---- local upstream servers (host-side, reached only via the proxy) -------

async fn spawn_fixed_upstream(body: &'static str) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind fixed upstream");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
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
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let head = "HTTP/1.1 200 OK\r\nContent-Length: 10000000\r\n\r\n";
                if sock.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for _ in 0..max_chunks {
                    if sock.write_all(b"0123456789").await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            });
        }
    });
    port
}

/// A probe's stdout/stderr contains none of the marker strings a
/// successful escape would print.
fn no_marker(out: &RunOutput, markers: &[&str]) -> bool {
    !markers
        .iter()
        .any(|m| out.stdout.contains(m) || out.stderr.contains(m))
}

// ============================ attempt 1: raw sockets ========================

const THREAT_BOUNDARY2: &str = "THREATMODEL trust boundary 2 (agent pane <-> host): all direct egress denied; only the per-pane loopback CONNECT proxy is a route out";

async fn attempt_raw_socket_egress() -> Attempt {
    const NAME: &str = "raw-socket-egress";
    if cfg!(target_os = "macos") {
        if let Err(a) = require_mac(NAME, THREAT_BOUNDARY2) {
            return a;
        }
        let fx = build_mac_fixture(None).await;
        let mut detail = Vec::new();
        let mut ok = true;

        let baseline = fx.host_run(&curl_direct("http://1.1.1.1/", "4")).await;
        if baseline.exit == Some(0) {
            detail
                .push("host baseline: 1.1.1.1:80 reachable outside the sandbox (decisive)".into());
        } else {
            detail.push(format!(
                "note: host-side baseline curl also failed (exit {:?}) — runner egress policy; the in-sandbox assertions below still hold",
                baseline.exit
            ));
        }

        for (label, url) in [
            ("tcp:80", "http://1.1.1.1/"),
            ("tcp:443", "https://1.1.1.1/"),
            ("tcp:8443", "http://1.1.1.1:8443/"),
        ] {
            let out = fx.run(&curl_direct(url, "6"), &[]).await;
            let bytes = out.stdout.trim().to_string();
            let blocked =
                !out.timed_out && out.exit.is_some() && out.exit != Some(0) && bytes == "0";
            detail.push(format!(
                "{label}: exit={:?} bytes={} -> {} (stderr: {})",
                out.exit,
                bytes,
                if blocked { "BLOCKED" } else { "ESCAPE!" },
                one_line(&out.stderr)
            ));
            ok &= blocked;
        }

        // F-01 leg: a real host-local service on 127.0.0.1:<other-port>
        // must be unreachable — the loopback carve-out names ONLY the
        // pane's proxy port.
        let other = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind F-01 leg");
        let other_port = other.local_addr().unwrap().port();
        let out = fx
            .run(
                &curl_direct(&format!("http://127.0.0.1:{other_port}/"), "6"),
                &[],
            )
            .await;
        let blocked =
            !out.timed_out && out.exit.is_some() && out.exit != Some(0) && out.stdout.trim() == "0";
        detail.push(format!(
            "loopback-other-port 127.0.0.1:{other_port}: exit={:?} bytes={} -> {} (F-01: carve-out is proxy-port-only)",
            out.exit,
            out.stdout.trim(),
            if blocked { "BLOCKED" } else { "ESCAPE!" }
        ));
        ok &= blocked;

        return finish(NAME, THREAT_BOUNDARY2, ok, detail);
    }
    if cfg!(target_os = "linux") {
        if let Err(a) = require_linux(NAME, THREAT_BOUNDARY2) {
            return a;
        }
        let Some(fx) = build_linux_fixture(vec!["127.0.0.1".to_string()]).await else {
            return Attempt::skip(
                NAME,
                THREAT_BOUNDARY2,
                linux_preflight().expect("fixture refused; preflight must carry the reason"),
            );
        };
        let mut detail = Vec::new();
        let mut ok = true;
        for (label, url) in [
            ("tcp:80", "http://1.1.1.1/"),
            ("tcp:443", "https://1.1.1.1/"),
            ("tcp:8443", "http://1.1.1.1:8443/"),
        ] {
            let out = fx.run_inner(curl_direct(url, "6"), &[]).await;
            let bytes = out.stdout.trim().to_string();
            let blocked =
                !out.timed_out && out.exit.is_some() && out.exit != Some(0) && bytes == "0";
            detail.push(format!(
                "{label}: exit={:?} bytes={} -> {} (stderr: {})",
                out.exit,
                bytes,
                if blocked { "BLOCKED" } else { "ESCAPE!" },
                one_line(&out.stderr)
            ));
            ok &= blocked;
        }
        return finish(NAME, THREAT_BOUNDARY2, ok, detail);
    }
    Attempt::skip(NAME, THREAT_BOUNDARY2, "unsupported OS".into())
}

// ============================ attempt 2: DNS ================================

async fn attempt_dns_resolution() -> Attempt {
    const NAME: &str = "dns-resolution";
    const THREAT: &str = "THREATMODEL boundary 2 / SECURITY.md: \"all direct egress denied, DNS included\" — getaddrinfo inside the sandbox must not resolve";
    if cfg!(target_os = "macos") {
        if let Err(a) = require_mac(NAME, THREAT) {
            return a;
        }
        let fx = build_mac_fixture(None).await;
        let out = fx
            .run(&curl_direct("http://api.anthropic.com/", "6"), &[])
            .await;
        let blocked =
            !out.timed_out && out.exit.is_some() && out.exit != Some(0) && out.stdout.trim() == "0";
        let mode = if out.stderr.to_lowercase().contains("resolve") {
            "resolution itself refused"
        } else {
            "resolver reached but the connect that follows was blocked"
        };
        let mut detail = vec![format!(
            "getaddrinfo probe (curl, --noproxy) to api.anthropic.com: exit={:?} bytes={} -> {}; observed mode: {mode} (stderr: {})",
            out.exit,
            out.stdout.trim(),
            if blocked { "BLOCKED" } else { "ESCAPE!" },
            one_line(&out.stderr)
        )];
        detail.push("no bytes can be exchanged with a real host either way — containment holds; macOS may vary between refusing resolution and refusing the connect that follows".into());
        return finish(NAME, THREAT, blocked, detail);
    }
    if cfg!(target_os = "linux") {
        let Some(_) = linux_preflight() else {
            return Attempt::skip(
                NAME,
                THREAT,
                linux_preflight().expect("fixture refused; preflight must carry the reason"),
            );
        };
        let Some(fx) = build_linux_fixture(vec!["127.0.0.1".to_string()]).await else {
            return Attempt::skip(
                NAME,
                THREAT,
                linux_preflight().expect("fixture refused; preflight must carry the reason"),
            );
        };
        let out = fx
            .run_inner(curl_direct("http://api.anthropic.com/", "6"), &[])
            .await;
        let blocked =
            !out.timed_out && out.exit.is_some() && out.exit != Some(0) && out.stdout.trim() == "0";
        let detail = vec![format!(
            "getaddrinfo probe (curl, --noproxy) inside the fresh netns: exit={:?} bytes={} -> {} (fresh netns has no DNS route; stderr: {})",
            out.exit,
            out.stdout.trim(),
            if blocked { "BLOCKED" } else { "ESCAPE!" },
            one_line(&out.stderr)
        )];
        return finish(NAME, THREAT, blocked, detail);
    }
    Attempt::skip(NAME, THREAT, "unsupported OS".into())
}

// ============================ attempt 3: SSH ================================

async fn attempt_ssh_egress() -> Attempt {
    const NAME: &str = "ssh-egress";
    const THREAT: &str = "THREATMODEL boundary 2: raw TCP to any host must fail — an SSH session (banner exchange, \"Permission denied\") would be direct egress";
    if !Path::new("/usr/bin/ssh").exists() {
        return Attempt::skip(NAME, THREAT, "/usr/bin/ssh not installed".into());
    }
    // 203.0.113.1 is TEST-NET-3 (RFC 5737) — never routable, so any
    // handshake marker is proof of egress and any exit-0 is an escape.
    let target = "203.0.113.1";
    if cfg!(target_os = "macos") {
        if let Err(a) = require_mac(NAME, THREAT) {
            return a;
        }
        let fx = build_mac_fixture(None).await;
        let out = fx.run(&ssh_probe(target), &[]).await;
        let handshake_markers = [
            "Permission denied",
            "Connection refused",
            "Welcome",
            "Last login",
        ];
        let blocked = !out.timed_out
            && out.exit.is_some()
            && out.exit != Some(0)
            && no_marker(&out, &handshake_markers)
            && !out.stderr.trim().is_empty();
        let detail = vec![format!(
            "ssh to {target}:22 inside the seatbelt sandbox: exit={:?} -> {} (stderr: {})",
            out.exit,
            if blocked { "BLOCKED" } else { "ESCAPE!" },
            one_line(&out.stderr)
        )];
        return finish(NAME, THREAT, blocked, detail);
    }
    if cfg!(target_os = "linux") {
        let Some(_) = linux_preflight() else {
            return Attempt::skip(
                NAME,
                THREAT,
                linux_preflight().expect("fixture refused; preflight must carry the reason"),
            );
        };
        let Some(fx) = build_linux_fixture(vec!["127.0.0.1".to_string()]).await else {
            return Attempt::skip(
                NAME,
                THREAT,
                linux_preflight().expect("fixture refused; preflight must carry the reason"),
            );
        };
        let out = fx.run_inner(ssh_probe(target), &[]).await;
        let handshake_markers = [
            "Permission denied",
            "Connection refused",
            "Welcome",
            "Last login",
        ];
        let blocked = !out.timed_out
            && out.exit.is_some()
            && out.exit != Some(0)
            && no_marker(&out, &handshake_markers)
            && !out.stderr.trim().is_empty();
        let detail = vec![format!(
            "ssh to {target}:22 inside the fresh netns: exit={:?} -> {} (stderr: {})",
            out.exit,
            if blocked { "BLOCKED" } else { "ESCAPE!" },
            one_line(&out.stderr)
        )];
        return finish(NAME, THREAT, blocked, detail);
    }
    Attempt::skip(NAME, THREAT, "unsupported OS".into())
}

// ============================ attempt 4: config dir =========================

const THREAT_CONFIG: &str = "THREATMODEL invariant 3 (F-03): the seatbelt profile denies reads AND writes of the whole app config dir; the Linux bwrap wrap replaces it with a fresh tmpfs";

fn plant_config_markers(dir: &Path) {
    std::fs::write(dir.join("egress-auth.json"), "AUTH-SECRET").expect("plant auth marker");
    std::fs::write(dir.join("egress.json"), "ALLOW-SECRET").expect("plant allowlist marker");
    std::fs::write(dir.join("egress-repo-consents.json"), "CONSENTS-SECRET")
        .expect("plant consents marker");
    std::fs::write(dir.join("events.jsonl"), "EVENT-LOG-SECRET").expect("plant events marker");
}

fn config_marker_files(dir: &Path) -> Vec<PathBuf> {
    [
        "egress-auth.json",
        "egress.json",
        "egress-repo-consents.json",
        "events.jsonl",
    ]
    .iter()
    .map(|f| dir.join(f))
    .collect()
}

async fn attempt_config_dir_isolation() -> Attempt {
    const NAME: &str = "config-dir-isolation";
    if cfg!(target_os = "macos") {
        if let Err(a) = require_mac(NAME, THREAT_CONFIG) {
            return a;
        }
        let fx = build_mac_fixture(None).await;
        plant_config_markers(&fx.config_dir);

        let out = fx
            .run(&read_probe_inner(&config_marker_files(&fx.config_dir)), &[])
            .await;
        let read_ok = !out.timed_out
            && out.exit == Some(0)
            && !out.stdout.contains("READABLE")
            && out.stdout.matches("DENIED:").count() == 4;

        let wout = fx
            .run(&write_probe_inner(&fx.config_dir.join("pwned")), &[])
            .await;
        let write_ok = !wout.timed_out && wout.exit == Some(0) && wout.stdout.contains("DENIED");
        let host_untouched = !fx.config_dir.join("pwned").exists()
            && std::fs::read_to_string(fx.config_dir.join("egress.json")).unwrap()
                == "ALLOW-SECRET";

        let mut detail = vec![format!(
            "read probe (egress-auth.json, egress.json, egress-repo-consents.json, events.jsonl): {} -> {}",
            out.stdout.trim().replace('\n', " | "),
            if read_ok { "ALL DENIED" } else { "ESCAPE!" }
        )];
        detail.push(format!(
            "write probe (new file in config dir + overwrite of egress.json): {} -> {}",
            one_line(&wout.stdout),
            if write_ok { "DENIED" } else { "ESCAPE!" }
        ));
        detail.push(format!(
            "host after probes: pwned absent={} egress.json bytes unchanged={}",
            !fx.config_dir.join("pwned").exists(),
            host_untouched
        ));
        return finish(
            NAME,
            THREAT_CONFIG,
            read_ok && write_ok && host_untouched,
            detail,
        );
    }
    if cfg!(target_os = "linux") {
        if let Err(a) = require_linux(NAME, THREAT_CONFIG) {
            return a;
        }
        let Some(fx) = build_linux_fixture(vec!["127.0.0.1".to_string()]).await else {
            return Attempt::skip(
                NAME,
                THREAT_CONFIG,
                linux_preflight().expect("fixture refused; preflight must carry the reason"),
            );
        };
        plant_config_markers(&fx.config_dir);
        let files = config_marker_files(&fx.config_dir);

        let out = fx.run_inner(linux_read_probe_inner(&files), &[]).await;
        let read_ok = !out.timed_out
            && out.exit == Some(0)
            && !out.stdout.contains("READABLE")
            && out.stdout.matches("HIDDEN:").count() == 4;

        let wout = fx
            .run_inner(write_probe_inner(&fx.config_dir.join("pwned")), &[])
            .await;
        // Under the tmpfs the write SUCCEEDS inside the sandbox — the
        // assertion is that it never lands on the host config dir.
        let host_untouched = !fx.config_dir.join("pwned").exists()
            && std::fs::read_to_string(fx.config_dir.join("egress.json")).unwrap()
                == "ALLOW-SECRET";

        let mut detail = vec![format!(
            "read probe inside the wrap: {} -> {} (bwrap --tmpfs hides the host config dir)",
            out.stdout.trim().replace('\n', " | "),
            if read_ok { "ALL HIDDEN" } else { "ESCAPE!" }
        )];
        detail.push(format!(
            "write probe: {} inside the tmpfs (expected — the write lands on the fresh tmpfs, not the host)",
            one_line(&wout.stdout)
        ));
        detail.push(format!(
            "host after probes: pwned absent={} egress.json bytes unchanged={}",
            !fx.config_dir.join("pwned").exists(),
            host_untouched
        ));
        return finish(NAME, THREAT_CONFIG, read_ok && host_untouched, detail);
    }
    Attempt::skip(NAME, THREAT_CONFIG, "unsupported OS".into())
}

// ============================ attempt 5: symlink variant ====================

async fn attempt_config_dir_symlink() -> Attempt {
    const NAME: &str = "config-dir-symlink-ancestor";
    const THREAT: &str = "seatbelt.rs canonical-path caveat: SBPL subpath rules match the path sandbox-exec resolves the operation to — a spelled app_data_dir whose REAL path differs (symlinked prefix, e.g. under /tmp) can silently fail to confine; production relies on the caller invariant (Tauri's app_data_dir has no symlinked ancestors; spawn-time refusal in parallel work)";
    if !cfg!(target_os = "macos") {
        return Attempt::skip(
            NAME,
            THREAT,
            "macOS-only caveat (sandbox-exec path canonicalization). The Linux bwrap rung has no path-resolution caveat: the config dir is replaced by a fresh tmpfs (attempt 4).".into(),
        );
    }
    if let Err(a) = require_mac(NAME, THREAT) {
        return a;
    }

    let mut detail = Vec::new();
    let mut ok = true;

    // Leg A: symlinked FINAL component under a real base (a config dir
    // reached through a link).
    {
        let home = std::env::home_dir().unwrap();
        let scratch = tempfile::TempDir::new_in(&home).expect("scratch base");
        let real = scratch.path().join("cfg-real");
        std::fs::create_dir_all(&real).expect("create real cfg dir");
        let link = scratch.path().join("cfg-link");
        std::os::unix::fs::symlink("cfg-real", &link).expect("create symlinked ancestor");
        std::fs::write(real.join("egress-auth.json"), "AUTH-SECRET").expect("plant auth marker");
        let hermetic_home = scratch.path().join("home");
        std::fs::create_dir_all(&hermetic_home).expect("create hermetic home");

        let symlinked_profile = seatbelt_profile(&link, 1, &hermetic_home);
        let out = mac_run_profile(
            &symlinked_profile,
            &read_probe_inner(&[link.join("egress-auth.json")]),
            &[],
        )
        .await;
        let real_profile = seatbelt_profile(&real, 1, &hermetic_home);
        let real_out = mac_run_profile(
            &real_profile,
            &read_probe_inner(&[real.join("egress-auth.json")]),
            &[],
        )
        .await;

        if !out.timed_out && out.stdout.contains("READABLE") {
            detail.push("leg A (link as final component, real base): READABLE — confinement did NOT hold for the symlinked spelling on this macOS".into());
        } else if !out.timed_out && out.stdout.contains("DENIED") {
            detail.push("leg A (link as final component, real base): DENIED — confinement held through this symlink shape on this macOS".into());
        } else {
            detail.push(format!(
                "leg A misbehaved: timed_out={} stdout={:?} stderr={}",
                out.timed_out,
                out.stdout.trim(),
                one_line(&out.stderr)
            ));
            ok = false;
        }
        if !real_out.timed_out && real_out.stdout.contains("DENIED") {
            detail.push("leg A control: real-spelling profile denies the same file (DENIED) — the spelling is the whole difference".into());
        } else {
            detail.push(format!(
                "leg A control anomaly: real-spelling profile read returned {:?} — investigate",
                real_out.stdout.trim()
            ));
            ok = false;
        }
    }

    // Leg B: the /tmp shape the caveat's own doc comment names — a spelled
    // path whose PREFIX is a symlink (/tmp -> /private/tmp), which is the
    // shape that reliably reproduces the caveat live on this host.
    {
        let scratch = tempfile::TempDir::new_in("/tmp").expect("scratch under /tmp");
        let real = scratch.path().join("cfg-real");
        std::fs::create_dir_all(&real).expect("create real cfg dir");
        std::fs::write(real.join("egress-auth.json"), "AUTH-SECRET").expect("plant auth marker");
        let hermetic_home = scratch.path().join("home");
        std::fs::create_dir_all(&hermetic_home).expect("create hermetic home");

        // The spelled path as a caller would hand it over — /tmp/... —
        // whose real path is /private/tmp/... .
        let spelled = PathBuf::from(format!(
            "/tmp/{}",
            scratch.path().file_name().unwrap().to_string_lossy()
        ))
        .join("cfg-real");
        let symlinked_profile = seatbelt_profile(&spelled, 1, &hermetic_home);
        let out = mac_run_profile(
            &symlinked_profile,
            &read_probe_inner(&[spelled.join("egress-auth.json")]),
            &[],
        )
        .await;
        // Control uses the CANONICAL spelling (/private/tmp/...) — the
        // profile production effectively gets, since Tauri's app_data_dir
        // has no symlinked ancestors.
        let real_canonical = std::fs::canonicalize(&real).expect("canonicalize real cfg dir");
        let canonical_profile = seatbelt_profile(&real_canonical, 1, &hermetic_home);
        let canonical_out = mac_run_profile(
            &canonical_profile,
            &read_probe_inner(&[real_canonical.join("egress-auth.json")]),
            &[],
        )
        .await;

        if !out.timed_out && out.stdout.contains("READABLE") {
            detail.push("leg B (/tmp-spelled prefix, the caveat's own example): READABLE — canonical-path caveat REPRODUCED live: the profile silently fails to confine a symlinked spelling".into());
            detail.push("production protection: the caller invariant — Tauri's app_data_dir (~/Library/Application Support/<id>) has no symlinked ancestors, so the production profile always confines (proven by the config-dir attempt); the parallel spawn-refusal change is the backstop for any future caller that hands a symlinked dir. Asserted here as the documented state, not an escape of the production path.".into());
        } else if !out.timed_out && out.stdout.contains("DENIED") {
            detail.push("leg B (/tmp-spelled prefix): DENIED — confinement held even for this spelling on this macOS (either outcome is the documented, honest state)".into());
        } else {
            detail.push(format!(
                "leg B misbehaved: timed_out={} stdout={:?} stderr={}",
                out.timed_out,
                out.stdout.trim(),
                one_line(&out.stderr)
            ));
            ok = false;
        }
        if !canonical_out.timed_out && canonical_out.stdout.contains("DENIED") {
            detail.push(
                "leg B control: canonical-spelling profile denies the same file (DENIED)".into(),
            );
        } else {
            detail.push(format!(
                "leg B control anomaly: canonical-spelling profile read returned {:?} — investigate",
                canonical_out.stdout.trim()
            ));
            ok = false;
        }
    }

    finish(NAME, THREAT, ok, detail)
}

// ============================ attempt 6: docker socket ======================

const THREAT_DOCKER: &str = "THREATMODEL invariant 3 + 4: the Docker/container-runtime socket is unreachable from a gapped pane, so `docker run -v /:/host`-style host-escape primitives are unreachable; Linux excludes ~/.docker + XDG_RUNTIME_DIR/docker.sock from the mount set";

/// Plants a REAL listening unix socket at `path` that answers every
/// connection with a fixed body — decisive evidence if a sandboxed probe
/// ever reaches it.
fn plant_answering_socket(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("create socket parent dir");
    let listener = UnixListener::bind(path).expect("plant real unix listener");
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                break;
            };
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nREACHED")
                .await;
        }
    });
}

async fn attempt_docker_socket() -> Attempt {
    const NAME: &str = "docker-socket";
    if cfg!(target_os = "macos") {
        if let Err(a) = require_mac(NAME, THREAT_DOCKER) {
            return a;
        }
        let fx = build_mac_fixture(None).await;
        let mut detail = Vec::new();
        let mut ok = true;

        for rel in [".docker/run/docker.sock", ".docker/desktop/docker.sock"] {
            let sock = fx.home.join(rel);
            plant_answering_socket(&sock);

            let baseline = fx.host_run(&curl_unix_socket(&sock)).await;
            if baseline.exit != Some(0) || baseline.stdout.trim() != "REACHED" {
                detail.push(format!(
                    "SKIP leg {rel}: host-side baseline did not reach the planted socket (exit={:?}, stdout={:?}) — probe not decisive here",
                    baseline.exit,
                    baseline.stdout.trim()
                ));
                continue;
            }
            let out = fx.run(&curl_unix_socket(&sock), &[]).await;
            let blocked = !out.timed_out
                && out.exit.is_some()
                && out.exit != Some(0)
                && out.stdout.trim() != "REACHED";
            detail.push(format!(
                "{rel}: host baseline REACHED; inside the production profile exit={:?} -> {} (stderr: {})",
                out.exit,
                if blocked { "BLOCKED" } else { "ESCAPE!" },
                one_line(&out.stderr)
            ));
            ok &= blocked;
        }

        // /var/run/docker.sock: plant a listener via sudo when the runner
        // allows it; otherwise note that the blocking rule
        // (network-outbound deny) is path-independent.
        match plant_var_run_socket().await {
            Some(()) => {
                let out = fx
                    .run(&curl_unix_socket(Path::new("/var/run/docker.sock")), &[])
                    .await;
                let blocked = !out.timed_out && out.exit.is_some() && out.exit != Some(0) && out.stdout.trim() != "REACHED";
                detail.push(format!(
                    "/var/run/docker.sock (planted listener): inside the production profile exit={:?} -> {}",
                    out.exit,
                    if blocked { "BLOCKED" } else { "ESCAPE!" }
                ));
                ok &= blocked;
                let _ = std::process::Command::new("sudo")
                    .args(["-n", "rm", "-f", "/var/run/docker.sock"])
                    .status();
            }
            None => detail.push(
                "SKIP /var/run leg: could not plant a listener there (sudo unavailable) — the profile's blocking rule for AF_UNIX connects is the blanket network-outbound deny, proven path-independent by the home-socket legs above".into(),
            ),
        }

        // The docker CLI leg: the actual `docker run -v /:/host` primitive
        // needs the daemon, so deny-the-socket transitively denies it.
        match ["/usr/local/bin/docker", "/opt/homebrew/bin/docker"]
            .iter()
            .find(|p| Path::new(p).exists())
        {
            Some(cli) => {
                let sock = fx.home.join(".docker/run/docker.sock");
                let argv = vec![
                    (*cli).to_string(),
                    "-H".to_string(),
                    format!("unix://{}", sock.display()),
                    "version".to_string(),
                ];
                let out = fx
                    .run(
                        &argv,
                        &[("PATH", "/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin")],
                    )
                    .await;
                let blocked = !out.timed_out
                    && out.exit.is_some()
                    && out.exit != Some(0)
                    // `docker version` prints the CLIENT block (with its
                    // own "Version:") to stdout even when the daemon is
                    // unreachable — the decisive marker is the SERVER
                    // block, which only a reachable daemon can produce.
                    && !out.stdout.contains("Server:");
                detail.push(format!(
                    "docker CLI ({cli}) -H unix://<planted> version: exit={:?} -> {} (stderr: {}). A denied socket transitively makes `docker run -v /:/host` unreachable.",
                    out.exit,
                    if blocked { "BLOCKED" } else { "ESCAPE!" },
                    one_line(&out.stderr)
                ));
                ok &= blocked;
            }
            None => detail.push(
                "SKIP docker-CLI leg: no docker binary on this runner — the socket-reachability legs above are the mechanism-level proof; the -v /:/host primitive requires the daemon, which is unreachable".into(),
            ),
        }

        detail.push("mechanism note (verified live while building this suite): seatbelt's blanket (deny network-outbound) is what blocks AF_UNIX connects — the literal file-read/write denies on the socket paths are inert for connects (tested against real sandbox-exec with and without them). The production profile carries both; protection holds either way.".into());
        return finish(NAME, THREAT_DOCKER, ok, detail);
    }
    if cfg!(target_os = "linux") {
        if let Err(a) = require_linux(NAME, THREAT_DOCKER) {
            return a;
        }
        let Some(fx) = build_linux_fixture(vec!["127.0.0.1".to_string()]).await else {
            return Attempt::skip(
                NAME,
                THREAT_DOCKER,
                linux_preflight().expect("fixture refused; preflight must carry the reason"),
            );
        };
        let mut detail = Vec::new();
        let mut ok = true;

        // Plant REAL listening sockets at the excluded paths — a mount-set
        // regression that exposed them would find them present and live.
        let home_sock = fx.home.join(".docker/run/docker.sock");
        let xdg_sock = fx.runtime_dir.join("docker.sock");
        plant_answering_socket(&home_sock);
        plant_answering_socket(&xdg_sock);

        let script = "for f in \"$@\"; do if [ -S \"$f\" ]; then echo \"EXPOSED:$f\"; else echo \"ABSENT:$f\"; fi; done";
        let mut inner: Vec<String> =
            vec!["/bin/sh".into(), "-c".into(), script.into(), "sh".into()];
        inner.push(home_sock.display().to_string());
        inner.push(xdg_sock.display().to_string());
        inner.push("/var/run/docker.sock".into());
        let out = fx.run_inner(inner, &[]).await;
        let legs = !out.timed_out
            && out.exit == Some(0)
            && !out.stdout.contains("EXPOSED")
            && out.stdout.matches("ABSENT:").count() == 3;
        detail.push(format!(
            "probe inside the wrap: {} -> {} (curated mount set excludes ~/.docker, the rootless socket dir, and /run is a fresh tmpfs)",
            out.stdout.trim().replace('\n', " | "),
            if legs { "ALL ABSENT" } else { "ESCAPE!" }
        ));
        ok &= legs;

        let docker_cli = ["/usr/bin/docker", "/usr/local/bin/docker"]
            .iter()
            .find(|p| Path::new(p).exists())
            .map(|s| s.to_string());
        match docker_cli {
            Some(cli) => {
                let inner = vec![
                    cli.clone(),
                    "-H".to_string(),
                    "unix:///var/run/docker.sock".to_string(),
                    "version".to_string(),
                ];
                let cout = fx.run_inner(inner, &[]).await;
                let blocked = !cout.timed_out
                    && cout.exit.is_some()
                    && cout.exit != Some(0)
                    // See the macOS docker-CLI leg: the client prints its
                    // own "Version:" to stdout regardless — the SERVER
                    // block is the decisive marker.
                    && !cout.stdout.contains("Server:");
                detail.push(format!(
                    "docker CLI ({cli}) version via /var/run/docker.sock: exit={:?} -> {} — a denied socket transitively makes `docker run -v /:/host` unreachable",
                    cout.exit,
                    if blocked { "BLOCKED" } else { "ESCAPE!" }
                ));
                ok &= blocked;
            }
            None => detail.push(
                "SKIP docker-CLI leg: no docker binary on this runner — the mount-exclusion legs above are the mechanism-level proof".into(),
            ),
        }
        return finish(NAME, THREAT_DOCKER, ok, detail);
    }
    Attempt::skip(NAME, THREAT_DOCKER, "unsupported OS".into())
}

/// Best-effort: plant a real listening socket at /var/run/docker.sock via
/// sudo (passwordless on GitHub macos runners; fails gracefully elsewhere).
async fn plant_var_run_socket() -> Option<()> {
    let sudo_ok = std::process::Command::new("sudo")
        .args(["-n", "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !sudo_ok {
        return None;
    }
    let helper = std::env::temp_dir().join("tome-escape-var-run.py");
    std::fs::write(
        &helper,
        r#"
import socket, sys, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    s.bind("/var/run/docker.sock")
except OSError as e:
    sys.stderr.write(f"bind failed: {e}\n"); sys.exit(1)
s.listen(1)
open("/tmp/tome-escape-var-run.ready", "w").write("ok")
deadline = time.time() + 20
s.settimeout(1)
while time.time() < deadline:
    try:
        c, _ = s.accept()
        c.send(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nREACHED")
        c.close()
    except socket.timeout:
        continue
"#,
    )
    .ok()?;
    let mut child = std::process::Command::new("sudo")
        .args(["-n", "/usr/bin/python3", helper.to_str()?])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let ready = Path::new("/tmp/tome-escape-var-run.ready");
    let _ = std::fs::remove_file(ready);
    for _ in 0..50 {
        if ready.exists() {
            return Some(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let _ = child.kill();
    None
}

// ============================ attempt 7: proxy ==============================

const THREAT_PROXY: &str = "THREATMODEL invariant 4/5 (TOME-002, F-05): the proxy is the only route out — blocked host refused; allowlisted host works only per the mode; unlock widens the PROXY never the sandbox; relock re-locks and severs live tunnels";

async fn attempt_proxy_allowlist() -> Attempt {
    const NAME: &str = "proxy-allowlist-unlock-relock";
    let upstream_port = spawn_fixed_upstream("UPSTREAM-OK").await;
    let drip_port = spawn_drip_upstream(40).await;

    if cfg!(target_os = "macos") {
        if let Err(a) = require_mac(NAME, THREAT_PROXY) {
            return a;
        }
        let blocked: Arc<Mutex<Vec<BlockedEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let blocked_for_cb = blocked.clone();
        let proxy = PaneProxy::spawn(vec!["127.0.0.1".to_string()], None, move |ev| {
            blocked_for_cb.lock().unwrap().push(ev);
        })
        .await
        .expect("spawn PaneProxy");
        proxy.set_allowed_ports(vec![80, 443, upstream_port, drip_port]);
        let proxy = Arc::new(proxy);
        let fx = build_mac_fixture(Some(proxy.port())).await;
        let proxy_url = format!("http://127.0.0.1:{}", proxy.port());
        let env = vec![
            ("HTTP_PROXY".to_string(), proxy_url.clone()),
            ("HTTPS_PROXY".to_string(), proxy_url.clone()),
            ("http_proxy".to_string(), proxy_url.clone()),
            ("https_proxy".to_string(), proxy_url),
            ("NO_PROXY".to_string(), String::new()),
            ("no_proxy".to_string(), String::new()),
        ];
        let seat = ProxySeat::Mac {
            fx: &fx,
            env: &env,
            proxy: &proxy,
        };
        return run_proxy_attempt(NAME, THREAT_PROXY, seat, upstream_port, drip_port, blocked)
            .await;
    }
    if cfg!(target_os = "linux") {
        if let Err(a) = require_linux(NAME, THREAT_PROXY) {
            return a;
        }
        let Some(fx) = build_linux_fixture(vec!["127.0.0.1".to_string()]).await else {
            return Attempt::skip(
                NAME,
                THREAT_PROXY,
                linux_preflight().expect("fixture refused; preflight must carry the reason"),
            );
        };
        fx.proxy
            .set_allowed_ports(vec![80, 443, upstream_port, drip_port]);
        let blocked = fx.blocked.clone();
        let seat = ProxySeat::Linux {
            fx: &fx,
            proxy: &fx.proxy,
        };
        return run_proxy_attempt(NAME, THREAT_PROXY, seat, upstream_port, drip_port, blocked)
            .await;
    }
    Attempt::skip(NAME, THREAT_PROXY, "unsupported OS".into())
}

/// Where a proxy probe script runs: inside the real macOS seatbelt wrap
/// (env = the pane's proxy env) or inside the real Linux bwrap wrap. The
/// seat carries the one pane's proxy so unlock/relock state persists
/// across every probe, exactly like a real pane's lifetime.
enum ProxySeat<'a> {
    Mac {
        fx: &'a MacFixture,
        env: &'a [(String, String)],
        proxy: &'a Arc<PaneProxy>,
    },
    Linux {
        fx: &'a LinuxFixture,
        proxy: &'a Arc<PaneProxy>,
    },
}

impl ProxySeat<'_> {
    fn proxy(&self) -> &Arc<PaneProxy> {
        match self {
            ProxySeat::Mac { proxy, .. } => proxy,
            ProxySeat::Linux { proxy, .. } => proxy,
        }
    }

    async fn run_script(&self, script: &str) -> RunOutput {
        match self {
            ProxySeat::Mac { fx, env, .. } => {
                let env_refs: Vec<(&str, &str)> =
                    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                fx.run(
                    &["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
                    &env_refs,
                )
                .await
            }
            ProxySeat::Linux { fx, .. } => {
                fx.run_inner(
                    vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
                    &[],
                )
                .await
            }
        }
    }

    /// Spawns (does not await) the probe — for the live-tunnel relock leg,
    /// which must relock WHILE the transfer is in flight.
    fn spawn_script(&self, script: &str) -> Child {
        match self {
            ProxySeat::Mac { fx, env, .. } => {
                let env_refs: Vec<(&str, &str)> =
                    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                fx.command(
                    &["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
                    &env_refs,
                )
                .spawn()
                .expect("spawn live-tunnel probe")
            }
            ProxySeat::Linux { fx, .. } => fx
                .command(
                    vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
                    &[],
                )
                .spawn()
                .expect("spawn live-tunnel probe"),
        }
    }
}

async fn run_proxy_attempt(
    name: &'static str,
    threat: &'static str,
    seat: ProxySeat<'_>,
    upstream_port: u16,
    drip_port: u16,
    blocked: Arc<Mutex<Vec<BlockedEvent>>>,
) -> Attempt {
    let mut detail = Vec::new();
    let mut ok = true;

    // 1. Blocked host refused (Providers mode): "localhost" resolves to the
    // same upstream but is NOT the allowlisted "127.0.0.1" — the same
    // presented-name distinction the proxy unit tests use.
    let script = format!(
        "curl -sS --max-time 8 --proxy \"$HTTP_PROXY\" -o /dev/null -w '%{{http_code}}' http://localhost:{upstream_port}/"
    );
    let out = seat.run_script(&script).await;
    let blocked_403 = !out.timed_out && out.exit == Some(0) && out.stdout.trim() == "403";
    detail.push(format!(
        "providers mode, host NOT allowlisted (localhost): http_code={} -> {}",
        out.stdout.trim(),
        if blocked_403 {
            "403 REFUSED"
        } else {
            "ESCAPE!"
        }
    ));
    ok &= blocked_403;

    // 2. Allowlisted host works while locked (F-05 port gate admitted:
    // the harness widened allowed_ports to the upstream's kernel-assigned
    // port, mirroring the integration tests' fixture).
    let script = format!(
        "curl -sS --max-time 8 --proxy \"$HTTP_PROXY\" -o /dev/null -w '%{{http_code}}' http://127.0.0.1:{upstream_port}/"
    );
    let out = seat.run_script(&script).await;
    let allowed_200 = !out.timed_out && out.exit == Some(0) && out.stdout.trim() == "200";
    detail.push(format!(
        "providers mode, allowlisted host (127.0.0.1): http_code={} -> {}",
        out.stdout.trim(),
        if allowed_200 {
            "200 (proxy works)"
        } else {
            "BROKEN"
        }
    ));
    ok &= allowed_200;

    // 3. Unlock: the same non-allowlisted host now works — unlock widens
    // the PROXY (second-factor-gated in production), never the sandbox.
    seat.proxy().unlock();
    let script = format!(
        "curl -sS --max-time 8 --proxy \"$HTTP_PROXY\" -o /dev/null -w '%{{http_code}}' http://localhost:{upstream_port}/"
    );
    let out = seat.run_script(&script).await;
    let unlocked_200 = !out.timed_out && out.exit == Some(0) && out.stdout.trim() == "200";
    detail.push(format!(
        "unlocked (Mode::Open): localhost http_code={} -> {}",
        out.stdout.trim(),
        if unlocked_200 {
            "200 (unlock widens the proxy only)"
        } else {
            "BROKEN"
        }
    ));
    ok &= unlocked_200;

    // 4. Relock: refused again — the re-lock sweep actually re-locks NEW
    // tunnels, not just flips a flag.
    seat.proxy().relock();
    let script = format!(
        "curl -sS --max-time 8 --proxy \"$HTTP_PROXY\" -o /dev/null -w '%{{http_code}}' http://localhost:{upstream_port}/"
    );
    let out = seat.run_script(&script).await;
    let relocked_403 = !out.timed_out && out.exit == Some(0) && out.stdout.trim() == "403";
    detail.push(format!(
        "after relock: localhost http_code={} -> {}",
        out.stdout.trim(),
        if relocked_403 {
            "403 REFUSED again"
        } else {
            "ESCAPE!"
        }
    ));
    ok &= relocked_403;

    // 5. Relock severs a LIVE tunnel mid-transfer (TOME-002), at mechanism
    // level inside the real sandbox.
    seat.proxy().unlock();
    let script = format!(
        "curl -sS --max-time 15 --proxy \"$HTTP_PROXY\" --proxytunnel -o /dev/null -w '%{{size_download}}' http://localhost:{drip_port}/"
    );
    let mut child = seat.spawn_script(&script);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let live = seat.proxy().live_tunnel_count();
    if live != 1 {
        let _ = child.kill().await;
        let out = child.wait_with_output().await;
        detail.push(format!(
            "live-tunnel leg: expected 1 live tunnel, saw {live} (stdout={:?} stderr={})",
            out.as_ref()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default(),
            out.as_ref()
                .map(|o| one_line(&String::from_utf8_lossy(&o.stderr)))
                .unwrap_or_default()
        ));
        ok = false;
        return finish(name, threat, ok, detail);
    }
    seat.proxy().relock();
    const POST_RELOCK_TIMEOUT: Duration = Duration::from_secs(3);
    let severed = match tokio::time::timeout(POST_RELOCK_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => {
            let exited_nonzero = o.status.code().is_some() && o.status.code() != Some(0);
            let tunnels_gone = seat.proxy().live_tunnel_count() == 0;
            detail.push(format!(
                "live tunnel severed by relock (TOME-002): curl exit={:?}, tunnels left={} -> {}",
                o.status.code(),
                seat.proxy().live_tunnel_count(),
                if exited_nonzero && tunnels_gone {
                    "SEVERED PROMPTLY"
                } else {
                    "ESCAPE!"
                }
            ));
            exited_nonzero && tunnels_gone
        }
        Ok(Err(e)) => {
            detail.push(format!("live-tunnel leg: wait failed: {e}"));
            false
        }
        Err(_) => {
            // The timeout future owned the child; dropping it arms
            // kill_on_drop, so the still-running curl dies with the
            // future — the probe is over either way.
            detail.push(format!(
                "live-tunnel leg: curl still running {POST_RELOCK_TIMEOUT:?} after relock — the tunnel was NOT severed promptly"
            ));
            false
        }
    };
    ok &= severed;

    // The host-side PaneProxy's on_blocked signal must have fired for the
    // refused legs — the exact mechanism production wires to the
    // persistent event log (`egress:blocked`).
    let blocked_hosts = blocked.lock().unwrap();
    let saw_blocked = blocked_hosts
        .iter()
        .any(|e| matches!(e, BlockedEvent::Attempt { host } if host == "localhost"));
    detail.push(format!(
        "on_blocked fired for \"localhost\" -> {}",
        if saw_blocked {
            "YES (event-log path)"
        } else {
            "NO"
        }
    ));
    ok &= saw_blocked;

    finish(name, threat, ok, detail)
}

// ============================ attempt 8: validation =========================

async fn attempt_repo_egress_validation() -> Attempt {
    const NAME: &str = "repo-egress-validation";
    const THREAT: &str = "THREATMODEL secondary invariant: \"A repo's .tome/egress.json is untrusted input\" — wildcards, localhost, wildcard TLD/base, URL syntax, and overlong entries must all be rejected";
    let mut detail = Vec::new();
    let mut ok = true;

    let reject_corpus = [
        "*",
        "*.com",
        "*.*",
        "localhost",
        "https://x.com",
        "x.com/path",
        "user@x.com",
        "has space.com",
        "",
        "*api.example.com",
        "api*.example.com",
    ];
    for p in reject_corpus {
        let r = validate_repo_allowlist(&[serde_json::json!(p)]);
        let rejected = r.ok.is_empty() && r.rejected.len() == 1 && !r.rejected[0].reason.is_empty();
        detail.push(format!(
            "pattern {p:?}: {}",
            if rejected {
                format!("REJECTED ({})", r.rejected[0].reason)
            } else {
                "ACCEPTED — ESCAPE!".to_string()
            }
        ));
        ok &= rejected;
    }

    let overlong = format!("{}.com", "a".repeat(250));
    let r = validate_repo_allowlist(&[serde_json::json!(overlong)]);
    let overlong_rejected = r.ok.is_empty() && r.rejected.len() == 1;
    ok &= overlong_rejected;
    detail.push(format!(
        "overlong ({} chars): {}",
        overlong.chars().count(),
        if overlong_rejected {
            "REJECTED"
        } else {
            "ACCEPTED — ESCAPE!"
        }
    ));

    let r = validate_repo_allowlist(&[
        serde_json::json!(42),
        serde_json::json!(null),
        serde_json::json!({}),
    ]);
    let nonstrings = r.ok.is_empty() && r.rejected.len() == 3;
    ok &= nonstrings;
    detail.push(format!(
        "non-string entries: {}",
        if nonstrings {
            "REJECTED"
        } else {
            "ACCEPTED — ESCAPE!"
        }
    ));

    let mixed = validate_repo_allowlist(&[
        serde_json::json!("api.example.com"),
        serde_json::json!("*"),
        serde_json::json!("*.example.com"),
    ]);
    let mixed_ok =
        mixed.ok == vec!["api.example.com", "*.example.com"] && mixed.rejected.len() == 1;
    ok &= mixed_ok;
    detail.push(format!(
        "mixed array keeps valid entries and drops the wildcard: {}",
        if mixed_ok { "OK" } else { "ESCAPE!" }
    ));

    let parsed = parse_repo_allowlist("{\"allow\": [\"api.example.com\"]}");
    let bad = parse_repo_allowlist("{not json");
    let not_array = parse_repo_allowlist("{\"allow\": \"api.example.com\"}");
    let parse_ok =
        parsed.is_ok() && parsed.unwrap().len() == 1 && bad.is_err() && not_array.is_err();
    ok &= parse_ok;
    detail.push(format!(
        "parse layer: valid arrays parse; malformed JSON and non-array `allow` are Err (treated as file-absent by callers — can never widen the gap): {}",
        if parse_ok { "OK" } else { "ESCAPE!" }
    ));

    // The shipped defaults must themselves satisfy the repo validator
    // (the bar users' repos are held to) and match their literal form.
    let defaults_ok = DEFAULT_ALLOW.iter().all(|p| {
        let valid = !validate_repo_allowlist(&[serde_json::json!(p)])
            .ok
            .is_empty();
        let literal = p.replace('*', "x");
        valid && is_allowed(&compile_allowlist([p]), &literal)
    });
    ok &= defaults_ok;
    detail.push(format!(
        "DEFAULT_ALLOW (16 shipped patterns) each validate AND match their literal form: {}",
        if defaults_ok { "OK" } else { "ESCAPE!" }
    ));

    finish(NAME, THREAT, ok, detail)
}

// ============================ attempt 9: second factor ======================

async fn attempt_second_factor() -> Attempt {
    const NAME: &str = "ungapped-spawn-second-factor";
    const THREAT: &str =
        "THREATMODEL invariant 2 / TOME-001: an UNCONTAINED (ungapped) spawn is unsandboxed with open egress, so pty:create demands a fresh second factor every time";
    let detail = vec![
        "DELEGATED: the gate lives in `pub(crate)` ipc code (`ipc::pty::pty_create`'s re-auth ceremony + `pty_authority::unrestricted_spawn_needs_reauth` + `AuthLock`), unreachable from this separate crate — asserted instead by in-crate IPC-layer unit tests that run in the same CI gates via `cargo test` on BOTH OSes:".to_string(),
        "- src/ipc/pty.rs::ungapped_spawn_with_a_real_configured_factor_demands_a_verified_second_factor (NEW in P5.3: real AuthLock; no payload -> NeedsCredentials, wrong passphrase -> Rejected + failure recorded, correct passphrase -> Verified)".to_string(),
        "- src/ipc/pty.rs::ungapped_spawn_with_no_configured_factor_needs_no_ceremony (NEW in P5.3)".to_string(),
        "- src/ipc/pty.rs::evaluate_reauth_* (three-way outcome) + ungapped_spawn_with_configured_auth_needs_the_reauth_ceremony / gapped_spawn_never_needs_* (pre-existing)".to_string(),
        "- src/authlock.rs::verify_passphrase_* / verify_totp_* / backoff_throttles_* (the factor checks + brute-force throttle, pre-existing)".to_string(),
    ];
    Attempt::new(NAME, THREAT, Outcome::Delegated, detail)
}

// ============================ driver ========================================

pub async fn run_all() -> Vec<Attempt> {
    vec![
        attempt_raw_socket_egress().await,
        attempt_dns_resolution().await,
        attempt_ssh_egress().await,
        attempt_config_dir_isolation().await,
        attempt_config_dir_symlink().await,
        attempt_docker_socket().await,
        attempt_proxy_allowlist().await,
        attempt_repo_egress_validation().await,
        attempt_second_factor().await,
    ]
}
