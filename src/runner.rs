//! Product-neutral runner boundary.
//!
//! Adapters receive phase-scoped work and return artifacts. They never receive
//! Eupho's GitHub publishing credentials and cannot mutate lifecycle state.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerPhase {
    Author,
    Repair,
    Review,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerRequest {
    pub run_id: String,
    pub phase: RunnerPhase,
    pub workspace: PathBuf,
    pub task_artifact: PathBuf,
    pub result_artifact: PathBuf,
    pub deadline: String,
    pub turn_budget: u64,
    pub token_budget: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerResult {
    pub run_id: String,
    pub phase: RunnerPhase,
    pub exit_code: i32,
    pub started_at: String,
    pub finished_at: String,
    pub output_artifact: PathBuf,
    pub usage: RunnerUsage,
}

pub trait RunnerAdapter {
    type Error: std::error::Error + Send + Sync + 'static;

    fn name(&self) -> &str;
    fn execute(&self, request: &RunnerRequest) -> Result<RunnerResult, Self::Error>;
    fn interrupt(&self, run_id: &str) -> Result<(), Self::Error>;
}
