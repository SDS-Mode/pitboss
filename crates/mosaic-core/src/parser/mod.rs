//! Line-oriented parser for Claude Code `--output-format stream-json` output.

pub mod events;

pub use events::{Event, TokenUsage};

use crate::error::ParseError;

pub fn parse_line(bytes: &[u8]) -> Result<Event, ParseError> {
    let raw = std::str::from_utf8(bytes)
        .map_err(|_| {
            ParseError::malformed("non-utf8 line", String::from_utf8_lossy(bytes).into_owned())
        })?
        .trim_end_matches(['\n', '\r']);

    if raw.is_empty() {
        return Err(ParseError::malformed("empty line", raw));
    }

    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| ParseError::malformed(format!("json parse: {e}"), raw))?;

    let ty = value.get("type").and_then(|v| v.as_str());

    match ty {
        Some("system") => {
            let subtype = value
                .get("subtype")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            Ok(Event::System { subtype })
        }
        Some("assistant") => parse_assistant(&value, raw),
        Some("user") => parse_user(&value, raw),
        Some("result") => parse_result(&value, raw),
        Some("rate_limit_event") => Ok(parse_rate_limit(&value)),
        _ => Ok(Event::Unknown {
            raw: raw.to_string(),
        }),
    }
}

fn parse_assistant(value: &serde_json::Value, raw: &str) -> Result<Event, ParseError> {
    let content = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .ok_or_else(|| ParseError::malformed("assistant missing message.content", raw))?;

    for block in content {
        let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match btype {
            "text" => {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    return Ok(Event::AssistantText {
                        text: text.to_string(),
                    });
                }
            }
            "tool_use" => {
                let tool_name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input_summary = block
                    .get("input")
                    .map(ToString::to_string)
                    .unwrap_or_default();
                return Ok(Event::AssistantToolUse {
                    tool_name,
                    input_summary,
                });
            }
            _ => {}
        }
    }

    Err(ParseError::malformed(
        "assistant content had no text or tool_use block",
        raw,
    ))
}

fn parse_user(value: &serde_json::Value, raw: &str) -> Result<Event, ParseError> {
    let content = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .ok_or_else(|| ParseError::malformed("user missing message.content", raw))?;

    for block in content {
        if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
            let c = block
                .get("content")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let content_summary = match c {
                serde_json::Value::String(s) => s,
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => c.to_string(),
                other => other.to_string(),
            };
            return Ok(Event::ToolResult { content_summary });
        }
    }
    Err(ParseError::malformed(
        "user content had no tool_result",
        raw,
    ))
}

fn parse_result(value: &serde_json::Value, raw: &str) -> Result<Event, ParseError> {
    let session_id = value
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ParseError::malformed("result missing session_id", raw))?
        .to_string();

    let usage_val = value
        .get("usage")
        .ok_or_else(|| ParseError::malformed("result missing usage", raw))?;

    let usage = TokenUsage {
        input: u64_field(usage_val, "input_tokens"),
        output: u64_field(usage_val, "output_tokens"),
        cache_read: u64_field(usage_val, "cache_read_input_tokens"),
        cache_creation: u64_field(usage_val, "cache_creation_input_tokens"),
    };

    let subtype = value
        .get("subtype")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let text = value
        .get("result")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Ok(Event::Result {
        subtype,
        session_id,
        text,
        usage,
    })
}

