#![allow(clippy::missing_errors_doc)]

//! Durable local infrastructure for Eupho's administrator-owned state.
//!
//! State written through this module is intentionally private, crash-safe, and
//! separate from runner-visible workspaces. The repository lock is an
//! operating-system advisory lock; the JSON file beside it is diagnostic only.

use fs4::{FileExt, TryLockError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct InfraError {
    pub code: &'static str,
    pub message: String,
    pub exit_code: i32,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl InfraError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: 1,
            source: None,
        }
    }

    fn with_exit_code(mut self, exit_code: i32) -> Self {
        self.exit_code = exit_code;
        self
    }

    fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }
}

impl fmt::Display for InfraError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for InfraError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// Creates (or tightens) a dispatcher-owned directory to mode 0700.
///
/// The final path is inspected with `symlink_metadata`, so Eupho never accepts
/// a symlink as a state directory.
pub fn ensure_private_directory(path: &Path) -> Result<(), InfraError> {
    fs::create_dir_all(path).map_err(|error| {
        InfraError::new(
            "private_directory_failed",
            format!(
                "cannot create private directory {}: {error}",
                path.display()
            ),
        )
        .with_source(error)
    })?;

    let metadata = fs::symlink_metadata(path).map_err(|error| {
        InfraError::new(
            "private_directory_failed",
            format!(
                "cannot inspect private directory {}: {error}",
                path.display()
            ),
        )
        .with_source(error)
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(InfraError::new(
            "unsafe_state_path",
            format!(
                "private state path {} is not a real directory",
                path.display()
            ),
        ));
    }

    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        InfraError::new(
            "private_directory_failed",
            format!(
                "cannot restrict private directory {}: {error}",
                path.display()
            ),
        )
        .with_source(error)
    })?;
    Ok(())
}

/// Creates private child directories beneath an already selected state root.
///
/// Every child component is inspected without following its final symlink.
/// Once the root is private (0700), this also removes the opportunity for an
/// untrusted workspace process to swap components between inspection and use.
pub fn ensure_private_subdirectory(
    state_root: &Path,
    components: &[&str],
) -> Result<PathBuf, InfraError> {
    ensure_private_directory(state_root)?;
    let mut current = fs::canonicalize(state_root).map_err(|error| {
        InfraError::new(
            "unsafe_state_path",
            format!(
                "cannot canonicalize private state root {}: {error}",
                state_root.display()
            ),
        )
        .with_source(error)
    })?;

    for component in components {
        if component.is_empty()
            || Path::new(component).components().count() != 1
            || matches!(
                Path::new(component).components().next(),
                Some(Component::ParentDir | Component::CurDir | Component::RootDir)
            )
        {
            return Err(InfraError::new(
                "unsafe_state_path",
                format!("unsafe private state component {component:?}"),
            ));
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(InfraError::new(
                    "unsafe_state_path",
                    format!(
                        "private state path {} is not a real directory",
                        current.display()
                    ),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| {
                    InfraError::new(
                        "private_directory_failed",
                        format!(
                            "cannot create private directory {}: {error}",
                            current.display()
                        ),
                    )
                    .with_source(error)
                })?;
            }
            Err(error) => {
                return Err(InfraError::new(
                    "private_directory_failed",
                    format!(
                        "cannot inspect private directory {}: {error}",
                        current.display()
                    ),
                )
                .with_source(error));
            }
        }
        #[cfg(unix)]
        fs::set_permissions(&current, fs::Permissions::from_mode(0o700)).map_err(|error| {
            InfraError::new(
                "private_directory_failed",
                format!(
                    "cannot restrict private directory {}: {error}",
                    current.display()
                ),
            )
            .with_source(error)
        })?;
    }
    Ok(current)
}

