//! `tome-runner`'s command-line contract — hand-parsed, mirroring
//! `crates/tome-shim/src/args.rs`'s own precedent (this workspace has no
//! `clap` dependency, and this slice's grant doesn't add one; see
//! `Cargo.toml`'s note). Unlike `tome-shim`'s argv (built entirely by one
//! trusted caller, never typed by a human — see that module's doc
//! comment), THIS argv genuinely is typed by a human at a server's
//! terminal, or written once into a systemd unit file by [`crate::schedule_cmd`]
//! and re-read from there every timer tick — so error messages here are
//! judged by "does this tell an operator what to fix," not merely
//! "does this tell a caller which line of Rust is wrong."
//!
//! Wire contract:
//!
//! ```text
//! tome-runner run <flow.json>
//! tome-runner schedule install <flow.json> --on-calendar <expr> [--unit-dir <dir>]
//! ```

use std::path::PathBuf;

/// The fully-parsed, validated shape of `tome-runner`'s argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `run <flow.json>` — execute one flow to completion.
    Run { flow_path: PathBuf },
    /// `schedule install <flow.json> --on-calendar <expr> [--unit-dir <dir>]`
    /// — write a systemd `--user` service+timer pair. `unit_dir: None`
    /// means the flag was omitted; [`crate::schedule_cmd`] applies
    /// [`crate::schedule_cmd::default_unit_dir`] in that case — resolving
    /// the default here would require reading `$HOME` inside argv
    /// parsing, which stays hermetic on purpose (see this module's own
    /// tests, none of which touch the environment).
    ScheduleInstall {
        flow_path: PathBuf,
        on_calendar: String,
        unit_dir: Option<PathBuf>,
    },
}

/// Every way [`parse_args`] can refuse an argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    /// No argv at all past the program name.
    MissingCommand,
    /// The first token wasn't `run` or `schedule`.
    UnknownCommand(String),
    /// `schedule` with nothing after it.
    MissingScheduleSubcommand,
    /// `schedule <x>` where `<x>` isn't `install`.
    UnknownScheduleSubcommand(String),
    /// `run`/`schedule install` reached the end with no flow-file
    /// positional.
    MissingFlowPath,
    /// A second bare positional showed up where only one was expected.
    UnexpectedArgument(String),
    /// A flag that takes a value was the last token, or was immediately
    /// followed by another flag instead of a value.
    MissingValue(&'static str),
    /// `schedule install` reached the end without a required flag.
    MissingFlag(&'static str),
    /// A `--`-prefixed token this command doesn't recognize.
    UnknownFlag(String),
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::MissingCommand => {
                write!(f, "missing command (expected \"run\" or \"schedule\")")
            }
            ArgError::UnknownCommand(c) => write!(
                f,
                "unknown command {c:?} (expected \"run\" or \"schedule\")"
            ),
            ArgError::MissingScheduleSubcommand => {
                write!(f, "\"schedule\" needs a subcommand (expected \"install\")")
            }
            ArgError::UnknownScheduleSubcommand(s) => write!(
                f,
                "unknown \"schedule\" subcommand {s:?} (expected \"install\")"
            ),
            ArgError::MissingFlowPath => write!(f, "missing the flow file path"),
            ArgError::UnexpectedArgument(a) => write!(f, "unexpected extra argument {a:?}"),
            ArgError::MissingValue(flag) => write!(f, "{flag} requires a value"),
            ArgError::MissingFlag(flag) => write!(f, "missing required flag {flag}"),
            ArgError::UnknownFlag(flag) => write!(f, "unrecognized flag {flag:?}"),
        }
    }
}

impl std::error::Error for ArgError {}

/// Parses `tome-runner`'s argv per this module's top doc comment. `args`
/// excludes `argv[0]` (the program name) — callers pass
/// `std::env::args().skip(1)`, same convention `tome-shim`'s own
/// `parse_args` uses.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Command, ArgError> {
    let mut iter = args.into_iter();
    let command = iter.next().ok_or(ArgError::MissingCommand)?;
    match command.as_str() {
        "run" => parse_run(iter),
        "schedule" => parse_schedule(iter),
        other => Err(ArgError::UnknownCommand(other.to_string())),
    }
}

