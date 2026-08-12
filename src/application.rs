//! Observe-only application flows shared by the CLI and tests.

use std::env;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process_group};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::VERSION;
use crate::config::{
    ConfigError, HostConfig, MergePolicy, RepositoryConfig, find_repository_config,
    load_host_config, load_repository_config, parse_repository_config_text,
};
use crate::domain::{
    CandidateSnapshot, DomainError, IssueSnapshot, RepositorySnapshot, plan_candidates,
};
use crate::github::{BranchPolicySnapshot, GhReader, GitHubError};
use crate::infra::{
    CandidateStore, InfraError, RepositoryLock, default_state_root, find_git_worktree_root,
    resolve_safe_state_root, rfc3339_now,
};

#[derive(Debug)]
pub struct ApplicationError {
    code: &'static str,
    message: String,
    exit_code: u8,
}

impl ApplicationError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        self.exit_code
    }

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: 1,
        }
    }

    fn with_exit_code(mut self, exit_code: u8) -> Self {
        self.exit_code = exit_code;
        self
    }
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApplicationError {}

impl From<ConfigError> for ApplicationError {
    fn from(error: ConfigError) -> Self {
        Self::new("invalid_config", error.to_string())
    }
}

impl From<DomainError> for ApplicationError {
    fn from(error: DomainError) -> Self {
        Self::new("domain_error", error.to_string())
    }
}

impl From<GitHubError> for ApplicationError {
    fn from(error: GitHubError) -> Self {
        Self::new(error.code, error.to_string())
    }
}

impl From<InfraError> for ApplicationError {
    fn from(error: InfraError) -> Self {
        let exit_code = u8::try_from(error.exit_code).unwrap_or(1);
        Self::new(error.code, error.to_string()).with_exit_code(exit_code)
    }
}

pub trait GitHubRead {
    fn repository(
        &self,
        repository: &str,
        configured_base_branch: Option<&str>,
    ) -> Result<RepositorySnapshot, GitHubError>;
    fn ready_issues(
        &self,
        repository: &str,
        ready_label: &str,
        limit: usize,
    ) -> Result<Vec<IssueSnapshot>, GitHubError>;
    fn active_issue_numbers(
        &self,
        repository: &str,
        labels: &[String],
        limit: usize,
    ) -> Result<Vec<u64>, GitHubError>;
    fn label_exists(&self, repository: &str, label: &str) -> Result<bool, GitHubError>;
    fn branch_policy(
        &self,
        repository: &str,
        branch: &str,
    ) -> Result<BranchPolicySnapshot, GitHubError>;
}

impl GitHubRead for GhReader {
    fn repository(
        &self,
        repository: &str,
        configured_base_branch: Option<&str>,
    ) -> Result<RepositorySnapshot, GitHubError> {
        self.repository(repository, configured_base_branch)
    }

    fn ready_issues(
        &self,
        repository: &str,
        ready_label: &str,
        limit: usize,
    ) -> Result<Vec<IssueSnapshot>, GitHubError> {
        self.ready_issues(repository, ready_label, limit)
    }

    fn active_issue_numbers(
        &self,
        repository: &str,
        labels: &[String],
        limit: usize,
    ) -> Result<Vec<u64>, GitHubError> {
        self.active_issue_numbers(repository, labels, limit)
    }

    fn label_exists(&self, repository: &str, label: &str) -> Result<bool, GitHubError> {
        self.label_exists(repository, label)
    }

    fn branch_policy(
        &self,
        repository: &str,
        branch: &str,
    ) -> Result<BranchPolicySnapshot, GitHubError> {
        self.branch_policy(repository, branch)
    }
}

#[derive(Debug)]
pub struct ResolvedPolicy {
    pub repository: RepositorySnapshot,
    pub config: RepositoryConfig,
    pub source: String,
    pub trusted_base: bool,
}

