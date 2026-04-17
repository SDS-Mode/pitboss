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
        _ => Ok(Event::Unknown {
            raw: raw.to_string(),
        }),
    }
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
}
