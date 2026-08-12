use eupho::infra::{
    CandidateStore, RepositoryLock, assert_safe_state_root, default_state_root_from,
    find_git_worktree_root, paths_overlap, resolve_safe_state_root,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    schema_version: u8,
    repository_id: u64,
    repository: String,
    base_sha: String,
    policy_digest: String,
    policy_source: String,
    trusted_base: bool,
    observed_at: String,
    candidates: Vec<Value>,
    diagnostics: Vec<Value>,
}

#[test]
fn candidate_store_round_trips_and_atomically_replaces() {
    let state = tempdir().unwrap();
    let store = CandidateStore::new(state.path());
    assert_eq!(store.get::<Snapshot>(42).unwrap(), None);
    assert!(store.list::<Snapshot>().unwrap().is_empty());

    let first = snapshot(42, "acme/widgets");
    store.put(&first).unwrap();
    assert_eq!(store.get::<Snapshot>(42).unwrap(), Some(first.clone()));

    let second = Snapshot {
        base_sha: "b".repeat(40),
        observed_at: "2026-08-12T00:01:00.000Z".to_owned(),
        candidates: Vec::new(),
        diagnostics: vec![json!({
            "code":"state_conflict",
            "issueNumber":11,
            "message":"Conflicting state labels"
        })],
        ..first
    };
    store.put(&second).unwrap();
    assert_eq!(store.list::<Snapshot>().unwrap(), vec![second]);
}

