//! Safe repository-local linking for agent instruction files.
//!
//! Eupho treats one instruction file as canonical and creates the other as a
//! relative symlink in the same repository root. The operation is deliberately
//! conservative: it creates a missing destination or accepts the exact link it
//! would create, but never replaces an existing filesystem entry.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const AGENTS_FILE: &str = "AGENTS.md";
const CLAUDE_FILE: &str = "CLAUDE.md";

/// The canonical repository instruction file.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InstructionSource {
    /// Keep `AGENTS.md` canonical and create `CLAUDE.md -> AGENTS.md`.
    #[default]
    Agents,
    /// Keep `CLAUDE.md` canonical and create `AGENTS.md -> CLAUDE.md`.
    Claude,
}

impl InstructionSource {
    /// File that must already exist as a regular file.
    #[must_use]
    pub const fn source_file(self) -> &'static str {
        match self {
            Self::Agents => AGENTS_FILE,
            Self::Claude => CLAUDE_FILE,
        }
    }

    /// File Eupho creates as a relative symlink.
    #[must_use]
    pub const fn destination_file(self) -> &'static str {
        match self {
            Self::Agents => CLAUDE_FILE,
            Self::Claude => AGENTS_FILE,
        }
    }
}

impl fmt::Display for InstructionSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Agents => "agents",
            Self::Claude => "claude",
        })
    }
}

impl FromStr for InstructionSource {
    type Err = InstructionLinkError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "agents" => Ok(Self::Agents),
            "claude" => Ok(Self::Claude),
            _ => Err(InstructionLinkError::InvalidSource {
                value: value.to_owned(),
            }),
        }
    }
}

/// Whether a link was created or was already in the requested state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkAction {
    Created,
    AlreadyLinked,
}

/// Successful result of [`link_instructions`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkOutcome {
    pub action: LinkAction,
    pub repository_root: PathBuf,
    pub source: InstructionSource,
    pub source_path: PathBuf,
    pub destination_path: PathBuf,
    /// Exact relative path stored in the symlink.
    pub link_target: PathBuf,
}

/// Coarse entry type used in fail-closed diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    RegularFile,
    Directory,
    Symlink,
    Other,
}

