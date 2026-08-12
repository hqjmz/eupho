//! Strict, fail-closed compilation of repository and host policy.
//!
//! Deserialization names follow the checked-in YAML while serialization names
//! intentionally follow the original TypeScript domain object. This preserves
//! policy digests (and therefore candidate identities) across the Rust port.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const REPOSITORY_CONFIG_PATHS: [&str; 2] =
    [".github/eupho.yml", ".github/agent-orchestrator.yml"];

#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        source_name: String,
        message: String,
    },
    Invalid {
        path: String,
        message: String,
    },
    Canonicalization(String),
}

impl ConfigError {
    fn invalid(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Invalid {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::Parse {
                source_name,
                message,
            } => write!(formatter, "cannot parse {source_name}: {message}"),
            Self::Invalid { path, message } => write!(formatter, "{path} {message}"),
            Self::Canonicalization(message) => {
                write!(
                    formatter,
                    "cannot canonicalize repository policy: {message}"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Attended,
    Unattended,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceType {
    Worktree,
    EphemeralClone,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergePolicy {
    AutonomousLowRisk,
    HumanFinalApproval,
    SuggestOnly,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct RepositoryConfig {
    pub version: u32,
    pub base_branch: String,
    pub concurrency: usize,
    pub poll_interval_seconds: u64,
    pub merge_policy: MergePolicy,
    pub execution: ExecutionConfig,
    pub github_app: RepositoryGitHubApp,
    pub labels: WorkflowLabels,
    pub routing: RoutingConfig,
    pub branches: BranchConfig,
    pub runners: RunnersConfig,
    pub limits: LimitsConfig,
    pub review: ReviewConfig,
    pub validation: ValidationConfig,
    pub policy: PolicyConfig,
    pub notifications: NotificationsConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct ExecutionConfig {
    pub default_mode: ExecutionMode,
    pub attended: ExecutionProfile,
    pub unattended: ExecutionProfile,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct ExecutionProfile {
    pub workspace: WorkspaceType,
    pub native_permission_prompts: bool,
    pub sandbox_profile: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct RepositoryGitHubApp {
    pub slug: String,
    pub required_check_source: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowLabels {
    pub ready: String,
    pub working: String,
    pub review: String,
    pub human: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct RoutingConfig {
    pub autonomous_classes: Vec<AutonomousClass>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct AutonomousClass {
    pub label: String,
    pub execution_mode: ExecutionMode,
    pub allowed_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct BranchConfig {
    pub pattern: String,
    pub merge_method: MergeMethod,
    pub require_up_to_date: bool,
    pub dismiss_stale_approvals: bool,
    pub merge_queue: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunnersConfig {
    pub author: RunnerProfile,
    pub reviewer: ReviewerProfile,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerProfile {
    pub adapter: String,
    pub profile: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct ReviewerProfile {
    pub adapter: String,
    pub profile: String,
    pub require_independent_context: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct LimitsConfig {
    pub author_minutes: u64,
    pub review_minutes: u64,
    pub repair_cycles: u64,
    pub model_turns_per_phase: u64,
    pub model_tokens_per_run: u64,
    pub model_cost_usd_per_run: String,
    pub model_cost_usd_per_repo_day: String,
    pub price_table_profile: String,
    pub max_changed_files: u64,
    pub max_diff_lines: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct ReviewConfig {
    pub required_check: String,
    pub blocking_severities: Vec<String>,
    pub always_blocking_categories: Vec<String>,
    pub base_drift_policy: BaseDriftPolicy,
    pub advisory_hosted_reviews: bool,
    pub enable_auto_merge: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseDriftPolicy {
    FullRereview,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationConfig {
    pub commands: Vec<ValidationCommand>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationCommand {
    pub name: String,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub struct PolicyConfig {
    pub protected_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationsConfig {
    pub events: Vec<String>,
    pub sinks: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub version: u32,
    pub state_root: String,
    pub workspace_root: String,
    pub metadata_signing: MetadataSigningConfig,
    pub github_app: HostGitHubApp,
    pub sandbox_profiles: BTreeMap<String, SandboxProfile>,
    pub workspace_profiles: BTreeMap<String, WorkspaceProfile>,
    pub price_table_profiles: BTreeMap<String, String>,
    pub notification_sinks: BTreeMap<String, NotificationSink>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MetadataSigningConfig {
    pub current_key_id: String,
    pub key_file: String,
    pub verification_key_files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HostGitHubApp {
    pub app_id: u64,
    pub private_key_file: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SandboxProfile {
    pub backend: SandboxBackend,
    pub network: SandboxNetwork,
    pub runner_state_access: RunnerStateAccess,
    pub shared_git_admin: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackend {
    Container,
    Vm,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxNetwork {
    DenyByDefault,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerStateAccess {
    None,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceProfile {
    pub shared_objects: bool,
    pub authenticated_remote: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NotificationSink {
    pub argv: Vec<String>,
    pub timeout_seconds: u64,
}

/// Resolves an explicit policy path or the first supported repository default.
///
/// # Errors
///
/// Returns [`ConfigError::Invalid`] when no supported policy file exists.
pub fn find_repository_config(cwd: &Path, explicit: Option<&Path>) -> Result<PathBuf, ConfigError> {
    if let Some(path) = explicit {
        return Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        });
    }
    for candidate in REPOSITORY_CONFIG_PATHS {
        let path = cwd.join(candidate);
        if path.exists() {
            return Ok(path);
        }
    }
    Err(ConfigError::invalid(
        cwd.display().to_string(),
        format!(
            "contains no repository policy; tried {}",
            REPOSITORY_CONFIG_PATHS.join(", ")
        ),
    ))
}

/// Reads and validates a repository policy file.
///
/// # Errors
///
/// Returns an I/O, YAML, or policy-validation error.
pub fn load_repository_config(path: &Path) -> Result<RepositoryConfig, ConfigError> {
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_repository_config_text(&text, &path.display().to_string())
}

/// Reads and validates an administrator-owned host policy file.
///
/// # Errors
///
/// Returns an I/O, YAML, or policy-validation error.
pub fn load_host_config(path: &Path) -> Result<HostConfig, ConfigError> {
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_host_config_text(&text, &path.display().to_string())
}

/// Parses strict repository policy YAML and enforces version-one invariants.
///
/// # Errors
///
/// Returns an error for malformed YAML, unknown fields, missing fields, or any
/// configuration that weakens a version-one safety invariant.
pub fn parse_repository_config_text(
    text: &str,
    source_name: &str,
) -> Result<RepositoryConfig, ConfigError> {
    let config: RepositoryConfig =
        serde_yaml::from_str(text).map_err(|error| ConfigError::Parse {
            source_name: source_name.to_owned(),
            message: error.to_string(),
        })?;
    validate_repository_config(&config, source_name)?;
    Ok(config)
}

/// Parses strict host policy YAML and normalizes administrator-owned paths.
///
/// # Errors
///
/// Returns an error for malformed YAML, unknown or missing fields, unsafe
/// profiles, relative paths, filesystem roots, or overlapping state roots.
pub fn parse_host_config_text(text: &str, source_name: &str) -> Result<HostConfig, ConfigError> {
    let mut config: HostConfig =
        serde_yaml::from_str(text).map_err(|error| ConfigError::Parse {
            source_name: source_name.to_owned(),
            message: error.to_string(),
        })?;
    validate_host_config(&mut config, source_name)?;
    Ok(config)
}

/// Computes a TypeScript-compatible canonical SHA-256 policy digest.
///
/// # Errors
///
/// Returns [`ConfigError::Canonicalization`] if the typed policy cannot be
/// represented as canonical JSON.
pub fn repository_policy_digest(config: &RepositoryConfig) -> Result<String, ConfigError> {
    let value = serde_json::to_value(config)
        .map_err(|error| ConfigError::Canonicalization(error.to_string()))?;
    let canonical = canonical_json(&value);
    let digest = Sha256::digest(canonical.as_bytes());
    Ok(format!("sha256:{}", hex_bytes(&digest)))
}

#[allow(clippy::too_many_lines)]
fn validate_repository_config(config: &RepositoryConfig, source: &str) -> Result<(), ConfigError> {
    require(config.version == 1, source, "version", "must be 1")?;
    require_non_empty(&config.base_branch, source, "base_branch")?;
    require_range(config.concurrency as u64, 1, 32, source, "concurrency")?;
    require_range(
        config.poll_interval_seconds,
        1,
        3600,
        source,
        "poll_interval_seconds",
    )?;
    require(
        config.merge_policy != MergePolicy::AutonomousLowRisk,
        source,
        "merge_policy",
        "cannot be autonomous-low-risk globally; use an explicit routing.autonomous_classes entry",
    )?;

    require(
        config.execution.attended.workspace == WorkspaceType::Worktree,
        source,
        "execution.attended.workspace",
        "must be worktree",
    )?;
    require(
        config.execution.attended.native_permission_prompts,
        source,
        "execution.attended.native_permission_prompts",
        "must be true",
    )?;
    require_non_empty(
        &config.execution.attended.sandbox_profile,
        source,
        "execution.attended.sandbox_profile",
    )?;
    require(
        config.execution.unattended.workspace == WorkspaceType::EphemeralClone,
        source,
        "execution.unattended.workspace",
        "must be ephemeral_clone",
    )?;
    require(
        !config.execution.unattended.native_permission_prompts,
        source,
        "execution.unattended.native_permission_prompts",
        "must be false",
    )?;
    require_non_empty(
        &config.execution.unattended.sandbox_profile,
        source,
        "execution.unattended.sandbox_profile",
    )?;

    let workflow_labels = [
        &config.labels.ready,
        &config.labels.working,
        &config.labels.review,
        &config.labels.human,
    ];
    for (index, label) in workflow_labels.iter().enumerate() {
        require_non_empty(label, source, &format!("labels[{index}]"))?;
    }
    require(
        workflow_labels
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            == 4,
        source,
        "labels",
        "all workflow-state labels must be distinct",
    )?;

    let mut route_labels = BTreeSet::new();
    for (index, route) in config.routing.autonomous_classes.iter().enumerate() {
        let path = format!("routing.autonomous_classes[{index}]");
        require_non_empty(&route.label, source, &format!("{path}.label"))?;
        require(
            route.execution_mode == ExecutionMode::Unattended,
            source,
            &format!("{path}.execution_mode"),
            "must be unattended",
        )?;
        require_non_empty_vec(
            &route.allowed_paths,
            source,
            &format!("{path}.allowed_paths"),
        )?;
        require(
            !workflow_labels.contains(&&route.label),
            source,
            "routing.autonomous_classes",
            format!("label {} must not be a workflow-state label", route.label),
        )?;
        require(
            route_labels.insert(&route.label),
            source,
            "routing.autonomous_classes",
            "must use a distinct label for every autonomous class",
        )?;
    }

    require_non_empty(&config.github_app.slug, source, "github_app.slug")?;
    require(
        config.github_app.required_check_source == config.github_app.slug,
        source,
        "github_app.required_check_source",
        "must match github_app.slug in version 1",
    )?;
    require_non_empty(&config.branches.pattern, source, "branches.pattern")?;
    require(
        config.branches.require_up_to_date,
        source,
        "branches.require_up_to_date",
        "must be true in version 1",
    )?;
    require(
        !config.branches.merge_queue,
        source,
        "branches.merge_queue",
        "must be false in version 1",
    )?;
    if config.merge_policy == MergePolicy::HumanFinalApproval {
        require(
            config.branches.dismiss_stale_approvals,
            source,
            "branches.dismiss_stale_approvals",
            "must be true for human-final-approval",
        )?;
    }

    validate_runner(&config.runners.author, source, "runners.author")?;
    require_non_empty(
        &config.runners.reviewer.adapter,
        source,
        "runners.reviewer.adapter",
    )?;
    require_non_empty(
        &config.runners.reviewer.profile,
        source,
        "runners.reviewer.profile",
    )?;
    require(
        config.runners.reviewer.require_independent_context,
        source,
        "runners.reviewer.require_independent_context",
        "must be true",
    )?;

    for (field, value, minimum) in [
        ("limits.author_minutes", config.limits.author_minutes, 1),
        ("limits.review_minutes", config.limits.review_minutes, 1),
        ("limits.repair_cycles", config.limits.repair_cycles, 0),
        (
            "limits.model_turns_per_phase",
            config.limits.model_turns_per_phase,
            1,
        ),
        (
            "limits.model_tokens_per_run",
            config.limits.model_tokens_per_run,
            1,
        ),
        (
            "limits.max_changed_files",
            config.limits.max_changed_files,
            1,
        ),
        ("limits.max_diff_lines", config.limits.max_diff_lines, 1),
    ] {
        require(
            value >= minimum,
            source,
            field,
            format!("must be at least {minimum}"),
        )?;
    }
    require_decimal(
        &config.limits.model_cost_usd_per_run,
        source,
        "limits.model_cost_usd_per_run",
    )?;
    require_decimal(
        &config.limits.model_cost_usd_per_repo_day,
        source,
        "limits.model_cost_usd_per_repo_day",
    )?;
    require_non_empty(
        &config.limits.price_table_profile,
        source,
        "limits.price_table_profile",
    )?;

    require_non_empty(
        &config.review.required_check,
        source,
        "review.required_check",
    )?;
    require_non_empty_vec(
        &config.review.blocking_severities,
        source,
        "review.blocking_severities",
    )?;
    require_non_empty_vec(
        &config.review.always_blocking_categories,
        source,
        "review.always_blocking_categories",
    )?;
    require(
        config
            .review
            .always_blocking_categories
            .iter()
            .any(|category| category == "weakened_or_deleted_tests"),
        source,
        "review.always_blocking_categories",
        "must include weakened_or_deleted_tests",
    )?;
    match config.merge_policy {
        MergePolicy::SuggestOnly => require(
            !config.review.enable_auto_merge,
            source,
            "review.enable_auto_merge",
            "must be false for suggest-only",
        )?,
        MergePolicy::HumanFinalApproval => require(
            config.review.enable_auto_merge,
            source,
            "review.enable_auto_merge",
            "must be true for the native human-final-approval wait",
        )?,
        MergePolicy::AutonomousLowRisk => unreachable!("rejected above"),
    }

    require(
        !config.validation.commands.is_empty(),
        source,
        "validation.commands",
        "must not be empty",
    )?;
    for (index, command) in config.validation.commands.iter().enumerate() {
        require_non_empty(
            &command.name,
            source,
            &format!("validation.commands[{index}].name"),
        )?;
        require_non_empty_vec(
            &command.argv,
            source,
            &format!("validation.commands[{index}].argv"),
        )?;
    }
    require_non_empty_vec(
        &config.policy.protected_paths,
        source,
        "policy.protected_paths",
    )?;
    require_non_empty_vec(&config.notifications.events, source, "notifications.events")?;
    require_non_empty_vec(&config.notifications.sinks, source, "notifications.sinks")?;
    Ok(())
}

fn validate_host_config(config: &mut HostConfig, source: &str) -> Result<(), ConfigError> {
    require(config.version == 1, source, "version", "must be 1")?;
    config.state_root = normalized_absolute(&config.state_root, source, "state_root")?;
    config.workspace_root = normalized_absolute(&config.workspace_root, source, "workspace_root")?;
    require(
        !paths_overlap(
            Path::new(&config.state_root),
            Path::new(&config.workspace_root),
        ),
        source,
        "state_root",
        "must not overlap workspace_root",
    )?;
    let workspace_root = PathBuf::from(&config.workspace_root);

    require_non_empty(
        &config.metadata_signing.current_key_id,
        source,
        "metadata_signing.current_key_id",
    )?;
    config.metadata_signing.key_file = normalized_absolute(
        &config.metadata_signing.key_file,
        source,
        "metadata_signing.key_file",
    )?;
    require_outside_workspace(
        &config.metadata_signing.key_file,
        &workspace_root,
        source,
        "metadata_signing.key_file",
    )?;
    for (key_id, path) in &mut config.metadata_signing.verification_key_files {
        require_non_empty(
            key_id,
            source,
            "metadata_signing.verification_key_files key",
        )?;
        *path = normalized_absolute(
            path,
            source,
            &format!("metadata_signing.verification_key_files.{key_id}"),
        )?;
        require_outside_workspace(
            path,
            &workspace_root,
            source,
            &format!("metadata_signing.verification_key_files.{key_id}"),
        )?;
    }
    require(
        config.github_app.app_id > 0,
        source,
        "github_app.app_id",
        "must be positive",
    )?;
    config.github_app.private_key_file = normalized_absolute(
        &config.github_app.private_key_file,
        source,
        "github_app.private_key_file",
    )?;
    require_outside_workspace(
        &config.github_app.private_key_file,
        &workspace_root,
        source,
        "github_app.private_key_file",
    )?;

    for (name, profile) in &config.sandbox_profiles {
        require_non_empty(name, source, "sandbox_profiles key")?;
        require(
            !profile.shared_git_admin,
            source,
            &format!("sandbox_profiles.{name}.shared_git_admin"),
            "must be false",
        )?;
    }
    for (name, profile) in &config.workspace_profiles {
        require(
            !profile.shared_objects,
            source,
            &format!("workspace_profiles.{name}.shared_objects"),
            "must be false",
        )?;
        require(
            !profile.authenticated_remote,
            source,
            &format!("workspace_profiles.{name}.authenticated_remote"),
            "must be false",
        )?;
    }
    for (name, path) in &mut config.price_table_profiles {
        *path = normalized_absolute(path, source, &format!("price_table_profiles.{name}"))?;
        require_outside_workspace(
            path,
            &workspace_root,
            source,
            &format!("price_table_profiles.{name}"),
        )?;
    }
    for (name, sink) in &mut config.notification_sinks {
        require_non_empty_vec(
            &sink.argv,
            source,
            &format!("notification_sinks.{name}.argv"),
        )?;
        sink.argv[0] = normalized_absolute(
            &sink.argv[0],
            source,
            &format!("notification_sinks.{name}.argv[0]"),
        )?;
        require_outside_workspace(
            &sink.argv[0],
            &workspace_root,
            source,
            &format!("notification_sinks.{name}.argv[0]"),
        )?;
        require_range(
            sink.timeout_seconds,
            1,
            3600,
            source,
            &format!("notification_sinks.{name}.timeout_seconds"),
        )?;
    }
    Ok(())
}

fn require_outside_workspace(
    value: &str,
    workspace_root: &Path,
    source: &str,
    field: &str,
) -> Result<(), ConfigError> {
    require(
        !Path::new(value).starts_with(workspace_root),
        source,
        field,
        "must be outside workspace_root",
    )
}

fn validate_runner(profile: &RunnerProfile, source: &str, field: &str) -> Result<(), ConfigError> {
    require_non_empty(&profile.adapter, source, &format!("{field}.adapter"))?;
    require_non_empty(&profile.profile, source, &format!("{field}.profile"))
}

fn require(
    condition: bool,
    source: &str,
    field: &str,
    message: impl Into<String>,
) -> Result<(), ConfigError> {
    if condition {
        Ok(())
    } else {
        Err(ConfigError::invalid(
            format!("{source}.{field}"),
            message.into(),
        ))
    }
}

fn require_non_empty(value: &str, source: &str, field: &str) -> Result<(), ConfigError> {
    require(
        !value.trim().is_empty(),
        source,
        field,
        "must be a non-empty string",
    )
}

fn require_non_empty_vec(values: &[String], source: &str, field: &str) -> Result<(), ConfigError> {
    require(!values.is_empty(), source, field, "must not be empty")?;
    for (index, value) in values.iter().enumerate() {
        require_non_empty(value, source, &format!("{field}[{index}]"))?;
    }
    Ok(())
}

fn require_range(
    value: u64,
    minimum: u64,
    maximum: u64,
    source: &str,
    field: &str,
) -> Result<(), ConfigError> {
    require(
        (minimum..=maximum).contains(&value),
        source,
        field,
        format!("must be an integer between {minimum} and {maximum}"),
    )
}

fn require_decimal(value: &str, source: &str, field: &str) -> Result<(), ConfigError> {
    let valid = if let Some((whole, fraction)) = value.split_once('.') {
        valid_whole(whole)
            && !fraction.is_empty()
            && fraction.len() <= 6
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
            && !fraction.contains('.')
    } else {
        valid_whole(value)
    };
    require(
        valid,
        source,
        field,
        "must be a non-negative decimal string with at most six decimals",
    )
}

fn valid_whole(value: &str) -> bool {
    value == "0"
        || (!value.starts_with('0')
            && !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn normalized_absolute(value: &str, source: &str, field: &str) -> Result<String, ConfigError> {
    require_non_empty(value, source, field)?;
    let path = Path::new(value);
    require(
        path.is_absolute(),
        source,
        field,
        "must be an absolute path",
    )?;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
        }
    }
    require(
        normalized.parent().is_some(),
        source,
        field,
        "must not resolve to the filesystem root",
    )?;
    Ok(normalized.to_string_lossy().into_owned())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => serde_json::to_string(value).expect("string JSON"),
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        serde_json::Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("object key JSON"),
                        canonical_json(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
