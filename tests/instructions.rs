#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use eupho::instructions::{
    EntryKind, InstructionLinkError, InstructionSource, LinkAction, link_instructions,
    resolve_repository_root,
};
use tempfile::TempDir;

fn repository() -> TempDir {
    let repository = tempfile::tempdir().expect("create test repository");
    fs::create_dir(repository.path().join(".git")).expect("create Git marker");
    repository
}

fn write(path: impl AsRef<Path>, text: &str) {
    fs::write(path, text).expect("write test file");
}

#[test]
fn default_source_creates_relative_claude_link_from_nested_path() {
    let repository = repository();
    write(repository.path().join("AGENTS.md"), "canonical\n");
    let nested = repository.path().join("src/nested");
    fs::create_dir_all(&nested).expect("create nested path");

    let result = link_instructions(&nested, InstructionSource::default()).expect("link succeeds");

    assert_eq!(result.action, LinkAction::Created);
    assert_eq!(
        result.repository_root,
        repository.path().canonicalize().unwrap()
    );
    assert_eq!(result.source, InstructionSource::Agents);
    assert_eq!(result.link_target, PathBuf::from("AGENTS.md"));
    assert_eq!(
        fs::read_link(repository.path().join("CLAUDE.md")).unwrap(),
        PathBuf::from("AGENTS.md")
    );
    assert_eq!(
        fs::read_to_string(repository.path().join("CLAUDE.md")).unwrap(),
        "canonical\n"
    );
}

#[test]
fn claude_source_creates_reverse_relative_link() {
    let repository = repository();
    write(repository.path().join("CLAUDE.md"), "canonical\n");

    let result = link_instructions(repository.path(), InstructionSource::Claude).unwrap();

    assert_eq!(result.action, LinkAction::Created);
    assert_eq!(result.link_target, PathBuf::from("CLAUDE.md"));
    assert_eq!(
        fs::read_link(repository.path().join("AGENTS.md")).unwrap(),
        PathBuf::from("CLAUDE.md")
    );
}

#[test]
fn exact_expected_link_is_idempotent() {
    let repository = repository();
    write(repository.path().join("AGENTS.md"), "canonical\n");

    let created = link_instructions(repository.path(), InstructionSource::Agents).unwrap();
    let repeated = link_instructions(repository.path(), InstructionSource::Agents).unwrap();

    assert_eq!(created.action, LinkAction::Created);
    assert_eq!(repeated.action, LinkAction::AlreadyLinked);
    assert_eq!(created.destination_path, repeated.destination_path);
    assert_eq!(
        fs::read_link(repeated.destination_path).unwrap(),
        Path::new("AGENTS.md")
    );
}

#[test]
fn source_selection_parses_only_explicit_supported_values() {
    assert_eq!(
        InstructionSource::from_str("agents").unwrap(),
        InstructionSource::Agents
    );
    assert_eq!(
        InstructionSource::from_str("claude").unwrap(),
        InstructionSource::Claude
    );
    assert!(matches!(
        InstructionSource::from_str("CLAUDE"),
        Err(InstructionLinkError::InvalidSource { .. })
    ));
}

#[test]
fn source_must_exist_and_destination_is_not_created_on_failure() {
    let repository = repository();

    let error = link_instructions(repository.path(), InstructionSource::Agents).unwrap_err();

    assert!(matches!(error, InstructionLinkError::SourceMissing { .. }));
    assert!(fs::symlink_metadata(repository.path().join("CLAUDE.md")).is_err());
}

#[test]
fn source_must_be_a_non_symlink_regular_file() {
    let repository = repository();
    fs::create_dir(repository.path().join("AGENTS.md")).unwrap();
    let error = link_instructions(repository.path(), InstructionSource::Agents).unwrap_err();
    assert!(matches!(
        error,
        InstructionLinkError::SourceNotRegularFile {
            kind: EntryKind::Directory,
            ..
        }
    ));

    fs::remove_dir(repository.path().join("AGENTS.md")).unwrap();
    write(repository.path().join("instructions.md"), "canonical\n");
    std::os::unix::fs::symlink("instructions.md", repository.path().join("AGENTS.md")).unwrap();
    let error = link_instructions(repository.path(), InstructionSource::Agents).unwrap_err();
    assert!(matches!(
        error,
        InstructionLinkError::SourceNotRegularFile {
            kind: EntryKind::Symlink,
            ..
        }
    ));
}

