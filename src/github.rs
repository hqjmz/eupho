#![allow(clippy::missing_errors_doc)]

//! Strict, observe-only access to GitHub through the `gh` executable.
//!
//! Every command is constructed as an argument vector (never a shell string),
//! prompts and update checks are disabled, responses are runtime validated, and
//! a fixed timeout plus output cap prevents a stuck or unbounded helper.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process_group};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const GH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024;
const REPOSITORY_CONFIG_PATHS: [&str; 2] = [".github/eupho.yml", ".github/agent-orchestrator.yml"];

pub use crate::domain::{IssueSnapshot, RepositorySnapshot};

#[derive(Debug)]
pub struct GitHubError {
    pub code: &'static str,
    pub message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl GitHubError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            source: None,
        }
    }

    fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }
}

impl fmt::Display for GitHubError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GitHubError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchPolicySource {
    ClassicProtection,
    Ruleset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequiredCheckSnapshot {
    pub context: String,
    pub app_id: Option<u64>,
    pub source: BranchPolicySource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchPolicySnapshot {
    pub strict_required_checks: bool,
    pub dismiss_stale_approvals: bool,
    pub required_approving_review_count: u64,
    pub bypass_app_ids: Vec<u64>,
    pub bypass_verification_complete: bool,
    pub required_checks: Vec<RequiredCheckSnapshot>,
    pub sources: Vec<BranchPolicySource>,
}

#[derive(Debug, Clone)]
pub struct GhReader {
    binary: PathBuf,
    environment: HashMap<OsString, OsString>,
    timeout: Duration,
}

impl Default for GhReader {
    fn default() -> Self {
        Self::new("gh")
    }
}

impl GhReader {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        let environment = github_environment();
        Self::with_environment(binary, environment)
    }

    pub fn with_environment(
        binary: impl Into<PathBuf>,
        mut environment: HashMap<OsString, OsString>,
    ) -> Self {
        environment.insert("GH_PROMPT_DISABLED".into(), "1".into());
        environment.insert("GH_NO_UPDATE_NOTIFIER".into(), "1".into());
        Self {
            binary: binary.into(),
            environment,
            timeout: GH_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_token(mut self, token: impl Into<OsString>) -> Self {
        self.environment.insert("GH_TOKEN".into(), token.into());
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn repository(
        &self,
        repository: &str,
        configured_base_branch: Option<&str>,
    ) -> Result<RepositorySnapshot, GitHubError> {
        assert_repository(repository)?;
        let metadata = object(
            self.api(&format!("repos/{repository}"), &[])?,
            "repository metadata",
        )?;
        let id = positive_integer(&metadata, "id", "repository metadata.id")?;
        let name_with_owner =
            nonempty_string(&metadata, "full_name", "repository metadata.full_name")?;
        assert_repository(&name_with_owner)
            .map_err(|_| invalid_response("repository metadata.full_name", "is invalid"))?;
        let default_branch = nonempty_string(
            &metadata,
            "default_branch",
            "repository metadata.default_branch",
        )?;
        let base_branch = configured_base_branch.unwrap_or(&default_branch);
        if base_branch.is_empty() {
            return Err(invalid_response("base branch", "must not be empty"));
        }
        let commit = object(
            self.api(
                &format!("repos/{repository}/commits/{}", percent_encode(base_branch)),
                &[],
            )?,
            "commit",
        )?;
        let base_sha = nonempty_string(&commit, "sha", "commit.sha")?;
        if !valid_sha(&base_sha) {
            return Err(invalid_response("commit.sha", "is invalid"));
        }

        let mut policy_path = None;
        let mut policy_content = None;
        for candidate in REPOSITORY_CONFIG_PATHS {
            let endpoint = format!("repos/{repository}/contents/{candidate}");
            let Some(raw) =
                self.api_optional(&endpoint, &["--field", &format!("ref={base_sha}")])?
            else {
                continue;
            };
            let content = object(raw, candidate)?;
            let encoding = nonempty_string(&content, "encoding", &format!("{candidate}.encoding"))?;
            let encoded = nonempty_string(&content, "content", &format!("{candidate}.content"))?;
            if encoding != "base64" {
                return Err(GitHubError::new(
                    "invalid_remote_policy",
                    format!("{candidate} is not a base64 GitHub content object"),
                ));
            }
            let compact = encoded
                .bytes()
                .filter(|byte| !byte.is_ascii_whitespace())
                .collect::<Vec<_>>();
            let decoded = BASE64_STANDARD.decode(compact).map_err(|error| {
                GitHubError::new(
                    "invalid_remote_policy",
                    format!("{candidate} contains invalid base64: {error}"),
                )
                .with_source(error)
            })?;
            policy_content = Some(String::from_utf8(decoded).map_err(|error| {
                GitHubError::new(
                    "invalid_remote_policy",
                    format!("{candidate} is not valid UTF-8: {error}"),
                )
                .with_source(error)
            })?);
            policy_path = Some(candidate.to_owned());
            break;
        }

        Ok(RepositorySnapshot {
            id,
            name_with_owner,
            default_branch,
            base_sha,
            policy_path,
            policy_content,
        })
    }

    pub fn ready_issues(
        &self,
        repository: &str,
        ready_label: &str,
        limit: usize,
    ) -> Result<Vec<IssueSnapshot>, GitHubError> {
        assert_repository(repository)?;
        let output = self.run(&[
            "issue",
            "list",
            "--repo",
            repository,
            "--state",
            "open",
            "--label",
            ready_label,
            "--limit",
            &limit.to_string(),
            "--json",
            "number,title,url,labels,updatedAt",
        ])?;
        parse_issue_list(parse_json(&output, "gh issue list")?)
    }

    pub fn active_issue_numbers(
        &self,
        repository: &str,
        active_labels: &[String],
        limit: usize,
    ) -> Result<Vec<u64>, GitHubError> {
        assert_repository(repository)?;
        let mut active = BTreeSet::new();
        for label in active_labels {
            let output = self.run(&[
                "issue",
                "list",
                "--repo",
                repository,
                "--state",
                "open",
                "--label",
                label,
                "--limit",
                &limit.to_string(),
                "--json",
                "number",
            ])?;
            let entries = array(parse_json(&output, "gh issue list")?, "active issue list")?;
            for (index, entry) in entries.into_iter().enumerate() {
                let entry = object(entry, &format!("active issue list[{index}]"))?;
                active.insert(positive_integer(
                    &entry,
                    "number",
                    &format!("active issue list[{index}].number"),
                )?);
            }
        }
        Ok(active.into_iter().collect())
    }

    pub fn label_exists(&self, repository: &str, label: &str) -> Result<bool, GitHubError> {
        assert_repository(repository)?;
        Ok(self
            .api_optional(
                &format!("repos/{repository}/labels/{}", percent_encode(label)),
                &[],
            )?
            .is_some())
    }

    #[allow(clippy::too_many_lines)]
    pub fn branch_policy(
        &self,
        repository: &str,
        branch: &str,
    ) -> Result<BranchPolicySnapshot, GitHubError> {
        assert_repository(repository)?;
        if branch.is_empty() {
            return Err(invalid_response("branch", "must not be empty"));
        }
        let classic = self.api_optional(
            &format!(
                "repos/{repository}/branches/{}/protection",
                percent_encode(branch)
            ),
            &[],
        )?;
        let rules = self.api_optional(
            &format!(
                "repos/{repository}/rules/branches/{}",
                percent_encode(branch)
            ),
            &["--field", "per_page=100"],
        )?;

        let mut snapshot = BranchPolicySnapshot {
            strict_required_checks: false,
            dismiss_stale_approvals: false,
            required_approving_review_count: 0,
            bypass_app_ids: Vec::new(),
            bypass_verification_complete: true,
            required_checks: Vec::new(),
            sources: Vec::new(),
        };
        let mut bypass_app_ids = BTreeSet::new();

        if let Some(classic) = classic {
            snapshot.sources.push(BranchPolicySource::ClassicProtection);
            let classic = object(classic, "classic branch protection")?;
            let status = optional_object(
                classic.get("required_status_checks"),
                "required_status_checks",
            )?;
            let reviews = optional_object(
                classic.get("required_pull_request_reviews"),
                "required_pull_request_reviews",
            )?;
            if let Some(status) = status {
                snapshot.strict_required_checks |=
                    optional_boolean(status.get("strict"), "required_status_checks.strict")?
                        .unwrap_or(false);
                if let Some(checks) =
                    optional_array(status.get("checks"), "required_status_checks.checks")?
                {
                    for (index, check) in checks.into_iter().enumerate() {
                        let check =
                            object(check, &format!("required_status_checks.checks[{index}]"))?;
                        snapshot.required_checks.push(RequiredCheckSnapshot {
                            context: nonempty_string(
                                &check,
                                "context",
                                &format!("required_status_checks.checks[{index}].context"),
                            )?,
                            app_id: optional_nonnegative_integer(
                                check.get("app_id"),
                                &format!("required_status_checks.checks[{index}].app_id"),
                            )?,
                            source: BranchPolicySource::ClassicProtection,
                        });
                    }
                }
                if let Some(contexts) =
                    optional_array(status.get("contexts"), "required_status_checks.contexts")?
                {
                    for (index, context) in contexts.into_iter().enumerate() {
                        let context = value_string(
                            &context,
                            &format!("required_status_checks.contexts[{index}]"),
                            false,
                        )?;
                        if !snapshot
                            .required_checks
                            .iter()
                            .any(|check| check.context == context)
                        {
                            snapshot.required_checks.push(RequiredCheckSnapshot {
                                context,
                                app_id: None,
                                source: BranchPolicySource::ClassicProtection,
                            });
                        }
                    }
                }
            }
            if let Some(reviews) = reviews {
                snapshot.dismiss_stale_approvals |= optional_boolean(
                    reviews.get("dismiss_stale_reviews"),
                    "required_pull_request_reviews.dismiss_stale_reviews",
                )?
                .unwrap_or(false);
                snapshot.required_approving_review_count =
                    snapshot.required_approving_review_count.max(
                        optional_nonnegative_integer(
                            reviews.get("required_approving_review_count"),
                            "required_pull_request_reviews.required_approving_review_count",
                        )?
                        .unwrap_or(0),
                    );
                if let Some(allowances) = optional_object(
                    reviews.get("bypass_pull_request_allowances"),
                    "required_pull_request_reviews.bypass_pull_request_allowances",
                )? {
                    if let Some(apps) = optional_array(
                        allowances.get("apps"),
                        "required_pull_request_reviews.bypass_pull_request_allowances.apps",
                    )? {
                        for (index, app) in apps.into_iter().enumerate() {
                            let app = object(
                                app,
                                &format!(
                                    "required_pull_request_reviews.bypass_pull_request_allowances.apps[{index}]"
                                ),
                            )?;
                            bypass_app_ids.insert(positive_integer(
                                &app,
                                "id",
                                &format!(
                                    "required_pull_request_reviews.bypass_pull_request_allowances.apps[{index}].id"
                                ),
                            )?);
                        }
                    }
                }
            }
        }

        if let Some(rules) = rules {
            let rules = array(rules, "active branch rules")?;
            let mut relevant_ruleset_ids = BTreeSet::new();
            let mut relevant_seen = false;
            for (index, rule) in rules.into_iter().enumerate() {
                let source = format!("active branch rules[{index}]");
                let rule = object(rule, &source)?;
                let rule_type = nonempty_string(&rule, "type", &format!("{source}.type"))?;
                let parameters =
                    optional_object(rule.get("parameters"), &format!("{source}.parameters"))?;
                if matches!(
                    rule_type.as_str(),
                    "required_status_checks" | "pull_request"
                ) {
                    relevant_seen = true;
                    let ruleset_id = optional_nonnegative_integer(
                        rule.get("ruleset_id"),
                        &format!("{source}.ruleset_id"),
                    )?
                    .filter(|id| *id > 0)
                    .ok_or_else(|| {
                        invalid_response(
                            &source,
                            &format!("{rule_type} rule is missing ruleset_id"),
                        )
                    })?;
                    relevant_ruleset_ids.insert(ruleset_id);
                }
                if rule_type == "required_status_checks" {
                    if let Some(parameters) = &parameters {
                        snapshot.strict_required_checks |= optional_boolean(
                            parameters.get("strict_required_status_checks_policy"),
                            &format!("{source}.parameters.strict_required_status_checks_policy"),
                        )?
                        .unwrap_or(false);
                        if let Some(checks) = optional_array(
                            parameters.get("required_status_checks"),
                            &format!("{source}.parameters.required_status_checks"),
                        )? {
                            for (check_index, check) in checks.into_iter().enumerate() {
                                let check_source = format!(
                                    "{source}.parameters.required_status_checks[{check_index}]"
                                );
                                let check = object(check, &check_source)?;
                                snapshot.required_checks.push(RequiredCheckSnapshot {
                                    context: nonempty_string(
                                        &check,
                                        "context",
                                        &format!("{check_source}.context"),
                                    )?,
                                    app_id: optional_nonnegative_integer(
                                        check.get("integration_id"),
                                        &format!("{check_source}.integration_id"),
                                    )?,
                                    source: BranchPolicySource::Ruleset,
                                });
                            }
                        }
                    }
                } else if rule_type == "pull_request" {
                    if let Some(parameters) = &parameters {
                        snapshot.dismiss_stale_approvals |= optional_boolean(
                            parameters.get("dismiss_stale_reviews_on_push"),
                            &format!("{source}.parameters.dismiss_stale_reviews_on_push"),
                        )?
                        .unwrap_or(false);
                        snapshot.required_approving_review_count =
                            snapshot.required_approving_review_count.max(
                                optional_nonnegative_integer(
                                    parameters.get("required_approving_review_count"),
                                    &format!("{source}.parameters.required_approving_review_count"),
                                )?
                                .unwrap_or(0),
                            );
                    }
                }
            }

            if relevant_seen {
                snapshot.sources.push(BranchPolicySource::Ruleset);
                for ruleset_id in relevant_ruleset_ids {
                    let detail = object(
                        self.api(
                            &format!("repos/{repository}/rulesets/{ruleset_id}"),
                            &["--field", "includes_parents=true"],
                        )?,
                        "ruleset detail",
                    )?;
                    if !detail.contains_key("bypass_actors") {
                        snapshot.bypass_verification_complete = false;
                        continue;
                    }
                    for (index, actor) in array(
                        detail.get("bypass_actors").cloned().unwrap_or(Value::Null),
                        "ruleset detail.bypass_actors",
                    )?
                    .into_iter()
                    .enumerate()
                    {
                        let actor =
                            object(actor, &format!("ruleset detail.bypass_actors[{index}]"))?;
                        let actor_type = nonempty_string(
                            &actor,
                            "actor_type",
                            &format!("ruleset detail.bypass_actors[{index}].actor_type"),
                        )?;
                        let actor_id = optional_nonnegative_integer(
                            actor.get("actor_id"),
                            &format!("ruleset detail.bypass_actors[{index}].actor_id"),
                        )?;
                        if actor_type == "Integration" {
                            if let Some(actor_id) = actor_id.filter(|id| *id > 0) {
                                bypass_app_ids.insert(actor_id);
                            }
                        }
                    }
                }
            }
        }

        snapshot.bypass_app_ids = bypass_app_ids.into_iter().collect();
        snapshot.sources.dedup();
        Ok(snapshot)
    }

    fn api_optional(&self, endpoint: &str, extra: &[&str]) -> Result<Option<Value>, GitHubError> {
        match self.api(endpoint, extra) {
            Ok(value) => Ok(Some(value)),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn api(&self, endpoint: &str, extra: &[&str]) -> Result<Value, GitHubError> {
        let mut arguments = vec![
            "api",
            "--method",
            "GET",
            endpoint,
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "X-GitHub-Api-Version: 2026-03-10",
        ];
        arguments.extend_from_slice(extra);
        parse_json(&self.run(&arguments)?, &format!("gh api {endpoint}"))
    }

    fn run(&self, arguments: &[&str]) -> Result<String, GitHubError> {
        let mut command = Command::new(&self.binary);
        command
            .args(arguments)
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|error| {
            GitHubError::new(
                "github_read_failed",
                format!("GitHub read failed ({}): {error}", self.binary.display()),
            )
            .with_source(error)
        })?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdout_reader = thread::spawn(move || read_bounded(stdout));
        let stderr_reader = thread::spawn(move || read_bounded(stderr));

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    terminate_process_tree(&mut child);
                    let _ = join_reader(stdout_reader);
                    let _ = join_reader(stderr_reader);
                    return Err(GitHubError::new(
                        "github_read_failed",
                        format!(
                            "GitHub read timed out after {} seconds ({} {})",
                            self.timeout.as_secs_f64(),
                            self.binary.display(),
                            arguments.join(" ")
                        ),
                    ));
                }
                Err(error) => {
                    terminate_process_tree(&mut child);
                    let _ = join_reader(stdout_reader);
                    let _ = join_reader(stderr_reader);
                    return Err(GitHubError::new(
                        "github_read_failed",
                        format!("GitHub read failed while waiting: {error}"),
                    )
                    .with_source(error));
                }
            }
        };
        let stdout = join_reader(stdout_reader)?;
        let stderr = join_reader(stderr_reader)?;
        if !status.success() {
            let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
            return Err(GitHubError::new(
                "github_read_failed",
                format!(
                    "GitHub read failed ({} {}): {}",
                    self.binary.display(),
                    arguments.join(" "),
                    if detail.is_empty() {
                        format!("process exited with {status}")
                    } else {
                        detail
                    }
                ),
            ));
        }
        String::from_utf8(stdout).map_err(|error| {
            GitHubError::new(
                "invalid_github_response",
                "gh returned stdout that is not valid UTF-8",
            )
            .with_source(error)
        })
    }
}

fn github_environment() -> HashMap<OsString, OsString> {
    const ALLOWED: &[&str] = &[
        "PATH",
        "HOME",
        "XDG_CONFIG_HOME",
        "GH_CONFIG_DIR",
        "GH_HOST",
        "GH_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_TOKEN",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "LANG",
        "LC_ALL",
    ];
    ALLOWED
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
        .collect()
}

fn terminate_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(raw_pid) = i32::try_from(child.id()) {
        if let Some(pid) = Pid::from_raw(raw_pid) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn read_bounded(mut stream: impl Read) -> Result<Vec<u8>, GitHubError> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = stream.read(&mut buffer).map_err(|error| {
            GitHubError::new(
                "github_read_failed",
                format!("cannot read gh output: {error}"),
            )
            .with_source(error)
        })?;
        if count == 0 {
            return Ok(output);
        }
        if output.len() + count > MAX_OUTPUT_BYTES {
            return Err(GitHubError::new(
                "github_read_failed",
                format!("gh output exceeded {MAX_OUTPUT_BYTES} bytes"),
            ));
        }
        output
            .write_all(&buffer[..count])
            .expect("Vec write cannot fail");
    }
}

fn join_reader(
    reader: thread::JoinHandle<Result<Vec<u8>, GitHubError>>,
) -> Result<Vec<u8>, GitHubError> {
    reader
        .join()
        .map_err(|_| GitHubError::new("github_read_failed", "gh output reader thread panicked"))?
}

fn parse_issue_list(value: Value) -> Result<Vec<IssueSnapshot>, GitHubError> {
    let entries = array(value, "issue list")?;
    let mut issues = Vec::with_capacity(entries.len());
    for (index, entry) in entries.into_iter().enumerate() {
        let source = format!("issue list[{index}]");
        let entry = object(entry, &source)?;
        let label_values = array(
            entry.get("labels").cloned().unwrap_or(Value::Null),
            &format!("{source}.labels"),
        )?;
        let mut labels = Vec::with_capacity(label_values.len());
        for (label_index, label) in label_values.into_iter().enumerate() {
            let label_source = format!("{source}.labels[{label_index}]");
            let label = object(label, &label_source)?;
            labels.push(nonempty_string(
                &label,
                "name",
                &format!("{label_source}.name"),
            )?);
        }
        let updated_at = nonempty_string(&entry, "updatedAt", &format!("{source}.updatedAt"))?;
        if !valid_iso_instant(&updated_at) {
            return Err(invalid_response(
                &format!("{source}.updatedAt"),
                "is invalid",
            ));
        }
        issues.push(IssueSnapshot {
            number: positive_integer(&entry, "number", &format!("{source}.number"))?,
            title: value_string(
                entry.get("title").unwrap_or(&Value::Null),
                &format!("{source}.title"),
                true,
            )?,
            url: nonempty_string(&entry, "url", &format!("{source}.url"))?,
            labels,
            updated_at,
        });
    }
    Ok(issues)
}

fn object(value: Value, source: &str) -> Result<Map<String, Value>, GitHubError> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(invalid_response(source, "must be an object")),
    }
}

