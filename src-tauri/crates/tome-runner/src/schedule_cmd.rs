//! `schedule install <flow.json> --on-calendar <expr> [--unit-dir <dir>]`
//! — writes a `systemd --user` service+timer pair that runs `tome-runner
//! run <flow.json>` on the given calendar expression, then prints the
//! `systemctl`/`loginctl` commands the server owner still has to run
//! themselves. This binary never invokes `systemctl` on its own behalf —
//! see [`install`]'s doc comment.
//!
//! The unit text itself ([`service_unit`]/[`timer_unit`]) is built by pure
//! string functions, kept separate from the filesystem action
//! ([`install`]) precisely so this slice's brief ("pure string builders —
//! unit-test them") can be satisfied with plain `assert_eq!`s against
//! exact expected text, no filesystem involved.

use std::path::{Path, PathBuf};

/// `systemd --user`'s own well-known per-user unit search path — the
/// `--unit-dir` default when the flag is omitted.
pub fn default_unit_dir(home: &Path) -> PathBuf {
    home.join(".config").join("systemd").join("user")
}

/// Derives the unit "name" from the flow file's own basename, so this
/// command never has to read and JSON-parse the flow file just to name
/// its own unit files. By convention every flow file in this codebase is
/// already named `<flow.name>.flow.json` (see
/// `tome_flow::flow::runner::tests`'s own `write_flow` fixture helper),
/// so stripping that suffix recovers the flow's name directly. Sanitized
/// the same way `flow::runner`'s own `log_name` sanitizes a node id into
/// a filename fragment: only `[A-Za-z0-9._-]` survive, so nothing in a
/// hand-renamed or unusual flow filename can smuggle a path separator (or
/// a `..` segment) into a systemd unit filename built from it.
pub fn unit_stem(flow_path: &Path) -> String {
    let file_name = flow_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("flow");
    let base = file_name
        .strip_suffix(".flow.json")
        .or_else(|| file_name.strip_suffix(".json"))
        .unwrap_or(file_name);
    let sanitized: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Not just "is empty" — a name that sanitizes to nothing BUT
    // underscores (e.g. "***.flow.json") is just as unhelpful in
    // `systemctl list-timers` as a literally empty one; "flow" is a
    // clearer unit name than "___" in both cases.
    if sanitized.chars().any(|c| c.is_ascii_alphanumeric()) {
        sanitized
    } else {
        "flow".to_string()
    }
}

pub fn service_unit_name(stem: &str) -> String {
    format!("tome-flow-{stem}.service")
}

pub fn timer_unit_name(stem: &str) -> String {
    format!("tome-flow-{stem}.timer")
}

/// `tome-flow-<stem>.service` text. `Type=oneshot` — systemd waits for
/// `ExecStart` to exit before considering the unit "done," which is what
/// makes the paired timer's `Persistent=true` meaningful (a run still in
/// flight when the next `OnCalendar` tick lands is never started a second
/// time on top of itself; see [`timer_unit`]). `tome_runner_bin`/
/// `flow_json` must already be absolute paths — `ExecStart=` does not
/// search `$PATH` and has no notion of "relative to the unit file" or "the
/// directory this command was typed from." `env_file` is
/// `EnvironmentFile=`, not `Environment=`: provider credentials belong in
/// a file the server owner controls the permissions of, never inlined
/// into a unit file that `systemctl cat`/`journalctl` can echo back
/// (see `docs/remote-runner.md`).
pub fn service_unit(tome_runner_bin: &Path, flow_json: &Path, env_file: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Tome flow run (tome-runner)\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={} run {}\n\
         EnvironmentFile={}\n",
        tome_runner_bin.display(),
        flow_json.display(),
        env_file.display(),
    )
}

/// `tome-flow-<stem>.timer` text. `Persistent=true` is systemd's own
/// documented "run once at the next opportunity if a tick was missed
/// while the unit wasn't loaded" behavior (a reboot, a maintenance
/// window) — not this binary's own logic, and a DIFFERENT policy from the
/// in-app desktop scheduler's deliberate no-catch-up choice
/// (`src-tauri/src/schedule.rs`'s own doc comment: a missed slot there is
/// usually a locked screen, not downtime). A server that was off is
/// usually down for maintenance, not "paused" — catching up once, rather
/// than silently skipping to the next natural tick, is the right default
/// here.
pub fn timer_unit(on_calendar: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Tome flow schedule (tome-runner)\n\
         \n\
         [Timer]\n\
         OnCalendar={on_calendar}\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    )
}

/// Everything [`install`] needs to actually write to disk — assembled
/// separately from `install` itself so a test can assert on the plan
/// without touching a filesystem.
pub struct InstallPlan {
    pub unit_dir: PathBuf,
    pub service_name: String,
    pub service_text: String,
    pub timer_name: String,
    pub timer_text: String,
}