#[test]
fn regular_destination_is_never_adopted_or_overwritten_even_when_identical() {
    for destination_text in ["canonical\n", "different\n"] {
        let repository = repository();
        write(repository.path().join("AGENTS.md"), "canonical\n");
        write(repository.path().join("CLAUDE.md"), destination_text);

        let error = link_instructions(repository.path(), InstructionSource::Agents).unwrap_err();

        assert!(matches!(
            error,
            InstructionLinkError::DestinationConflict {
                kind: EntryKind::RegularFile,
                actual_target: None,
                ..
            }
        ));
        assert_eq!(
            fs::read_to_string(repository.path().join("CLAUDE.md")).unwrap(),
            destination_text
        );
        assert!(
            !fs::symlink_metadata(repository.path().join("CLAUDE.md"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn directory_destination_is_never_replaced() {
    let repository = repository();
    write(repository.path().join("AGENTS.md"), "canonical\n");
    fs::create_dir(repository.path().join("CLAUDE.md")).unwrap();

    let error = link_instructions(repository.path(), InstructionSource::Agents).unwrap_err();

    assert!(matches!(
        error,
        InstructionLinkError::DestinationConflict {
            kind: EntryKind::Directory,
            ..
        }
    ));
    assert!(repository.path().join("CLAUDE.md").is_dir());
}

#[test]
fn unexpected_relative_absolute_and_normalized_symlinks_fail_closed() {
    for unexpected_target in [
        PathBuf::from("other.md"),
        PathBuf::from("./AGENTS.md"),
        PathBuf::from("/tmp/AGENTS.md"),
        PathBuf::from("../AGENTS.md"),
    ] {
        let repository = repository();
        write(repository.path().join("AGENTS.md"), "canonical\n");
        std::os::unix::fs::symlink(&unexpected_target, repository.path().join("CLAUDE.md"))
            .unwrap();

        let error = link_instructions(repository.path(), InstructionSource::Agents).unwrap_err();

        match error {
            InstructionLinkError::DestinationConflict {
                kind: EntryKind::Symlink,
                expected_target,
                actual_target: Some(actual_target),
                ..
            } => {
                assert_eq!(expected_target, Path::new("AGENTS.md"));
                assert_eq!(actual_target, unexpected_target);
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}

#[test]
fn repository_root_resolution_accepts_worktree_marker_and_starting_file() {
    let repository = tempfile::tempdir().unwrap();
    write(repository.path().join(".git"), "gitdir: /tmp/example\n");
    let nested = repository.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let file = nested.join("file.txt");
    write(&file, "x");

    assert_eq!(
        resolve_repository_root(&file).unwrap(),
        repository.path().canonicalize().unwrap()
    );
}

#[test]
fn repository_root_resolution_uses_nearest_repository() {
    let outer = repository();
    let inner = outer.path().join("nested/repository");
    fs::create_dir_all(inner.join(".git")).unwrap();
    let working = inner.join("src");
    fs::create_dir(&working).unwrap();

    assert_eq!(
        resolve_repository_root(&working).unwrap(),
        inner.canonicalize().unwrap()
    );
}

#[test]
fn repository_discovery_fails_without_marker_and_on_symlink_marker() {
    let outside = tempfile::tempdir().unwrap();
    assert!(matches!(
        resolve_repository_root(outside.path()),
        Err(InstructionLinkError::RepositoryNotFound { .. })
    ));

    let unsafe_repository = tempfile::tempdir().unwrap();
    fs::create_dir(unsafe_repository.path().join("actual-git")).unwrap();
    std::os::unix::fs::symlink("actual-git", unsafe_repository.path().join(".git")).unwrap();
    assert!(matches!(
        resolve_repository_root(unsafe_repository.path()),
        Err(InstructionLinkError::UnsafeRepositoryMarker {
            kind: EntryKind::Symlink,
            ..
        })
    ));
}