/// Atomically replaces a JSON document and fsyncs both the file and directory.
pub fn atomic_write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), InfraError> {
    let parent = path.parent().ok_or_else(|| {
        InfraError::new(
            "atomic_write_failed",
            format!("state file {} has no parent directory", path.display()),
        )
    })?;
    ensure_private_directory(parent)?;

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            InfraError::new(
                "atomic_write_failed",
                format!("state file {} has an invalid file name", path.display()),
            )
        })?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));

    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).map_err(|error| {
            InfraError::new(
                "atomic_write_failed",
                format!("cannot create {}: {error}", temporary.display()),
            )
            .with_source(error)
        })?;

        serde_json::to_writer_pretty(&mut file, value).map_err(|error| {
            InfraError::new(
                "atomic_write_failed",
                format!("cannot serialize state for {}: {error}", path.display()),
            )
            .with_source(error)
        })?;
        file.write_all(b"\n").map_err(|error| {
            InfraError::new(
                "atomic_write_failed",
                format!("cannot finish writing {}: {error}", temporary.display()),
            )
            .with_source(error)
        })?;
        file.sync_all().map_err(|error| {
            InfraError::new(
                "atomic_write_failed",
                format!("cannot sync {}: {error}", temporary.display()),
            )
            .with_source(error)
        })?;
        drop(file);

        fs::rename(&temporary, path).map_err(|error| {
            InfraError::new(
                "atomic_write_failed",
                format!(
                    "cannot atomically replace {} with {}: {error}",
                    path.display(),
                    temporary.display()
                ),
            )
            .with_source(error)
        })?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                InfraError::new(
                    "atomic_write_failed",
                    format!("cannot sync state directory {}: {error}", parent.display()),
                )
                .with_source(error)
            })?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn read_json_file<T: DeserializeOwned>(path: &Path) -> Result<T, InfraError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        InfraError::new(
            "state_read_failed",
            format!("cannot inspect state file {}: {error}", path.display()),
        )
        .with_source(error)
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(InfraError::new(
            "unsafe_state_path",
            format!("state file {} is not a regular file", path.display()),
        ));
    }
    let bytes = fs::read(path).map_err(|error| {
        InfraError::new(
            "state_read_failed",
            format!("cannot read valid JSON from {}: {error}", path.display()),
        )
        .with_source(error)
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        InfraError::new(
            "state_read_failed",
            format!("cannot read valid JSON from {}: {error}", path.display()),
        )
        .with_source(error)
    })
}

fn read_json_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, InfraError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(InfraError::new(
                "unsafe_state_path",
                format!("state file {} is not a regular file", path.display()),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(InfraError::new(
                "state_read_failed",
                format!("cannot inspect state file {}: {error}", path.display()),
            )
            .with_source(error));
        }
    }
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            InfraError::new(
                "state_read_failed",
                format!("cannot read valid JSON from {}: {error}", path.display()),
            )
            .with_source(error)
        }),
        Err(error) => Err(InfraError::new(
            "state_read_failed",
            format!("cannot read valid JSON from {}: {error}", path.display()),
        )
        .with_source(error)),
    }
}

#[derive(Debug, Clone)]
pub struct CandidateStore {
    state_root: PathBuf,
}

impl CandidateStore {
    pub fn new(state_root: impl Into<PathBuf>) -> Self {
        Self {
            state_root: state_root.into(),
        }
    }

    pub fn put<T: Serialize>(&self, snapshot: &T) -> Result<(), InfraError> {
        let value = serde_json::to_value(snapshot).map_err(|error| {
            InfraError::new(
                "invalid_candidate_state",
                format!("candidate state cannot be represented as JSON: {error}"),
            )
            .with_source(error)
        })?;
        validate_candidate_snapshot(&value)?;
        let repository_id = value
            .get("repositoryId")
            .and_then(Value::as_u64)
            .ok_or_else(invalid_candidate_state)?;
        let repository_directory =
            self.repository_directory(repository_id, true)?
                .ok_or_else(|| {
                    InfraError::new(
                        "state_write_failed",
                        format!("repository state directory for {repository_id} was not created"),
                    )
                })?;
        atomic_write_json(&repository_directory.join("candidates.json"), &value)
    }