/// Assembles an [`InstallPlan`] from already-resolved, already-absolute
/// inputs — resolving `$HOME`, canonicalizing the flow path, and applying
/// the `--unit-dir` default are this module's own [`run`] function's job
/// (below), kept out of this pure builder for the same reason
/// `unit_stem`/`service_unit`/`timer_unit` are pure.
pub fn plan(
    tome_runner_bin: &Path,
    flow_json_abs: &Path,
    env_file: &Path,
    on_calendar: &str,
    unit_dir: PathBuf,
) -> InstallPlan {
    let stem = unit_stem(flow_json_abs);
    InstallPlan {
        service_name: service_unit_name(&stem),
        service_text: service_unit(tome_runner_bin, flow_json_abs, env_file),
        timer_name: timer_unit_name(&stem),
        timer_text: timer_unit(on_calendar),
        unit_dir,
    }
}

/// Writes both unit files into `plan.unit_dir` (creating it if needed) and
/// prints the exact `systemctl`/`loginctl` commands the server owner
/// still has to run themselves. This binary deliberately never shells out
/// to `systemctl` on its own behalf: enabling a unit is a privileged,
/// persistent change to what runs unattended on this machine, and that
/// stays an explicit, typed-by-a-human action — a tool that can install
/// its OWN autostart is one compromise away from a tool that re-installs
/// itself.
pub fn install(plan: &InstallPlan) -> std::io::Result<()> {
    std::fs::create_dir_all(&plan.unit_dir)?;
    std::fs::write(plan.unit_dir.join(&plan.service_name), &plan.service_text)?;
    std::fs::write(plan.unit_dir.join(&plan.timer_name), &plan.timer_text)?;
    println!(
        "Installed {} and {} in {}",
        plan.service_name,
        plan.timer_name,
        plan.unit_dir.display()
    );
    println!();
    println!("Next steps:");
    println!("  systemctl --user daemon-reload");
    println!("  systemctl --user enable --now {}", plan.timer_name);
    println!("  loginctl enable-linger \"$USER\"   # keep the timer running after you log out");
    Ok(())
}