fn parse_run<I: Iterator<Item = String>>(iter: I) -> Result<Command, ArgError> {
    let flow_path = parse_one_positional(iter)?;
    Ok(Command::Run { flow_path })
}

fn parse_schedule<I: Iterator<Item = String>>(mut iter: I) -> Result<Command, ArgError> {
    match iter.next() {
        None => Err(ArgError::MissingScheduleSubcommand),
        Some(sub) if sub == "install" => parse_schedule_install(iter),
        Some(other) => Err(ArgError::UnknownScheduleSubcommand(other)),
    }
}

/// Consumes every remaining token as a single bare positional — used by
/// `run`, which takes no flags at all: any `--`-prefixed token is
/// therefore always unrecognized, never a value-taking flag this parser
/// silently swallows.
fn parse_one_positional<I: Iterator<Item = String>>(iter: I) -> Result<PathBuf, ArgError> {
    let mut flow_path: Option<PathBuf> = None;
    for token in iter {
        if let Some(flag) = token.strip_prefix("--") {
            return Err(ArgError::UnknownFlag(format!("--{flag}")));
        }
        if flow_path.is_some() {
            return Err(ArgError::UnexpectedArgument(token));
        }
        flow_path = Some(PathBuf::from(token));
    }
    flow_path.ok_or(ArgError::MissingFlowPath)
}

/// `schedule install`'s own flags may appear in any order, and either
/// side of the one bare positional — same "trusted-enough, order doesn't
/// matter" posture `tome-shim::args::parse_args` takes for its own flags.
fn parse_schedule_install<I: Iterator<Item = String>>(mut iter: I) -> Result<Command, ArgError> {
    let mut flow_path: Option<PathBuf> = None;
    let mut on_calendar: Option<String> = None;
    let mut unit_dir: Option<PathBuf> = None;

    while let Some(token) = iter.next() {
        match token.as_str() {
            "--on-calendar" => {
                let v = iter.next().ok_or(ArgError::MissingValue("--on-calendar"))?;
                on_calendar = Some(v);
            }
            "--unit-dir" => {
                let v = iter.next().ok_or(ArgError::MissingValue("--unit-dir"))?;
                unit_dir = Some(PathBuf::from(v));
            }
            t if t.starts_with("--") => return Err(ArgError::UnknownFlag(t.to_string())),
            _ => {
                if flow_path.is_some() {
                    return Err(ArgError::UnexpectedArgument(token));
                }
                flow_path = Some(PathBuf::from(token));
            }
        }
    }

    // Checked in a fixed order (flow path, then --on-calendar) so "both
    // are missing" reports one deterministic reason rather than whichever
    // the loop happened to notice last — same discipline tome-shim's own
    // `a_completely_empty_argv_reports_the_first_missing_flag_checked_not_the_separator`
    // pins for its parser.
    let flow_path = flow_path.ok_or(ArgError::MissingFlowPath)?;
    let on_calendar = on_calendar.ok_or(ArgError::MissingFlag("--on-calendar"))?;
    Ok(Command::ScheduleInstall {
        flow_path,
        on_calendar,
        unit_dir,
    })
}

