use std::fs;
use std::path::Path;

use eupho::config::{ExecutionMode, MergePolicy, WorkspaceType, parse_repository_config_text};
use eupho::domain::{
    AttentionCode, IssueSnapshot, RepositorySnapshot, RunAttempts, RunEvent, RunEventType,
    RunPhase, RunRecord, RunState, RunUsage, can_transition, issue_label_projection,
    plan_candidates, transition,
};

fn config() -> eupho::config::RepositoryConfig {
    let source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/eupho.yml"))
            .unwrap();
    parse_repository_config_text(&source, "policy.yml").unwrap()
}

fn repository() -> RepositorySnapshot {
    RepositorySnapshot {
        id: 4242,
        name_with_owner: "example/eupho-target".into(),
        default_branch: "main".into(),
        base_sha: "1111111111111111111111111111111111111111".into(),
        policy_path: Some(".github/eupho.yml".into()),
        policy_content: None,
    }
}

fn issue(number: u64, labels: &[&str]) -> IssueSnapshot {
    IssueSnapshot {
        number,
        title: format!("Issue {number}"),
        url: format!("https://example.invalid/{number}"),
        labels: labels.iter().map(|label| (*label).to_owned()).collect(),
        updated_at: "2026-08-12T00:00:00.000Z".into(),
    }
}

#[test]
fn planning_is_deterministic_sorted_and_capacity_bound() {
    let issues = vec![
        issue(9, &["agent:ready"]),
        issue(5, &["agent:ready", "agent:wip"]),
        issue(1, &["agent:ready", "agent:risk:docs-only"]),
        issue(7, &["agent:ready"]),
        issue(2, &["triage"]),
    ];
    let first = plan_candidates(&repository(), &issues, &config(), &[]).unwrap();
    let reversed = issues.iter().cloned().rev().collect::<Vec<_>>();
    let repeated = plan_candidates(&repository(), &reversed, &config(), &[]).unwrap();
    assert_eq!(first, repeated);
    assert_eq!(
        first
            .candidates
            .iter()
            .map(|candidate| candidate.issue_number)
            .collect::<Vec<_>>(),
        [1, 7]
    );
    assert_eq!(
        first
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code.as_str(), diagnostic.issue_number))
            .collect::<Vec<_>>(),
        [
            ("ineligible_state_labels", 2),
            ("ineligible_state_labels", 5),
            ("capacity_deferred", 9),
        ]
    );
}

#[test]
fn routes_and_candidate_identity_bind_execution_base_and_policy() {
    let policy = config();
    let result = plan_candidates(
        &repository(),
        &[
            issue(2, &["agent:ready"]),
            issue(1, &["agent:ready", "agent:risk:docs-only"]),
        ],
        &policy,
        &[],
    )
    .unwrap();
    let routed = result
        .candidates
        .iter()
        .find(|candidate| candidate.issue_number == 1)
        .unwrap();
    assert_eq!(routed.execution_mode, ExecutionMode::Unattended);
    assert_eq!(routed.workspace_type, WorkspaceType::EphemeralClone);
    assert_eq!(routed.merge_policy, MergePolicy::AutonomousLowRisk);
    assert_eq!(routed.route_label.as_deref(), Some("agent:risk:docs-only"));

    let original = plan_candidates(&repository(), &[issue(3, &["agent:ready"])], &policy, &[])
        .unwrap()
        .candidates
        .remove(0);
    assert_eq!(original.candidate_id, "candidate-3b80bbd0b689fd2683bf");
    let mut changed_base = repository();
    changed_base.base_sha = "2222222222222222222222222222222222222222".into();
    let rebound =
        plan_candidates(&changed_base, &[issue(3, &["agent:ready"])], &policy, &[]).unwrap();
    assert_ne!(original.candidate_id, rebound.candidates[0].candidate_id);

    let mut changed_policy = policy;
    changed_policy.limits.max_diff_lines += 1;
    let changed = plan_candidates(
        &repository(),
        &[issue(3, &["agent:ready"])],
        &changed_policy,
        &[],
    )
    .unwrap();
    assert_ne!(original.policy_digest, changed.policy_digest);
    assert_ne!(original.candidate_id, changed.candidates[0].candidate_id);
}

#[test]
fn active_runs_consume_capacity_and_ambiguous_routes_fail_closed() {
    let mut policy = config();
    let capacity = plan_candidates(
        &repository(),
        &[issue(1, &["agent:ready"]), issue(2, &["agent:ready"])],
        &policy,
        &[90, 90],
    )
    .unwrap();
    assert_eq!(capacity.candidates.len(), 1);
    assert_eq!(capacity.candidates[0].issue_number, 1);

    let mut second = policy.routing.autonomous_classes[0].clone();
    second.label = "agent:risk:test-only".into();
    policy.routing.autonomous_classes.push(second);
    let ambiguous = plan_candidates(
        &repository(),
        &[issue(
            8,
            &[
                "agent:ready",
                "agent:risk:docs-only",
                "agent:risk:test-only",
            ],
        )],
        &policy,
        &[],
    )
    .unwrap();
    assert!(ambiguous.candidates.is_empty());
    assert_eq!(ambiguous.diagnostics[0].code, "ambiguous_autonomous_route");
}

#[test]
fn a_ready_issue_also_observed_as_active_is_never_selected() {
    let policy = config();
    let result = plan_candidates(
        &repository(),
        &[issue(1, &["agent:ready"]), issue(2, &["agent:ready"])],
        &policy,
        &[1],
    )
    .unwrap();

    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.issue_number)
            .collect::<Vec<_>>(),
        vec![2]
    );
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.issue_number == 1 && diagnostic.code == "already_active"
        })
    );
}