pub fn resolve_policy<R: GitHubRead>(
    reader: &R,
    repository_name: &str,
    cwd: &Path,
    explicit_config: Option<&Path>,
) -> Result<ResolvedPolicy, ApplicationError> {
    if let Some(explicit_config) = explicit_config {
        let path = find_repository_config(cwd, Some(explicit_config))?;
        let config = load_repository_config(&path)?;
        let repository = reader.repository(repository_name, Some(&config.base_branch))?;
        return Ok(ResolvedPolicy {
            repository,
            config,
            source: path.display().to_string(),
            trusted_base: false,
        });
    }

    let mut repository = reader.repository(repository_name, None)?;
    let mut config = parse_remote_policy(&repository)?;
    if config.base_branch != repository.default_branch {
        let selected_base = config.base_branch.clone();
        repository = reader.repository(repository_name, Some(&selected_base))?;
        config = parse_remote_policy(&repository)?;
        if config.base_branch != selected_base {
            return Err(ApplicationError::new(
                "unstable_policy_base",
                format!(
                    "policy selected {selected_base}, but that branch selected {}",
                    config.base_branch
                ),
            ));
        }
    }
    let policy_path = repository.policy_path.as_deref().unwrap_or("<missing>");
    let source = format!(
        "github:{}/{policy_path}@{}",
        repository.name_with_owner, repository.base_sha
    );
    Ok(ResolvedPolicy {
        repository,
        config,
        source,
        trusted_base: true,
    })
}

fn parse_remote_policy(
    repository: &RepositorySnapshot,
) -> Result<RepositoryConfig, ApplicationError> {
    let (Some(path), Some(content)) = (&repository.policy_path, &repository.policy_content) else {
        return Err(ApplicationError::new(
            "remote_policy_not_found",
            format!(
                "no Eupho repository policy exists on {}@{}",
                repository.name_with_owner, repository.base_sha
            ),
        ));
    };
    let source = format!(
        "github:{}/{path}@{}",
        repository.name_with_owner, repository.base_sha
    );
    parse_repository_config_text(content, &source).map_err(Into::into)
}

