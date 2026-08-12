use eupho::github::{BranchPolicySource, GhReader, RequiredCheckSnapshot};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tempfile::tempdir;

#[cfg(unix)]
#[test]
fn github_reader_validates_and_normalizes_read_only_responses() {
    with_fake_gh(HashMap::new(), |reader| {
        let repository = reader.repository("acme/widgets", None).unwrap();
        assert_eq!(repository.id, 77);
        assert_eq!(repository.base_sha, "a".repeat(40));
        assert_eq!(repository.policy_path.as_deref(), Some(".github/eupho.yml"));
        assert_eq!(repository.policy_content.as_deref(), Some("version: 1\n"));

        let issues = reader
            .ready_issues("acme/widgets", "agent:ready", 100)
            .unwrap();
        assert_eq!(
            issues.iter().map(|issue| issue.number).collect::<Vec<_>>(),
            vec![7]
        );
        let labels = vec!["agent:wip".to_owned(), "in-review".to_owned()];
        assert_eq!(
            reader
                .active_issue_numbers("acme/widgets", &labels, 100)
                .unwrap(),
            vec![9]
        );
        assert!(reader.label_exists("acme/widgets", "agent:ready").unwrap());
        let policy = reader.branch_policy("acme/widgets", "main").unwrap();
        assert!(policy.strict_required_checks);
        assert!(policy.dismiss_stale_approvals);
        assert_eq!(policy.required_approving_review_count, 1);
        assert!(policy.bypass_app_ids.is_empty());
        assert!(policy.bypass_verification_complete);
        assert_eq!(
            policy.required_checks,
            vec![RequiredCheckSnapshot {
                context: "agent-review".to_owned(),
                app_id: Some(123456),
                source: BranchPolicySource::ClassicProtection,
            }]
        );
        assert_eq!(policy.sources, vec![BranchPolicySource::ClassicProtection]);
    });
}

#[cfg(unix)]
#[test]
fn malformed_external_response_fails_closed() {
    with_fake_gh(
        HashMap::from([(OsString::from("FAKE_BAD_RESPONSE"), OsString::from("1"))]),
        |reader| {
            let error = reader.repository("acme/widgets", None).unwrap_err();
            assert_eq!(error.code, "invalid_github_response");
        },
    );
}

#[cfg(unix)]
#[test]
fn ruleset_bypass_by_expected_app_is_visible() {
    with_fake_gh(
        HashMap::from([(OsString::from("FAKE_RULESET"), OsString::from("1"))]),
        |reader| {
            let policy = reader.branch_policy("acme/widgets", "main").unwrap();
            assert_eq!(policy.bypass_app_ids, vec![123456]);
            assert!(policy.bypass_verification_complete);
            assert_eq!(
                policy.sources,
                vec![
                    BranchPolicySource::ClassicProtection,
                    BranchPolicySource::Ruleset
                ]
            );
        },
    );
}

#[cfg(unix)]
#[test]
fn repository_names_are_not_interpreted_by_a_shell() {
    with_fake_gh(HashMap::new(), |reader| {
        let error = reader
            .repository("acme/widgets;touch-pwned", None)
            .unwrap_err();
        assert_eq!(error.code, "invalid_repository");
    });
}

#[cfg(unix)]
#[test]
fn gh_process_has_a_strict_timeout() {
    with_fake_gh(
        HashMap::from([(OsString::from("FAKE_SLEEP"), OsString::from("1"))]),
        |reader| {
            let reader = reader.with_timeout(Duration::from_millis(30));
            let error = reader.repository("acme/widgets", None).unwrap_err();
            assert_eq!(error.code, "github_read_failed");
            assert!(error.message.contains("timed out"));
        },
    );
}

#[cfg(unix)]
fn with_fake_gh(extra_environment: HashMap<OsString, OsString>, operation: impl FnOnce(GhReader)) {
    let root = tempdir().unwrap();
    let binary = root.path().join("fake-gh");
    let source = format!(
        r#"#!/bin/sh
if [ "$FAKE_SLEEP" = "1" ]; then sleep 2; fi
if [ "$FAKE_BAD_RESPONSE" = "1" ]; then
  printf '%s' '{{"id":"not-a-number","full_name":"acme/widgets","default_branch":"main"}}'
  exit 0
fi
if [ "$1" = "api" ]; then
  endpoint="$4"
  case "$endpoint" in
    repos/acme/widgets) printf '%s' '{{"id":77,"full_name":"acme/widgets","default_branch":"main"}}' ;;
    repos/acme/widgets/commits/*) printf '%s' '{{"sha":"{sha}"}}' ;;
    repos/acme/widgets/contents/.github/eupho.yml) printf '%s' '{{"encoding":"base64","content":"dmVyc2lvbjogMQo="}}' ;;
    repos/acme/widgets/labels/*) printf '%s' '{{"name":"agent:ready"}}' ;;
    repos/acme/widgets/branches/main/protection) printf '%s' '{{"required_status_checks":{{"strict":true,"checks":[{{"context":"agent-review","app_id":123456}}]}},"required_pull_request_reviews":{{"dismiss_stale_reviews":true,"required_approving_review_count":1}}}}' ;;
    repos/acme/widgets/rules/branches/main)
      if [ "$FAKE_RULESET" = "1" ]; then
        printf '%s' '[{{"type":"required_status_checks","ruleset_id":42,"parameters":{{"strict_required_status_checks_policy":true,"required_status_checks":[{{"context":"agent-review","integration_id":123456}}]}}}}]'
      else printf '%s' '[]'; fi ;;
    repos/acme/widgets/rulesets/42) printf '%s' '{{"id":42,"bypass_actors":[{{"actor_id":123456,"actor_type":"Integration","bypass_mode":"always"}}]}}' ;;
    *) printf '%s\n' 'HTTP 404: Not Found' >&2; exit 1 ;;
  esac
elif [ "$1" = "issue" ] && [ "$2" = "list" ]; then
  last=''
  for arg in "$@"; do
    if [ "$last" = "--json" ]; then fields="$arg"; fi
    last="$arg"
  done
  if [ "$fields" = "number" ]; then printf '%s' '[{{"number":9}}]'
  else printf '%s' '[{{"number":7,"title":"Safe title","url":"https://github.com/acme/widgets/issues/7","labels":[{{"name":"agent:ready"}}],"updatedAt":"2026-08-12T00:00:00.000Z"}}]'; fi
else
  printf '%s\n' 'unexpected fake gh arguments' >&2
  exit 2
fi
"#,
        sha = "a".repeat(40)
    );
    fs::write(&binary, source).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let mut environment = HashMap::new();
    if let Some(path) = std::env::var_os("PATH") {
        environment.insert(OsString::from("PATH"), path);
    }
    environment.extend(extra_environment);
    operation(GhReader::with_environment(binary, environment));
}