fn parse_rate_limit(value: &serde_json::Value) -> Event {
    let info = value.get("rate_limit_info");
    let status = info
        .and_then(|i| i.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let rate_limit_type = info
        .and_then(|i| i.get("rateLimitType"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let resets_at = info
        .and_then(|i| i.get("resetsAt"))
        .and_then(serde_json::Value::as_u64);
    Event::RateLimit {
        status,
        rate_limit_type,
        resets_at,
    }
}

fn u64_field(obj: &serde_json::Value, key: &str) -> u64 {
    obj.get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_line_is_malformed() {
        let err = parse_line(b"").unwrap_err();
        assert!(matches!(err, ParseError::Malformed { .. }));
    }

    #[test]
    fn parses_system_init() {
        let line = br#"{"type":"system","subtype":"init","session_id":"s1"}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(
            ev,
            Event::System {
                subtype: Some("init".into())
            }
        );
    }

    #[test]
    fn parses_system_without_subtype() {
        let line = br#"{"type":"system"}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(ev, Event::System { subtype: None });
    }

    #[test]
    fn parses_assistant_text() {
        let line =
            br#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello world"}]}}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(
            ev,
            Event::AssistantText {
                text: "hello world".into()
            }
        );
    }

    #[test]
    fn parses_assistant_tool_use() {
        let line = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"x.rs"}}]}}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::AssistantToolUse {
                tool_name,
                input_summary,
            } => {
                assert_eq!(tool_name, "Write");
                assert!(input_summary.contains("file_path"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn parses_assistant_text_takes_first_text_block() {
        let line = br#"{"type":"assistant","message":{"content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]}}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(
            ev,
            Event::AssistantText {
                text: "first".into()
            }
        );
    }

    #[test]
    fn assistant_without_content_is_malformed() {
        let line = br#"{"type":"assistant","message":{}}"#;
        let err = parse_line(line).unwrap_err();
        assert!(matches!(err, ParseError::Malformed { .. }));
    }

    #[test]
    fn parses_user_tool_result_string() {
        let line = br#"{"type":"user","message":{"content":[{"type":"tool_result","content":"file written"}]}}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(
            ev,
            Event::ToolResult {
                content_summary: "file written".into()
            }
        );
    }

    #[test]
    fn parses_user_tool_result_array() {
        let line = br#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"ok"}]}]}}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::ToolResult { content_summary } => assert!(content_summary.contains("ok")),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn parses_result_with_usage() {
        let line = br#"{"type":"result","subtype":"success","session_id":"sess_abc","result":"done","usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":5,"cache_creation_input_tokens":2}}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::Result {
                session_id,
                subtype,
                text,
                usage,
            } => {
                assert_eq!(session_id, "sess_abc");
                assert_eq!(subtype.as_deref(), Some("success"));
                assert_eq!(text.as_deref(), Some("done"));
                assert_eq!(
                    usage,
                    TokenUsage {
                        input: 10,
                        output: 20,
                        cache_read: 5,
                        cache_creation: 2
                    }
                );
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn result_without_session_id_is_malformed() {
        let line = br#"{"type":"result","usage":{"input_tokens":0,"output_tokens":0}}"#;
        let err = parse_line(line).unwrap_err();
        assert!(matches!(err, ParseError::Malformed { .. }));
    }

    #[test]
    fn result_without_usage_is_malformed() {
        let line = br#"{"type":"result","session_id":"s"}"#;
        let err = parse_line(line).unwrap_err();
        assert!(matches!(err, ParseError::Malformed { .. }));
    }

    #[test]
    fn result_missing_optional_cache_fields_defaults_zero() {
        let line =
            br#"{"type":"result","session_id":"s","usage":{"input_tokens":1,"output_tokens":2}}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::Result { usage, .. } => {
                assert_eq!(usage.input, 1);
                assert_eq!(usage.output, 2);
                assert_eq!(usage.cache_read, 0);
                assert_eq!(usage.cache_creation, 0);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn parses_rate_limit_event() {
        let line = br#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1776402000,"rateLimitType":"five_hour","overageStatus":"rejected"}}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::RateLimit {
                status,
                rate_limit_type,
                resets_at,
            } => {
                assert_eq!(status, "allowed");
                assert_eq!(rate_limit_type.as_deref(), Some("five_hour"));
                assert_eq!(resets_at, Some(1_776_402_000));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rate_limit_event_with_minimal_fields() {
        let line = br#"{"type":"rate_limit_event","rate_limit_info":{"status":"throttled"}}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::RateLimit {
                status,
                rate_limit_type,
                resets_at,
            } => {
                assert_eq!(status, "throttled");
                assert!(rate_limit_type.is_none());
                assert!(resets_at.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_type_is_unknown_not_error() {
        let line = br#"{"type":"never_before_seen","foo":"bar"}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::Unknown { raw } => assert!(raw.contains("never_before_seen")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_on_known_types_are_ignored() {
        let line = br#"{"type":"system","subtype":"init","future_field":42}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(
            ev,
            Event::System {
                subtype: Some("init".into())
            }
        );
    }

    #[test]
    fn invalid_json_is_malformed() {
        let line = br"{not json";
        let err = parse_line(line).unwrap_err();
        assert!(matches!(err, ParseError::Malformed { .. }));
    }

    #[test]
    fn missing_type_field_is_unknown() {
        let line = br#"{"message":"hi"}"#;
        let ev = parse_line(line).unwrap();
        assert!(matches!(ev, Event::Unknown { .. }));
    }
}