#[derive(Debug)]
pub struct DoctorOptions {
    pub cwd: PathBuf,
    pub repository: Option<String>,
    pub config_path: Option<PathBuf>,
    pub host_config_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatus {
    Pass,
    Fail,
    Warn,
    Skip,
}

impl DiagnosticStatus {
    #[must_use]
    pub const fn as_uppercase(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Warn => "WARN",
            Self::Skip => "SKIP",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub status: DiagnosticStatus,
    pub message: String,
    pub remediation: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub ok: bool,
    pub repository: Option<String>,
    pub policy_source: Option<String>,
    pub checks: Vec<Diagnostic>,
}

pub fn doctor(options: DoctorOptions) -> Result<DoctorReport, ApplicationError> {
    let mut checks = vec![pass(
        "runtime.eupho",
        format!("Eupho {VERSION} is a native Rust binary"),
    )];
    checks.push(executable_check("runtime.git", "git"));
    checks.push(executable_check("runtime.gh", "gh"));

    let host_config = match &options.host_config_path {
        Some(path) => match load_host_config(path) {
            Ok(config) => {
                checks.push(pass(
                    "config.host",
                    format!("Loaded host configuration from {}", path.display()),
                ));
                Some(config)
            }
            Err(error) => {
                checks.push(fail(
                    "config.host",
                    error.to_string(),
                    "Fix the administrator-owned host configuration.",
                ));
                None
            }
        },
        None => {
            checks.push(warn(
                "config.host",
                "No host configuration supplied; local syntax checks can continue",
                "Pass --host-config before strict GitHub or unattended checks.",
            ));
            None
        }
    };

    let mut repository_config = None;
    let mut policy_source = None;
    if let Some(repository_name) = &options.repository {
        match env::var("EUPHO_DOCTOR_TOKEN") {
            Ok(token) if !token.is_empty() => {
                checks.push(pass(
                    "github.doctor_token",
                    "Separate operator diagnostic credential is present",
                ));
                let reader = GhReader::default().with_token(token);
                match resolve_policy(
                    &reader,
                    repository_name,
                    &options.cwd,
                    options.config_path.as_deref(),
                ) {
                    Ok(resolved) => {
                        policy_source = Some(resolved.source.clone());
                        checks.push(if resolved.trusted_base {
                            pass(
                                "config.repository",
                                format!("Loaded trusted base policy from {}", resolved.source),
                            )
                        } else {
                            warn(
                                "config.repository",
                                format!("Using local policy override {}", resolved.source),
                                "Omit --config for production preflight against the trusted base SHA.",
                            )
                        });
                        for (role, label) in [
                            ("ready", &resolved.config.labels.ready),
                            ("working", &resolved.config.labels.working),
                            ("review", &resolved.config.labels.review),
                            ("human", &resolved.config.labels.human),
                        ] {
                            match reader.label_exists(repository_name, label) {
                                Ok(true) => checks.push(pass(
                                    format!("github.label.{role}"),
                                    format!("{role} label {label} exists"),
                                )),
                                Ok(false) => checks.push(fail(
                                    format!("github.label.{role}"),
                                    format!("{role} label {label} does not exist"),
                                    "Create every configured workflow label before dispatch.",
                                )),
                                Err(error) => checks.push(fail(
                                    "github.read",
                                    error.to_string(),
                                    "Verify repository access and diagnostic permissions.",
                                )),
                            }
                        }
                        if let Some(host) = &host_config {
                            match reader.branch_policy(
                                repository_name,
                                &resolved.config.base_branch,
                            ) {
                                Ok(policy) => checks.extend(evaluate_branch_policy(
                                    &policy,
                                    &resolved.config,
                                    host.github_app.app_id,
                                )),
                                Err(error) => checks.push(fail(
                                    "github.read",
                                    error.to_string(),
                                    "Verify repository access and Administration:read permission.",
                                )),
                            }
                        } else {
                            checks.push(fail(
                                "github.branch_policy",
                                "Host configuration is required to verify the expected GitHub App source",
                                "Pass --host-config with the installed App ID.",
                            ));
                        }
                        repository_config = Some(resolved.config);
                    }
                    Err(error) => checks.push(fail(
                        "github.read",
                        error.to_string(),
                        "Verify repository access and diagnostic permissions.",
                    )),
                }
            }
            _ => checks.push(fail(
                "github.doctor_token",
                "EUPHO_DOCTOR_TOKEN is required for strict GitHub diagnostics",
                "Supply a separate operator token with repository read and Administration:read access.",
            )),
        }
    } else {
        match find_repository_config(&options.cwd, options.config_path.as_deref())
            .and_then(|path| load_repository_config(&path).map(|config| (path, config)))
        {
            Ok((path, config)) => {
                policy_source = Some(path.display().to_string());
                checks.push(pass(
                    "config.repository",
                    format!("Loaded repository policy from {}", path.display()),
                ));
                repository_config = Some(config);
            }
            Err(error) => checks.push(fail(
                "config.repository",
                error.to_string(),
                "Fix or supply the repository policy.",
            )),
        }
    }

    if let Some(config) = &repository_config {
        checks.extend(evaluate_repository_config(config, host_config.as_ref()));
    }
    let ok = checks
        .iter()
        .all(|check| check.status != DiagnosticStatus::Fail);
    Ok(DoctorReport {
        ok,
        repository: options.repository,
        policy_source,
        checks,
    })
}

pub fn evaluate_branch_policy(
    policy: &BranchPolicySnapshot,
    config: &RepositoryConfig,
    expected_app_id: u64,
) -> Vec<Diagnostic> {
    let mut checks = Vec::new();
    if policy.sources.is_empty() {
        checks.push(fail(
            "github.protection_sources",
            "No active classic protection or repository ruleset was found",
            "Configure a protected branch or ruleset.",
        ));
    } else {
        let sources = policy
            .sources
            .iter()
            .map(|source| match source {
                crate::github::BranchPolicySource::ClassicProtection => "classic protection",
                crate::github::BranchPolicySource::Ruleset => "ruleset",
            })
            .collect::<Vec<_>>()
            .join(" and ");
        checks.push(pass(
            "github.protection_sources",
            format!("Evaluated {sources}"),
        ));
    }
    checks.push(if policy.strict_required_checks {
        pass(
            "github.strict_checks",
            "Required checks use strict up-to-date policy",
        )
    } else {
        fail(
            "github.strict_checks",
            "Required checks are not strict",
            "Require branches to be up to date before merging.",
        )
    });
    let expected_check = policy.required_checks.iter().any(|check| {
        check.context == config.review.required_check && check.app_id == Some(expected_app_id)
    });
    checks.push(if expected_check {
        pass(
            "github.expected_check_source",
            format!(
                "{} is bound to App {expected_app_id}",
                config.review.required_check
            ),
        )
    } else {
        fail(
            "github.expected_check_source",
            format!(
                "{} is not bound to App {expected_app_id}",
                config.review.required_check
            ),
            "Bind the required check to the installed Eupho GitHub App, not any source.",
        )
    });
    checks.push(if policy.bypass_verification_complete {
        pass(
            "github.bypass_visibility",
            "Ruleset and branch bypass actors were visible",
        )
    } else {
        fail(
            "github.bypass_visibility",
            "Ruleset bypass actors were not visible to the diagnostic credential",
            "Use an operator credential whose owner can inspect the applicable rulesets.",
        )
    });
    checks.push(if policy.bypass_app_ids.contains(&expected_app_id) {
        fail(
            "github.no_app_bypass",
            format!("App {expected_app_id} can bypass the protected merge path"),
            "Remove the Eupho App from branch-protection and ruleset bypass actors.",
        )
    } else {
        pass(
            "github.no_app_bypass",
            format!("App {expected_app_id} has no configured merge bypass"),
        )
    });
    if config.merge_policy == MergePolicy::HumanFinalApproval {
        checks.push(if policy.dismiss_stale_approvals {
            pass(
                "github.stale_approvals",
                "Stale approvals are dismissed on push",
            )
        } else {
            fail(
                "github.stale_approvals",
                "Stale approvals are not dismissed on push",
                "Enable stale approval dismissal for human-final-approval.",
            )
        });
        checks.push(if policy.required_approving_review_count >= 1 {
            pass(
                "github.required_approval",
                format!(
                    "Branch policy requires {} approving review(s)",
                    policy.required_approving_review_count
                ),
            )
        } else {
            fail(
                "github.required_approval",
                "Branch policy does not require an approving review",
                "Require at least one approving review for human-final-approval.",
            )
        });
    }
    checks
}

fn evaluate_repository_config(
    config: &RepositoryConfig,
    host: Option<&HostConfig>,
) -> Vec<Diagnostic> {
    let mut checks = vec![
        if config.branches.require_up_to_date {
            pass(
                "policy.strict_binding",
                "Repository policy requires up-to-date branches",
            )
        } else {
            fail(
                "policy.strict_binding",
                "Repository policy permits stale base bindings",
                "Set branches.require_up_to_date to true.",
            )
        },
        if config
            .notifications
            .events
            .iter()
            .any(|event| event == "awaiting_approval")
        {
            pass(
                "policy.approval_notification",
                "awaiting_approval notifications are enabled",
            )
        } else {
            fail(
                "policy.approval_notification",
                "awaiting_approval is missing from notification events",
                "Add awaiting_approval to notifications.events.",
            )
        },
    ];
    let Some(host) = host else {
        return checks;
    };
    let sandbox = &config.execution.unattended.sandbox_profile;
    checks.push(if host.sandbox_profiles.contains_key(sandbox) {
        pass(
            "host.sandbox_profile",
            format!("Sandbox profile {sandbox} exists"),
        )
    } else {
        fail(
            "host.sandbox_profile",
            format!("Sandbox profile {sandbox} is unknown"),
            "Predeclare the selected sandbox profile in host configuration.",
        )
    });
    checks.push(if host.workspace_profiles.contains_key("ephemeral_clone") {
        pass(
            "host.workspace_profile",
            "Disposable ephemeral_clone profile exists",
        )
    } else {
        fail(
            "host.workspace_profile",
            "Disposable ephemeral_clone profile is missing",
            "Declare an ephemeral_clone profile with no shared objects or authenticated remote.",
        )
    });
    let price_table = &config.limits.price_table_profile;
    checks.push(if host.price_table_profiles.contains_key(price_table) {
        pass(
            "host.price_table",
            format!("Price table profile {price_table} exists"),
        )
    } else {
        fail(
            "host.price_table",
            format!("Price table profile {price_table} is unknown"),
            "Predeclare the selected price table profile in host configuration.",
        )
    });
    for sink in &config.notifications.sinks {
        checks.push(if host.notification_sinks.contains_key(sink) {
            pass(
                "host.notification_sink",
                format!("Notification sink {sink} exists"),
            )
        } else {
            fail(
                "host.notification_sink",
                format!("Notification sink {sink} is unknown"),
                "Predeclare the notification sink in host configuration.",
            )
        });
    }
    checks
}

fn executable_check(code: &str, executable: &str) -> Diagnostic {
    match run_version_probe(executable) {
        Ok(version) => pass(code, version),
        Err(error) => fail(
            code,
            format!("{executable} is unavailable: {error}"),
            format!("Install {executable}."),
        ),
    }
}

fn run_version_probe(executable: &str) -> Result<String, String> {
    const TIMEOUT: Duration = Duration::from_secs(2);
    const OUTPUT_LIMIT: u64 = 64 * 1024;

    let mut command = Command::new(executable);
    command
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for variable in ["PATH", "SystemRoot", "WINDIR", "PATHEXT"] {
        if let Some(value) = env::var_os(variable) {
            command.env(variable, value);
        }
    }
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "version probe stdout was not captured".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "version probe stderr was not captured".to_owned())?;
    let stdout_reader = thread::spawn(move || read_probe_output(stdout, OUTPUT_LIMIT));
    let stderr_reader = thread::spawn(move || read_probe_output(stderr, OUTPUT_LIMIT));

    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                terminate_probe_tree(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!("timed out after {} seconds", TIMEOUT.as_secs()));
            }
            Err(error) => {
                terminate_probe_tree(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!("failed while waiting: {error}"));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "stdout reader panicked".to_owned())??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "stderr reader panicked".to_owned())??;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr);
        return Err(if detail.trim().is_empty() {
            format!("exited with {status}")
        } else {
            detail.trim().to_owned()
        });
    }
    let stdout = String::from_utf8(stdout).map_err(|_| "returned non-UTF-8 output".to_owned())?;
    Ok(stdout
        .trim()
        .lines()
        .next()
        .unwrap_or(executable)
        .to_owned())
}