    pub fn get<T: DeserializeOwned>(&self, repository_id: u64) -> Result<Option<T>, InfraError> {
        let Some(repository_directory) = self.repository_directory(repository_id, false)? else {
            return Ok(None);
        };
        let Some(value) =
            read_json_optional::<Value>(&repository_directory.join("candidates.json"))?
        else {
            return Ok(None);
        };
        validate_candidate_snapshot(&value)?;
        serde_json::from_value(value).map(Some).map_err(|error| {
            InfraError::new(
                "invalid_candidate_state",
                format!("candidate state is malformed: {error}"),
            )
            .with_source(error)
        })
    }

    pub fn list<T: DeserializeOwned>(&self) -> Result<Vec<T>, InfraError> {
        let root = ensure_private_subdirectory(&self.state_root, &["repositories"])?;
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(InfraError::new(
                    "state_read_failed",
                    format!(
                        "cannot list repository state in {}: {error}",
                        root.display()
                    ),
                )
                .with_source(error));
            }
        };
        let mut repository_ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                InfraError::new(
                    "state_read_failed",
                    format!("cannot inspect state entry: {error}"),
                )
                .with_source(error)
            })?;
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(id) = name.parse::<u64>() {
                    if id > 0 {
                        repository_ids.push(id);
                    }
                }
            }
        }
        repository_ids.sort_unstable();

        let mut snapshots = Vec::with_capacity(repository_ids.len());
        for repository_id in repository_ids {
            if let Some(snapshot) = self.get(repository_id)? {
                snapshots.push(snapshot);
            }
        }
        Ok(snapshots)
    }

    fn repository_directory(
        &self,
        repository_id: u64,
        create: bool,
    ) -> Result<Option<PathBuf>, InfraError> {
        if repository_id == 0 {
            return Err(InfraError::new(
                "invalid_repository_id",
                "repository ID must be positive",
            ));
        }
        let repositories = ensure_private_subdirectory(&self.state_root, &["repositories"])?;
        let repository = repositories.join(repository_id.to_string());
        match fs::symlink_metadata(&repository) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                Err(InfraError::new(
                    "unsafe_state_path",
                    format!(
                        "repository state path {} is not a real directory",
                        repository.display()
                    ),
                ))
            }
            Ok(_) => Ok(Some(repository)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure_private_subdirectory(
                    &self.state_root,
                    &["repositories", &repository_id.to_string()],
                )
                .map(Some)
            }
            Err(error) => Err(InfraError::new(
                "state_read_failed",
                format!(
                    "cannot inspect repository state path {}: {error}",
                    repository.display()
                ),
            )
            .with_source(error)),
        }
    }
}