/// Short usage synopsis — printed by `main.rs` alongside any [`ArgError`].
pub const USAGE: &str = "usage:\n  \
    tome-runner run <flow.json>\n  \
    tome-runner schedule install <flow.json> --on-calendar <expr> [--unit-dir <dir>]";

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // ---- run ----

    #[test]
    fn parses_run_with_a_flow_path() {
        assert_eq!(
            parse_args(v(&["run", "flow.json"])).unwrap(),
            Command::Run {
                flow_path: PathBuf::from("flow.json")
            }
        );
    }

    #[test]
    fn parses_run_with_an_absolute_flow_path() {
        assert_eq!(
            parse_args(v(&["run", "/srv/repo/.tome/flows/nightly.flow.json"])).unwrap(),
            Command::Run {
                flow_path: PathBuf::from("/srv/repo/.tome/flows/nightly.flow.json")
            }
        );
    }

    #[test]
    fn run_with_no_flow_path_is_missing_flow_path() {
        assert_eq!(parse_args(v(&["run"])), Err(ArgError::MissingFlowPath));
    }

    #[test]
    fn run_rejects_any_flag_it_has_none_of_its_own() {
        assert_eq!(
            parse_args(v(&["run", "--force", "flow.json"])),
            Err(ArgError::UnknownFlag("--force".to_string()))
        );
    }

    #[test]
    fn run_rejects_a_second_positional() {
        assert_eq!(
            parse_args(v(&["run", "a.flow.json", "b.flow.json"])),
            Err(ArgError::UnexpectedArgument("b.flow.json".to_string()))
        );
    }

    // ---- schedule install ----

    #[test]
    fn parses_schedule_install_with_the_required_flag_only() {
        assert_eq!(
            parse_args(v(&[
                "schedule",
                "install",
                "flow.json",
                "--on-calendar",
                "daily"
            ]))
            .unwrap(),
            Command::ScheduleInstall {
                flow_path: PathBuf::from("flow.json"),
                on_calendar: "daily".to_string(),
                unit_dir: None,
            }
        );
    }

    #[test]
    fn parses_schedule_install_with_unit_dir_too() {
        assert_eq!(
            parse_args(v(&[
                "schedule",
                "install",
                "flow.json",
                "--on-calendar",
                "*-*-* 03:00:00",
                "--unit-dir",
                "/etc/systemd/user"
            ]))
            .unwrap(),
            Command::ScheduleInstall {
                flow_path: PathBuf::from("flow.json"),
                on_calendar: "*-*-* 03:00:00".to_string(),
                unit_dir: Some(PathBuf::from("/etc/systemd/user")),
            }
        );
    }

    #[test]
    fn schedule_install_flags_may_come_before_the_positional() {
        assert_eq!(
            parse_args(v(&[
                "schedule",
                "install",
                "--on-calendar",
                "daily",
                "flow.json"
            ]))
            .unwrap(),
            Command::ScheduleInstall {
                flow_path: PathBuf::from("flow.json"),
                on_calendar: "daily".to_string(),
                unit_dir: None,
            }
        );
    }

    #[test]
    fn schedule_install_flags_may_be_interleaved_around_the_positional() {
        assert_eq!(
            parse_args(v(&[
                "schedule",
                "install",
                "--unit-dir",
                "/x",
                "flow.json",
                "--on-calendar",
                "daily"
            ]))
            .unwrap(),
            Command::ScheduleInstall {
                flow_path: PathBuf::from("flow.json"),
                on_calendar: "daily".to_string(),
                unit_dir: Some(PathBuf::from("/x")),
            }
        );
    }

    #[test]
    fn schedule_install_missing_on_calendar_is_reported_by_name() {
        assert_eq!(
            parse_args(v(&["schedule", "install", "flow.json"])),
            Err(ArgError::MissingFlag("--on-calendar"))
        );
    }

    #[test]
    fn schedule_install_missing_flow_path_is_reported_even_with_on_calendar_present() {
        assert_eq!(
            parse_args(v(&["schedule", "install", "--on-calendar", "daily"])),
            Err(ArgError::MissingFlowPath)
        );
    }

    #[test]
    fn schedule_install_missing_both_reports_flow_path_first() {
        // Pinned deterministic order — see parse_schedule_install's own
        // comment on why flow_path is checked before --on-calendar.
        assert_eq!(
            parse_args(v(&["schedule", "install"])),
            Err(ArgError::MissingFlowPath)
        );
    }

    #[test]
    fn schedule_install_rejects_an_unknown_flag() {
        assert_eq!(
            parse_args(v(&[
                "schedule",
                "install",
                "flow.json",
                "--on-calendar",
                "daily",
                "--bogus"
            ])),
            Err(ArgError::UnknownFlag("--bogus".to_string()))
        );
    }

    #[test]
    fn schedule_install_rejects_a_second_positional() {
        assert_eq!(
            parse_args(v(&[
                "schedule",
                "install",
                "a.flow.json",
                "b.flow.json",
                "--on-calendar",
                "daily"
            ])),
            Err(ArgError::UnexpectedArgument("b.flow.json".to_string()))
        );
    }

    #[test]
    fn schedule_install_errors_when_on_calendar_is_the_last_token_with_no_value() {
        assert_eq!(
            parse_args(v(&["schedule", "install", "flow.json", "--on-calendar"])),
            Err(ArgError::MissingValue("--on-calendar"))
        );
    }

    #[test]
    fn schedule_install_errors_when_unit_dir_is_the_last_token_with_no_value() {
        assert_eq!(
            parse_args(v(&[
                "schedule",
                "install",
                "flow.json",
                "--on-calendar",
                "daily",
                "--unit-dir"
            ])),
            Err(ArgError::MissingValue("--unit-dir"))
        );
    }

    #[test]
    fn last_value_wins_when_on_calendar_is_repeated() {
        let parsed = parse_args(v(&[
            "schedule",
            "install",
            "flow.json",
            "--on-calendar",
            "hourly",
            "--on-calendar",
            "daily",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            Command::ScheduleInstall {
                flow_path: PathBuf::from("flow.json"),
                on_calendar: "daily".to_string(),
                unit_dir: None,
            }
        );
    }

    // ---- schedule subcommand dispatch ----

    #[test]
    fn schedule_with_no_subcommand_is_reported_by_name() {
        assert_eq!(
            parse_args(v(&["schedule"])),
            Err(ArgError::MissingScheduleSubcommand)
        );
    }

    #[test]
    fn schedule_with_an_unknown_subcommand_is_reported_by_name() {
        assert_eq!(
            parse_args(v(&["schedule", "uninstall"])),
            Err(ArgError::UnknownScheduleSubcommand("uninstall".to_string()))
        );
    }

    // ---- top-level dispatch / usage errors ----

    #[test]
    fn empty_argv_is_missing_command() {
        assert_eq!(parse_args(v(&[])), Err(ArgError::MissingCommand));
    }

    #[test]
    fn unknown_top_level_command_is_reported_by_name() {
        assert_eq!(
            parse_args(v(&["frobnicate"])),
            Err(ArgError::UnknownCommand("frobnicate".to_string()))
        );
    }

    // ---- Display ----

    #[test]
    fn display_messages_name_the_flag_or_command_involved() {
        assert!(ArgError::MissingCommand.to_string().contains("run"));
        assert!(ArgError::UnknownCommand("x".to_string())
            .to_string()
            .contains("\"x\""));
        assert!(ArgError::MissingScheduleSubcommand
            .to_string()
            .contains("install"));
        assert!(ArgError::UnknownScheduleSubcommand("x".to_string())
            .to_string()
            .contains("\"x\""));
        assert!(ArgError::MissingFlowPath.to_string().contains("flow"));
        assert!(ArgError::UnexpectedArgument("x".to_string())
            .to_string()
            .contains("\"x\""));
        assert!(ArgError::MissingValue("--on-calendar")
            .to_string()
            .contains("--on-calendar"));
        assert!(ArgError::MissingFlag("--on-calendar")
            .to_string()
            .contains("--on-calendar"));
        assert!(ArgError::UnknownFlag("--bogus".to_string())
            .to_string()
            .contains("--bogus"));
    }

    #[test]
    fn usage_mentions_both_subcommands() {
        assert!(USAGE.contains("tome-runner run"));
        assert!(USAGE.contains("tome-runner schedule install"));
        assert!(USAGE.contains("--on-calendar"));
    }
}