fn array(value: Value, source: &str) -> Result<Vec<Value>, GitHubError> {
    match value {
        Value::Array(array) => Ok(array),
        _ => Err(invalid_response(source, "must be an array")),
    }
}

fn optional_object(
    value: Option<&Value>,
    source: &str,
) -> Result<Option<Map<String, Value>>, GitHubError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => object(value.clone(), source).map(Some),
    }
}

fn optional_array(value: Option<&Value>, source: &str) -> Result<Option<Vec<Value>>, GitHubError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => array(value.clone(), source).map(Some),
    }
}

fn nonempty_string(
    object: &Map<String, Value>,
    field: &str,
    source: &str,
) -> Result<String, GitHubError> {
    value_string(object.get(field).unwrap_or(&Value::Null), source, false)
}

fn value_string(value: &Value, source: &str, allow_empty: bool) -> Result<String, GitHubError> {
    match value.as_str() {
        Some(value) if allow_empty || !value.is_empty() => Ok(value.to_owned()),
        _ => Err(invalid_response(source, "must be a string")),
    }
}

fn positive_integer(
    object: &Map<String, Value>,
    field: &str,
    source: &str,
) -> Result<u64, GitHubError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid_response(source, "must be positive"))
}

fn optional_nonnegative_integer(
    value: Option<&Value>,
    source: &str,
) -> Result<Option<u64>, GitHubError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid_response(source, "must be non-negative")),
    }
}

