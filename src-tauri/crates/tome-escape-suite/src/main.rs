//! Tome escape suite (P5.3) — the dynamic half of launch security hygiene.
//!
//! Spawns REAL contained panes — macOS `sandbox-exec` under the production
//! seatbelt profile, Linux `bwrap` + `tome-shim` under the production argv
//! — and attempts the THREATMODEL escape list, asserting every attempt is
//! blocked. Each attempt prints PASS/FAIL with its threat-model mapping; a
//! failing attempt means the thing escaped, and the suite exits non-zero.
//! CI-gated on both OSes: `.github/workflows/linux-sandbox.yml` (bwrap)
//! and the `escape-suite-macos` job in `.github/workflows/build.yml`
//! (sandbox-exec).

mod attempts;
mod sandbox;

use attempts::Outcome;

#[tokio::main]
async fn main() {
    println!(
        "TOME ESCAPE SUITE (P5.3) — dynamic sandbox-escape harness on {}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("driving the production spawn seams: seatbelt_profile + sandbox-exec (macOS), build_bwrap_argv + tome-shim (Linux)");

    let attempts = attempts::run_all().await;
    let mut failed = 0usize;
    let mut passed = 0usize;
    let mut skipped = 0usize;
    let mut delegated = 0usize;

    for (i, a) in attempts.iter().enumerate() {
        let label = match a.outcome {
            Outcome::Pass => {
                passed += 1;
                "PASS"
            }
            Outcome::Fail => {
                failed += 1;
                "FAIL"
            }
            Outcome::Skip => {
                skipped += 1;
                "SKIP"
            }
            Outcome::Delegated => {
                delegated += 1;
                "DELEGATED"
            }
        };
        println!("\n[{}/{}] {} {}", i + 1, attempts.len(), label, a.name);
        println!("      threat: {}", a.threat);
        for line in &a.detail {
            println!("      {line}");
        }
    }

    println!(
        "\nSUMMARY: {} attempts — {} PASS, {} SKIP, {} DELEGATED, {} FAIL",
        attempts.len(),
        passed,
        skipped,
        delegated,
        failed
    );
    if failed > 0 {
        eprintln!("ESCAPE DETECTED: {failed} attempt(s) succeeded where they must be blocked.");
        std::process::exit(1);
    }
    println!("no escape succeeded — every dynamic attempt was blocked");
}