/// Failure from repository discovery or instruction-link creation.
#[derive(Debug)]
pub enum InstructionLinkError {
    InvalidSource {
        value: String,
    },
    RepositoryNotFound {
        start: PathBuf,
    },
    UnsafeRepositoryRoot {
        path: PathBuf,
    },
    UnsafeRepositoryMarker {
        path: PathBuf,
        kind: EntryKind,
    },
    UnsupportedStartEntry {
        path: PathBuf,
        kind: EntryKind,
    },
    SourceMissing {
        path: PathBuf,
    },
    SourceNotRegularFile {
        path: PathBuf,
        kind: EntryKind,
    },
    DestinationConflict {
        path: PathBuf,
        kind: EntryKind,
        expected_target: PathBuf,
        actual_target: Option<PathBuf>,
    },
    UnsupportedPlatform,
    Filesystem {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for InstructionLinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSource { value } => {
                write!(
                    formatter,
                    "unknown instruction source {value:?}; expected agents or claude"
                )
            }
            Self::RepositoryNotFound { start } => write!(
                formatter,
                "cannot find a Git repository at or above {}",
                start.display()
            ),
            Self::UnsafeRepositoryRoot { path } => write!(
                formatter,
                "refusing to manage instruction links at filesystem root {}",
                path.display()
            ),
            Self::UnsafeRepositoryMarker { path, kind } => write!(
                formatter,
                "repository marker {} has unsafe type {kind:?}",
                path.display()
            ),
            Self::UnsupportedStartEntry { path, kind } => write!(
                formatter,
                "repository search path {} has unsupported type {kind:?}",
                path.display()
            ),
            Self::SourceMissing { path } => {
                write!(
                    formatter,
                    "canonical instruction file {} does not exist",
                    path.display()
                )
            }
            Self::SourceNotRegularFile { path, kind } => write!(
                formatter,
                "canonical instruction file {} must be a regular file, not {kind:?}",
                path.display()
            ),
            Self::DestinationConflict {
                path,
                kind,
                expected_target,
                actual_target,
            } => {
                write!(
                    formatter,
                    "refusing to replace {kind:?} at {}; expected a symlink to {}",
                    path.display(),
                    expected_target.display()
                )?;
                if let Some(actual_target) = actual_target {
                    write!(formatter, ", found symlink to {}", actual_target.display())?;
                }
                Ok(())
            }
            Self::UnsupportedPlatform => formatter.write_str(
                "instruction linking is currently supported only on Unix-compatible platforms",
            ),
            Self::Filesystem {
                operation,
                path,
                source,
            } => write!(formatter, "cannot {operation} {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for InstructionLinkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Filesystem { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Find the nearest Git repository root at or above `start`.
///
/// Both normal repositories (`.git` directory) and linked worktrees (`.git`
/// regular file) are recognized. A symbolic-link marker fails closed rather
/// than redirecting repository discovery.
pub fn resolve_repository_root(start: &Path) -> Result<PathBuf, InstructionLinkError> {
    let canonical_start =
        fs::canonicalize(start).map_err(|source| InstructionLinkError::Filesystem {
            operation: "resolve repository search path",
            path: start.to_path_buf(),
            source,
        })?;
    let start_metadata =
        fs::metadata(&canonical_start).map_err(|source| InstructionLinkError::Filesystem {
            operation: "inspect repository search path",
            path: canonical_start.clone(),
            source,
        })?;
    let mut candidate = if start_metadata.is_dir() {
        canonical_start.clone()
    } else if start_metadata.is_file() {
        canonical_start
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| InstructionLinkError::UnsafeRepositoryRoot {
                path: canonical_start.clone(),
            })?
    } else {
        return Err(InstructionLinkError::UnsupportedStartEntry {
            path: canonical_start,
            kind: entry_kind(&start_metadata.file_type()),
        });
    };

    loop {
        let marker = candidate.join(".git");
        match fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.is_dir() || metadata.is_file() => {
                if candidate.parent().is_none() {
                    return Err(InstructionLinkError::UnsafeRepositoryRoot { path: candidate });
                }
                return Ok(candidate);
            }
            Ok(metadata) => {
                return Err(InstructionLinkError::UnsafeRepositoryMarker {
                    path: marker,
                    kind: entry_kind(&metadata.file_type()),
                });
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(InstructionLinkError::Filesystem {
                    operation: "inspect repository marker",
                    path: marker,
                    source,
                });
            }
        }

        let Some(parent) = candidate.parent() else {
            return Err(InstructionLinkError::RepositoryNotFound {
                start: canonical_start,
            });
        };
        candidate = parent.to_path_buf();
    }
}

/// Create the non-canonical instruction filename as a relative symlink.
///
/// The canonical source must already exist as a non-symlink regular file. The
/// destination must be absent or be the exact relative symlink Eupho expects.
/// No regular file, directory, special entry, or unexpected link is replaced.
pub fn link_instructions(
    start: &Path,
    source: InstructionSource,
) -> Result<LinkOutcome, InstructionLinkError> {
    if !cfg!(unix) {
        return Err(InstructionLinkError::UnsupportedPlatform);
    }

    let repository_root = resolve_repository_root(start)?;
    let source_path = repository_root.join(source.source_file());
    let destination_path = repository_root.join(source.destination_file());
    let link_target = PathBuf::from(source.source_file());

    require_regular_source(&source_path)?;

    match inspect_destination(&destination_path, &link_target)? {
        DestinationState::ExpectedLink => {
            return Ok(outcome(
                LinkAction::AlreadyLinked,
                repository_root,
                source,
                source_path,
                destination_path,
                link_target,
            ));
        }
        DestinationState::Absent => {}
    }

    match create_relative_symlink(&link_target, &destination_path) {
        Ok(()) => {
            // Verify what is now at the path. This detects an immediate
            // concurrent replacement without ever deleting an entry.
            match inspect_destination(&destination_path, &link_target)? {
                DestinationState::ExpectedLink => Ok(outcome(
                    LinkAction::Created,
                    repository_root,
                    source,
                    source_path,
                    destination_path,
                    link_target,
                )),
                DestinationState::Absent => Err(InstructionLinkError::Filesystem {
                    operation: "verify newly created instruction symlink",
                    path: destination_path,
                    source: io::Error::new(
                        io::ErrorKind::NotFound,
                        "symlink disappeared immediately after creation",
                    ),
                }),
            }
        }
        Err(InstructionLinkError::Filesystem {
            source: filesystem_error,
            ..
        }) if filesystem_error.kind() == io::ErrorKind::AlreadyExists => {
            match inspect_destination(&destination_path, &link_target)? {
                DestinationState::ExpectedLink => Ok(outcome(
                    LinkAction::AlreadyLinked,
                    repository_root,
                    source,
                    source_path,
                    destination_path,
                    link_target,
                )),
                DestinationState::Absent => Err(InstructionLinkError::Filesystem {
                    operation: "create instruction symlink",
                    path: destination_path,
                    source: filesystem_error,
                }),
            }
        }
        Err(error) => Err(error),
    }
}

fn outcome(
    action: LinkAction,
    repository_root: PathBuf,
    source: InstructionSource,
    source_path: PathBuf,
    destination_path: PathBuf,
    link_target: PathBuf,
) -> LinkOutcome {
    LinkOutcome {
        action,
        repository_root,
        source,
        source_path,
        destination_path,
        link_target,
    }
}

fn require_regular_source(path: &Path) -> Result<(), InstructionLinkError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(metadata) => Err(InstructionLinkError::SourceNotRegularFile {
            path: path.to_path_buf(),
            kind: entry_kind(&metadata.file_type()),
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            Err(InstructionLinkError::SourceMissing {
                path: path.to_path_buf(),
            })
        }
        Err(source) => Err(InstructionLinkError::Filesystem {
            operation: "inspect canonical instruction file",
            path: path.to_path_buf(),
            source,
        }),
    }
}

