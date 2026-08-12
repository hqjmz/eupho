use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use eupho::application::{DoctorOptions, OnceOptions, StatusOptions, doctor, observe_once, status};
use eupho::instructions::{InstructionSource, LinkAction, link_instructions};
use eupho::terminal::terminal_text;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "eupho",
    version,
    about = "GitHub-native control plane for coding agents",
    long_about = "Eupho is a GitHub-native control plane for coding agents.\n\nPhase 1 is observe-only: GitHub-backed commands never change GitHub state."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate local configuration and optional GitHub merge policy.
    Doctor {
        #[arg(long, value_name = "OWNER/REPO")]
        repo: Option<String>,
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        #[arg(long = "host-config", value_name = "PATH")]
        host_config: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Perform one read-only reconciliation and record its candidate plan.
    Once {
        #[arg(long, value_name = "OWNER/REPO", required = true)]
        repo: String,
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        #[arg(long = "host-config", value_name = "PATH")]
        host_config: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Show durable local observe-only snapshots.
    Status {
        #[arg(long = "state-root", value_name = "PATH")]
        state_root: Option<PathBuf>,
        #[arg(long = "host-config", value_name = "PATH")]
        host_config: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Manage repository instruction files shared by coding agents.
    Instructions {
        #[command(subcommand)]
        command: InstructionsCommand,
    },
}

#[derive(Debug, Subcommand)]
enum InstructionsCommand {
    /// Create a safe relative link between AGENTS.md and CLAUDE.md.
    Link {
        /// Canonical source file: agents creates CLAUDE.md -> AGENTS.md.
        #[arg(long, default_value = "agents", value_name = "agents|claude")]
        source: InstructionSource,
        /// A path inside the local Git repository.
        #[arg(
            long,
            visible_alias = "repo-root",
            default_value = ".",
            value_name = "PATH"
        )]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let json_requested = env::args_os().any(|argument| argument == "--json");
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if json_requested {
                let rendered = serde_json::json!({
                    "ok": false,
                    "error": { "code": error.code, "message": error.message },
                });
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&rendered).expect("JSON value")
                );
            } else {
                eprintln!("eupho: {}", safe_text(&error.message));
            }
            ExitCode::from(error.exit_code)
        }
    }
}

fn run() -> Result<u8, CliError> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            print!("{error}");
            return Ok(0);
        }
        Err(error) => {
            let code = clap_error_code(error.kind());
            return Err(CliError::usage(code, error));
        }
    };
    let Some(command) = cli.command else {
        Cli::command()
            .print_help()
            .map_err(|error| CliError::runtime("help_failed", error))?;
        println!();
        return Ok(0);
    };
    let cwd = env::current_dir().map_err(|error| CliError::runtime("cwd_failed", error))?;

    match command {
        Command::Doctor {
            repo,
            config,
            host_config,
            json,
        } => {
            let report = doctor(DoctorOptions {
                cwd,
                repository: repo,
                config_path: config,
                host_config_path: host_config,
            })
            .map_err(CliError::application)?;
            write_result(&report, json, render_doctor)?;
            Ok(u8::from(!report.ok))
        }
        Command::Once {
            repo,
            config,
            host_config,
            json,
        } => {
            let report = observe_once(OnceOptions {
                cwd,
                repository: repo,
                config_path: config,
                host_config_path: host_config,
            })
            .map_err(CliError::application)?;
            write_result(&report, json, render_once)?;
            Ok(0)
        }
        Command::Status {
            state_root,
            host_config,
            json,
        } => {
            let report = status(StatusOptions {
                cwd,
                state_root,
                host_config_path: host_config,
            })
            .map_err(CliError::application)?;
            write_result(&report, json, render_status)?;
            Ok(0)
        }
        Command::Instructions { command } => match command {
            InstructionsCommand::Link { source, path, json } => {
                let outcome = link_instructions(&path, source).map_err(|error| CliError {
                    code: "instruction_link_failed".to_owned(),
                    message: error.to_string(),
                    exit_code: 1,
                })?;
                let view = InstructionLinkView::from(&outcome);
                write_result(&view, json, render_instruction_link)?;
                Ok(0)
            }
        },
    }
}

fn write_result<T: Serialize>(
    value: &T,
    json: bool,
    render: fn(&T) -> String,
) -> Result<(), CliError> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(value)
                .map_err(|error| CliError::runtime("json_render_failed", error))?
        );
    } else {
        println!("{}", render(value));
    }
    Ok(())
}

fn render_doctor(report: &eupho::application::DoctorReport) -> String {
    let mut lines = vec!["Eupho doctor".to_owned(), String::new()];
    for check in &report.checks {
        lines.push(format!(
            "[{}] {}: {}",
            check.status.as_uppercase(),
            safe_text(&check.code),
            safe_text(&check.message)
        ));
        if let Some(remediation) = &check.remediation {
            lines.push(format!("       {}", safe_text(remediation)));
        }
    }
    lines.push(String::new());
    lines.push(if report.ok {
        "Ready for the checked capabilities.".to_owned()
    } else {
        "Preflight failed. Resolve the failures above.".to_owned()
    });
    lines.join("\n")
}

