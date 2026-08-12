//! Pure domain model, deterministic candidate planning, and lifecycle rules.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{
    ConfigError, ExecutionMode, MergePolicy, RepositoryConfig, WorkspaceType,
    repository_policy_digest,
};

#[derive(Debug)]
pub enum DomainError {
    Config(ConfigError),
    InvalidTransition(String),
    RevisionOverflow(String),
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
            Self::InvalidTransition(message) => formatter.write_str(message),
            Self::RevisionOverflow(run_id) => {
                write!(formatter, "run {run_id} revision overflowed")
            }
        }
    }
}

impl std::error::Error for DomainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ConfigError> for DomainError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSnapshot {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub labels: Vec<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositorySnapshot {
    pub id: u64,
    pub name_with_owner: String,
    pub default_branch: String,
    pub base_sha: String,
    pub policy_path: Option<String>,
    pub policy_content: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryCapacitySnapshot {
    pub active_issue_numbers: Vec<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannedAction {
    WouldClaim,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidatePlan {
    pub candidate_id: String,
    pub action: PlannedAction,
    pub repository_id: u64,
    pub repository: String,
    pub issue_number: u64,
    pub issue_title: String,
    pub issue_url: String,
    pub base_sha: String,
    pub policy_digest: String,
    pub execution_mode: ExecutionMode,
    pub workspace_type: WorkspaceType,
    pub merge_policy: MergePolicy,
    pub route_label: Option<String>,
    pub preconditions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanningDiagnostic {
    pub code: String,
    pub issue_number: u64,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateSnapshot {
    pub schema_version: u32,
    pub repository_id: u64,
    pub repository: String,
    pub base_sha: String,
    pub policy_digest: String,
    pub policy_source: String,
    pub trusted_base: bool,
    pub observed_at: String,
    pub candidates: Vec<CandidatePlan>,
    pub diagnostics: Vec<PlanningDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanResult {
    pub candidates: Vec<CandidatePlan>,
    pub diagnostics: Vec<PlanningDiagnostic>,
    pub policy_digest: String,
}

/// Produces the same stable identity as the TypeScript implementation.
#[must_use]
pub fn stable_candidate_id(
    repository_id: u64,
    issue_number: u64,
    base_sha: &str,
    policy_digest: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(repository_id.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(issue_number.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(base_sha.as_bytes());
    hasher.update([0]);
    hasher.update(policy_digest.as_bytes());
    let digest = hasher.finalize();
    format!("candidate-{}", hex_prefix(&digest, 20))
}

/// Plans an observe-only reconciliation pass.
///
/// Inputs are never mutated. Issues are ordered by issue number, active issue
/// numbers are de-duplicated before consuming capacity, and ambiguous policy
/// matches fail closed with a diagnostic.
///
/// # Errors
///
/// Returns an error if the repository policy cannot be canonically digested.
#[allow(clippy::too_many_lines)]
pub fn plan_candidates(
    repository: &RepositorySnapshot,
    issues: &[IssueSnapshot],
    config: &RepositoryConfig,
    active_issue_numbers: &[u64],
) -> Result<PlanResult, DomainError> {
    let policy_digest = repository_policy_digest(config)?;
    let workflow_labels = BTreeSet::from([
        config.labels.ready.as_str(),
        config.labels.working.as_str(),
        config.labels.review.as_str(),
        config.labels.human.as_str(),
    ]);
    let mut diagnostics = Vec::new();
    let mut eligible = Vec::new();
    let active_issue_numbers = active_issue_numbers
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut sorted_issues = issues.iter().collect::<Vec<_>>();
    sorted_issues.sort_by_key(|issue| issue.number);

    for issue in sorted_issues {
        let active_workflow_labels = issue
            .labels
            .iter()
            .filter(|label| workflow_labels.contains(label.as_str()))
            .map(String::as_str)
            .collect::<Vec<_>>();
        if active_workflow_labels.len() != 1
            || active_workflow_labels.first().copied() != Some(config.labels.ready.as_str())
        {
            diagnostics.push(PlanningDiagnostic {
                code: "ineligible_state_labels".to_owned(),
                issue_number: issue.number,
                message: format!(
                    "Expected only {}; found {}",
                    config.labels.ready,
                    if active_workflow_labels.is_empty() {
                        "none".to_owned()
                    } else {
                        active_workflow_labels.join(", ")
                    }
                ),
            });
            continue;
        }

        if active_issue_numbers.contains(&issue.number) {
            diagnostics.push(PlanningDiagnostic {
                code: "already_active".to_owned(),
                issue_number: issue.number,
                message: "Issue was also observed in an active workflow state; refusing to plan a duplicate run"
                    .to_owned(),
            });
            continue;
        }

        let matching_routes = config
            .routing
            .autonomous_classes
            .iter()
            .filter(|route| issue.labels.iter().any(|label| label == &route.label))
            .collect::<Vec<_>>();
        if matching_routes.len() > 1 {
            diagnostics.push(PlanningDiagnostic {
                code: "ambiguous_autonomous_route".to_owned(),
                issue_number: issue.number,
                message: format!(
                    "Issue matches multiple autonomous classes: {}",
                    matching_routes
                        .iter()
                        .map(|route| route.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
            continue;
        }
        let route = matching_routes.first().copied();
        let execution_mode =
            route.map_or(config.execution.default_mode, |route| route.execution_mode);
        let workspace_type = match execution_mode {
            ExecutionMode::Attended => config.execution.attended.workspace,
            ExecutionMode::Unattended => config.execution.unattended.workspace,
        };
        let merge_policy = if route.is_some() {
            MergePolicy::AutonomousLowRisk
        } else {
            config.merge_policy
        };

        eligible.push(CandidatePlan {
            candidate_id: stable_candidate_id(
                repository.id,
                issue.number,
                &repository.base_sha,
                &policy_digest,
            ),
            action: PlannedAction::WouldClaim,
            repository_id: repository.id,
            repository: repository.name_with_owner.clone(),
            issue_number: issue.number,
            issue_title: issue.title.clone(),
            issue_url: issue.url.clone(),
            base_sha: repository.base_sha.clone(),
            policy_digest: policy_digest.clone(),
            execution_mode,
            workspace_type,
            merge_policy,
            route_label: route.map(|route| route.label.clone()),
            preconditions: vec![
                format!("issue remains open with only {}", config.labels.ready),
                format!("base remains {}", repository.base_sha),
                "repository capacity remains available".to_owned(),
            ],
        });
    }

    let active_count = active_issue_numbers.len();
    let available_capacity = config.concurrency.saturating_sub(active_count);
    let deferred = eligible.split_off(eligible.len().min(available_capacity));
    for candidate in deferred {
        diagnostics.push(PlanningDiagnostic {
            code: "capacity_deferred".to_owned(),
            issue_number: candidate.issue_number,
            message: format!(
                "Deferred by concurrency limit {}; {active_count} active run(s) observed",
                config.concurrency
            ),
        });
    }

    Ok(PlanResult {
        candidates: eligible,
        diagnostics,
        policy_digest,
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Ready,
    WorkInProgress,
    InReview,
    NeedsHuman,
    Merged,
    CompletedNoChange,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Intake,
    Author,
    Validation,
    Review,
    Repair,
    AwaitingApproval,
    MergeWait,
    Paused,
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionCode {
    NoChange,
    Escalation,
    ExternalChange,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewBinding {
    pub base_sha: String,
    pub head_sha: String,
    pub diff_hash: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunAttempts {
    pub author: u64,
    pub validation: u64,
    pub review: u64,
    pub repair: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunUsage {
    pub model_tokens: u64,
    pub cost_usd: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub schema_version: u32,
    pub revision: u64,
    pub run_id: String,
    pub repository_id: u64,
    pub repository: String,
    pub issue_number: u64,
    pub state: RunState,
    pub phase: RunPhase,
    pub execution_mode: ExecutionMode,
    pub workspace_type: WorkspaceType,
    pub merge_policy: MergePolicy,
    pub base_branch: String,
    pub base_sha: String,
    pub branch: Option<String>,
    pub pull_request: Option<u64>,
    pub head_sha: Option<String>,
    pub review_binding: Option<ReviewBinding>,
    pub attempts: RunAttempts,
    pub usage: RunUsage,
    pub attention_code: Option<AttentionCode>,
    pub attention_reason: Option<String>,
    pub resume_phase: Option<RunPhase>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunEventType {
    Claim,
    AuthorCompleted,
    NoChange,
    ValidationCompleted,
    ReviewHasFindings,
    ReviewCleanAutonomous,
    ReviewCleanRequiresApproval,
    ReviewCleanSuggestOnly,
    ApprovalRecorded,
    BindingChanged,
    Merged,
    Escalate,
    Resume,
    AcceptNoChange,
    Cancel,
    ExternalChange,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunEvent {
    #[serde(rename = "type")]
    pub event_type: RunEventType,
    pub at: String,
    pub reason: Option<String>,
}

#[derive(Clone, Copy)]
struct TransitionTarget {
    state: RunState,
    phase: RunPhase,
}

#[must_use]
pub fn can_transition(run: &RunRecord, event_type: RunEventType) -> bool {
    resolve_transition(run, event_type).is_ok()
}

/// Applies one lifecycle event without mutating the source record.
///
/// # Errors
///
/// Returns [`DomainError::InvalidTransition`] if the exact state/phase does not
/// accept the event or its merge-policy guard fails. Revision overflow also
/// fails closed.
pub fn transition(run: &RunRecord, event: RunEvent) -> Result<RunRecord, DomainError> {
    let target = resolve_transition(run, event.event_type)?;
    let escalating = target.state == RunState::NeedsHuman;
    let resuming = event.event_type == RunEventType::Resume;
    let mut next = run.clone();
    next.revision = run
        .revision
        .checked_add(1)
        .ok_or_else(|| DomainError::RevisionOverflow(run.run_id.clone()))?;
    next.state = target.state;
    next.phase = target.phase;
    if escalating {
        next.attention_code = Some(attention_code_for(event.event_type)?);
        next.attention_reason = Some(
            event
                .reason
                .unwrap_or_else(|| event_name(event.event_type).to_owned()),
        );
        next.resume_phase = Some(run.phase);
    } else if resuming {
        next.attention_code = None;
        next.attention_reason = None;
        next.resume_phase = None;
    }
    next.updated_at = event.at;
    Ok(next)
}

#[must_use]
pub fn issue_label_projection(state: RunState, _phase: RunPhase) -> Option<&'static str> {
    match state {
        RunState::Ready => Some("ready"),
        RunState::WorkInProgress => Some("working"),
        RunState::InReview => Some("review"),
        RunState::NeedsHuman => Some("human"),
        RunState::Merged | RunState::CompletedNoChange | RunState::Cancelled => None,
    }
}

#[allow(clippy::too_many_lines, clippy::match_same_arms)]
fn resolve_transition(
    run: &RunRecord,
    event: RunEventType,
) -> Result<TransitionTarget, DomainError> {
    use RunEventType as Event;
    use RunPhase as Phase;
    use RunState as State;

    let cancelled = TransitionTarget {
        state: State::Cancelled,
        phase: Phase::Complete,
    };
    let paused = TransitionTarget {
        state: State::NeedsHuman,
        phase: Phase::Paused,
    };
    let revalidate = TransitionTarget {
        state: State::WorkInProgress,
        phase: Phase::Validation,
    };
    let target = match (run.state, run.phase, event) {
        (State::Ready, Phase::Intake, Event::Claim) => TransitionTarget {
            state: State::WorkInProgress,
            phase: Phase::Author,
        },

        (State::WorkInProgress, Phase::Author, Event::AuthorCompleted) => revalidate,
        (State::WorkInProgress, Phase::Validation, Event::ValidationCompleted) => {
            TransitionTarget {
                state: State::InReview,
                phase: Phase::Review,
            }
        }
        (State::InReview, Phase::Review, Event::ReviewHasFindings) => TransitionTarget {
            state: State::InReview,
            phase: Phase::Repair,
        },
        (State::InReview, Phase::Repair, Event::AuthorCompleted) => revalidate,

        (State::InReview, Phase::Review, Event::ReviewCleanAutonomous) => {
            require_policy(run, MergePolicy::AutonomousLowRisk)?;
            TransitionTarget {
                state: State::InReview,
                phase: Phase::MergeWait,
            }
        }
        (State::InReview, Phase::Review, Event::ReviewCleanRequiresApproval) => {
            require_policy(run, MergePolicy::HumanFinalApproval)?;
            TransitionTarget {
                state: State::InReview,
                phase: Phase::AwaitingApproval,
            }
        }
        (State::InReview, Phase::Review, Event::ReviewCleanSuggestOnly) => {
            require_policy(run, MergePolicy::SuggestOnly)?;
            TransitionTarget {
                state: State::InReview,
                phase: Phase::MergeWait,
            }
        }
        (State::InReview, Phase::AwaitingApproval, Event::ApprovalRecorded) => {
            require_policy(run, MergePolicy::HumanFinalApproval)?;
            TransitionTarget {
                state: State::InReview,
                phase: Phase::AwaitingApproval,
            }
        }
        (State::InReview, Phase::AwaitingApproval, Event::Merged) => {
            require_policy(run, MergePolicy::HumanFinalApproval)?;
            TransitionTarget {
                state: State::Merged,
                phase: Phase::Complete,
            }
        }
        (State::InReview, Phase::MergeWait, Event::Merged) => {
            require_one_of_policies(
                run,
                &[MergePolicy::AutonomousLowRisk, MergePolicy::SuggestOnly],
            )?;
            TransitionTarget {
                state: State::Merged,
                phase: Phase::Complete,
            }
        }

        (
            State::InReview,
            Phase::Review | Phase::Repair | Phase::AwaitingApproval | Phase::MergeWait,
            Event::BindingChanged,
        ) => revalidate,

        (
            State::WorkInProgress,
            Phase::Author | Phase::Validation,
            Event::NoChange | Event::Escalate | Event::ExternalChange,
        )
        | (
            State::InReview,
            Phase::Review | Phase::Repair | Phase::AwaitingApproval | Phase::MergeWait,
            Event::Escalate | Event::ExternalChange,
        ) => paused,

        (State::NeedsHuman, Phase::Paused, Event::Resume) => resume_target(run)?,
        (State::NeedsHuman, Phase::Paused, Event::AcceptNoChange) => {
            if run.attention_code != Some(AttentionCode::NoChange) {
                return Err(DomainError::InvalidTransition(format!(
                    "Run {} is not awaiting no-change confirmation",
                    run.run_id
                )));
            }
            TransitionTarget {
                state: State::CompletedNoChange,
                phase: Phase::Complete,
            }
        }

        (State::Ready, Phase::Intake, Event::Cancel)
        | (State::WorkInProgress, Phase::Author | Phase::Validation, Event::Cancel)
        | (
            State::InReview,
            Phase::Review | Phase::Repair | Phase::AwaitingApproval | Phase::MergeWait,
            Event::Cancel,
        )
        | (State::NeedsHuman, Phase::Paused, Event::Cancel) => cancelled,

        _ => return Err(invalid_transition(run, event)),
    };
    Ok(target)
}

fn require_policy(run: &RunRecord, expected: MergePolicy) -> Result<(), DomainError> {
    if run.merge_policy == expected {
        Ok(())
    } else {
        Err(DomainError::InvalidTransition(format!(
            "{} requires merge policy {}, not {}",
            run.run_id,
            policy_name(expected),
            policy_name(run.merge_policy)
        )))
    }
}

fn require_one_of_policies(run: &RunRecord, expected: &[MergePolicy]) -> Result<(), DomainError> {
    if expected.contains(&run.merge_policy) {
        Ok(())
    } else {
        Err(DomainError::InvalidTransition(format!(
            "{} requires merge policy {}, not {}",
            run.run_id,
            expected
                .iter()
                .map(|policy| policy_name(*policy))
                .collect::<Vec<_>>()
                .join(" or "),
            policy_name(run.merge_policy)
        )))
    }
}

fn resume_target(run: &RunRecord) -> Result<TransitionTarget, DomainError> {
    match run.resume_phase {
        Some(phase @ (RunPhase::Author | RunPhase::Validation)) => Ok(TransitionTarget {
            state: RunState::WorkInProgress,
            phase,
        }),
        Some(
            phase @ (RunPhase::Review
            | RunPhase::Repair
            | RunPhase::AwaitingApproval
            | RunPhase::MergeWait),
        ) => Ok(TransitionTarget {
            state: RunState::InReview,
            phase,
        }),
        _ => Err(DomainError::InvalidTransition(format!(
            "Run {} has no safe resume phase",
            run.run_id
        ))),
    }
}

fn attention_code_for(event: RunEventType) -> Result<AttentionCode, DomainError> {
    match event {
        RunEventType::NoChange => Ok(AttentionCode::NoChange),
        RunEventType::Escalate => Ok(AttentionCode::Escalation),
        RunEventType::ExternalChange => Ok(AttentionCode::ExternalChange),
        _ => Err(DomainError::InvalidTransition(format!(
            "{} cannot create an attention state",
            event_name(event)
        ))),
    }
}

fn invalid_transition(run: &RunRecord, event: RunEventType) -> DomainError {
    DomainError::InvalidTransition(format!(
        "Cannot apply {} while run {} is {}/{}",
        event_name(event),
        run.run_id,
        state_name(run.state),
        phase_name(run.phase)
    ))
}

fn event_name(event: RunEventType) -> &'static str {
    match event {
        RunEventType::Claim => "claim",
        RunEventType::AuthorCompleted => "author_completed",
        RunEventType::NoChange => "no_change",
        RunEventType::ValidationCompleted => "validation_completed",
        RunEventType::ReviewHasFindings => "review_has_findings",
        RunEventType::ReviewCleanAutonomous => "review_clean_autonomous",
        RunEventType::ReviewCleanRequiresApproval => "review_clean_requires_approval",
        RunEventType::ReviewCleanSuggestOnly => "review_clean_suggest_only",
        RunEventType::ApprovalRecorded => "approval_recorded",
        RunEventType::BindingChanged => "binding_changed",
        RunEventType::Merged => "merged",
        RunEventType::Escalate => "escalate",
        RunEventType::Resume => "resume",
        RunEventType::AcceptNoChange => "accept_no_change",
        RunEventType::Cancel => "cancel",
        RunEventType::ExternalChange => "external_change",
    }
}

fn policy_name(policy: MergePolicy) -> &'static str {
    match policy {
        MergePolicy::AutonomousLowRisk => "autonomous-low-risk",
        MergePolicy::HumanFinalApproval => "human-final-approval",
        MergePolicy::SuggestOnly => "suggest-only",
    }
}

fn state_name(state: RunState) -> &'static str {
    match state {
        RunState::Ready => "ready",
        RunState::WorkInProgress => "work_in_progress",
        RunState::InReview => "in_review",
        RunState::NeedsHuman => "needs_human",
        RunState::Merged => "merged",
        RunState::CompletedNoChange => "completed_no_change",
        RunState::Cancelled => "cancelled",
    }
}

fn phase_name(phase: RunPhase) -> &'static str {
    match phase {
        RunPhase::Intake => "intake",
        RunPhase::Author => "author",
        RunPhase::Validation => "validation",
        RunPhase::Review => "review",
        RunPhase::Repair => "repair",
        RunPhase::AwaitingApproval => "awaiting_approval",
        RunPhase::MergeWait => "merge_wait",
        RunPhase::Paused => "paused",
        RunPhase::Complete => "complete",
    }
}

fn hex_prefix(bytes: &[u8], digits: usize) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(digits);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
        if output.len() >= digits {
            output.truncate(digits);
            break;
        }
    }
    output
}
