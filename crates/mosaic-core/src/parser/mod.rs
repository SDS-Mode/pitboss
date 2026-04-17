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
}