fn read_probe_output(stream: impl Read, limit: u64) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    stream
        .take(limit + 1)
        .read_to_end(&mut output)
        .map_err(|error| error.to_string())?;
    if output.len() as u64 > limit {
        return Err(format!("output exceeded {limit} bytes"));
    }
    Ok(output)
}

fn terminate_probe_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(raw_pid) = i32::try_from(child.id()) {
        if let Some(pid) = Pid::from_raw(raw_pid) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn pass(code: impl Into<String>, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        status: DiagnosticStatus::Pass,
        message: message.into(),
        remediation: None,
    }
}

fn fail(
    code: impl Into<String>,
    message: impl Into<String>,
    remediation: impl Into<String>,
) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        status: DiagnosticStatus::Fail,
        message: message.into(),
        remediation: Some(remediation.into()),
    }
}

fn warn(
    code: impl Into<String>,
    message: impl Into<String>,
    remediation: impl Into<String>,
) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        status: DiagnosticStatus::Warn,
        message: message.into(),
        remediation: Some(remediation.into()),
    }
}

#[derive(Debug)]
pub struct OnceOptions {
    pub cwd: PathBuf,
    pub repository: String,
    pub config_path: Option<PathBuf>,
    pub host_config_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct OnceReport {
    #[serde(flatten)]
    pub snapshot: CandidateSnapshot,
    #[serde(rename = "observeOnly")]
    pub observe_only: bool,
}

pub fn observe_once(options: OnceOptions) -> Result<OnceReport, ApplicationError> {
    let reader = match env::var("EUPHO_GITHUB_TOKEN") {
        Ok(token) if !token.is_empty() => GhReader::default().with_token(token),
        _ => GhReader::default(),
    };
    observe_once_with(&options, &reader, None, &rfc3339_now())
}

pub fn observe_once_with<R: GitHubRead>(
    options: &OnceOptions,
    reader: &R,
    state_root_override: Option<&Path>,
    observed_at: &str,
) -> Result<OnceReport, ApplicationError> {
    let identity = resolve_policy(
        reader,
        &options.repository,
        &options.cwd,
        options.config_path.as_deref(),
    )?;
    let repository_root = find_git_worktree_root(&options.cwd)?;
    let configured_state_root = if let Some(path) = state_root_override {
        path.to_path_buf()
    } else if let Some(path) = &options.host_config_path {
        PathBuf::from(load_host_config(path)?.state_root)
    } else {
        default_state_root()?
    };
    let state_root = resolve_safe_state_root(&configured_state_root, repository_root.as_deref())?;
    let mut lock = RepositoryLock::acquire(&state_root, identity.repository.id)?;

    let operation = (|| {
        let resolved = resolve_policy(
            reader,
            &options.repository,
            &options.cwd,
            options.config_path.as_deref(),
        )?;
        if resolved.repository.id != identity.repository.id
            || resolved.repository.name_with_owner != identity.repository.name_with_owner
        {
            return Err(ApplicationError::new(
                "repository_identity_changed",
                format!(
                    "repository identity changed while acquiring the lock for {}",
                    options.repository
                ),
            ));
        }
        lock.assert_held()?;
        let issue_limit = 100_usize.max(resolved.config.concurrency.saturating_mul(4));
        let issues = reader.ready_issues(
            &options.repository,
            &resolved.config.labels.ready,
            issue_limit,
        )?;
        let active_labels = vec![
            resolved.config.labels.working.clone(),
            resolved.config.labels.review.clone(),
            resolved.config.labels.human.clone(),
        ];
        let active =
            reader.active_issue_numbers(&options.repository, &active_labels, issue_limit)?;
        let plan = plan_candidates(&resolved.repository, &issues, &resolved.config, &active)?;
        lock.assert_held()?;
        let snapshot = CandidateSnapshot {
            schema_version: 1,
            repository_id: resolved.repository.id,
            repository: resolved.repository.name_with_owner,
            base_sha: resolved.repository.base_sha,
            policy_digest: plan.policy_digest,
            policy_source: resolved.source,
            trusted_base: resolved.trusted_base,
            observed_at: observed_at.to_owned(),
            candidates: plan.candidates,
            diagnostics: plan.diagnostics,
        };
        CandidateStore::new(&state_root).put(&snapshot)?;
        Ok(OnceReport {
            snapshot,
            observe_only: true,
        })
    })();
    let release = lock.release().map_err(ApplicationError::from);
    match (operation, release) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(report), Ok(())) => Ok(report),
    }
}

#[derive(Debug)]
pub struct StatusOptions {
    pub cwd: PathBuf,
    pub state_root: Option<PathBuf>,
    pub host_config_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusReport {
    pub state_root: PathBuf,
    pub repositories: Vec<CandidateSnapshot>,
}

pub fn status(options: StatusOptions) -> Result<StatusReport, ApplicationError> {
    let repository_root = find_git_worktree_root(&options.cwd)?;
    let configured = if let Some(path) = options.state_root {
        path
    } else if let Some(path) = options.host_config_path {
        PathBuf::from(load_host_config(&path)?.state_root)
    } else {
        default_state_root()?
    };
    let state_root = resolve_safe_state_root(&configured, repository_root.as_deref())?;
    let repositories = CandidateStore::new(&state_root).list::<CandidateSnapshot>()?;
    Ok(StatusReport {
        state_root,
        repositories,
    })
}