#[test]
fn lifecycle_enforces_phase_order_and_human_approval_policy() {
    let mut current = run();
    current = apply(&current, RunEventType::Claim, 1);
    current = apply(&current, RunEventType::AuthorCompleted, 2);
    current = apply(&current, RunEventType::ValidationCompleted, 3);
    current = apply(&current, RunEventType::ReviewCleanRequiresApproval, 4);
    assert_eq!(current.state, RunState::InReview);
    assert_eq!(current.phase, RunPhase::AwaitingApproval);
    assert_eq!(
        issue_label_projection(current.state, current.phase),
        Some("review")
    );
    assert!(!can_transition(
        &current,
        RunEventType::ReviewCleanAutonomous
    ));
    current = apply(&current, RunEventType::ApprovalRecorded, 5);
    current = apply(&current, RunEventType::Merged, 6);
    assert_eq!(
        (current.state, current.phase),
        (RunState::Merged, RunPhase::Complete)
    );
    assert_eq!(current.revision, 6);
    assert_eq!(issue_label_projection(current.state, current.phase), None);

    let authoring = RunRecord {
        state: RunState::WorkInProgress,
        phase: RunPhase::Author,
        ..run()
    };
    assert!(!can_transition(
        &authoring,
        RunEventType::ValidationCompleted
    ));
}

#[test]
fn escalation_resume_no_change_and_binding_changes_are_reason_bound() {
    let validating = RunRecord {
        state: RunState::WorkInProgress,
        phase: RunPhase::Validation,
        ..run()
    };
    let paused = transition(
        &validating,
        event(
            RunEventType::Escalate,
            1,
            Some("validation needs permission"),
        ),
    )
    .unwrap();
    assert_eq!(paused.attention_code, Some(AttentionCode::Escalation));
    assert_eq!(paused.resume_phase, Some(RunPhase::Validation));
    assert!(!can_transition(&paused, RunEventType::AcceptNoChange));
    let resumed = apply(&paused, RunEventType::Resume, 2);
    assert_eq!(
        (resumed.state, resumed.phase),
        (RunState::WorkInProgress, RunPhase::Validation)
    );
    assert_eq!(resumed.attention_code, None);

    let authoring = RunRecord {
        state: RunState::WorkInProgress,
        phase: RunPhase::Author,
        ..run()
    };
    let no_change = apply(&authoring, RunEventType::NoChange, 3);
    assert_eq!(no_change.attention_code, Some(AttentionCode::NoChange));
    let accepted = apply(&no_change, RunEventType::AcceptNoChange, 4);
    assert_eq!(accepted.state, RunState::CompletedNoChange);

    let approval = RunRecord {
        state: RunState::InReview,
        phase: RunPhase::AwaitingApproval,
        ..run()
    };
    let rebound = apply(&approval, RunEventType::BindingChanged, 5);
    assert_eq!(
        (rebound.state, rebound.phase),
        (RunState::WorkInProgress, RunPhase::Validation)
    );
}

#[test]
fn autonomous_human_and_suggest_only_merge_paths_are_disjoint() {
    let human_review = RunRecord {
        state: RunState::InReview,
        phase: RunPhase::Review,
        ..run()
    };
    assert!(!can_transition(
        &human_review,
        RunEventType::ReviewCleanAutonomous
    ));

    let mut autonomous = RunRecord {
        state: RunState::InReview,
        phase: RunPhase::Review,
        merge_policy: MergePolicy::AutonomousLowRisk,
        execution_mode: ExecutionMode::Unattended,
        workspace_type: WorkspaceType::EphemeralClone,
        ..run()
    };
    autonomous = apply(&autonomous, RunEventType::ReviewCleanAutonomous, 1);
    autonomous = apply(&autonomous, RunEventType::Merged, 2);
    assert_eq!(autonomous.state, RunState::Merged);

    let mut suggest = RunRecord {
        state: RunState::InReview,
        phase: RunPhase::Review,
        merge_policy: MergePolicy::SuggestOnly,
        ..run()
    };
    suggest = apply(&suggest, RunEventType::ReviewCleanSuggestOnly, 3);
    assert_eq!(suggest.phase, RunPhase::MergeWait);
    assert!(!can_transition(
        &suggest,
        RunEventType::ReviewCleanRequiresApproval
    ));
}

fn run() -> RunRecord {
    RunRecord {
        schema_version: 1,
        revision: 0,
        run_id: "run-001".into(),
        repository_id: 4242,
        repository: "example/eupho-target".into(),
        issue_number: 17,
        state: RunState::Ready,
        phase: RunPhase::Intake,
        execution_mode: ExecutionMode::Attended,
        workspace_type: WorkspaceType::Worktree,
        merge_policy: MergePolicy::HumanFinalApproval,
        base_branch: "main".into(),
        base_sha: "1111111111111111111111111111111111111111".into(),
        branch: None,
        pull_request: None,
        head_sha: None,
        review_binding: None,
        attempts: RunAttempts::default(),
        usage: RunUsage {
            model_tokens: 0,
            cost_usd: "0".into(),
        },
        attention_code: None,
        attention_reason: None,
        resume_phase: None,
        created_at: at(0),
        updated_at: at(0),
    }
}

fn at(second: u8) -> String {
    format!("2026-08-12T00:00:{second:02}.000Z")
}

fn event(event_type: RunEventType, second: u8, reason: Option<&str>) -> RunEvent {
    RunEvent {
        event_type,
        at: at(second),
        reason: reason.map(str::to_owned),
    }
}

fn apply(run: &RunRecord, event_type: RunEventType, second: u8) -> RunRecord {
    transition(run, event(event_type, second, None)).unwrap()
}
