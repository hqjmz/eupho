//! Dispatcher-owned workspace boundary for future execution phases.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{ExecutionMode, WorkspaceType};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRequest {
    pub run_id: String,
    pub repository: String,
    pub base_sha: String,
    pub execution_mode: ExecutionMode,
    pub workspace_type: WorkspaceType,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLease {
    pub run_id: String,
    pub path: PathBuf,
    pub workspace_type: WorkspaceType,
    pub base_sha: String,
}

/// Phase 1 provides only this port; no reachable command creates a workspace.
pub trait WorkspaceManager {
    type Error: std::error::Error + Send + Sync + 'static;

    fn create(&self, request: &WorkspaceRequest) -> Result<WorkspaceLease, Self::Error>;
    fn dispose(&self, lease: &WorkspaceLease) -> Result<(), Self::Error>;
}
