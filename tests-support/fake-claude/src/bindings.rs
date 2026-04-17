//! Script-local variable bindings and template substitution.
//!
//! Scripts in MCP-client mode can bind MCP tool results under a name
//! (`bind: "w1"`) and reference them in later action args via whole-string
//! templates like `"$w1.task_id"`. This module provides the map and the
//! substitution walker.

#![allow(dead_code)]

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use serde_json::Value;

pub type Bindings = HashMap<String, Value>;

/// Recursively walk `value` in place. Any JSON string whose full content
/// matches `^\$<name>(\.<field>)*$` is replaced with the bound value at
/// that path. Returns Err if a reference names an unknown binding or the
/// path can't be walked.
pub fn substitute(value: &mut Value, bindings: &Bindings) -> Result<()> {
    match value {
        Value::String(s) => {
            if let Some(replacement) = resolve(s, bindings)? {
                *value = replacement;
            }
        }
        Value::Array(items) => {
            for v in items {
                substitute(v, bindings)?;
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                substitute(v, bindings)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// If `s` is a whole-string template reference (starts with `$` and
/// contains only name + optional `.field` segments), return the resolved
/// value. Ok(None) if `s` isn't a template. Err if resolution fails.
fn resolve(s: &str, bindings: &Bindings) -> Result<Option<Value>> {
    if !s.starts_with('$') {
        return Ok(None);
    }
    // Reject partial-string templates like "prefix $w1 suffix". We only
    // support whole-string references — tests that need interpolation
    // can build the string in Rust.
    if s.contains(' ') {
        return Ok(None);
    }
    let rest = &s[1..];
    if rest.is_empty() {
        return Err(anyhow!("empty binding reference: {s:?}"));
    }
    let mut parts = rest.split('.');
    let name = parts.next().unwrap(); // split always yields at least one element
    if name.is_empty() {
        return Err(anyhow!("empty binding name in {s:?}"));
    }
    let root = bindings
        .get(name)
        .ok_or_else(|| anyhow!("unknown binding {name:?} referenced in {s:?}"))?;
    let mut current = root;
    for field in parts {
        current = current.get(field).ok_or_else(|| {
            anyhow!("binding {name:?} has no path .{field} (referenced in {s:?})")
        })?;
    }
    Ok(Some(current.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn substitute_noop_on_plain_string() {
        let mut v = json!("hello");
        let b = Bindings::new();
        substitute(&mut v, &b).unwrap();
        assert_eq!(v, json!("hello"));
    }

    #[test]
    fn substitute_whole_string_reference() {
        let mut v = json!("$w1");
        let mut b = Bindings::new();
        b.insert("w1".into(), json!({"task_id": "worker-abc"}));
        substitute(&mut v, &b).unwrap();
        assert_eq!(v, json!({"task_id": "worker-abc"}));
    }

    #[test]
    fn substitute_path_walk() {
        let mut v = json!({"task_id": "$w1.task_id"});
        let mut b = Bindings::new();
        b.insert("w1".into(), json!({"task_id": "worker-abc"}));
        substitute(&mut v, &b).unwrap();
        assert_eq!(v, json!({"task_id": "worker-abc"}));
    }

    #[test]
    fn substitute_nested_array() {
        let mut v = json!({"task_ids": ["$w1.task_id", "$w2.task_id"]});
        let mut b = Bindings::new();
        b.insert("w1".into(), json!({"task_id": "a"}));
        b.insert("w2".into(), json!({"task_id": "b"}));
        substitute(&mut v, &b).unwrap();
        assert_eq!(v, json!({"task_ids": ["a", "b"]}));
    }

    #[test]
    fn substitute_missing_binding_errors() {
        let mut v = json!("$w9.task_id");
        let b = Bindings::new();
        let err = substitute(&mut v, &b).unwrap_err();
        assert!(err.to_string().contains("unknown binding"), "got: {err}");
    }

    #[test]
    fn substitute_missing_path_errors() {
        let mut v = json!("$w1.nonexistent");
        let mut b = Bindings::new();
        b.insert("w1".into(), json!({"task_id": "a"}));
        let err = substitute(&mut v, &b).unwrap_err();
        assert!(err.to_string().contains("no path"), "got: {err}");
    }

    #[test]
    fn substitute_ignores_dollar_within_text() {
        let mut v = json!("some $text here");
        let b = Bindings::new();
        substitute(&mut v, &b).unwrap();
        assert_eq!(v, json!("some $text here"));
    }
}