enum DestinationState {
    Absent,
    ExpectedLink,
}

fn inspect_destination(
    path: &Path,
    expected_target: &Path,
) -> Result<DestinationState, InstructionLinkError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let actual_target =
                fs::read_link(path).map_err(|source| InstructionLinkError::Filesystem {
                    operation: "read instruction symlink",
                    path: path.to_path_buf(),
                    source,
                })?;
            if actual_target == expected_target {
                Ok(DestinationState::ExpectedLink)
            } else {
                Err(InstructionLinkError::DestinationConflict {
                    path: path.to_path_buf(),
                    kind: EntryKind::Symlink,
                    expected_target: expected_target.to_path_buf(),
                    actual_target: Some(actual_target),
                })
            }
        }
        Ok(metadata) => Err(InstructionLinkError::DestinationConflict {
            path: path.to_path_buf(),
            kind: entry_kind(&metadata.file_type()),
            expected_target: expected_target.to_path_buf(),
            actual_target: None,
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(DestinationState::Absent),
        Err(source) => Err(InstructionLinkError::Filesystem {
            operation: "inspect instruction symlink destination",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn entry_kind(file_type: &fs::FileType) -> EntryKind {
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_file() {
        EntryKind::RegularFile
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::Other
    }
}

#[cfg(unix)]
fn create_relative_symlink(target: &Path, destination: &Path) -> Result<(), InstructionLinkError> {
    std::os::unix::fs::symlink(target, destination).map_err(|source| {
        InstructionLinkError::Filesystem {
            operation: "create instruction symlink",
            path: destination.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn create_relative_symlink(
    _target: &Path,
    _destination: &Path,
) -> Result<(), InstructionLinkError> {
    Err(InstructionLinkError::UnsupportedPlatform)
}