fn render_once(report: &eupho::application::OnceReport) -> String {
    let snapshot = &report.snapshot;
    let mut lines = vec![
        "Eupho observe-only pass".to_owned(),
        String::new(),
        format!("Repository: {}", safe_text(&snapshot.repository)),
        format!("Base:       {}", safe_text(&snapshot.base_sha)),
        format!(
            "Policy:     {}{}",
            safe_text(&snapshot.policy_source),
            if snapshot.trusted_base {
                " (trusted base)"
            } else {
                " (local override)"
            }
        ),
        format!("Observed:   {}", safe_text(&snapshot.observed_at)),
        String::new(),
    ];
    if snapshot.candidates.is_empty() {
        lines.push("No eligible issues would be claimed.".to_owned());
    } else {
        lines.push(format!(
            "{} issue(s) would be claimed:",
            snapshot.candidates.len()
        ));
        for candidate in &snapshot.candidates {
            lines.push(format!(
                "  #{} {}",
                candidate.issue_number,
                terminal_text(&candidate.issue_title, 300)
            ));
            lines.push(format!(
                "    {} / {} / {}",
                execution_mode_name(candidate.execution_mode),
                workspace_type_name(candidate.workspace_type),
                merge_policy_name(candidate.merge_policy)
            ));
        }
    }
    if !snapshot.diagnostics.is_empty() {
        lines.push(String::new());
        lines.push("Diagnostics:".to_owned());
        for diagnostic in &snapshot.diagnostics {
            lines.push(format!(
                "  #{} {}: {}",
                diagnostic.issue_number,
                safe_text(&diagnostic.code),
                safe_text(&diagnostic.message)
            ));
        }
    }
    lines.push(String::new());
    lines.push("No GitHub state was changed.".to_owned());
    lines.join("\n")
}

const fn execution_mode_name(mode: eupho::config::ExecutionMode) -> &'static str {
    match mode {
        eupho::config::ExecutionMode::Attended => "attended",
        eupho::config::ExecutionMode::Unattended => "unattended",
    }
}

const fn workspace_type_name(workspace: eupho::config::WorkspaceType) -> &'static str {
    match workspace {
        eupho::config::WorkspaceType::Worktree => "worktree",
        eupho::config::WorkspaceType::EphemeralClone => "ephemeral_clone",
    }
}

const fn merge_policy_name(policy: eupho::config::MergePolicy) -> &'static str {
    match policy {
        eupho::config::MergePolicy::AutonomousLowRisk => "autonomous-low-risk",
        eupho::config::MergePolicy::HumanFinalApproval => "human-final-approval",
        eupho::config::MergePolicy::SuggestOnly => "suggest-only",
    }
}

fn render_status(report: &eupho::application::StatusReport) -> String {
    let mut lines = vec![
        "Eupho local status".to_owned(),
        String::new(),
        format!(
            "State root: {}",
            safe_text(&report.state_root.display().to_string())
        ),
        String::new(),
    ];
    if report.repositories.is_empty() {
        lines.push("No observed repository snapshots.".to_owned());
    } else {
        for repository in &report.repositories {
            lines.push(format!(
                "{} @ {}",
                safe_text(&repository.repository),
                safe_text(&repository.base_sha)
            ));
            lines.push(format!(
                "  observed {}; {} candidate(s), {} diagnostic(s)",
                safe_text(&repository.observed_at),
                repository.candidates.len(),
                repository.diagnostics.len()
            ));
        }
    }
    lines.join("\n")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstructionLinkView {
    ok: bool,
    action: &'static str,
    repository_root: String,
    source: &'static str,
    source_path: String,
    destination_path: String,
    link_target: String,
}

impl From<&eupho::instructions::LinkOutcome> for InstructionLinkView {
    fn from(outcome: &eupho::instructions::LinkOutcome) -> Self {
        Self {
            ok: true,
            action: match outcome.action {
                LinkAction::Created => "created",
                LinkAction::AlreadyLinked => "already_linked",
            },
            repository_root: outcome.repository_root.display().to_string(),
            source: match outcome.source {
                InstructionSource::Agents => "agents",
                InstructionSource::Claude => "claude",
            },
            source_path: outcome.source_path.display().to_string(),
            destination_path: outcome.destination_path.display().to_string(),
            link_target: outcome.link_target.display().to_string(),
        }
    }
}

fn render_instruction_link(view: &InstructionLinkView) -> String {
    match view.action {
        "created" => format!(
            "Linked {} -> {} (canonical source: {}).",
            safe_text(&view.destination_path),
            safe_text(&view.link_target),
            safe_text(&view.source_path)
        ),
        _ => format!(
            "Instruction link already correct: {} -> {}.",
            safe_text(&view.destination_path),
            safe_text(&view.link_target)
        ),
    }
}

fn safe_text(value: &str) -> String {
    terminal_text(value, 1_000)
}

const fn clap_error_code(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::InvalidSubcommand => "unknown_command",
        ErrorKind::UnknownArgument => "unknown_option",
        ErrorKind::MissingRequiredArgument => "missing_required_option",
        ErrorKind::MissingSubcommand => "missing_command",
        ErrorKind::ArgumentConflict => "duplicate_or_conflicting_option",
        ErrorKind::InvalidValue | ErrorKind::ValueValidation | ErrorKind::InvalidUtf8 => {
            "invalid_option_value"
        }
        ErrorKind::NoEquals
        | ErrorKind::TooManyValues
        | ErrorKind::TooFewValues
        | ErrorKind::WrongNumberOfValues => "invalid_option_arity",
        _ => "invalid_arguments",
    }
}

struct CliError {
    code: String,
    message: String,
    exit_code: u8,
}

impl CliError {
    fn usage(code: &str, error: impl std::fmt::Display) -> Self {
        Self {
            code: code.to_owned(),
            message: error.to_string(),
            exit_code: 2,
        }
    }

    fn runtime(code: &str, error: impl std::fmt::Display) -> Self {
        Self {
            code: code.to_owned(),
            message: error.to_string(),
            exit_code: 1,
        }
    }

    fn application(error: eupho::application::ApplicationError) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string(),
            exit_code: error.exit_code(),
        }
    }
}