fn optional_boolean(value: Option<&Value>, source: &str) -> Result<Option<bool>, GitHubError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| invalid_response(source, "must be boolean")),
    }
}

fn parse_json(text: &str, source: &str) -> Result<Value, GitHubError> {
    serde_json::from_str(text).map_err(|error| {
        GitHubError::new(
            "invalid_github_response",
            format!("{source} returned invalid JSON"),
        )
        .with_source(error)
    })
}

fn invalid_response(source: &str, detail: &str) -> GitHubError {
    GitHubError::new("invalid_github_response", format!("{source} {detail}"))
}

fn assert_repository(repository: &str) -> Result<(), GitHubError> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if parts.next().is_some() || !valid_repository_part(owner) || !valid_repository_part(name) {
        return Err(GitHubError::new(
            "invalid_repository",
            format!("expected OWNER/REPOSITORY, received {repository}"),
        ));
    }
    Ok(())
}

fn valid_repository_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn valid_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_iso_instant(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !matches!(bytes.len(), 20 | 24)
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || bytes.last() != Some(&b'Z')
        || (bytes.len() == 24 && bytes.get(19) != Some(&b'.'))
    {
        return false;
    }
    let Some(year) = decimal(bytes, 0, 4) else {
        return false;
    };
    let Some(month) = decimal(bytes, 5, 7) else {
        return false;
    };
    let Some(day) = decimal(bytes, 8, 10) else {
        return false;
    };
    let Some(hour) = decimal(bytes, 11, 13) else {
        return false;
    };
    let Some(minute) = decimal(bytes, 14, 16) else {
        return false;
    };
    let Some(second) = decimal(bytes, 17, 19) else {
        return false;
    };
    (bytes.len() == 20 || decimal(bytes, 20, 23).is_some())
        && year > 0
        && (1..=12).contains(&month)
        && day > 0
        && day <= days_in_month(year, month)
        && hour < 24
        && minute < 60
        && second < 60
}

fn decimal(bytes: &[u8], start: usize, end: usize) -> Option<u64> {
    bytes
        .get(start..end)?
        .iter()
        .try_fold(0_u64, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + u64::from(byte - b'0'))
        })
}

fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => 31,
    }
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push(char::from(HEX[(byte >> 4) as usize]));
            output.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    output
}

fn is_not_found(error: &GitHubError) -> bool {
    let message = error.message.to_ascii_lowercase();
    message.contains("http 404")
        || message.contains("status code 404")
        || message.contains("not found")
}
