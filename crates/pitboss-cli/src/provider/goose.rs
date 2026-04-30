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
            task.goose_max_turns.or(self.default_max_turns),
            task.resume_session_id.as_deref(),
            &task.prompt,
        )
    }

    fn base_args(
        &self,
        provider: &Provider,
        model: &str,
        max_turns: Option<u32>,
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
        if let Some(max_turns) = max_turns {
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

    #[must_use]
    pub fn actor_args(&self, req: GooseActorArgs<'_>) -> Vec<String> {
        let mut args = vec![
            "run".to_string(),
            "--no-profile".to_string(),
            "--quiet".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--provider".to_string(),
            req.provider.goose_arg().into_owned(),
            "--model".to_string(),
            req.model.to_string(),
        ];
        if let Some(max_turns) = req.max_turns.or(self.default_max_turns) {
            args.push("--max-turns".to_string());
            args.push(max_turns.to_string());
        }
        for extension in req.extensions {
            args.push("--with-extension".to_string());
            args.push(extension.clone());
        }
        if let Some(session_name) = req.session_name {
            if req.resume {
                args.push("--resume".to_string());
            }
            args.push("--name".to_string());
            args.push(session_name.to_string());
        } else {
            args.push("--no-session".to_string());
        }
        args.push("-t".to_string());
        args.push(req.prompt.to_string());
        args
    }
}

pub struct GooseActorArgs<'a> {
    pub provider: &'a Provider,
    pub model: &'a str,
    pub max_turns: Option<u32>,
    pub session_name: Option<&'a str>,
    pub resume: bool,
    pub extensions: &'a [String],
    pub prompt: &'a str,
}

#[must_use]
pub fn shell_word(raw: &str) -> String {
    if raw.is_empty() {
        return "''".to_string();
    }
    if raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'='))
    {
        return raw.to_string();
    }
    format!("'{}'", raw.replace('\'', r#"'\''"#))
}

#[must_use]
pub fn extension_command(command: &std::path::Path, args: &[String]) -> String {
    std::iter::once(shell_word(&command.to_string_lossy()))
        .chain(args.iter().map(|arg| shell_word(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_args_use_named_sessions_for_hierarchical_actors() {
        let extensions = vec!["pitboss mcp-bridge /tmp/sock".to_string()];
        let args = GooseSpawner::default().actor_args(GooseActorArgs {
            provider: &Provider::OpenAi,
            model: "gpt-4o",
            max_turns: Some(8),
            session_name: Some("pitboss-lead-123"),
            resume: false,
            extensions: &extensions,
            prompt: "lead prompt",
        });

        assert_eq!(args[0], "run");
        assert!(args.windows(2).any(|w| w == ["--provider", "openai"]));
        assert!(args.windows(2).any(|w| w == ["--model", "gpt-4o"]));
        assert!(args.windows(2).any(|w| w == ["--max-turns", "8"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--with-extension", "pitboss mcp-bridge /tmp/sock"]));
        assert!(args.windows(2).any(|w| w == ["--name", "pitboss-lead-123"]));
        assert!(!args.iter().any(|arg| arg == "--no-session"));
        assert!(!args.iter().any(|arg| arg == "--resume"));
    }

    #[test]
    fn actor_args_emit_resume_before_session_name() {
        let args = GooseSpawner::default().actor_args(GooseActorArgs {
            provider: &Provider::Anthropic,
            model: "claude-haiku-4-5",
            max_turns: None,
            session_name: Some("worker-1"),
            resume: true,
            extensions: &[],
            prompt: "continue",
        });

        let resume_pos = args.iter().position(|arg| arg == "--resume").unwrap();
        let name_pos = args.iter().position(|arg| arg == "--name").unwrap();
        assert!(resume_pos < name_pos);
    }

    #[test]
    fn extension_command_quotes_spaces_and_quotes() {
        let cmd = std::path::Path::new("/tmp/pit boss");
        let args = vec![
            "mcp-bridge".to_string(),
            "--actor-id".to_string(),
            "lead one".to_string(),
            "tok'en".to_string(),
        ];
        assert_eq!(
            extension_command(cmd, &args),
            "'/tmp/pit boss' mcp-bridge --actor-id 'lead one' 'tok'\\''en'"
        );
    }
}
