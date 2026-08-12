use std::fs;
use std::path::Path;

use eupho::config::{
    ExecutionMode, MergePolicy, WorkspaceType, parse_host_config_text,
    parse_repository_config_text, repository_policy_digest,
};

fn repository_policy() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/eupho.yml"))
        .expect("checked-in policy")
}

fn host_policy() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("config/examples/host.yml"))
        .expect("checked-in host policy")
}

#[test]
fn repository_policy_compiles_to_the_strict_domain_shape() {
    let config = parse_repository_config_text(&repository_policy(), "policy.yml").unwrap();
    assert_eq!(config.version, 1);
    assert_eq!(config.base_branch, "main");
    assert_eq!(config.concurrency, 2);
    assert_eq!(config.execution.default_mode, ExecutionMode::Attended);
    assert_eq!(config.execution.attended.workspace, WorkspaceType::Worktree);
    assert_eq!(
        config.execution.unattended.workspace,
        WorkspaceType::EphemeralClone
    );
    assert_eq!(config.merge_policy, MergePolicy::HumanFinalApproval);
    assert!(
        config
            .review
            .always_blocking_categories
            .iter()
            .any(|category| category == "weakened_or_deleted_tests")
    );
    assert_eq!(config.limits.model_cost_usd_per_run, "8.00");
}

#[test]
fn unknown_fields_and_ambiguous_workflow_labels_fail_closed() {
    let policy = repository_policy();
    let unknown = policy.replace(
        "concurrency: 2",
        "concurrency: 2\nexperimental_shortcut: true",
    );
    let error = parse_repository_config_text(&unknown, "unknown.yml").unwrap_err();
    assert!(error.to_string().contains("unknown field"));
    assert!(error.to_string().contains("experimental_shortcut"));

    let duplicate = policy.replace("working: agent:wip", "working: agent:ready");
    let error = parse_repository_config_text(&duplicate, "duplicate.yml").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("workflow-state labels must be distinct")
    );
}

#[test]
fn execution_and_merge_safety_invariants_are_validated() {
    let policy = repository_policy();
    let prompts = policy.replace(
        "unattended:\n    workspace: ephemeral_clone\n    native_permission_prompts: false",
        "unattended:\n    workspace: ephemeral_clone\n    native_permission_prompts: true",
    );
    assert!(
        parse_repository_config_text(&prompts, "unsafe.yml")
            .unwrap_err()
            .to_string()
            .contains("native_permission_prompts must be false")
    );

    let global_autonomous = policy.replace(
        "merge_policy: human-final-approval",
        "merge_policy: autonomous-low-risk",
    );
    assert!(
        parse_repository_config_text(&global_autonomous, "global.yml")
            .unwrap_err()
            .to_string()
            .contains("cannot be autonomous-low-risk globally")
    );

    let weakened_tests = policy.replace(
        "always_blocking_categories:\n    - weakened_or_deleted_tests",
        "always_blocking_categories:\n    - security",
    );
    assert!(
        parse_repository_config_text(&weakened_tests, "test-integrity.yml")
            .unwrap_err()
            .to_string()
            .contains("must include weakened_or_deleted_tests")
    );
}

#[test]
fn policy_digest_matches_the_typescript_compatibility_vector() {
    let config = parse_repository_config_text(&repository_policy(), "policy.yml").unwrap();
    assert_eq!(
        repository_policy_digest(&config).unwrap(),
        "sha256:9e542ed7f8173119904e28a09a6c1905daed8475c534db7a2d7e21cee09f5850"
    );
}

#[test]
fn host_policy_rejects_relative_root_overlap_and_unsafe_profiles() {
    let source = host_policy();
    let config = parse_host_config_text(&source, "host.yml").unwrap();
    assert_eq!(config.github_app.app_id, 123456);
    assert!(
        !config
            .sandbox_profiles
            .get("hardened-container")
            .unwrap()
            .shared_git_admin
    );

    let relative = source.replace(
        "state_root: /absolute/admin-owned/path/eupho/state",
        "state_root: relative/state",
    );
    assert!(
        parse_host_config_text(&relative, "relative.yml")
            .unwrap_err()
            .to_string()
            .contains("must be an absolute path")
    );

    let overlap = source.replace(
        "workspace_root: /absolute/admin-owned/path/eupho/workspaces",
        "workspace_root: /absolute/admin-owned/path/eupho/state/workspaces",
    );
    assert!(
        parse_host_config_text(&overlap, "overlap.yml")
            .unwrap_err()
            .to_string()
            .contains("must not overlap workspace_root")
    );

    let shared = source.replace("shared_objects: false", "shared_objects: true");
    assert!(
        parse_host_config_text(&shared, "shared.yml")
            .unwrap_err()
            .to_string()
            .contains("shared_objects must be false")
    );

    let exposed_key = source.replace(
        "key_file: /absolute/admin-owned/path/eupho/keys/metadata-hmac.key",
        "key_file: /absolute/admin-owned/path/eupho/workspaces/exposed.key",
    );
    assert!(
        parse_host_config_text(&exposed_key, "exposed-key.yml")
            .unwrap_err()
            .to_string()
            .contains("must be outside workspace_root")
    );
}