fn validate_candidate_snapshot(value: &Value) -> Result<(), InfraError> {
    let object = value.as_object().ok_or_else(invalid_candidate_state)?;
    let repository_id = positive_u64(object, "repositoryId")?;
    let repository = string(object, "repository")?;
    if !valid_repository(repository) {
        return Err(invalid_candidate_state());
    }
    if integer(object, "schemaVersion")? != 1
        || !valid_sha(string(object, "baseSha")?)
        || !valid_digest(string(object, "policyDigest")?)
        || string(object, "policySource")?.is_empty()
        || object.get("trustedBase").and_then(Value::as_bool).is_none()
        || !valid_iso_instant(string(object, "observedAt")?)
    {
        return Err(invalid_candidate_state());
    }

    let candidates = object
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(invalid_candidate_state)?;
    for candidate in candidates {
        let candidate = candidate.as_object().ok_or_else(invalid_candidate_state)?;
        let candidate_id = string(candidate, "candidateId")?;
        if !candidate_id
            .strip_prefix("candidate-")
            .is_some_and(|suffix| suffix.len() == 20 && is_lower_hex(suffix))
            || string(candidate, "action")? != "would_claim"
            || positive_u64(candidate, "repositoryId")? != repository_id
            || string(candidate, "repository")? != repository
            || positive_u64(candidate, "issueNumber").is_err()
            || candidate
                .get("issueTitle")
                .and_then(Value::as_str)
                .is_none()
            || candidate.get("issueUrl").and_then(Value::as_str).is_none()
            || string(candidate, "baseSha")? != string(object, "baseSha")?
            || string(candidate, "policyDigest")? != string(object, "policyDigest")?
        {
            return Err(invalid_candidate_state());
        }
        let execution_mode = string(candidate, "executionMode")?;
        let workspace_type = string(candidate, "workspaceType")?;
        if !matches!(execution_mode, "attended" | "unattended")
            || !matches!(workspace_type, "worktree" | "ephemeral_clone")
            || (execution_mode == "attended" && workspace_type != "worktree")
            || (execution_mode == "unattended" && workspace_type != "ephemeral_clone")
            || !matches!(
                string(candidate, "mergePolicy")?,
                "autonomous-low-risk" | "human-final-approval" | "suggest-only"
            )
            || !candidate
                .get("routeLabel")
                .is_some_and(|value| value.is_null() || value.is_string())
            || !string_array(candidate.get("preconditions"))
        {
            return Err(invalid_candidate_state());
        }
    }

    let diagnostics = object
        .get("diagnostics")
        .and_then(Value::as_array)
        .ok_or_else(invalid_candidate_state)?;
    for diagnostic in diagnostics {
        let diagnostic = diagnostic.as_object().ok_or_else(invalid_candidate_state)?;
        if string(diagnostic, "code").is_err()
            || positive_u64(diagnostic, "issueNumber").is_err()
            || string(diagnostic, "message").is_err()
        {
            return Err(invalid_candidate_state());
        }
    }
    Ok(())
}

fn invalid_candidate_state() -> InfraError {
    InfraError::new("invalid_candidate_state", "candidate state is malformed")
}

fn integer(object: &Map<String, Value>, field: &str) -> Result<u64, InfraError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(invalid_candidate_state)
}

fn positive_u64(object: &Map<String, Value>, field: &str) -> Result<u64, InfraError> {
    let value = integer(object, field)?;
    if value == 0 {
        return Err(invalid_candidate_state());
    }
    Ok(value)
}

fn string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, InfraError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(invalid_candidate_state)
}

fn string_array(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.iter().all(Value::is_string))
}

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(repository) = parts.next() else {
        return false;
    };
    parts.next().is_none() && valid_repository_part(owner) && valid_repository_part(repository)
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