/// `schedule install`'s full dispatch: resolves the absolute paths
/// [`plan`] needs, then calls [`install`]. Returns `tome-runner`'s process
/// exit code (2 for a configuration problem — a flow file that doesn't
/// exist, an unresolvable `$HOME`/binary path, or a write failure; there
/// is no "run" to fail or cancel here, so 0 or 1 never apply).
pub fn run(flow_path: &Path, on_calendar: &str, unit_dir_arg: Option<PathBuf>) -> i32 {
    let flow_json_abs = match std::fs::canonicalize(flow_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "tome-runner: could not resolve flow file {}: {e}",
                flow_path.display()
            );
            return 2;
        }
    };
    let tome_runner_bin = match std::env::current_exe().and_then(std::fs::canonicalize) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("tome-runner: could not resolve this binary's own absolute path: {e}");
            return 2;
        }
    };
    let Some(home) = crate::home::home_dir() else {
        eprintln!(
            "tome-runner: $HOME is not set — cannot resolve ~/.config/tome-runner/env or the default --unit-dir"
        );
        return 2;
    };
    let env_file = crate::home::config_dir(&home).join("env");
    let unit_dir = unit_dir_arg.unwrap_or_else(|| default_unit_dir(&home));

    let install_plan = plan(
        &tome_runner_bin,
        &flow_json_abs,
        &env_file,
        on_calendar,
        unit_dir,
    );
    match install(&install_plan) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("tome-runner: could not install the schedule: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- unit_stem ----

    #[test]
    fn unit_stem_strips_the_dot_flow_dot_json_suffix() {
        assert_eq!(
            unit_stem(Path::new("/srv/repo/.tome/flows/nightly.flow.json")),
            "nightly"
        );
    }

    #[test]
    fn unit_stem_falls_back_to_stripping_plain_dot_json() {
        assert_eq!(unit_stem(Path::new("nightly.json")), "nightly");
    }

    #[test]
    fn unit_stem_sanitizes_characters_outside_the_safe_set() {
        assert_eq!(unit_stem(Path::new("my flow!.flow.json")), "my_flow_");
    }

    #[test]
    fn unit_stem_falls_back_to_flow_when_sanitization_empties_the_name() {
        assert_eq!(unit_stem(Path::new("***.flow.json")), "flow");
    }

    #[test]
    fn unit_stem_keeps_dots_underscores_and_dashes() {
        assert_eq!(
            unit_stem(Path::new("nightly-build_v2.flow.json")),
            "nightly-build_v2"
        );
    }

    // ---- unit names ----

    #[test]
    fn service_and_timer_names_follow_the_tome_flow_prefix_convention() {
        assert_eq!(service_unit_name("nightly"), "tome-flow-nightly.service");
        assert_eq!(timer_unit_name("nightly"), "tome-flow-nightly.timer");
    }

    // ---- service_unit / timer_unit (exact text) ----

    #[test]
    fn service_unit_matches_the_exact_expected_text() {
        let text = service_unit(
            Path::new("/opt/tome/bin/tome-runner"),
            Path::new("/srv/repo/.tome/flows/nightly.flow.json"),
            Path::new("/home/tester/.config/tome-runner/env"),
        );
        assert_eq!(
            text,
            "[Unit]\n\
             Description=Tome flow run (tome-runner)\n\
             \n\
             [Service]\n\
             Type=oneshot\n\
             ExecStart=/opt/tome/bin/tome-runner run /srv/repo/.tome/flows/nightly.flow.json\n\
             EnvironmentFile=/home/tester/.config/tome-runner/env\n"
        );
    }

    #[test]
    fn timer_unit_matches_the_exact_expected_text() {
        let text = timer_unit("*-*-* 03:00:00");
        assert_eq!(
            text,
            "[Unit]\n\
             Description=Tome flow schedule (tome-runner)\n\
             \n\
             [Timer]\n\
             OnCalendar=*-*-* 03:00:00\n\
             Persistent=true\n\
             \n\
             [Install]\n\
             WantedBy=timers.target\n"
        );
    }

    #[test]
    fn timer_unit_embeds_the_on_calendar_expression_verbatim() {
        let text = timer_unit("Mon..Fri 09:00");
        assert!(text.contains("OnCalendar=Mon..Fri 09:00\n"));
    }

    // ---- default_unit_dir ----

    #[test]
    fn default_unit_dir_is_the_systemd_user_convention() {
        assert_eq!(
            default_unit_dir(Path::new("/home/tester")),
            PathBuf::from("/home/tester/.config/systemd/user")
        );
    }

    // ---- plan ----

    #[test]
    fn plan_assembles_names_and_text_consistently() {
        let p = plan(
            Path::new("/opt/tome/bin/tome-runner"),
            Path::new("/srv/repo/.tome/flows/nightly.flow.json"),
            Path::new("/home/tester/.config/tome-runner/env"),
            "daily",
            PathBuf::from("/home/tester/.config/systemd/user"),
        );
        assert_eq!(p.service_name, "tome-flow-nightly.service");
        assert_eq!(p.timer_name, "tome-flow-nightly.timer");
        assert_eq!(
            p.unit_dir,
            PathBuf::from("/home/tester/.config/systemd/user")
        );
        assert!(p.service_text.contains(
            "ExecStart=/opt/tome/bin/tome-runner run /srv/repo/.tome/flows/nightly.flow.json"
        ));
        assert!(p.timer_text.contains("OnCalendar=daily"));
    }

    // ---- install (real filesystem, hand-rolled scratch dir) ----

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tome-runner-schedule-test-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir
    }

    #[test]
    fn install_writes_both_unit_files_with_their_exact_text() {
        let unit_dir = scratch_dir("install");
        let p = plan(
            Path::new("/opt/tome/bin/tome-runner"),
            Path::new("/srv/repo/.tome/flows/nightly.flow.json"),
            Path::new("/home/tester/.config/tome-runner/env"),
            "daily",
            unit_dir.clone(),
        );
        install(&p).unwrap();
        assert_eq!(
            std::fs::read_to_string(unit_dir.join("tome-flow-nightly.service")).unwrap(),
            p.service_text
        );
        assert_eq!(
            std::fs::read_to_string(unit_dir.join("tome-flow-nightly.timer")).unwrap(),
            p.timer_text
        );
        let _ = std::fs::remove_dir_all(&unit_dir);
    }

    #[test]
    fn install_creates_the_unit_dir_when_it_does_not_exist_yet() {
        let unit_dir = scratch_dir("install-mkdir").join("nested").join("dir");
        assert!(!unit_dir.exists());
        let p = plan(
            Path::new("/opt/tome/bin/tome-runner"),
            Path::new("/srv/repo/x.flow.json"),
            Path::new("/home/tester/.config/tome-runner/env"),
            "hourly",
            unit_dir.clone(),
        );
        install(&p).unwrap();
        assert!(unit_dir.join("tome-flow-x.service").exists());
        let _ = std::fs::remove_dir_all(unit_dir.ancestors().nth(2).unwrap());
    }

    // ---- run (dispatch-level error paths that don't need a real flow file) ----

    #[test]
    fn run_reports_a_missing_flow_file_as_exit_2() {
        let code = run(
            Path::new("/definitely/not/a/real/flow-xyz.flow.json"),
            "daily",
            None,
        );
        assert_eq!(code, 2);
    }
}
