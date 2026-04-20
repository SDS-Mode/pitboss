//! Manifest [[notification]] section parsing + env-var substitution.

#![allow(dead_code)]

use std::net::IpAddr;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::Severity;

/// Only env vars whose name starts with this prefix are substitutable from
/// notification URLs. This keeps a rogue manifest from exfiltrating arbitrary
/// host env vars (`ANTHROPIC_API_KEY`, `AWS_SECRET_ACCESS_KEY`, …) to a
/// chosen webhook endpoint; operators who need to inject hook URLs just
/// rename their env var to start with `PITBOSS_`.
const ENV_VAR_ALLOWED_PREFIX: &str = "PITBOSS_";

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
/// `std::env::var(IDENT)`. Errors if any `${IDENT}` has no matching env var
/// or if the name does not start with `PITBOSS_` (see ENV_VAR_ALLOWED_PREFIX).
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
            if !name.starts_with(ENV_VAR_ALLOWED_PREFIX) {
                bail!(
                    "notification uses ${{{name}}} but only env vars prefixed \
                     with `{ENV_VAR_ALLOWED_PREFIX}` may be substituted \
                     (rename the var to `{ENV_VAR_ALLOWED_PREFIX}{name}` or similar)"
                );
            }
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
            let url = cfg.url.as_deref().unwrap_or("");
            if url.is_empty() {
                bail!(
                    "notification kind={:?} requires a non-empty 'url' field",
                    cfg.kind
                );
            }
            validate_webhook_url(url)?;
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

/// Reject webhook URLs that would let a rogue manifest SSRF the host's
/// internal network. Requires `https://` and a non-loopback / non-private
/// host. Parse errors fail closed.
fn validate_webhook_url(raw: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(raw)
        .map_err(|e| anyhow::anyhow!("notification url {raw:?} is not a valid URL: {e}"))?;

    if parsed.scheme() != "https" {
        bail!(
            "notification url must use https:// (got scheme {:?} in {raw:?})",
            parsed.scheme()
        );
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("notification url {raw:?} has no host"))?;

    // Block by name first: covers `localhost`, `localhost.localdomain`, etc.
    let host_lc = host.to_ascii_lowercase();
    if host_lc == "localhost" || host_lc.ends_with(".localhost") {
        bail!("notification url {raw:?} points at a loopback host");
    }

    // Block by IP: loopback, private, link-local, unspecified.
    // `url::Host` yields the bracketed form for IPv6 via `host_str()`; strip
    // brackets before parsing.
    let ip_candidate = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(host);
    if let Ok(ip) = ip_candidate.parse::<IpAddr>() {
        if is_disallowed_ip(&ip) {
            bail!(
                "notification url {raw:?} points at a private / loopback / link-local address"
            );
        }
    }

    Ok(())
}

fn is_disallowed_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || {
                    // Carrier-grade NAT 100.64.0.0/10 — not covered by is_private.
                    let o = v4.octets();
                    o[0] == 100 && (o[1] & 0xc0) == 0x40
                }
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique-local
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // fe80::/10 link-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
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
    fn env_var_without_pitboss_prefix_rejected() {
        std::env::set_var("NOTIFY_TEST_FOREIGN", "leaked");
        let err = substitute_env_vars("${NOTIFY_TEST_FOREIGN}").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("PITBOSS_"),
            "expected prefix message, got: {msg}"
        );
    }

    fn cfg_webhook(url: &str) -> NotificationConfig {
        NotificationConfig {
            kind: SinkKind::Webhook,
            url: Some(url.to_string()),
            events: None,
            severity_min: Severity::Info,
        }
    }

    #[test]
    fn webhook_rejects_http_scheme() {
        let err = validate(&cfg_webhook("http://example.com/hook")).unwrap_err();
        assert!(err.to_string().contains("https://"), "{err}");
    }

    #[test]
    fn webhook_rejects_file_scheme() {
        let err = validate(&cfg_webhook("file:///etc/passwd")).unwrap_err();
        assert!(err.to_string().contains("https://"), "{err}");
    }

    #[test]
    fn webhook_rejects_loopback_ipv4() {
        let err = validate(&cfg_webhook("https://127.0.0.1/hook")).unwrap_err();
        assert!(err.to_string().contains("private / loopback"), "{err}");
    }

    #[test]
    fn webhook_rejects_loopback_hostname() {
        let err = validate(&cfg_webhook("https://localhost/hook")).unwrap_err();
        assert!(err.to_string().contains("loopback"), "{err}");
    }

    #[test]
    fn webhook_rejects_link_local_metadata_ip() {
        let err = validate(&cfg_webhook("https://169.254.169.254/latest")).unwrap_err();
        assert!(err.to_string().contains("private / loopback"), "{err}");
    }

    #[test]
    fn webhook_rejects_private_ipv4() {
        for ip in ["10.0.0.1", "192.168.1.1", "172.16.0.1"] {
            let err = validate(&cfg_webhook(&format!("https://{ip}/hook"))).unwrap_err();
            assert!(
                err.to_string().contains("private / loopback"),
                "ip={ip} err={err}"
            );
        }
    }

    #[test]
    fn webhook_rejects_ipv6_loopback() {
        let err = validate(&cfg_webhook("https://[::1]/hook")).unwrap_err();
        assert!(err.to_string().contains("private / loopback"), "{err}");
    }

    #[test]
    fn webhook_accepts_public_https_url() {
        assert!(validate(&cfg_webhook("https://hooks.slack.com/services/x")).is_ok());
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