fn valid_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| digest.len() == 64 && is_lower_hex(digest))
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_iso_instant(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 24
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || bytes.get(19) != Some(&b'.')
        || bytes.get(23) != Some(&b'Z')
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
    decimal(bytes, 20, 23).is_some()
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

/// Process-lifetime repository exclusivity on the supported single host.
#[derive(Debug)]
pub struct RepositoryLock {
    path: PathBuf,
    file: Option<File>,
}

impl RepositoryLock {
    pub fn acquire(state_root: &Path, repository_id: u64) -> Result<Self, InfraError> {
        if repository_id == 0 {
            return Err(InfraError::new(
                "invalid_repository_id",
                "repository ID must be positive",
            ));
        }
        let directory = ensure_private_subdirectory(state_root, &["locks"])?;
        let path = directory.join(format!("{repository_id}.lock"));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let mut file = options.open(&path).map_err(|error| {
            InfraError::new(
                "repository_locked",
                format!(
                    "repository {repository_id} could not open the OS lock at {}: {error}",
                    path.display()
                ),
            )
            .with_exit_code(2)
            .with_source(error)
        })?;
        let metadata = file.metadata().map_err(|error| {
            InfraError::new(
                "repository_locked",
                format!("cannot inspect lock file {}: {error}", path.display()),
            )
            .with_exit_code(2)
            .with_source(error)
        })?;
        if !metadata.is_file() {
            return Err(InfraError::new(
                "unsafe_state_path",
                format!("lock path {} is not a regular file", path.display()),
            )
            .with_exit_code(2));
        }
        match FileExt::try_lock(&file) {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(InfraError::new(
                    "repository_locked",
                    format!(
                        "repository {repository_id} is already locked at {}",
                        path.display()
                    ),
                )
                .with_exit_code(2));
            }
            Err(TryLockError::Error(error)) => {
                return Err(InfraError::new(
                    "repository_locked",
                    format!(
                        "repository {repository_id} could not acquire the OS lock at {}: {error}",
                        path.display()
                    ),
                )
                .with_exit_code(2)
                .with_source(error));
            }
        }

        let owner = serde_json::json!({
            "pid": std::process::id(),
            "holderPid": std::process::id(),
            "acquiredAt": rfc3339_now(),
        });
        file.set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)))
            .and_then(|_| {
                serde_json::to_writer(&mut file, &owner)
                    .map_err(std::io::Error::other)
                    .and_then(|()| file.write_all(b"\n"))
            })
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                let _ = FileExt::unlock(&file);
                InfraError::new(
                    "repository_locked",
                    format!("cannot record lock owner in {}: {error}", path.display()),
                )
                .with_exit_code(2)
                .with_source(error)
            })?;
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                let _ = FileExt::unlock(&file);
                InfraError::new(
                    "repository_locked",
                    format!("cannot restrict lock file {}: {error}", path.display()),
                )
                .with_exit_code(2)
                .with_source(error)
            })?;

        Ok(Self {
            path,
            file: Some(file),
        })
    }

    pub fn assert_held(&self) -> Result<(), InfraError> {
        if self.file.is_none() {
            return Err(InfraError::new(
                "repository_lock_lost",
                format!("OS lock at {} is no longer held", self.path.display()),
            ));
        }
        Ok(())
    }

    pub fn release(&mut self) -> Result<(), InfraError> {
        let Some(file) = self.file.take() else {
            return Ok(());
        };
        FileExt::unlock(&file).map_err(|error| {
            InfraError::new(
                "lock_release_failed",
                format!("cannot release OS lock at {}: {error}", self.path.display()),
            )
            .with_source(error)
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RepositoryLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
        }
    }
}

pub fn default_state_root() -> Result<PathBuf, InfraError> {
    default_state_root_from(env::var_os("XDG_STATE_HOME"), env::var_os("HOME"))
}

pub fn default_state_root_from(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, InfraError> {
    let parent = xdg_state_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from).map(|home| {
                if cfg!(target_os = "macos") {
                    home.join("Library").join("Application Support")
                } else {
                    home.join(".local").join("state")
                }
            })
        })
        .ok_or_else(|| {
            InfraError::new(
                "unsafe_state_root",
                "cannot determine a default state root without HOME",
            )
        })?;
    assert_safe_state_root(&parent.join("eupho"), None)
}