#[test]
fn candidate_store_rejects_malformed_snapshots() {
    let state = tempdir().unwrap();
    let store = CandidateStore::new(state.path());
    let mut invalid = serde_json::to_value(snapshot(1, "acme/invalid")).unwrap();
    invalid["schemaVersion"] = json!(2);
    assert_eq!(
        store.put(&invalid).unwrap_err().code,
        "invalid_candidate_state"
    );
    assert!(store.list::<Value>().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn persisted_state_has_private_permissions() {
    let state = tempdir().unwrap();
    let store = CandidateStore::new(state.path());
    store.put(&snapshot(5, "acme/private")).unwrap();
    let directory = state.path().join("repositories/5");
    let file = directory.join("candidates.json");
    assert_eq!(
        fs::metadata(directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn repository_lock_is_exclusive_and_reacquirable() {
    let state = tempdir().unwrap();
    let mut lock = RepositoryLock::acquire(state.path(), 99).unwrap();
    lock.assert_held().unwrap();
    let owner: Value = serde_json::from_slice(&fs::read(lock.path()).unwrap()).unwrap();
    assert_eq!(owner["pid"].as_u64(), Some(u64::from(std::process::id())));
    assert!(owner["acquiredAt"].as_str().unwrap().ends_with('Z'));

    let blocked = RepositoryLock::acquire(state.path(), 99).unwrap_err();
    assert_eq!(blocked.code, "repository_locked");
    assert_eq!(blocked.exit_code, 2);
    lock.release().unwrap();
    assert_eq!(lock.assert_held().unwrap_err().code, "repository_lock_lost");

    let mut reacquired = RepositoryLock::acquire(state.path(), 99).unwrap();
    reacquired.assert_held().unwrap();
    reacquired.release().unwrap();
}

#[cfg(unix)]
#[test]
fn repository_lock_refuses_a_symlink_without_touching_its_target() {
    let state = tempdir().unwrap();
    let locks = state.path().join("locks");
    fs::create_dir(&locks).unwrap();
    let victim = state.path().join("victim.txt");
    fs::write(&victim, "do not overwrite\n").unwrap();
    std::os::unix::fs::symlink(&victim, locks.join("99.lock")).unwrap();

    let error = RepositoryLock::acquire(state.path(), 99).unwrap_err();

    assert!(matches!(
        error.code,
        "repository_locked" | "unsafe_state_path"
    ));
    assert_eq!(fs::read_to_string(victim).unwrap(), "do not overwrite\n");
}

#[cfg(unix)]
#[test]
fn candidate_store_refuses_intermediate_state_symlinks() {
    let state = tempdir().unwrap();
    let outside = tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), state.path().join("repositories")).unwrap();
    let store = CandidateStore::new(state.path());

    let error = store.put(&snapshot(8, "acme/safe")).unwrap_err();

    assert_eq!(error.code, "unsafe_state_path");
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[test]
fn state_root_is_component_safe_and_external_to_repository() {
    let root = default_state_root_from(
        Some(OsString::from("/tmp/eupho-user-state")),
        Some(OsString::from("/unused")),
    )
    .unwrap();
    assert_eq!(root, std::path::Path::new("/tmp/eupho-user-state/eupho"));
    assert!(!paths_overlap(std::path::Path::new("/tmp/repo"), &root).unwrap());

    assert_eq!(
        assert_safe_state_root(std::path::Path::new("/"), None)
            .unwrap_err()
            .code,
        "unsafe_state_root"
    );
    assert_eq!(
        assert_safe_state_root(
            std::path::Path::new("/tmp/repo/.eupho"),
            Some(std::path::Path::new("/tmp/repo")),
        )
        .unwrap_err()
        .code,
        "unsafe_state_root"
    );
    assert!(
        paths_overlap(
            std::path::Path::new("/tmp/eupho/state"),
            std::path::Path::new("/tmp/eupho/state/workspaces")
        )
        .unwrap()
    );
    assert!(
        !paths_overlap(
            std::path::Path::new("/tmp/eupho/state"),
            std::path::Path::new("/tmp/eupho/state-other")
        )
        .unwrap()
    );
}

#[test]
fn state_root_canonicalizes_existing_ancestors_and_rejects_symlink_root() {
    let parent = tempdir().unwrap();
    let desired = parent.path().join("future/new-state");
    let resolved = resolve_safe_state_root(&desired, None).unwrap();
    assert!(resolved.ends_with("future/new-state"));

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(parent.path(), parent.path().join("linked")).unwrap();
        assert_eq!(
            resolve_safe_state_root(&parent.path().join("linked"), None)
                .unwrap_err()
                .code,
            "unsafe_state_root"
        );
    }
}

#[test]
fn git_worktree_discovery_requires_dot_git() {
    let root = tempdir().unwrap();
    assert_eq!(find_git_worktree_root(root.path()).unwrap(), None);
    fs::create_dir(root.path().join(".git")).unwrap();
    fs::create_dir(root.path().join("nested")).unwrap();
    assert_eq!(
        find_git_worktree_root(&root.path().join("nested")).unwrap(),
        Some(root.path().to_path_buf())
    );
}

fn snapshot(repository_id: u64, repository: &str) -> Snapshot {
    Snapshot {
        schema_version: 1,
        repository_id,
        repository: repository.to_owned(),
        base_sha: "a".repeat(40),
        policy_digest: format!("sha256:{}", "0".repeat(64)),
        policy_source: format!("github:{repository}/.github/eupho.yml@{}", "a".repeat(40)),
        trusted_base: true,
        observed_at: "2026-08-12T00:00:00.000Z".to_owned(),
        candidates: vec![json!({
            "candidateId": format!("candidate-{:020x}", repository_id),
            "action":"would_claim",
            "repositoryId":repository_id,
            "repository":repository,
            "issueNumber":11,
            "issueTitle":"Exercise the control plane",
            "issueUrl":format!("https://github.com/{repository}/issues/11"),
            "baseSha":"a".repeat(40),
            "policyDigest":format!("sha256:{}", "0".repeat(64)),
            "executionMode":"attended",
            "workspaceType":"worktree",
            "mergePolicy":"human-final-approval",
            "routeLabel":null,
            "preconditions":["repository_lock"]
        })],
        diagnostics: Vec::new(),
    }
}
