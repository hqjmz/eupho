#![allow(clippy::missing_errors_doc)]

//! Canonical metadata signing and rollback-resistant local revision anchors.

use hmac::{Hmac, KeyInit, Mac};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

use crate::infra::{InfraError, atomic_write_json, ensure_private_subdirectory, read_json_file};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug)]
pub struct SecurityError {
    pub code: &'static str,
    pub message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl SecurityError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            source: None,
        }
    }

    fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }
}

impl fmt::Display for SecurityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SecurityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

impl From<InfraError> for SecurityError {
    fn from(error: InfraError) -> Self {
        SecurityError::new(error.code, error.message)
    }
}

/// Serializes according to the JSON Canonicalization Scheme (RFC 8785).
pub fn canonical_json<T: Serialize>(value: &T) -> Result<String, SecurityError> {
    serde_json_canonicalizer::to_string(value).map_err(|error| {
        SecurityError::new(
            "non_canonical_value",
            format!("value cannot be represented as canonical JSON: {error}"),
        )
        .with_source(error)
    })
}

pub fn canonical_digest<T: Serialize>(value: &T) -> Result<String, SecurityError> {
    Ok(format!(
        "sha256:{}",
        hex_encode(&Sha256::digest(canonical_json(value)?.as_bytes()))
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedEnvelope<T> {
    pub scheme: String,
    pub key_id: String,
    pub payload: T,
    pub mac: String,
}

pub fn sign_envelope<T: Serialize + Clone>(
    payload: &T,
    key_id: &str,
    key: &[u8],
) -> Result<SignedEnvelope<T>, SecurityError> {
    validate_key_id(key_id)?;
    let mut mac = HmacSha256::new_from_slice(key).map_err(|error| {
        SecurityError::new("invalid_signing_key", format!("invalid HMAC key: {error}"))
            .with_source(error)
    })?;
    mac.update(signing_body(payload)?.as_bytes());
    Ok(SignedEnvelope {
        scheme: "hmac-sha256".to_owned(),
        key_id: key_id.to_owned(),
        payload: payload.clone(),
        mac: hex_encode(&mac.finalize().into_bytes()),
    })
}

pub fn verify_envelope<T: Serialize + Clone, S: BuildHasher>(
    envelope: &SignedEnvelope<T>,
    keys: &HashMap<String, Vec<u8>, S>,
) -> Result<T, SecurityError> {
    if envelope.scheme != "hmac-sha256" {
        return Err(SecurityError::new(
            "unsupported_signature",
            format!("unsupported signature scheme {}", envelope.scheme),
        ));
    }
    let key = keys.get(&envelope.key_id).ok_or_else(|| {
        SecurityError::new(
            "unknown_signing_key",
            format!("unknown signing key {}", envelope.key_id),
        )
    })?;
    let actual = hex_decode(&envelope.mac).ok_or_else(|| {
        SecurityError::new("invalid_signature", "signed metadata has a malformed HMAC")
    })?;
    let mut mac = HmacSha256::new_from_slice(key).map_err(|error| {
        SecurityError::new("invalid_signing_key", format!("invalid HMAC key: {error}"))
            .with_source(error)
    })?;
    mac.update(signing_body(&envelope.payload)?.as_bytes());
    mac.verify_slice(&actual).map_err(|_| {
        SecurityError::new(
            "invalid_signature",
            "signed metadata failed HMAC verification",
        )
    })?;
    Ok(envelope.payload.clone())
}

pub fn envelope_payload_digest<T: Serialize>(payload: &T) -> Result<String, SecurityError> {
    Ok(hex_encode(&Sha256::digest(
        canonical_json(payload)?.as_bytes(),
    )))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevisionAnchor {
    pub schema_version: u8,
    pub run_id: String,
    pub revision: u64,
    pub payload_digest: String,
    pub key_id: String,
    pub confirmed: bool,
}

#[derive(Debug, Clone)]
pub struct RevisionLedger {
    state_root: PathBuf,
}

impl RevisionLedger {
    pub fn new(state_root: impl Into<PathBuf>) -> Self {
        Self {
            state_root: state_root.into(),
        }
    }

    pub fn assert_fresh(
        &self,
        run_id: &str,
        revision: u64,
        payload_digest: &str,
    ) -> Result<(), SecurityError> {
        validate_revision_input(run_id, revision, payload_digest)?;
        let Some(current) = self.read(run_id)? else {
            return Ok(());
        };
        if revision < current.revision {
            return Err(SecurityError::new(
                "metadata_rollback",
                format!(
                    "run {run_id} revision {revision} is below local high-water mark {}",
                    current.revision
                ),
            ));
        }
        if revision == current.revision && payload_digest != current.payload_digest {
            return Err(SecurityError::new(
                "metadata_fork",
                format!("run {run_id} revision {revision} has a different payload digest"),
            ));
        }
        Ok(())
    }

    pub fn prepare(
        &self,
        run_id: &str,
        revision: u64,
        payload_digest: &str,
        key_id: &str,
    ) -> Result<(), SecurityError> {
        self.assert_fresh(run_id, revision, payload_digest)?;
        validate_key_id(key_id)?;
        if let Some(current) = self.read(run_id)? {
            if current.revision == revision && current.key_id != key_id {
                return Err(SecurityError::new(
                    "metadata_fork",
                    format!(
                        "run {run_id} revision {revision} changed signing key without advancing"
                    ),
                ));
            }
        }
        atomic_write_json(
            &self.path_for(run_id)?,
            &RevisionAnchor {
                schema_version: 1,
                run_id: run_id.to_owned(),
                revision,
                payload_digest: payload_digest.to_owned(),
                key_id: key_id.to_owned(),
                confirmed: false,
            },
        )?;
        Ok(())
    }

    pub fn confirm(
        &self,
        run_id: &str,
        revision: u64,
        payload_digest: &str,
    ) -> Result<(), SecurityError> {
        validate_revision_input(run_id, revision, payload_digest)?;
        let current = self.read(run_id)?.ok_or_else(|| {
            SecurityError::new(
                "revision_confirmation_mismatch",
                format!("cannot confirm {run_id} revision {revision}"),
            )
        })?;
        if current.revision != revision || current.payload_digest != payload_digest {
            return Err(SecurityError::new(
                "revision_confirmation_mismatch",
                format!("cannot confirm {run_id} revision {revision}"),
            ));
        }
        atomic_write_json(
            &self.path_for(run_id)?,
            &RevisionAnchor {
                confirmed: true,
                ..current
            },
        )?;
        Ok(())
    }

    pub fn read(&self, run_id: &str) -> Result<Option<RevisionAnchor>, SecurityError> {
        let path = self.path_for(run_id)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(SecurityError::new(
                    "invalid_revision_anchor",
                    format!("revision anchor for {run_id} is not a regular file"),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(SecurityError::new(
                    "state_read_failed",
                    format!("cannot inspect revision anchor for {run_id}: {error}"),
                )
                .with_source(error));
            }
        }
        let value: RevisionAnchor = read_json_file(&path)?;
        if value.schema_version != 1
            || value.run_id != run_id
            || value.revision == 0
            || !valid_hex_digest(&value.payload_digest)
            || validate_key_id(&value.key_id).is_err()
        {
            return Err(SecurityError::new(
                "invalid_revision_anchor",
                format!("invalid revision anchor for {run_id}"),
            ));
        }
        Ok(Some(value))
    }

    fn path_for(&self, run_id: &str) -> Result<PathBuf, SecurityError> {
        validate_run_id(run_id)?;
        Ok(
            ensure_private_subdirectory(&self.state_root, &["revisions"])?
                .join(format!("{run_id}.json")),
        )
    }
}

fn validate_revision_input(
    run_id: &str,
    revision: u64,
    payload_digest: &str,
) -> Result<(), SecurityError> {
    validate_run_id(run_id)?;
    if revision == 0 {
        return Err(SecurityError::new(
            "invalid_revision",
            format!("invalid revision {revision} for {run_id}"),
        ));
    }
    if !valid_hex_digest(payload_digest) {
        return Err(SecurityError::new(
            "invalid_payload_digest",
            format!("invalid payload digest for {run_id}"),
        ));
    }
    Ok(())
}

fn validate_run_id(run_id: &str) -> Result<(), SecurityError> {
    if run_id.is_empty()
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(SecurityError::new(
            "invalid_run_id",
            format!("unsafe run ID {run_id}"),
        ));
    }
    Ok(())
}

fn validate_key_id(key_id: &str) -> Result<(), SecurityError> {
    if key_id.is_empty()
        || key_id.len() > 128
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(SecurityError::new(
            "invalid_key_id",
            format!("unsafe signing key ID {key_id}"),
        ));
    }
    Ok(())
}

fn valid_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn signing_body<T: Serialize>(payload: &T) -> Result<String, SecurityError> {
    Ok(format!("eupho:v1\n{}", canonical_json(payload)?))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn hex_decode(value: &str) -> Option<Vec<u8>> {
    if value.len() % 2 != 0 {
        return None;
    }
    let mut output = Vec::with_capacity(value.len() / 2);
    let bytes = value.as_bytes();
    for pair in bytes.chunks_exact(2) {
        output.push((hex_value(pair[0])? << 4) | hex_value(pair[1])?);
    }
    Some(output)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

/// Convenience helper for callers deserializing an authenticated payload.
pub fn verify_and_decode<T: Serialize + Clone + DeserializeOwned, S: BuildHasher>(
    encoded: &[u8],
    keys: &HashMap<String, Vec<u8>, S>,
) -> Result<T, SecurityError> {
    let envelope: SignedEnvelope<T> = serde_json::from_slice(encoded).map_err(|error| {
        SecurityError::new(
            "invalid_signature",
            format!("signed metadata envelope is malformed: {error}"),
        )
        .with_source(error)
    })?;
    verify_envelope(&envelope, keys)
}

#[allow(dead_code)]
fn _assert_path_is_send_sync(_: &Path) {}
