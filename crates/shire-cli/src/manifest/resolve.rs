#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::schema::{Defaults, Effort, Manifest, Task, Template, WorktreeCleanup};

/// Fully resolved task ready for dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedTask {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: String,
    pub branch: Option<String>,
    pub model: String,
    pub effort: Effort,
    pub tools: Vec<String>,
    pub timeout_secs: u64,
    pub use_worktree: bool,
    pub env: HashMap<String, String>,
    /// When set, pass `--resume <id>` to claude so it continues a prior session.
    #[serde(default)]
    pub resume_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedManifest {
    pub max_parallel: u32,
    pub halt_on_failure: bool,
    pub run_dir: PathBuf,
    pub worktree_cleanup: WorktreeCleanup,
    pub emit_event_stream: bool,
    pub tasks: Vec<ResolvedTask>,
}

const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_EFFORT: Effort = Effort::High;
const DEFAULT_TIMEOUT_SECS: u64 = 3600;
const DEFAULT_MAX_PARALLEL: u32 = 4;
fn default_tools() -> Vec<String> {
    ["Read", "Write", "Edit", "Bash", "Glob", "Grep"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub fn resolve(manifest: Manifest, env_max_parallel: Option<u32>) -> Result<ResolvedManifest> {
    let templates: HashMap<String, &Template> = manifest
        .templates
        .iter()
        .map(|t| (t.id.clone(), t))
        .collect();

    let mut resolved = Vec::with_capacity(manifest.tasks.len());
    for task in &manifest.tasks {
        resolved.push(resolve_task(task, &manifest.defaults, &templates)?);
    }

    let max_parallel = manifest
        .run
        .max_parallel
        .or(env_max_parallel)
        .unwrap_or(DEFAULT_MAX_PARALLEL);

    let run_dir = manifest.run.run_dir.unwrap_or_else(default_run_dir);

    Ok(ResolvedManifest {
        max_parallel,
        halt_on_failure: manifest.run.halt_on_failure,
        run_dir,
        worktree_cleanup: manifest.run.worktree_cleanup,
        emit_event_stream: manifest.run.emit_event_stream,
        tasks: resolved,
    })
}

fn resolve_task(
    task: &Task,
    defaults: &Defaults,
    templates: &HashMap<String, &Template>,
) -> Result<ResolvedTask> {
    let prompt = match (&task.prompt, &task.template) {
        (Some(p), None) => p.clone(),
        (None, Some(tid)) => {
            let tmpl = templates.get(tid).ok_or_else(|| {
                anyhow!("task '{}' references unknown template '{}'", task.id, tid)
            })?;
            substitute(&tmpl.prompt, &task.vars)
                .with_context(|| format!("rendering template '{}' for task '{}'", tid, task.id))?
        }
        (Some(_), Some(_)) => bail!("task '{}' sets both prompt and template", task.id),
        (None, None) => bail!("task '{}' has no prompt and no template", task.id),
    };

    let mut env = defaults.env.clone();
    env.extend(task.env.clone());

    Ok(ResolvedTask {
        id: task.id.clone(),
        directory: task.directory.clone(),
        prompt,
        branch: task.branch.clone(),
        model: task
            .model
            .clone()
            .or_else(|| defaults.model.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        effort: task.effort.or(defaults.effort).unwrap_or(DEFAULT_EFFORT),
        tools: task
            .tools
            .clone()
            .or_else(|| defaults.tools.clone())
            .unwrap_or_else(default_tools),
        timeout_secs: task
            .timeout_secs
            .or(defaults.timeout_secs)
            .unwrap_or(DEFAULT_TIMEOUT_SECS),
        use_worktree: task.use_worktree.or(defaults.use_worktree).unwrap_or(true),
        env,
        resume_session_id: None,
    })
}

fn substitute(template: &str, vars: &HashMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut iter = template.chars().peekable();
    while let Some(c) = iter.next() {
        match c {
            '\\' => {
                if matches!(iter.peek(), Some('{') | Some('}')) {
                    out.push(iter.next().unwrap());
                } else {
                    out.push(c);
                }
            }
            '{' => {
                let mut name = String::new();
                for nc in iter.by_ref() {
                    if nc == '}' {
                        let value = vars
                            .get(&name)
                            .ok_or_else(|| anyhow!("undeclared var '{}' in template", name))?;
                        out.push_str(value);
                        break;
                    }
                    name.push(nc);
                }
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

fn default_run_dir() -> PathBuf {
    if let Some(h) = dirs_home() {
        h.join(".local/share/shire/runs")
    } else {
        PathBuf::from("./shire-runs")
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn man(src: &str) -> Manifest {
        toml::from_str(src).unwrap()
    }

    #[test]
    fn resolves_inline_prompt() {
        let m = man(r#"
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "hi"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].prompt, "hi");
        assert_eq!(r.max_parallel, 4);
    }

    #[test]
    fn resolves_template_with_vars() {
        let m = man(r#"
            [[template]]
            id = "t"
            prompt = "hi {name}"
            [[task]]
            id = "a"
            directory = "/tmp"
            template = "t"
            vars = { name = "ada" }
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].prompt, "hi ada");
    }

    #[test]
    fn undeclared_var_errors() {
        let m = man(r#"
            [[template]]
            id = "t"
            prompt = "hi {missing}"
            [[task]]
            id = "a"
            directory = "/tmp"
            template = "t"
        "#);
        assert!(resolve(m, None).is_err());
    }

    #[test]
    fn task_overrides_defaults() {
        let m = man(r#"
            [defaults]
            model  = "default-m"
            tools  = ["Read"]
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
            model  = "override-m"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].model, "override-m");
        assert_eq!(r.tasks[0].tools, vec!["Read"]);
    }

    #[test]
    fn env_var_precedence_applies() {
        let m = man(r#"
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, Some(16)).unwrap();
        assert_eq!(r.max_parallel, 16);
    }

    #[test]
    fn manifest_max_parallel_wins_over_env() {
        let m = man(r#"
            [run]
            max_parallel = 2
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, Some(16)).unwrap();
        assert_eq!(r.max_parallel, 2);
    }

    #[test]
    fn escaped_braces_are_literal() {
        let m = man(r#"
            [[template]]
            id = "t"
            prompt = 'literal \{ and \}'
            [[task]]
            id = "a"
            directory = "/tmp"
            template = "t"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].prompt, "literal { and }");
    }

    #[test]
    fn resume_session_id_defaults_to_none() {
        let m = man(r#"
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, None).unwrap();
        assert!(
            r.tasks[0].resume_session_id.is_none(),
            "resume_session_id should default to None"
        );
    }
}
