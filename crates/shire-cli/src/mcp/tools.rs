//! The six MCP tool handlers exposed to the lead. Real implementations
//! land in Tasks 10-16; this file establishes the types + signatures.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnWorkerArgs {
    pub prompt: String,
    #[serde(default)]
    pub directory: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnWorkerResult {
    pub task_id: String,
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub state: String,
    pub started_at: Option<String>,
    pub partial_usage: mosaic_core::parser::TokenUsage,
    pub last_text_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSummary {
    pub task_id: String,
    pub state: String,
    pub prompt_preview: String,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelResult {
    pub ok: bool,
}
