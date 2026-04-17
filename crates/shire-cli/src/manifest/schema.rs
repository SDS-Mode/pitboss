#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub run: RunConfig,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "template")]
    pub templates: Vec<Template>,
    #[serde(default, rename = "task")]
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    pub max_parallel:      Option<u32>,
    #[serde(default)]
    pub halt_on_failure:   bool,
    pub run_dir:           Option<PathBuf>,
    #[serde(default = "default_cleanup")]
    pub worktree_cleanup:  WorktreeCleanup,
    #[serde(default)]
    pub emit_event_stream: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_parallel: None,
            halt_on_failure: false,
            run_dir: None,
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
        }
    }
}

fn default_cleanup() -> WorktreeCleanup { WorktreeCleanup::OnSuccess }

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCleanup { Always, OnSuccess, Never }

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub model:        Option<String>,
    pub effort:       Option<Effort>,
    pub tools:        Option<Vec<String>>,
    pub timeout_secs: Option<u64>,
    pub use_worktree: Option<bool>,
    #[serde(default)]
    pub env:          HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Effort { Low, Medium, High, Xhigh, Max }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: Option<String>,
    pub template: Option<String>,
    #[serde(default)]
    pub vars: HashMap<String, String>,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub tools: Option<Vec<String>>,
    pub timeout_secs: Option<u64>,
    pub use_worktree: Option<bool>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_top_level_key() {
        let toml_src = r#"
            wibble = "surprise"
            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
        "#;
        let err: Result<Manifest, _> = toml::from_str(toml_src);
        assert!(err.is_err(), "should reject unknown key");
    }

    #[test]
    fn accepts_minimal_manifest() {
        let toml_src = r#"
            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "hi"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.tasks.len(), 1);
        assert_eq!(m.tasks[0].id, "x");
    }

    #[test]
    fn parses_full_manifest_with_template() {
        let toml_src = r#"
            [run]
            max_parallel = 8
            halt_on_failure = true
            worktree_cleanup = "never"

            [defaults]
            model = "claude-sonnet-4-6"
            effort = "high"
            tools = ["Read", "Bash"]

            [[template]]
            id = "sweep"
            prompt = "Audit {pm} in {dir}"

            [[task]]
            id = "t1"
            directory = "/tmp"
            template = "sweep"
            vars = { pm = "npm", dir = "/tmp" }
            branch = "feat/x"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.run.max_parallel, Some(8));
        assert!(m.run.halt_on_failure);
        assert_eq!(m.templates.len(), 1);
        assert_eq!(m.tasks[0].template.as_deref(), Some("sweep"));
    }
}
