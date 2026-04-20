//! Manifest [[notification]] section parsing + env-var substitution.

#![allow(dead_code)]

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::Severity;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SinkKind {
    Log,
    Webhook,
    Slack,
    Discord,
}

/// One [[notification]] section after env-var substitution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationConfig {
    pub kind: SinkKind,
    /// Required for webhook/slack/discord; ignored for log.
    #[serde(default)]
    pub url: Option<String>,
    /// Event-kind filter. None ⇒ all three events.
    #[serde(default)]
    pub events: Option<Vec<String>>,
    /// Minimum severity to emit. Defaults to Info (emit all).
    #[serde(default = "default_severity_min")]
    pub severity_min: Severity,
}

fn default_severity_min() -> Severity {
    Severity::Info
}

/// Walk a mutable string: replace `${IDENT}` tokens with the value of
/// `std::env::var(IDENT)`. Errors if any `${IDENT}` has no matching env var.
pub fn substitute_env_vars(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'{' {
            // Find closing brace.
            let end = bytes[i + 2..]
                .iter()
                .position(|&b| b == b'}')
                .map(|p| i + 2 + p)
                .ok_or_else(|| anyhow::anyhow!("unterminated ${{…}} in {s:?}"))?;
            let name = std::str::from_utf8(&bytes[i + 2..end])
                .map_err(|_| anyhow::anyhow!("non-utf8 env var name in {s:?}"))?;
            let val = std::env::var(name).map_err(|_| {
                anyhow::anyhow!("notification uses ${{{name}}} but env var is not set")
            })?;
            out.push_str(&val);
            i = end + 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    Ok(out)
}

/// Run env-var substitution over every string field of the config.
/// Currently just `url`; keep extensible if we add more string fields.
pub fn apply_env_substitution(cfg: &mut NotificationConfig) -> Result<()> {
    if let Some(url) = cfg.url.as_mut() {
        *url = substitute_env_vars(url)?;
    }
    Ok(())
}

/// Validate a single config against kind-specific required fields.
/// Called from manifest/validate.rs.
pub fn validate(cfg: &NotificationConfig) -> Result<()> {
    match cfg.kind {
        SinkKind::Log => {}
        SinkKind::Webhook | SinkKind::Slack | SinkKind::Discord => {
            if cfg.url.as_deref().unwrap_or("").is_empty() {
                bail!(
                    "notification kind={:?} requires a non-empty 'url' field",
                    cfg.kind
                );
            }
        }
    }
    if let Some(events) = &cfg.events {
        for e in events {
            match e.as_str() {
                "approval_request" | "approval_pending" | "run_finished" | "budget_exceeded" => {}
                other => bail!(
                    "unknown event: {other:?}; valid: approval_request, approval_pending, run_finished, budget_exceeded"
                ),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_substitution_replaces_tokens() {
        std::env::set_var("PITBOSS_TEST_URL", "https://example.com/hook");
        let out = substitute_env_vars("${PITBOSS_TEST_URL}/sub").unwrap();
        assert_eq!(out, "https://example.com/hook/sub");
    }

    #[test]
    fn env_var_missing_fails_loud() {
        std::env::remove_var("PITBOSS_TEST_MISSING_XYZ");
        let err = substitute_env_vars("${PITBOSS_TEST_MISSING_XYZ}").unwrap_err();
        assert!(err.to_string().contains("env var is not set"));
    }

    #[test]
    fn env_var_unterminated_fails() {
        let err = substitute_env_vars("${FOO").unwrap_err();
        assert!(err.to_string().contains("unterminated"));
    }

    #[test]
    fn env_var_no_tokens_passthrough() {
        let out = substitute_env_vars("https://example.com/plain").unwrap();
        assert_eq!(out, "https://example.com/plain");
    }

    #[test]
    fn unknown_kind_rejected_at_parse() {
        let toml_src = r#"kind = "owlpost"
url = "https://example.com""#;
        let err: Result<NotificationConfig, _> = toml::from_str(toml_src);
        assert!(err.is_err(), "parsing should fail for unknown kind");
    }

    #[test]
    fn webhook_requires_url() {
        let cfg = NotificationConfig {
            kind: SinkKind::Webhook,
            url: None,
            events: None,
            severity_min: Severity::Info,
        };
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("requires a non-empty 'url'"));
    }

    #[test]
    fn unknown_event_rejected() {
        let cfg = NotificationConfig {
            kind: SinkKind::Log,
            url: None,
            events: Some(vec!["hallucinate".into()]),
            severity_min: Severity::Info,
        };
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("unknown event"));
    }

    #[test]
    fn log_sink_no_url_ok() {
        let cfg = NotificationConfig {
            kind: SinkKind::Log,
            url: None,
            events: None,
            severity_min: Severity::Info,
        };
        assert!(validate(&cfg).is_ok());
    }
}