pub fn find_git_worktree_root(start: &Path) -> Result<Option<PathBuf>, InfraError> {
    let mut candidate = absolute_normalized(start)?;
    loop {
        match fs::symlink_metadata(candidate.join(".git")) {
            Ok(_) => return Ok(Some(candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(InfraError::new(
                    "git_root_discovery_failed",
                    format!("cannot inspect {}: {error}", candidate.display()),
                )
                .with_source(error));
            }
        }
        if !candidate.pop() {
            return Ok(None);
        }
    }
}

pub fn assert_safe_state_root(
    path: &Path,
    repository_root: Option<&Path>,
) -> Result<PathBuf, InfraError> {
    let normalized = absolute_normalized(path)?;
    if normalized.parent().is_none() {
        return Err(InfraError::new(
            "unsafe_state_root",
            format!("state root cannot resolve to {}", normalized.display()),
        ));
    }
    if let Some(repository_root) = repository_root {
        let repository_root = absolute_normalized(repository_root)?;
        if contains_path(&repository_root, &normalized) {
            return Err(InfraError::new(
                "unsafe_state_root",
                format!(
                    "state root {} must be outside the working repository {}",
                    normalized.display(),
                    repository_root.display()
                ),
            ));
        }
    }
    Ok(normalized)
}

pub fn resolve_safe_state_root(
    path: &Path,
    repository_root: Option<&Path>,
) -> Result<PathBuf, InfraError> {
    let lexical = assert_safe_state_root(path, repository_root)?;
    let mut existing = lexical.clone();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&existing) {
            Ok(metadata) => {
                if existing == lexical && metadata.file_type().is_symlink() {
                    return Err(InfraError::new(
                        "unsafe_state_root",
                        format!(
                            "state root {} must not itself be a symbolic link",
                            lexical.display()
                        ),
                    ));
                }
                if !metadata.is_dir() && !metadata.file_type().is_symlink() {
                    return Err(InfraError::new(
                        "unsafe_state_root",
                        format!(
                            "state root ancestor {} is not a directory",
                            existing.display()
                        ),
                    ));
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or_else(|| {
                    InfraError::new("unsafe_state_root", "state root has no existing ancestor")
                })?;
                missing.push(name.to_owned());
                if !existing.pop() {
                    return Err(InfraError::new(
                        "unsafe_state_root",
                        "state root has no existing ancestor",
                    ));
                }
            }
            Err(error) => {
                return Err(InfraError::new(
                    "unsafe_state_root",
                    format!("cannot inspect state root {}: {error}", existing.display()),
                )
                .with_source(error));
            }
        }
    }

    let canonical = fs::canonicalize(&existing).map_err(|error| {
        InfraError::new(
            "unsafe_state_root",
            format!("cannot canonicalize {}: {error}", existing.display()),
        )
        .with_source(error)
    })?;
    if canonical.parent().is_none() && existing.parent().is_some() {
        return Err(InfraError::new(
            "unsafe_state_root",
            format!(
                "state root ancestor {} resolves to the filesystem root",
                existing.display()
            ),
        ));
    }
    let mut resolved = canonical;
    for segment in missing.into_iter().rev() {
        resolved.push(segment);
    }
    if let Some(repository_root) = repository_root {
        let canonical_repository = fs::canonicalize(repository_root).map_err(|error| {
            InfraError::new(
                "unsafe_state_root",
                format!(
                    "cannot canonicalize repository root {}: {error}",
                    repository_root.display()
                ),
            )
            .with_source(error)
        })?;
        if contains_path(&canonical_repository, &resolved) {
            return Err(InfraError::new(
                "unsafe_state_root",
                format!(
                    "state root {} must be outside the working repository {}",
                    resolved.display(),
                    canonical_repository.display()
                ),
            ));
        }
    }
    assert_safe_state_root(&resolved, None)
}

pub fn paths_overlap(left: &Path, right: &Path) -> Result<bool, InfraError> {
    let left = absolute_normalized(left)?;
    let right = absolute_normalized(right)?;
    Ok(contains_path(&left, &right) || contains_path(&right, &left))
}

fn contains_path(parent: &Path, candidate: &Path) -> bool {
    candidate == parent || candidate.starts_with(parent)
}

fn absolute_normalized(path: &Path) -> Result<PathBuf, InfraError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| {
                InfraError::new(
                    "unsafe_state_root",
                    format!("cannot resolve current directory: {error}"),
                )
                .with_source(error)
            })?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    Ok(normalized)
}

#[must_use]
pub fn rfc3339_now() -> String {
    let seconds = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX);
    let days = seconds.div_euclid(86_400);
    let daytime = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = daytime / 3_600;
    let minute = (daytime % 3_600) / 60;
    let second = daytime % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

// Gregorian calendar conversion adapted from Howard Hinnant's public-domain
// civil calendar algorithms. Input is days since 1970-01-01.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}
