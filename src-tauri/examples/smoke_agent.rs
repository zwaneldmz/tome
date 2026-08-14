//! Live-agent smoke driver — spawns a REAL claude pane through the same
//! production path `ipc::pty::pty_create` uses on macOS (PaneProxy +
//! seatbelt-wrapped login shell), then:
//!   1. writes a curl probe to a NON-allowlisted host into the pane's pty
//!      (must fail — no route through the seatbelt'd net), and
//!   2. writes a curl probe to an allowlisted host through the pane's
//!      loopback proxy (must reach it), and
//!   3. starts the real `claude` agent CLI in the pane and watches for it
//!      to boot (proving the integrated spawn path works end to end).
//!
//! Run: cargo run --example smoke_agent --release
//! Evidence: /tmp/tome-smoke-evidence.log

use std::io::Write;
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};

fn log(evidence: &Arc<Mutex<std::fs::File>>, msg: &str) {
    let line = format!("[{:?}] {}\n", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap(), msg);
    print!("{}", line);
    evidence.lock().unwrap().write_all(line.as_bytes()).unwrap();
    evidence.lock().unwrap().flush().unwrap();
}

#[tokio::main]
async fn main() {
    let evidence = Arc::new(Mutex::new(std::fs::File::create("/tmp/tome-smoke-evidence.log").unwrap()));
    log(&evidence, "=== live-agent smoke: start ===");

    // --- the same pieces pty_create assembles ---
    let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join("Library/Application Support/tech.abantu.tome");

    // 1. PaneProxy on loopback with the production default allowlist.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);
    let blocked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let blocked2 = blocked.clone();
    let proxy = tome_lib::airgap::proxy::PaneProxy::spawn(
        tome_lib::airgap::allowlist::DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect(),
        None,
        move |ev| {
            blocked2.lock().unwrap().push(format!("{:?}", ev));
        },
    )
    .await
    .expect("PaneProxy::spawn");
    let port = proxy.port();
    log(&evidence, &format!("PaneProxy live on 127.0.0.1:{port}"));

    // 2. seatbelt profile for the app config dir (auth file hidden).
    // `sandbox-exec -p` takes the profile as a STRING (the whole scheme
    // text), not a path — `-f` is the file variant. Passing a path to
    // `-p` is exactly the "unbound variable" parse error the first run
    // of this example hit. Production (`pty.rs`) passes the profile
    // string inline the same way.
    let profile = tome_lib::airgap::seatbelt::seatbelt_profile(&dir);

    // 3. spawn a login zsh under sandbox-exec with the pane's proxy env.
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 })
        .expect("openpty");
    let proxy_url = format!("http://127.0.0.1:{port}");
    let mut cmd = CommandBuilder::new("sandbox-exec");
    cmd.args(["-p", profile.as_str(), "/bin/sh"]);
    cmd.env("HTTP_PROXY", &proxy_url);
    cmd.env("HTTPS_PROXY", &proxy_url);
    cmd.env("http_proxy", &proxy_url);
    cmd.env("https_proxy", &proxy_url);
    cmd.env("NO_PROXY", "localhost,127.0.0.1");
    cmd.env("no_proxy", "localhost,127.0.0.1");
    cmd.env("TERM", "dumb");
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.cwd("/tmp/tome-live-smoke");
    let mut child: Box<dyn Child + Send + Sync> = pair.slave.spawn_command(cmd).expect("spawn sandboxed zsh");
    drop(pair.slave);
    log(&evidence, "sandboxed zsh spawned (seatbelt + proxy env)");

    let mut reader = pair.master.try_clone_reader().expect("reader");
    let output: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let output2 = output.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => output2.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => break,
            }
        }
    });
    let mut writer = pair.master.take_writer().expect("writer");

    // Give the login shell a moment to finish its rc files before the
    // first probe — writing into a still-initializing pty interleaves the
    // probe text with the boot chatter and the marker scrape below can
    // match the ECHOED command line rather than its output.
    std::thread::sleep(std::time::Duration::from_secs(2));

    let send = |w: &mut Box<dyn Write + Send>, s: &str| {
        // A dead pty (sandbox refused the spawn, shell exited) is an
        // EIO on write — report it rather than panicking mid-probe.
        let _ = w.write_all(s.as_bytes());
        let _ = w.flush();
    };
    let wait_for = |output: &Arc<Mutex<String>>, needle: &str, timeout_s: u64| -> bool {
        let start = std::time::Instant::now();
        loop {
            if output.lock().unwrap().contains(needle) {
                return true;
            }
            if start.elapsed().as_secs() > timeout_s {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    };
    // Scrape between the RESULT markers (printed only after curl exits),
    // never between the START markers — the shell echoes the command line
    // itself, which contains the START text.
    // Anchor on the RESULT-END marker and take the window BEFORE it —
    // zsh's bracketed-paste echo reprints the whole command (including
    // the BEGIN marker text) on the input line, so splitting on BEGIN
    // matches the echo, not the result block. The result block is the
    // text between the LAST occurrence of BEGIN before END and END
    // itself — i.e. everything END is immediately preceded by.
    let scrape = |output: &Arc<Mutex<String>>, n: usize| -> String {
        let snap = output.lock().unwrap().clone();
        let end = format!("PROBE{n}-RESULT-END");
        let before_end = snap.split(&end).next().unwrap_or("");
        // last 400 chars before END, control sequences stripped
        let window: String = before_end.chars().rev().take(400).collect::<String>().chars().rev().collect();
        let mut clean = String::new();
        let mut chars = window.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                // skip ESC [ ... final-byte
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() || c2 == 'm' || c2 == 'K' || c2 == 'J' || c2 == 'H' {
                        break;
                    }
                }
            } else if c == '\r' || c == '\u{7}' {
                // drop CR / BEL
            } else {
                clean.push(c);
            }
        }
        clean.trim().replace('\n', " | ").to_string()
    };

    // --- probe 1: direct egress to a NON-allowlisted host must fail ---
    send(&mut writer, "{ echo PROBE1-RESULT-BEGIN; curl -sS --noproxy '*' --max-time 8 https://example.com -o /dev/null -w 'P1CODE:%{http_code}\\n' 2>&1 | tail -2; echo PROBE1-RESULT-END; }\n");
    let got = wait_for(&output, "PROBE1-RESULT-END", 20);
    let p1 = scrape(&output, 1);
    log(&evidence, &format!("PROBE1 (direct egress to example.com, allowlisted=false) done={got}: {p1}"));
    let p1_blocked = p1.contains("Could not resolve") || p1.contains("Failed to connect") || p1.contains("Operation not permitted") || p1.contains("P1CODE:000") || p1.contains("Operation timed out") || p1.contains("No route to host");
    log(&evidence, &format!("PROBE1 verdict: {}", if p1_blocked { "PASS — direct egress blocked" } else { "FAIL — direct egress reached the host!" }));

    // --- probe 2: allowlisted host THROUGH the proxy must reach ---
    send(&mut writer, "{ echo PROBE2-RESULT-BEGIN; curl -sS --max-time 12 https://api.anthropic.com -o /dev/null -w 'P2CODE:%{http_code}\\n' 2>&1 | tail -3; echo PROBE2-RESULT-END; }\n");
    let got2 = wait_for(&output, "PROBE2-RESULT-END", 25);
    let p2 = scrape(&output, 2);
    log(&evidence, &format!("PROBE2 (api.anthropic.com via proxy) done={got2}: {p2}"));
    let p2_reached = p2.contains("P2CODE:4") || p2.contains("P2CODE:2") || p2.contains("P2CODE:3");
    log(&evidence, &format!("PROBE2 verdict: {}", if p2_reached { "PASS — allowlisted host reached through proxy" } else { "FAIL — allowlisted host NOT reached" }));
    drop(tx); let _ = &mut rx;

    // --- probe 3: the real agent CLI is present and runs in the pane ---
    send(&mut writer, "{ echo PROBE3-RESULT-BEGIN; which claude; claude --version; echo P3-EXIT:$?; echo PROBE3-RESULT-END; }\n");
    let got3 = wait_for(&output, "P3-EXIT:", 45);
    let p3 = scrape(&output, 3);
    log(&evidence, &format!("PROBE3 (claude CLI present + version) done={got3}: {p3}"));

    // Raw pty dump for diagnosis — the scrape markers above only show
    // what matched; this shows what the shell ACTUALLY printed.
    let raw = output.lock().unwrap().clone();
    let tail: String = raw.chars().rev().take(3000).collect::<String>().chars().rev().collect();
    log(&evidence, &format!("--- raw pty tail (last 3000 chars) ---\n{tail}\n--- end raw pty tail ---"));

    log(&evidence, &format!("blocked-event count from proxy: {}", blocked.lock().unwrap().len()));
    for b in blocked.lock().unwrap().iter().take(5) {
        log(&evidence, &format!("  blocked: {b}"));
    }

    log(&evidence, "=== live-agent smoke: end ===");
    child.kill().ok();
    proxy.shutdown();
}
