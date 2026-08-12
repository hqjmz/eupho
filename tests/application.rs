use std::cell::{Cell, RefCell};
use std::fs;
use std::path::Path;

use eupho::application::{
    GitHubRead, OnceOptions, evaluate_branch_policy, observe_once_with, resolve_policy,
};
use eupho::config::load_repository_config;
use eupho::domain::{CandidateSnapshot, IssueSnapshot, RepositorySnapshot};
use eupho::github::{BranchPolicySnapshot, BranchPolicySource, GitHubError, RequiredCheckSnapshot};
use eupho::infra::CandidateStore;

struct FakeReader {
    policy: String,
    repository_reads: Cell<usize>,
    requested_bases: RefCell<Vec<Option<String>>>,
    second_policy: Option<String>,
}

impl FakeReader {
    fn new(policy: String) -> Self {
        Self {
            policy,
            repository_reads: Cell::new(0),
            requested_bases: RefCell::new(Vec::new()),
            second_policy: None,
        }
    }

    fn with_second_policy(mut self, policy: String) -> Self {
        self.second_policy = Some(policy);
        self
    }
}

impl GitHubRead for FakeReader {
    fn repository(
        &self,
        _repository: &str,
        configured_base_branch: Option<&str>,
    ) -> Result<RepositorySnapshot, GitHubError> {
        self.requested_bases
            .borrow_mut()
            .push(configured_base_branch.map(str::to_owned));
        let read = self.repository_reads.get();
        self.repository_reads.set(read + 1);
        Ok(RepositorySnapshot {
            id: 77,
            name_with_owner: "acme/widgets".to_owned(),
            default_branch: "main".to_owned(),
            base_sha: if read == 0 { "a" } else { "b" }.repeat(40),
            policy_path: Some(".github/eupho.yml".to_owned()),
            policy_content: Some(
                self.second_policy
                    .as_ref()
                    .filter(|_| read > 0)
                    .unwrap_or(&self.policy)
                    .clone(),
            ),
        })
    }

    fn ready_issues(
        &self,
        _repository: &str,
        _ready_label: &str,
        _limit: usize,
    ) -> Result<Vec<IssueSnapshot>, GitHubError> {
        Ok(vec![IssueSnapshot {
            number: 7,
            title: "Observed after lock".to_owned(),
            url: "https://github.com/acme/widgets/issues/7".to_owned(),
            labels: vec!["agent:ready".to_owned()],
            updated_at: "2026-08-12T00:00:00.000Z".to_owned(),
        }])
    }

    fn active_issue_numbers(
        &self,
        _repository: &str,
        _labels: &[String],
        _limit: usize,
    ) -> Result<Vec<u64>, GitHubError> {
        Ok(Vec::new())
    }

    fn label_exists(&self, _repository: &str, _label: &str) -> Result<bool, GitHubError> {
        Ok(true)
    }

    fn branch_policy(
        &self,
        _repository: &str,
        _branch: &str,
    ) -> Result<BranchPolicySnapshot, GitHubError> {
        Ok(strict_policy(123_456))
    }
}

fn policy_text() -> String {
    fs::read_to_string(".github/eupho.yml").expect("read policy")
}

fn strict_policy(app_id: u64) -> BranchPolicySnapshot {
    BranchPolicySnapshot {
        strict_required_checks: true,
        dismiss_stale_approvals: true,
        required_approving_review_count: 1,
        bypass_app_ids: Vec::new(),
        bypass_verification_complete: true,
        required_checks: vec![RequiredCheckSnapshot {
            context: "agent-review".to_owned(),
            app_id: Some(app_id),
            source: BranchPolicySource::Ruleset,
        }],
        sources: vec![BranchPolicySource::Ruleset],
    }
}

#[test]
fn trusted_policy_may_select_one_stable_non_default_base() {
    let policy = policy_text().replace("base_branch: main", "base_branch: release");
    let reader = FakeReader::new(policy);

    let resolved =
        resolve_policy(&reader, "acme/widgets", Path::new("."), None).expect("policy resolves");

    assert_eq!(resolved.config.base_branch, "release");
    assert!(resolved.trusted_base);
    assert_eq!(
        reader.requested_bases.into_inner(),
        vec![None, Some("release".to_owned())]
    );
}

#[test]
fn policy_redirect_chain_fails_closed() {
    let first = policy_text().replace("base_branch: main", "base_branch: release");
    let second = policy_text().replace("base_branch: main", "base_branch: other");
    let reader = FakeReader::new(first).with_second_policy(second);

    let error = resolve_policy(&reader, "acme/widgets", Path::new("."), None).unwrap_err();

    assert_eq!(error.code(), "unstable_policy_base");
}

#[test]
fn observe_only_pass_rereads_under_lock_and_persists_the_second_base() {
    let temporary = tempfile::tempdir().unwrap();
    let state_root = temporary.path().join("state");
    let reader = FakeReader::new(policy_text());
    let options = OnceOptions {
        cwd: temporary.path().to_path_buf(),
        repository: "acme/widgets".to_owned(),
        config_path: None,
        host_config_path: None,
    };

    let report = observe_once_with(
        &options,
        &reader,
        Some(&state_root),
        "2026-08-12T01:00:00.000Z",
    )
    .expect("observe pass");

    assert_eq!(reader.repository_reads.get(), 2);
    assert_eq!(report.snapshot.base_sha, "b".repeat(40));
    assert!(report.snapshot.trusted_base);
    assert_eq!(report.snapshot.candidates.len(), 1);
    let stored = CandidateStore::new(state_root)
        .get::<CandidateSnapshot>(77)
        .unwrap()
        .unwrap();
    assert_eq!(stored.base_sha, "b".repeat(40));
}

#[test]
fn branch_policy_binds_the_required_check_to_the_expected_app() {
    let config = load_repository_config(Path::new(".github/eupho.yml")).unwrap();
    let accepted = evaluate_branch_policy(&strict_policy(123_456), &config, 123_456);
    assert!(
        accepted
            .iter()
            .all(|check| check.status != eupho::application::DiagnosticStatus::Fail)
    );

    let rejected = evaluate_branch_policy(&strict_policy(999_999), &config, 123_456);
    assert_eq!(
        rejected
            .iter()
            .find(|check| check.code == "github.expected_check_source")
            .unwrap()
            .status,
        eupho::application::DiagnosticStatus::Fail
    );
}
