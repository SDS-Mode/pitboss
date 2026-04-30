use pitboss_core::provider::Provider;

use crate::manifest::resolve::ResolvedTask;

#[derive(Debug, Clone, Copy, Default)]
pub struct GooseSpawner {
    pub default_max_turns: Option<u32>,
}

impl GooseSpawner {
    #[must_use]
    pub fn flat_task_args(&self, task: &ResolvedTask) -> Vec<String> {
        self.base_args(
            &task.provider,
            &task.model,
            task.resume_session_id.as_deref(),
            &task.prompt,
        )
    }

    fn base_args(
        &self,
        provider: &Provider,
        model: &str,
        resume_session: Option<&str>,
        prompt: &str,
    ) -> Vec<String> {
        let mut args = vec![
            "run".to_string(),
            "--no-profile".to_string(),
            "--quiet".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--provider".to_string(),
            provider.goose_arg().into_owned(),
            "--model".to_string(),
            model.to_string(),
        ];
        if let Some(max_turns) = self.default_max_turns {
            args.push("--max-turns".to_string());
            args.push(max_turns.to_string());
        }
        if let Some(session) = resume_session {
            args.push("--resume".to_string());
            args.push("--name".to_string());
            args.push(session.to_string());
        } else {
            args.push("--no-session".to_string());
        }
        args.push("-t".to_string());
        args.push(prompt.to_string());
        args
    }
}
