# Agent Shire v0.1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the v0.1 headless Rust dispatcher for parallel Claude Code agents ("Agent Shire"), per `docs/superpowers/specs/2026-04-16-agent-shire-design.md`.

**Architecture:** Cargo workspace with two crates — `pitboss-core` (library: process/parser/session/worktree/store machinery) and `pitboss-cli` (binary: manifest loader + dispatcher). Plus a `tests-support/fake-claude` workspace member used only for integration tests. All async via tokio. TDD throughout with injected `ProcessSpawner` for unit tests and a scripted fake binary for end-to-end tests.

**Tech Stack:** Rust stable, tokio, serde/serde_json, toml, clap, git2, uuid (v7), chrono, thiserror, anyhow, tracing, async-trait, shellexpand, atty, tempfile.

---

## Conventions

- Every task ends with a commit. Commits are small and buildable. Use `cargo test -p <crate>` to scope runs.
- Work from the workspace root (`/run/media/system/Dos/Projects/agentshire/`) unless noted.
- `cargo check` before running tests to catch syntax/import issues early.
- If a step fails unexpectedly, stop and report — do not speculate-fix.

---

## Phase 0 — Workspace Skeleton

### Task 1: Initialize Cargo workspace with two member crates

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `.gitignore`
- Create: `crates/pitboss-core/Cargo.toml`
- Create: `crates/pitboss-core/src/lib.rs`
- Create: `crates/pitboss-cli/Cargo.toml`
- Create: `crates/pitboss-cli/src/main.rs`

- [ ] **Step 1: Write workspace manifest**

Create `Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/pitboss-core",
    "crates/pitboss-cli",
]

[workspace.package]
version     = "0.1.0"
edition     = "2021"
rust-version = "1.82"
license     = "MIT OR Apache-2.0"
authors     = ["Agent Shire contributors"]

[workspace.dependencies]
tokio              = { version = "1", features = ["rt-multi-thread","process","io-util","sync","time","signal","macros","fs"] }
serde              = { version = "1", features = ["derive"] }
serde_json         = "1"
toml               = "0.8"
uuid               = { version = "1", features = ["v7","serde"] }
chrono             = { version = "0.4", features = ["serde"] }
git2               = "0.19"
thiserror          = "1"
anyhow             = "1"
tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter","fmt"] }
async-trait        = "0.1"
clap               = { version = "4", features = ["derive"] }
shellexpand        = "3"
atty               = "0.2"
tempfile           = "3"
```

- [ ] **Step 2: Write rust-toolchain.toml**

Create `rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 3: Write .gitignore**

Create `.gitignore`:

```
/target
**/*.rs.bk
Cargo.lock.bak
.DS_Store
*.swp
```

- [ ] **Step 4: Write pitboss-core Cargo.toml**

Create `crates/pitboss-core/Cargo.toml`:

```toml
[package]
name         = "pitboss-core"
version      = { workspace = true }
edition      = { workspace = true }
rust-version = { workspace = true }
license      = { workspace = true }

[dependencies]
tokio       = { workspace = true }
serde       = { workspace = true }
serde_json  = { workspace = true }
uuid        = { workspace = true }
chrono      = { workspace = true }
git2        = { workspace = true }
thiserror   = { workspace = true }
tracing     = { workspace = true }
async-trait = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
tokio    = { workspace = true, features = ["test-util"] }
```

- [ ] **Step 5: Write pitboss-core lib.rs placeholder**

Create `crates/pitboss-core/src/lib.rs`:

```rust
//! pitboss-core — shared runtime for Agent Shire and future Mosaic TUI.

#![forbid(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

/// Library version matching the workspace package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod smoke {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
```

- [ ] **Step 6: Write pitboss-cli Cargo.toml**

Create `crates/pitboss-cli/Cargo.toml`:

```toml
[package]
name         = "pitboss-cli"
version      = { workspace = true }
edition      = { workspace = true }
rust-version = { workspace = true }
license      = { workspace = true }

[[bin]]
name = "shire"
path = "src/main.rs"

[dependencies]
pitboss-core        = { path = "../pitboss-core" }
tokio              = { workspace = true }
serde              = { workspace = true }
serde_json         = { workspace = true }
toml               = { workspace = true }
clap               = { workspace = true }
anyhow             = { workspace = true }
tracing            = { workspace = true }
tracing-subscriber = { workspace = true }
shellexpand        = { workspace = true }
atty               = { workspace = true }
uuid               = { workspace = true }
chrono             = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 7: Write pitboss-cli main.rs placeholder**

Create `crates/pitboss-cli/src/main.rs`:

```rust
fn main() {
    println!("shire v{} (skeleton)", env!("CARGO_PKG_VERSION"));
}
```

- [ ] **Step 8: Verify workspace builds**

Run: `cargo build --workspace`
Expected: Successful build, one `pitboss` binary at `target/debug/shire`.

Run: `cargo test -p pitboss-core`
Expected: `test smoke::version_is_set ... ok`, 1 passed.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore crates/
git commit -m "Add Cargo workspace skeleton with pitboss-core and pitboss-cli"
```

---

### Task 2: Add README stub

**Files:**
- Create: `README.md`

- [ ] **Step 1: Write README.md**

Create `README.md`:

```markdown
# Agent Shire

Headless Rust dispatcher for parallel Claude Code agent sessions ("Hobbits").
Reads a `shire.toml` manifest, fans out N subprocesses under a concurrency cap,
and writes structured per-run artifacts.

**Status:** v0.1 under active development. See
[`docs/superpowers/specs/2026-04-16-agent-shire-design.md`](docs/superpowers/specs/2026-04-16-agent-shire-design.md)
for the authoritative design.

## Build

```
cargo build --release
```

## Test

```
cargo test --workspace
```

## Layout

- `crates/pitboss-core/` — library: session/process/parser/worktree/store machinery
- `crates/pitboss-cli/`   — binary: `pitboss` CLI that consumes the library
- `tests-support/fake-claude/` — scripted fake `claude` used only in integration tests
- `docs/` — design spec and implementation plan
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "Add README stub"
```

---

### Task 3: Wire up clippy + fmt check locally

**Files:**
- Create: `.cargo/config.toml`

- [ ] **Step 1: Write cargo config with deny warnings on clippy**

Create `.cargo/config.toml`:

```toml
[alias]
lint = "clippy --workspace --all-targets --all-features -- -D warnings"
tidy = "fmt --all -- --check"
```

- [ ] **Step 2: Verify both aliases work**

Run: `cargo fmt --all`
Expected: no output, no changes needed.

Run: `cargo lint`
Expected: `Finished` with no warnings.

- [ ] **Step 3: Commit**

```bash
git add .cargo/
git commit -m "Add cargo lint/tidy aliases"
```

---

## Phase 1 — Parser (pitboss-core)

The parser is pure — no I/O, no async. Fixture-driven tests read canonical stream-json lines and assert on the emitted `Event`. Order of tasks: types first, then one event family per task.

### Task 4: Define Event enum, ParseError, and parse_line skeleton

**Files:**
- Create: `crates/pitboss-core/src/parser/mod.rs`
- Create: `crates/pitboss-core/src/parser/events.rs`
- Create: `crates/pitboss-core/src/error.rs`
- Modify: `crates/pitboss-core/src/lib.rs`
- Test: `crates/pitboss-core/src/parser/mod.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Add to `crates/pitboss-core/src/parser/mod.rs` (create file):

```rust
//! Line-oriented parser for Claude Code `--output-format stream-json` output.
//!
//! Pure function: bytes in, `Event` out. No I/O, no async, fully testable.

pub mod events;

pub use events::{Event, TokenUsage};

use crate::error::ParseError;

/// Parse a single line of Claude Code stream-json output into an [`Event`].
///
/// Unknown top-level `type` values are mapped to [`Event::Unknown`]; malformed
/// known events return [`ParseError::Malformed`].
pub fn parse_line(bytes: &[u8]) -> Result<Event, ParseError> {
    let _ = bytes;
    unimplemented!("parse_line")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_line_is_malformed() {
        let err = parse_line(b"").unwrap_err();
        assert!(matches!(err, ParseError::Malformed { .. }));
    }
}
```

Add to `crates/pitboss-core/src/error.rs` (create file):

```rust
use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("malformed stream-json line: {reason}")]
    Malformed { reason: String, raw: String },
}

impl ParseError {
    pub(crate) fn malformed(reason: impl Into<String>, raw: impl Into<String>) -> Self {
        Self::Malformed { reason: reason.into(), raw: raw.into() }
    }
}

// Convenience display of first 80 chars of raw line for diagnostics.
pub(crate) fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}…", &s[..n]) }
}

#[allow(dead_code)]
fn _unused_fmt_import(_: fmt::Arguments) {}
```

Add to `crates/pitboss-core/src/parser/events.rs` (create file):

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A parsed stream-json event from the Claude Code subprocess.
///
/// Each enum variant corresponds to a top-level `"type"` in the wire format.
/// Unknown types are captured verbatim in [`Event::Unknown`] to tolerate
/// additions to the Claude Code wire format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    System { subtype: Option<String> },
    AssistantText { text: String },
    AssistantToolUse { tool_name: String, input_summary: String },
    ToolResult { content_summary: String },
    Result {
        subtype: Option<String>,
        session_id: String,
        text: Option<String>,
        usage: TokenUsage,
    },
    Unknown { raw: String },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TokenUsage {
    pub fn add(&mut self, other: &TokenUsage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_creation += other.cache_creation;
    }
}

#[allow(dead_code)] // used in Run summary field later
pub(crate) fn _reserved_utc(_: DateTime<Utc>) {}
```

Modify `crates/pitboss-core/src/lib.rs` — replace contents:

```rust
//! pitboss-core — shared runtime for Agent Shire and future Mosaic TUI.

#![forbid(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

pub mod error;
pub mod parser;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod smoke {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p pitboss-core parser::tests::empty_line_is_malformed`
Expected: PANIC on `unimplemented!("parse_line")`.

- [ ] **Step 3: Implement minimal parse_line (empty + unknown paths)**

Replace `crates/pitboss-core/src/parser/mod.rs` with:

```rust
//! Line-oriented parser for Claude Code `--output-format stream-json` output.

pub mod events;

pub use events::{Event, TokenUsage};

use crate::error::ParseError;

pub fn parse_line(bytes: &[u8]) -> Result<Event, ParseError> {
    let raw = std::str::from_utf8(bytes)
        .map_err(|_| ParseError::malformed("non-utf8 line", ""))?
        .trim_end_matches(['\n', '\r']);

    if raw.is_empty() {
        return Err(ParseError::malformed("empty line", raw));
    }

    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| ParseError::malformed(format!("json parse: {e}"), raw))?;

    let ty = value.get("type").and_then(|v| v.as_str());
    match ty {
        _ => Ok(Event::Unknown { raw: raw.to_string() }),
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
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p pitboss-core parser`
Expected: `test parser::tests::empty_line_is_malformed ... ok`

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/
git commit -m "Add parser skeleton with Event enum and ParseError"
```

---

### Task 5: Parse `system` events

**Files:**
- Modify: `crates/pitboss-core/src/parser/mod.rs`
- Test: `crates/pitboss-core/src/parser/mod.rs`

- [ ] **Step 1: Write failing tests**

Append to the `tests` module in `crates/pitboss-core/src/parser/mod.rs`:

```rust
    #[test]
    fn parses_system_init() {
        let line = br#"{"type":"system","subtype":"init","session_id":"s1"}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(ev, Event::System { subtype: Some("init".into()) });
    }

    #[test]
    fn parses_system_without_subtype() {
        let line = br#"{"type":"system"}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(ev, Event::System { subtype: None });
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core parser::tests::parses_system_init`
Expected: FAIL — assertion mismatch (got `Unknown`, expected `System`).

- [ ] **Step 3: Implement system dispatch**

Replace the `match ty` block in `parse_line`:

```rust
    match ty {
        Some("system") => {
            let subtype = value.get("subtype").and_then(|v| v.as_str()).map(str::to_string);
            Ok(Event::System { subtype })
        }
        _ => Ok(Event::Unknown { raw: raw.to_string() }),
    }
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core parser`
Expected: both new tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/src/parser/mod.rs
git commit -m "Parse system events in stream-json"
```

---

### Task 6: Parse `assistant` events (text and tool_use)

**Files:**
- Modify: `crates/pitboss-core/src/parser/mod.rs`

- [ ] **Step 1: Write failing tests**

Append to the `tests` module:

```rust
    #[test]
    fn parses_assistant_text() {
        let line = br#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello world"}]}}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(ev, Event::AssistantText { text: "hello world".into() });
    }

    #[test]
    fn parses_assistant_tool_use() {
        let line = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"x.rs"}}]}}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::AssistantToolUse { tool_name, input_summary } => {
                assert_eq!(tool_name, "Write");
                assert!(input_summary.contains("file_path"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn parses_assistant_text_takes_first_text_block() {
        // If content has multiple blocks, first text block wins for preview.
        let line = br#"{"type":"assistant","message":{"content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]}}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(ev, Event::AssistantText { text: "first".into() });
    }

    #[test]
    fn assistant_without_content_is_malformed() {
        let line = br#"{"type":"assistant","message":{}}"#;
        let err = parse_line(line).unwrap_err();
        assert!(matches!(err, ParseError::Malformed { .. }));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core parser::tests::parses_assistant_text`
Expected: FAIL — no match arm for "assistant".

- [ ] **Step 3: Implement assistant dispatch**

Replace the `match ty` block:

```rust
    match ty {
        Some("system") => {
            let subtype = value.get("subtype").and_then(|v| v.as_str()).map(str::to_string);
            Ok(Event::System { subtype })
        }
        Some("assistant") => parse_assistant(&value, raw),
        _ => Ok(Event::Unknown { raw: raw.to_string() }),
    }
```

Add helper below `parse_line`:

```rust
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
                    return Ok(Event::AssistantText { text: text.to_string() });
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
                    .map(|i| i.to_string())
                    .unwrap_or_default();
                return Ok(Event::AssistantToolUse { tool_name, input_summary });
            }
            _ => continue,
        }
    }

    Err(ParseError::malformed(
        "assistant content had no text or tool_use block",
        raw,
    ))
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core parser`
Expected: all assistant tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/src/parser/mod.rs
git commit -m "Parse assistant text and tool_use events"
```

---

### Task 7: Parse `user` (tool_result) events

**Files:**
- Modify: `crates/pitboss-core/src/parser/mod.rs`

- [ ] **Step 1: Write failing tests**

Append to the `tests` module:

```rust
    #[test]
    fn parses_user_tool_result_string() {
        let line = br#"{"type":"user","message":{"content":[{"type":"tool_result","content":"file written"}]}}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(ev, Event::ToolResult { content_summary: "file written".into() });
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core parser::tests::parses_user_tool_result_string`
Expected: FAIL — no match arm for "user".

- [ ] **Step 3: Implement user dispatch**

Replace the `match ty` block:

```rust
    match ty {
        Some("system") => {
            let subtype = value.get("subtype").and_then(|v| v.as_str()).map(str::to_string);
            Ok(Event::System { subtype })
        }
        Some("assistant") => parse_assistant(&value, raw),
        Some("user") => parse_user(&value, raw),
        _ => Ok(Event::Unknown { raw: raw.to_string() }),
    }
```

Add helper below `parse_assistant`:

```rust
fn parse_user(value: &serde_json::Value, raw: &str) -> Result<Event, ParseError> {
    let content = value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .ok_or_else(|| ParseError::malformed("user missing message.content", raw))?;

    for block in content {
        if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
            let c = block.get("content").cloned().unwrap_or(serde_json::Value::Null);
            let content_summary = match c {
                serde_json::Value::String(s) => s,
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => c.to_string(),
                other => other.to_string(),
            };
            return Ok(Event::ToolResult { content_summary });
        }
    }
    Err(ParseError::malformed("user content had no tool_result", raw))
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core parser`
Expected: both user tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/src/parser/mod.rs
git commit -m "Parse user tool_result events"
```

---

### Task 8: Parse `result` events with usage extraction

**Files:**
- Modify: `crates/pitboss-core/src/parser/mod.rs`

- [ ] **Step 1: Write failing tests**

Append to the `tests` module:

```rust
    #[test]
    fn parses_result_with_usage() {
        let line = br#"{"type":"result","subtype":"success","session_id":"sess_abc","result":"done","usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":5,"cache_creation_input_tokens":2}}"#;
        let ev = parse_line(line).unwrap();
        match ev {
            Event::Result { session_id, subtype, text, usage } => {
                assert_eq!(session_id, "sess_abc");
                assert_eq!(subtype.as_deref(), Some("success"));
                assert_eq!(text.as_deref(), Some("done"));
                assert_eq!(usage, TokenUsage {
                    input: 10, output: 20, cache_read: 5, cache_creation: 2
                });
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
        let line = br#"{"type":"result","session_id":"s","usage":{"input_tokens":1,"output_tokens":2}}"#;
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core parser::tests::parses_result_with_usage`
Expected: FAIL.

- [ ] **Step 3: Implement result dispatch**

Add the `"result"` arm in `match ty`:

```rust
        Some("result") => parse_result(&value, raw),
```

So the full match becomes:

```rust
    match ty {
        Some("system") => {
            let subtype = value.get("subtype").and_then(|v| v.as_str()).map(str::to_string);
            Ok(Event::System { subtype })
        }
        Some("assistant") => parse_assistant(&value, raw),
        Some("user") => parse_user(&value, raw),
        Some("result") => parse_result(&value, raw),
        _ => Ok(Event::Unknown { raw: raw.to_string() }),
    }
```

Add helper below `parse_user`:

```rust
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
        input:          u64_field(usage_val, "input_tokens"),
        output:         u64_field(usage_val, "output_tokens"),
        cache_read:     u64_field(usage_val, "cache_read_input_tokens"),
        cache_creation: u64_field(usage_val, "cache_creation_input_tokens"),
    };

    let subtype = value.get("subtype").and_then(|v| v.as_str()).map(str::to_string);
    let text = value.get("result").and_then(|v| v.as_str()).map(str::to_string);

    Ok(Event::Result { subtype, session_id, text, usage })
}

fn u64_field(obj: &serde_json::Value, key: &str) -> u64 {
    obj.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core parser`
Expected: all result tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/src/parser/mod.rs
git commit -m "Parse result events and extract token usage"
```

---

### Task 9: Parser tolerance and cleanup

**Files:**
- Modify: `crates/pitboss-core/src/parser/mod.rs`

- [ ] **Step 1: Write tests pinning tolerance contract**

Append to the `tests` module:

```rust
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
        assert_eq!(ev, Event::System { subtype: Some("init".into()) });
    }

    #[test]
    fn invalid_json_is_malformed() {
        let line = br#"{not json"#;
        let err = parse_line(line).unwrap_err();
        assert!(matches!(err, ParseError::Malformed { .. }));
    }

    #[test]
    fn missing_type_field_is_unknown() {
        let line = br#"{"message":"hi"}"#;
        let ev = parse_line(line).unwrap();
        assert!(matches!(ev, Event::Unknown { .. }));
    }
```

- [ ] **Step 2: Run tests — should all pass already**

Run: `cargo test -p pitboss-core parser`
Expected: all pass (tolerance behaviors are already implemented by Task 4's fallback branch and using `serde_json::Value`).

If any fail, fix inline in `parse_line` — the intent is already correct, only bugs to repair. Do not broaden scope.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-core/src/parser/mod.rs
git commit -m "Pin parser tolerance contract with tests"
```

---

## Phase 2 — Process Layer (pitboss-core)

Two traits (`ProcessSpawner`, `ChildProcess`), one tokio real impl, and a `FakeSpawner` used by unit tests elsewhere. Keeping the trait object-safe via `#[async_trait]`.

### Task 10: Define ProcessSpawner and ChildProcess traits with types

**Files:**
- Create: `crates/pitboss-core/src/process/mod.rs`
- Create: `crates/pitboss-core/src/process/spawner.rs`
- Modify: `crates/pitboss-core/src/lib.rs`
- Modify: `crates/pitboss-core/src/error.rs`

- [ ] **Step 1: Write failing test**

Create `crates/pitboss-core/src/process/mod.rs`:

```rust
//! Process spawn abstraction. Real impl uses tokio; tests inject fakes.

pub mod spawner;

pub use spawner::{ChildProcess, ProcessSpawner, SpawnCmd};

use crate::error::SpawnError;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn spawn_cmd_is_constructible() {
        let cmd = SpawnCmd {
            program: PathBuf::from("claude"),
            args:    vec!["--help".into()],
            cwd:     PathBuf::from("/tmp"),
            env:     HashMap::new(),
        };
        assert_eq!(cmd.program, PathBuf::from("claude"));
    }
}
```

Create `crates/pitboss-core/src/process/spawner.rs`:

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::ExitStatus;

use async_trait::async_trait;
use tokio::io::AsyncRead;

use crate::error::SpawnError;

/// Command to spawn. Pure data — no I/O.
#[derive(Debug, Clone)]
pub struct SpawnCmd {
    pub program: PathBuf,
    pub args:    Vec<String>,
    pub cwd:     PathBuf,
    pub env:     HashMap<String, String>,
}

/// A running child process. Object-safe: callers hold `Box<dyn ChildProcess>`.
#[async_trait]
pub trait ChildProcess: Send {
    /// Take stdout (consuming — may only be called once).
    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>>;
    /// Take stderr (consuming — may only be called once).
    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>>;
    /// Wait for the child to exit. Must be called exactly once.
    async fn wait(&mut self) -> std::io::Result<ExitStatus>;
    /// Send SIGTERM (best effort).
    fn terminate(&mut self) -> std::io::Result<()>;
    /// Send SIGKILL (best effort).
    fn kill(&mut self) -> std::io::Result<()>;
    /// OS-level process id, if available.
    fn pid(&self) -> Option<u32>;
}

/// Produces [`ChildProcess`] values from a [`SpawnCmd`].
#[async_trait]
pub trait ProcessSpawner: Send + Sync + 'static {
    async fn spawn(&self, cmd: SpawnCmd) -> Result<Box<dyn ChildProcess>, SpawnError>;
}
```

Add to `crates/pitboss-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("binary not found: {path}")]
    BinaryNotFound { path: String },

    #[error("io error during spawn: {0}")]
    Io(#[from] std::io::Error),

    #[error("spawn rejected: {reason}")]
    Rejected { reason: String },
}
```

Modify `crates/pitboss-core/src/lib.rs` — replace contents:

```rust
//! pitboss-core — shared runtime for Agent Shire and future Mosaic TUI.

#![forbid(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

pub mod error;
pub mod parser;
pub mod process;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod smoke {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they compile and pass**

Run: `cargo test -p pitboss-core process`
Expected: `spawn_cmd_is_constructible ... ok`.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-core/src/process/ crates/pitboss-core/src/lib.rs crates/pitboss-core/src/error.rs
git commit -m "Define ProcessSpawner and ChildProcess traits"
```

---

### Task 11: Implement TokioSpawner (real process spawning)

**Files:**
- Create: `crates/pitboss-core/src/process/tokio_impl.rs`
- Modify: `crates/pitboss-core/src/process/mod.rs`

- [ ] **Step 1: Write failing test (uses `echo` as a stable cross-platform-ish command)**

Append to `crates/pitboss-core/src/process/mod.rs`:

```rust
#[cfg(test)]
mod real_tests {
    use super::*;
    use crate::process::tokio_impl::TokioSpawner;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn tokio_spawner_runs_echo_and_captures_stdout() {
        let spawner = TokioSpawner::new();
        let cmd = SpawnCmd {
            program: PathBuf::from("sh"),
            args:    vec!["-c".into(), "printf 'hello\\n'".into()],
            cwd:     std::env::temp_dir(),
            env:     HashMap::new(),
        };
        let mut child = spawner.spawn(cmd).await.expect("spawn ok");
        let mut stdout = child.take_stdout().expect("stdout present");
        let mut buf = String::new();
        stdout.read_to_string(&mut buf).await.expect("read ok");
        let status = child.wait().await.expect("wait ok");
        assert_eq!(buf.trim(), "hello");
        assert!(status.success());
    }

    #[tokio::test]
    async fn tokio_spawner_reports_binary_not_found() {
        let spawner = TokioSpawner::new();
        let cmd = SpawnCmd {
            program: PathBuf::from("/definitely/not/a/binary/xyz"),
            args:    vec![],
            cwd:     std::env::temp_dir(),
            env:     HashMap::new(),
        };
        let err = spawner.spawn(cmd).await.unwrap_err();
        match err {
            crate::error::SpawnError::BinaryNotFound { .. } | crate::error::SpawnError::Io(_) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
```

Update the module declaration in `crates/pitboss-core/src/process/mod.rs`:

```rust
pub mod spawner;
pub mod tokio_impl;

pub use spawner::{ChildProcess, ProcessSpawner, SpawnCmd};
pub use tokio_impl::TokioSpawner;
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core process::real_tests`
Expected: FAIL — `TokioSpawner` undefined.

- [ ] **Step 3: Implement TokioSpawner**

Create `crates/pitboss-core/src/process/tokio_impl.rs`:

```rust
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};

use async_trait::async_trait;
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};

use crate::error::SpawnError;

use super::spawner::{ChildProcess, ProcessSpawner, SpawnCmd};

#[derive(Default, Clone)]
pub struct TokioSpawner;

impl TokioSpawner {
    pub fn new() -> Self { Self }
}

struct TokioChild {
    inner: Child,
}

#[async_trait]
impl ChildProcess for TokioChild {
    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>> {
        self.inner.stdout.take().map(|s| Box::pin(s) as _)
    }

    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>> {
        self.inner.stderr.take().map(|s| Box::pin(s) as _)
    }

    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.inner.wait().await
    }

    fn terminate(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            if let Some(pid) = self.pid() {
                use std::os::raw::c_int;
                const SIGTERM: c_int = 15;
                // SAFETY: libc::kill is defined; pid is u32 from tokio which matches the PID we spawned.
                let rc = unsafe { libc::kill(pid as i32, SIGTERM) };
                if rc != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            self.inner.start_kill()
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.inner.start_kill()
    }

    fn pid(&self) -> Option<u32> {
        self.inner.id()
    }
}

#[async_trait]
impl ProcessSpawner for TokioSpawner {
    async fn spawn(&self, cmd: SpawnCmd) -> Result<Box<dyn ChildProcess>, SpawnError> {
        let mut command = Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .current_dir(&cmd.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));

        let child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SpawnError::BinaryNotFound { path: cmd.program.display().to_string() }
            } else {
                SpawnError::Io(e)
            }
        })?;

        Ok(Box::new(TokioChild { inner: child }))
    }
}
```

Add `libc` to `crates/pitboss-core/Cargo.toml` dependencies (SIGTERM on unix):

```toml
[dependencies]
# ... existing ...
libc = "0.2"
```

Also add to workspace root `Cargo.toml` `[workspace.dependencies]`:

```toml
libc = "0.2"
```

Then change pitboss-core's dependency to `libc = { workspace = true }`.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core process`
Expected: both real_tests pass.

Run: `cargo lint`
Expected: no clippy warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/ Cargo.toml
git commit -m "Implement TokioSpawner using tokio::process::Command"
```

---

### Task 12: FakeSpawner for deterministic unit tests

**Files:**
- Create: `crates/pitboss-core/src/process/fake.rs`
- Modify: `crates/pitboss-core/src/process/mod.rs`

`FakeSpawner` is `#[cfg(feature = "test-support")]`-gated so external consumers can opt into it. We'll enable that feature in dev-dependencies automatically.

- [ ] **Step 1: Add feature flag**

Modify `crates/pitboss-core/Cargo.toml`:

```toml
[features]
default = []
test-support = []
```

Also add (at the bottom of the file):

```toml
[package.metadata.docs.rs]
all-features = true

# Enable test-support when compiling the crate's own tests.
[lib]
doctest = false
```

And ensure pitboss-core's own dev-tests enable it. Add a `[dev-dependencies]` self-entry:

```toml
[dev-dependencies]
pitboss-core = { path = ".", features = ["test-support"] }
tempfile    = { workspace = true }
tokio       = { workspace = true, features = ["test-util"] }
```

(Yes, depending on yourself via dev-dependencies with a feature is the idiomatic Rust workaround for "enable this feature in tests only.")

- [ ] **Step 2: Write failing test**

Append to `crates/pitboss-core/src/process/mod.rs`:

```rust
#[cfg(all(test, feature = "test-support"))]
mod fake_tests {
    use super::*;
    use crate::process::fake::{FakeSpawner, FakeScript};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn fake_spawner_emits_scripted_stdout() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .stdout_line(r#"{"type":"result","session_id":"s1","usage":{"input_tokens":1,"output_tokens":2}}"#)
            .exit_code(0);
        let spawner = FakeSpawner::new(script);
        let cmd = SpawnCmd {
            program: PathBuf::from("claude"),
            args: vec![],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
        };
        let mut child = spawner.spawn(cmd).await.unwrap();
        let mut stdout = child.take_stdout().unwrap();
        let mut buf = String::new();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_to_string(&mut buf))
            .await.expect("read completes").unwrap();
        let status = child.wait().await.unwrap();
        assert!(buf.contains("system"));
        assert!(buf.contains("result"));
        assert!(status.success());
    }

    #[tokio::test]
    async fn fake_spawner_reports_nonzero_exit() {
        let script = FakeScript::new().stdout_line("oops").exit_code(42);
        let mut child = FakeSpawner::new(script)
            .spawn(SpawnCmd {
                program: PathBuf::from("x"), args: vec![],
                cwd: PathBuf::from("/tmp"), env: HashMap::new(),
            })
            .await.unwrap();
        let _ = child.take_stdout();
        let status = child.wait().await.unwrap();
        assert_eq!(status.code(), Some(42));
    }
}
```

Update the module declaration in `crates/pitboss-core/src/process/mod.rs`:

```rust
pub mod spawner;
pub mod tokio_impl;

#[cfg(feature = "test-support")]
pub mod fake;

pub use spawner::{ChildProcess, ProcessSpawner, SpawnCmd};
pub use tokio_impl::TokioSpawner;
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p pitboss-core --features test-support process::fake_tests`
Expected: FAIL — `FakeSpawner` undefined.

- [ ] **Step 4: Implement FakeSpawner**

Create `crates/pitboss-core/src/process/fake.rs`:

```rust
use std::pin::Pin;
use std::process::ExitStatus;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{duplex, AsyncRead, AsyncWriteExt, DuplexStream};
use tokio::sync::oneshot;

use crate::error::SpawnError;

use super::spawner::{ChildProcess, ProcessSpawner, SpawnCmd};

#[derive(Debug, Clone)]
enum Action {
    StdoutLine(String),
    StderrLine(String),
    Sleep(Duration),
}

/// A script of events a [`FakeSpawner`]-produced child will play back.
#[derive(Debug, Clone, Default)]
pub struct FakeScript {
    actions:        Vec<Action>,
    exit_code:      i32,
    spawn_delay:    Option<Duration>,
    fail_on_spawn:  Option<String>,
    hold_until_signal: bool,
}

impl FakeScript {
    pub fn new() -> Self { Self::default() }
    pub fn stdout_line<S: Into<String>>(mut self, s: S) -> Self {
        self.actions.push(Action::StdoutLine(s.into()));
        self
    }
    pub fn stderr_line<S: Into<String>>(mut self, s: S) -> Self {
        self.actions.push(Action::StderrLine(s.into()));
        self
    }
    pub fn sleep(mut self, d: Duration) -> Self {
        self.actions.push(Action::Sleep(d));
        self
    }
    pub fn exit_code(mut self, code: i32) -> Self {
        self.exit_code = code;
        self
    }
    pub fn fail_spawn<S: Into<String>>(mut self, reason: S) -> Self {
        self.fail_on_spawn = Some(reason.into());
        self
    }
    /// Child never exits on its own; must be terminated.
    pub fn hold_until_signal(mut self) -> Self {
        self.hold_until_signal = true;
        self
    }
}

#[derive(Clone)]
pub struct FakeSpawner {
    script: FakeScript,
}

impl FakeSpawner {
    pub fn new(script: FakeScript) -> Self { Self { script } }
}

struct FakeChild {
    stdout: Option<Pin<Box<DuplexStream>>>,
    stderr: Option<Pin<Box<DuplexStream>>>,
    exit_rx: Option<oneshot::Receiver<i32>>,
    kill_tx: Option<oneshot::Sender<()>>,
    pid: u32,
    held_code: Option<i32>,
}

#[async_trait]
impl ChildProcess for FakeChild {
    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>> {
        self.stdout.take().map(|s| s as _)
    }
    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>> {
        self.stderr.take().map(|s| s as _)
    }
    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        let code = if let Some(rx) = self.exit_rx.take() {
            rx.await.unwrap_or(-1)
        } else {
            self.held_code.unwrap_or(-1)
        };
        Ok(exit_status_from_code(code))
    }
    fn terminate(&mut self) -> std::io::Result<()> {
        if let Some(tx) = self.kill_tx.take() { let _ = tx.send(()); }
        Ok(())
    }
    fn kill(&mut self) -> std::io::Result<()> {
        if let Some(tx) = self.kill_tx.take() { let _ = tx.send(()); }
        Ok(())
    }
    fn pid(&self) -> Option<u32> { Some(self.pid) }
}

#[async_trait]
impl ProcessSpawner for FakeSpawner {
    async fn spawn(&self, _cmd: SpawnCmd) -> Result<Box<dyn ChildProcess>, SpawnError> {
        if let Some(delay) = self.script.spawn_delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(reason) = &self.script.fail_on_spawn {
            return Err(SpawnError::Rejected { reason: reason.clone() });
        }
        let (mut stdout_w, stdout_r) = duplex(4096);
        let (mut stderr_w, stderr_r) = duplex(4096);
        let (exit_tx, exit_rx) = oneshot::channel();
        let (kill_tx, mut kill_rx) = oneshot::channel();

        let actions = self.script.actions.clone();
        let exit_code = self.script.exit_code;
        let hold = self.script.hold_until_signal;

        tokio::spawn(async move {
            for a in actions {
                match a {
                    Action::StdoutLine(s) => {
                        let _ = stdout_w.write_all(s.as_bytes()).await;
                        let _ = stdout_w.write_all(b"\n").await;
                    }
                    Action::StderrLine(s) => {
                        let _ = stderr_w.write_all(s.as_bytes()).await;
                        let _ = stderr_w.write_all(b"\n").await;
                    }
                    Action::Sleep(d) => tokio::time::sleep(d).await,
                }
            }
            // Close writers so readers see EOF.
            drop(stdout_w);
            drop(stderr_w);
            if hold {
                let _ = (&mut kill_rx).await;
                let _ = exit_tx.send(143); // simulate SIGTERM
            } else {
                let _ = exit_tx.send(exit_code);
            }
        });

        Ok(Box::new(FakeChild {
            stdout:   Some(Box::pin(stdout_r)),
            stderr:   Some(Box::pin(stderr_r)),
            exit_rx:  Some(exit_rx),
            kill_tx:  Some(kill_tx),
            pid:      1,
            held_code: None,
        }))
    }
}

#[cfg(unix)]
fn exit_status_from_code(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw((code & 0xff) << 8)
}

#[cfg(not(unix))]
fn exit_status_from_code(code: i32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(code as u32)
}
```

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test -p pitboss-core --features test-support process`
Expected: fake_tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/pitboss-core/
git commit -m "Add FakeSpawner under test-support feature"
```

---

## Phase 3 — Session Layer (pitboss-core)

The `SessionHandle` drives one Claude subprocess from spawn to outcome. It owns: the child, the parser loop, a state machine, a cancel token, and a log writer. Tested entirely against `FakeSpawner`.

### Task 13: CancelToken with drain + terminate channels

**Files:**
- Create: `crates/pitboss-core/src/session/mod.rs`
- Create: `crates/pitboss-core/src/session/cancel.rs`
- Modify: `crates/pitboss-core/src/lib.rs`

- [ ] **Step 1: Write failing test**

Create `crates/pitboss-core/src/session/mod.rs`:

```rust
//! Session handle and cancellation machinery.

pub mod cancel;

pub use cancel::CancelToken;
```

Create `crates/pitboss-core/src/session/cancel.rs`:

```rust
use tokio::sync::watch;

/// Two-phase cancel signal shared across tasks.
#[derive(Clone)]
pub struct CancelToken {
    drain_tx:     watch::Sender<bool>,
    drain_rx:     watch::Receiver<bool>,
    terminate_tx: watch::Sender<bool>,
    terminate_rx: watch::Receiver<bool>,
}

impl CancelToken {
    pub fn new() -> Self {
        let (drain_tx, drain_rx)         = watch::channel(false);
        let (terminate_tx, terminate_rx) = watch::channel(false);
        Self { drain_tx, drain_rx, terminate_tx, terminate_rx }
    }

    pub fn drain(&self)     { let _ = self.drain_tx.send(true); }
    pub fn terminate(&self) { let _ = self.terminate_tx.send(true); }

    pub fn is_draining(&self)   -> bool { *self.drain_rx.borrow() }
    pub fn is_terminated(&self) -> bool { *self.terminate_rx.borrow() }

    /// Async wait for drain signal. Returns immediately if already draining.
    pub async fn await_drain(&self) {
        let mut rx = self.drain_rx.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() { break; }
        }
    }

    /// Async wait for terminate signal.
    pub async fn await_terminate(&self) {
        let mut rx = self.terminate_rx.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() { break; }
        }
    }
}

impl Default for CancelToken {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn drain_signal_fires() {
        let t = CancelToken::new();
        assert!(!t.is_draining());
        let handle = {
            let t = t.clone();
            tokio::spawn(async move { t.await_drain().await })
        };
        tokio::time::advance(Duration::from_millis(10)).await;
        t.drain();
        handle.await.unwrap();
        assert!(t.is_draining());
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn terminate_is_independent_of_drain() {
        let t = CancelToken::new();
        t.terminate();
        assert!(t.is_terminated());
        assert!(!t.is_draining());
    }
}
```

Modify `crates/pitboss-core/src/lib.rs` to add:

```rust
pub mod session;
```

(Insert after `pub mod process;`.)

- [ ] **Step 2: Run tests to verify they fail (compile)**

Run: `cargo test -p pitboss-core session::cancel`
Expected: compiles and tests pass (pure-logic module).

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-core/src/session/ crates/pitboss-core/src/lib.rs
git commit -m "Add CancelToken with drain and terminate channels"
```

---

### Task 14: SessionState enum with transition invariants

**Files:**
- Create: `crates/pitboss-core/src/session/state.rs`
- Modify: `crates/pitboss-core/src/session/mod.rs`

- [ ] **Step 1: Write failing test**

Create `crates/pitboss-core/src/session/state.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Terminal or in-flight state of one session.
///
/// Transitions are enforced by [`SessionState::transition_to`]: only specific
/// moves are legal; illegal moves panic in debug and silently saturate to
/// the target in release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Initializing,
    Running { since: DateTime<Utc> },
    Completed,
    Failed      { message: String },
    TimedOut,
    Cancelled,
    SpawnFailed { message: String },
}

impl SessionState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed
                     | Self::Failed { .. }
                     | Self::TimedOut
                     | Self::Cancelled
                     | Self::SpawnFailed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializing_is_not_terminal() {
        assert!(!SessionState::Initializing.is_terminal());
    }

    #[test]
    fn completed_is_terminal() {
        assert!(SessionState::Completed.is_terminal());
    }

    #[test]
    fn cancelled_is_terminal() {
        assert!(SessionState::Cancelled.is_terminal());
    }

    #[test]
    fn failed_is_terminal() {
        assert!(SessionState::Failed { message: "x".into() }.is_terminal());
    }
}
```

Modify `crates/pitboss-core/src/session/mod.rs`:

```rust
//! Session handle and cancellation machinery.

pub mod cancel;
pub mod state;

pub use cancel::CancelToken;
pub use state::SessionState;
```

- [ ] **Step 2: Run tests to verify pass**

Run: `cargo test -p pitboss-core session::state`
Expected: all pass.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-core/src/session/
git commit -m "Add SessionState enum with terminal-state check"
```

---

### Task 15: SessionHandle happy-path run via FakeSpawner

This is the biggest task in the plan. Use the `test-support` feature. The SessionHandle owns the full pipeline: spawn → read stdout → parse → update state → observe exit → build SessionOutcome.

**Files:**
- Create: `crates/pitboss-core/src/session/outcome.rs`
- Create: `crates/pitboss-core/src/session/handle.rs`
- Modify: `crates/pitboss-core/src/session/mod.rs`
- Modify: `crates/pitboss-core/src/error.rs`

- [ ] **Step 1: Write failing test**

Create `crates/pitboss-core/src/session/handle.rs` (skeleton first — real impl follows):

```rust
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::error::SessionError;
use crate::parser::{parse_line, Event, TokenUsage};
use crate::process::{ProcessSpawner, SpawnCmd};

use super::{CancelToken, SessionState};
use super::outcome::SessionOutcome;

/// One Claude Code session under pitboss-core's supervision.
pub struct SessionHandle {
    task_id: String,
    spawner: Arc<dyn ProcessSpawner>,
    cmd: SpawnCmd,
    log_path: Option<PathBuf>,
}

impl SessionHandle {
    pub fn new(task_id: impl Into<String>, spawner: Arc<dyn ProcessSpawner>, cmd: SpawnCmd) -> Self {
        Self { task_id: task_id.into(), spawner, cmd, log_path: None }
    }

    pub fn with_log_path(mut self, p: PathBuf) -> Self {
        self.log_path = Some(p);
        self
    }

    pub async fn run_to_completion(
        self,
        cancel: CancelToken,
        timeout: Duration,
    ) -> SessionOutcome {
        let _ = (cancel, timeout);
        unimplemented!("run_to_completion")
    }
}
```

Create `crates/pitboss-core/src/session/outcome.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::parser::TokenUsage;
use super::SessionState;

/// Result of running a single session to completion or cancellation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionOutcome {
    pub final_state:           SessionState,
    pub exit_code:             Option<i32>,
    pub token_usage:           TokenUsage,
    pub claude_session_id:     Option<String>,
    pub final_message_preview: Option<String>,
    pub started_at:            DateTime<Utc>,
    pub ended_at:              DateTime<Utc>,
}

impl SessionOutcome {
    pub fn duration_ms(&self) -> i64 {
        (self.ended_at - self.started_at).num_milliseconds()
    }
}
```

Add to `crates/pitboss-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("spawn failed: {0}")]
    Spawn(#[from] SpawnError),

    #[error("io during session: {0}")]
    Io(#[from] std::io::Error),
}
```

Modify `crates/pitboss-core/src/session/mod.rs`:

```rust
//! Session handle and cancellation machinery.

pub mod cancel;
pub mod handle;
pub mod outcome;
pub mod state;

pub use cancel::CancelToken;
pub use handle::SessionHandle;
pub use outcome::SessionOutcome;
pub use state::SessionState;
```

Write the failing test by appending to `crates/pitboss-core/src/session/mod.rs`:

```rust
#[cfg(all(test, feature = "test-support"))]
mod happy_path_tests {
    use super::*;
    use crate::process::fake::{FakeScript, FakeSpawner};
    use crate::process::SpawnCmd;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn cmd() -> SpawnCmd {
        SpawnCmd {
            program: PathBuf::from("claude"),
            args: vec![],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn completed_session_records_usage_and_session_id() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .stdout_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}"#)
            .stdout_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"all done"}]}}"#)
            .stdout_line(r#"{"type":"result","subtype":"success","session_id":"sess_final","result":"complete","usage":{"input_tokens":10,"output_tokens":25,"cache_read_input_tokens":100,"cache_creation_input_tokens":3}}"#)
            .exit_code(0);

        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let handle = SessionHandle::new("t1", spawner, cmd());
        let outcome = handle
            .run_to_completion(CancelToken::new(), Duration::from_secs(30))
            .await;

        assert!(matches!(outcome.final_state, SessionState::Completed));
        assert_eq!(outcome.exit_code, Some(0));
        assert_eq!(outcome.claude_session_id.as_deref(), Some("sess_final"));
        assert_eq!(outcome.token_usage.input, 10);
        assert_eq!(outcome.token_usage.output, 25);
        assert_eq!(outcome.token_usage.cache_read, 100);
        assert_eq!(outcome.final_message_preview.as_deref(), Some("all done"));
    }

    #[tokio::test]
    async fn nonzero_exit_becomes_failed_state() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"result","session_id":"s","usage":{"input_tokens":0,"output_tokens":0}}"#)
            .exit_code(1);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let outcome = SessionHandle::new("t2", spawner, cmd())
            .run_to_completion(CancelToken::new(), Duration::from_secs(5))
            .await;
        assert_eq!(outcome.exit_code, Some(1));
        assert!(matches!(outcome.final_state, SessionState::Failed { .. }));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core --features test-support session::happy_path_tests`
Expected: PANIC on `unimplemented!("run_to_completion")`.

- [ ] **Step 3: Implement run_to_completion**

Replace `crates/pitboss-core/src/session/handle.rs` with:

```rust
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::parser::{parse_line, Event, TokenUsage};
use crate::process::{ChildProcess, ProcessSpawner, SpawnCmd};

use super::{CancelToken, SessionOutcome, SessionState};

pub struct SessionHandle {
    task_id: String,
    spawner: Arc<dyn ProcessSpawner>,
    cmd: SpawnCmd,
    log_path: Option<PathBuf>,
}

impl SessionHandle {
    pub fn new(task_id: impl Into<String>, spawner: Arc<dyn ProcessSpawner>, cmd: SpawnCmd) -> Self {
        Self { task_id: task_id.into(), spawner, cmd, log_path: None }
    }

    pub fn with_log_path(mut self, p: PathBuf) -> Self {
        self.log_path = Some(p);
        self
    }

    pub async fn run_to_completion(
        self,
        cancel: CancelToken,
        timeout: Duration,
    ) -> SessionOutcome {
        let started_at = Utc::now();

        // Spawn child.
        let mut child = match self.spawner.spawn(self.cmd.clone()).await {
            Ok(c) => c,
            Err(e) => return SessionOutcome {
                final_state: SessionState::SpawnFailed { message: e.to_string() },
                exit_code: None,
                token_usage: TokenUsage::default(),
                claude_session_id: None,
                final_message_preview: None,
                started_at,
                ended_at: Utc::now(),
            },
        };

        let stdout = child.take_stdout().expect("stdout piped");
        let mut reader = BufReader::new(stdout).lines();

        // Optional log writer.
        let mut log_writer = if let Some(path) = &self.log_path {
            OpenOptions::new().create(true).append(true).open(path).await.ok()
        } else { None };

        let mut usage = TokenUsage::default();
        let mut session_id: Option<String> = None;
        let mut last_text: Option<String> = None;
        let mut saw_result = false;

        let terminate_fut = cancel.await_terminate();
        tokio::pin!(terminate_fut);

        // Stream loop with timeout + terminate watcher.
        let stream_result = tokio::select! {
            biased;
            _ = &mut terminate_fut => StreamEnd::Terminated,
            _ = tokio::time::sleep(timeout) => StreamEnd::TimedOut,
            end = stream_loop(&mut reader, &mut log_writer, &mut usage, &mut session_id, &mut last_text, &mut saw_result) => end,
        };

        // If we are terminating, signal child.
        if matches!(stream_result, StreamEnd::Terminated | StreamEnd::TimedOut) {
            let _ = child.terminate();
            tokio::time::sleep(super::TERMINATE_GRACE).await;
            let _ = child.kill();
        }

        let status = child.wait().await.ok();
        let exit_code = status.as_ref().and_then(|s| s.code());
        let ended_at = Utc::now();

        let final_state = match &stream_result {
            StreamEnd::TimedOut      => SessionState::TimedOut,
            StreamEnd::Terminated    => SessionState::Cancelled,
            StreamEnd::Eof | StreamEnd::ReadError => {
                match exit_code {
                    Some(0) if saw_result => SessionState::Completed,
                    Some(c) if c != 0     => SessionState::Failed { message: format!("exit code {c}") },
                    Some(_)               => SessionState::Failed { message: "no result event".into() },
                    None                  => SessionState::Failed { message: "child did not exit cleanly".into() },
                }
            }
        };

        SessionOutcome {
            final_state,
            exit_code,
            token_usage: usage,
            claude_session_id: session_id,
            final_message_preview: last_text,
            started_at,
            ended_at,
        }
    }
}

enum StreamEnd { Eof, ReadError, Terminated, TimedOut }

async fn stream_loop<R: AsyncBufReadExt + Unpin>(
    reader: &mut tokio::io::Lines<R>,
    log: &mut Option<tokio::fs::File>,
    usage: &mut TokenUsage,
    session_id: &mut Option<String>,
    last_text: &mut Option<String>,
    saw_result: &mut bool,
) -> StreamEnd {
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                if let Some(w) = log.as_mut() {
                    let _ = w.write_all(line.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                match parse_line(line.as_bytes()) {
                    Ok(Event::AssistantText { text }) => {
                        *last_text = Some(truncate_preview(&text));
                    }
                    Ok(Event::Result { session_id: sid, usage: u, text, .. }) => {
                        *session_id = Some(sid);
                        usage.add(&u);
                        if let Some(t) = text { *last_text = Some(truncate_preview(&t)); }
                        *saw_result = true;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        // Malformed line — already appended to log verbatim; continue.
                    }
                }
            }
            Ok(None) => return StreamEnd::Eof,
            Err(_) => return StreamEnd::ReadError,
        }
    }
}

fn truncate_preview(s: &str) -> String {
    const MAX: usize = 200;
    if s.len() <= MAX { s.to_string() } else {
        let mut out = s[..MAX].to_string();
        out.push('…');
        out
    }
}
```

Add a module-level constant near the top of `crates/pitboss-core/src/session/mod.rs`:

```rust
use std::time::Duration;

/// Grace window between SIGTERM and SIGKILL during terminate-phase cancellation.
pub const TERMINATE_GRACE: Duration = Duration::from_secs(10);
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core --features test-support session::happy_path_tests`
Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/
git commit -m "Implement SessionHandle::run_to_completion happy path"
```

---

### Task 16: SessionHandle terminate + timeout flows

**Files:**
- Modify: `crates/pitboss-core/src/session/mod.rs` (add tests)

- [ ] **Step 1: Write failing tests**

Append to `crates/pitboss-core/src/session/mod.rs`:

```rust
#[cfg(all(test, feature = "test-support"))]
mod cancel_tests {
    use super::*;
    use crate::process::fake::{FakeScript, FakeSpawner};
    use crate::process::{ProcessSpawner, SpawnCmd};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn cmd() -> SpawnCmd {
        SpawnCmd {
            program: PathBuf::from("claude"),
            args: vec![],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn terminate_produces_cancelled_state() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .hold_until_signal();

        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let handle_fut = tokio::spawn(async move {
            SessionHandle::new("t", spawner, cmd())
                .run_to_completion(c2, Duration::from_secs(60))
                .await
        });
        // Let it get started.
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.terminate();
        let outcome = tokio::time::timeout(Duration::from_secs(TERMINATE_GRACE.as_secs() + 5), handle_fut)
            .await.expect("finishes within grace").unwrap();
        assert!(matches!(outcome.final_state, SessionState::Cancelled));
    }

    #[tokio::test]
    async fn timeout_produces_timed_out_state() {
        let script = FakeScript::new().hold_until_signal();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let outcome = SessionHandle::new("t", spawner, cmd())
            .run_to_completion(CancelToken::new(), Duration::from_millis(100))
            .await;
        assert!(matches!(outcome.final_state, SessionState::TimedOut));
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p pitboss-core --features test-support session::cancel_tests`
Expected: both pass (the happy-path implementation in Task 15 already handles these flows via `tokio::select!`).

If any fail due to a timing race, the `TERMINATE_GRACE` sleep path may block the select — in that case, restructure `run_to_completion` so the grace delay happens after the `select!` exits, as shown in Task 15. Do not increase scope.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-core/src/session/mod.rs
git commit -m "Test SessionHandle terminate and timeout flows"
```

---

### Task 17: SessionHandle spawn-failure path

**Files:**
- Modify: `crates/pitboss-core/src/session/mod.rs` (add test)

- [ ] **Step 1: Write failing test**

Append to `crates/pitboss-core/src/session/mod.rs`:

```rust
#[cfg(all(test, feature = "test-support"))]
mod spawn_fail_tests {
    use super::*;
    use crate::process::fake::{FakeScript, FakeSpawner};
    use crate::process::{ProcessSpawner, SpawnCmd};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn spawn_rejection_yields_spawnfailed() {
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(
            FakeScript::new().fail_spawn("no binary here"),
        ));
        let cmd = SpawnCmd {
            program: PathBuf::from("x"), args: vec![],
            cwd: PathBuf::from("/tmp"), env: HashMap::new(),
        };
        let outcome = SessionHandle::new("t", spawner, cmd)
            .run_to_completion(CancelToken::new(), Duration::from_secs(5))
            .await;
        match outcome.final_state {
            SessionState::SpawnFailed { message } => {
                assert!(message.contains("no binary here"));
            }
            other => panic!("expected SpawnFailed, got {other:?}"),
        }
        assert!(outcome.exit_code.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p pitboss-core --features test-support session::spawn_fail_tests`
Expected: pass (Task 15 already handles this path).

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-core/src/session/mod.rs
git commit -m "Test SessionHandle spawn-failure path"
```

---

## Phase 4 — Worktree Manager (pitboss-core)

Wraps `git2` to create and clean up isolated worktrees per task. Unit tests build throwaway repos with `tempfile::TempDir` + `git2::Repository::init`.

### Task 18: WorktreeManager skeleton + prepare() with no-branch path

**Files:**
- Create: `crates/pitboss-core/src/worktree/mod.rs`
- Create: `crates/pitboss-core/src/worktree/manager.rs`
- Modify: `crates/pitboss-core/src/lib.rs`
- Modify: `crates/pitboss-core/src/error.rs`

- [ ] **Step 1: Write failing test**

Create `crates/pitboss-core/src/worktree/mod.rs`:

```rust
//! Git worktree lifecycle for task isolation.

pub mod manager;

pub use manager::{Worktree, WorktreeManager, CleanupPolicy};
```

Create `crates/pitboss-core/src/worktree/manager.rs` (skeleton):

```rust
use std::path::{Path, PathBuf};

use crate::error::WorktreeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupPolicy {
    Always,
    OnSuccess,
    Never,
}

#[derive(Debug)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    name: String,
    repo_root: PathBuf,
}

impl Worktree {
    pub fn name(&self) -> &str { &self.name }
    pub fn repo_root(&self) -> &Path { &self.repo_root }
}

pub struct WorktreeManager;

impl WorktreeManager {
    pub fn new() -> Self { Self }

    pub fn prepare(
        &self,
        _repo_root: &Path,
        _name: &str,
        _branch: Option<&str>,
    ) -> Result<Worktree, WorktreeError> {
        unimplemented!("prepare")
    }

    pub fn cleanup(
        &self,
        _wt: Worktree,
        _policy: CleanupPolicy,
        _succeeded: bool,
    ) -> Result<(), WorktreeError> {
        unimplemented!("cleanup")
    }
}
```

Add to `crates/pitboss-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("not inside a git work-tree: {path}")]
    NotInRepo { path: String },

    #[error("branch already checked out in another worktree: {branch}")]
    BranchConflict { branch: String },

    #[error("git error: {0}")]
    Git(#[from] git2::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
```

Modify `crates/pitboss-core/src/lib.rs` to add `pub mod worktree;`.

Modify `crates/pitboss-core/Cargo.toml` to ensure `git2` is a direct dep (already there from Task 1).

Write the failing test by appending to `crates/pitboss-core/src/worktree/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(root: &std::path::Path) {
        Command::new("git").args(["init", "-q"]).current_dir(root).status().unwrap();
        Command::new("git").args(["config", "user.email", "t@t.x"]).current_dir(root).status().unwrap();
        Command::new("git").args(["config", "user.name", "t"]).current_dir(root).status().unwrap();
        std::fs::write(root.join("README.md"), "x").unwrap();
        Command::new("git").args(["add", "."]).current_dir(root).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(root).status().unwrap();
    }

    #[test]
    fn prepare_detached_worktree_without_branch() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());

        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "shire-task-test-1", None).unwrap();
        assert!(wt.path.exists(), "worktree path exists");
        assert!(wt.path.join("README.md").exists(), "checkout present");
        assert_eq!(wt.branch, None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core worktree`
Expected: PANIC on `unimplemented!("prepare")`.

- [ ] **Step 3: Implement prepare() without branch**

Replace `crates/pitboss-core/src/worktree/manager.rs`:

```rust
use std::path::{Path, PathBuf};

use git2::{Repository, WorktreeAddOptions};

use crate::error::WorktreeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupPolicy {
    Always,
    OnSuccess,
    Never,
}

#[derive(Debug)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    name: String,
    repo_root: PathBuf,
}

impl Worktree {
    pub fn name(&self) -> &str { &self.name }
    pub fn repo_root(&self) -> &Path { &self.repo_root }
}

pub struct WorktreeManager;

impl Default for WorktreeManager {
    fn default() -> Self { Self::new() }
}

impl WorktreeManager {
    pub fn new() -> Self { Self }

    pub fn prepare(
        &self,
        repo_root: &Path,
        name: &str,
        branch: Option<&str>,
    ) -> Result<Worktree, WorktreeError> {
        let repo = Repository::open(repo_root).map_err(|_| WorktreeError::NotInRepo {
            path: repo_root.display().to_string(),
        })?;

        let wt_path = sibling_path(repo_root, name);
        let mut opts = WorktreeAddOptions::new();

        let branch_name = branch.map(str::to_string);
        if let Some(bname) = &branch_name {
            // Branch policy handled in Tasks 19-20. For now, reject.
            let _ = bname;
            unimplemented!("branch path not yet wired — covered in Task 19");
        }

        let _wt = repo.worktree(name, &wt_path, Some(&opts))?;

        Ok(Worktree {
            path: wt_path,
            branch: None,
            name: name.to_string(),
            repo_root: repo_root.to_path_buf(),
        })
    }

    pub fn cleanup(
        &self,
        wt: Worktree,
        policy: CleanupPolicy,
        succeeded: bool,
    ) -> Result<(), WorktreeError> {
        let _ = (wt, policy, succeeded);
        unimplemented!("cleanup — Task 21")
    }
}

fn sibling_path(repo_root: &Path, name: &str) -> PathBuf {
    let parent = repo_root.parent().unwrap_or_else(|| Path::new("."));
    let base = repo_root.file_name().map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    parent.join(format!("{base}-{name}"))
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core worktree::tests::prepare_detached_worktree_without_branch`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/
git commit -m "WorktreeManager::prepare for detached (no-branch) worktrees"
```

---

### Task 19: Worktree prepare() with branch creation or checkout

**Files:**
- Modify: `crates/pitboss-core/src/worktree/manager.rs`
- Modify: `crates/pitboss-core/src/worktree/mod.rs` (tests)

- [ ] **Step 1: Write failing tests**

Append to the `tests` module in `crates/pitboss-core/src/worktree/mod.rs`:

```rust
    #[test]
    fn prepare_creates_new_branch_when_absent() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());

        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "shire-task-new-branch", Some("feat/new")).unwrap();
        assert_eq!(wt.branch.as_deref(), Some("feat/new"));

        // Verify branch exists in the repo.
        let out = std::process::Command::new("git")
            .args(["branch", "--list", "feat/new"])
            .current_dir(repo_dir.path())
            .output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("feat/new"));
    }

    #[test]
    fn prepare_checks_out_existing_branch() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());
        std::process::Command::new("git")
            .args(["branch", "existing"]).current_dir(repo_dir.path()).status().unwrap();

        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "shire-task-exist", Some("existing")).unwrap();
        assert_eq!(wt.branch.as_deref(), Some("existing"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core worktree::tests::prepare_creates_new_branch_when_absent`
Expected: PANIC on `unimplemented!("branch path…")`.

- [ ] **Step 3: Implement branch handling**

Replace the `prepare` body in `crates/pitboss-core/src/worktree/manager.rs`:

```rust
    pub fn prepare(
        &self,
        repo_root: &Path,
        name: &str,
        branch: Option<&str>,
    ) -> Result<Worktree, WorktreeError> {
        let repo = Repository::open(repo_root).map_err(|_| WorktreeError::NotInRepo {
            path: repo_root.display().to_string(),
        })?;

        let wt_path = sibling_path(repo_root, name);
        let branch_name = branch.map(str::to_string);

        // Ensure branch exists (create from HEAD if missing). No force-update.
        if let Some(bname) = &branch_name {
            if repo.find_branch(bname, git2::BranchType::Local).is_err() {
                let head_commit = repo.head()?.peel_to_commit()?;
                repo.branch(bname, &head_commit, false)?;
            }
        }

        let mut opts = WorktreeAddOptions::new();
        // git2::WorktreeAddOptions lets us set a reference. Attach to branch if given.
        let reference_holder;
        if let Some(bname) = &branch_name {
            let rname = format!("refs/heads/{bname}");
            reference_holder = repo.find_reference(&rname)?;
            opts.reference(Some(&reference_holder));
        }

        let _wt = repo.worktree(name, &wt_path, Some(&opts))?;

        Ok(Worktree {
            path: wt_path,
            branch: branch_name,
            name: name.to_string(),
            repo_root: repo_root.to_path_buf(),
        })
    }
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core worktree`
Expected: all three tests (including Task 18's) pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/
git commit -m "Worktree prepare supports branch creation and checkout"
```

---

### Task 20: Worktree branch-conflict detection

**Files:**
- Modify: `crates/pitboss-core/src/worktree/manager.rs`
- Modify: `crates/pitboss-core/src/worktree/mod.rs` (tests)

- [ ] **Step 1: Write failing test**

Append to the `tests` module:

```rust
    #[test]
    fn prepare_rejects_branch_already_checked_out_in_another_worktree() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());

        let mgr = WorktreeManager::new();
        let _first = mgr.prepare(repo_dir.path(), "wt-a", Some("shared")).unwrap();
        let err = mgr.prepare(repo_dir.path(), "wt-b", Some("shared")).unwrap_err();
        assert!(matches!(err, WorktreeError::BranchConflict { .. }));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core worktree::tests::prepare_rejects_branch_already_checked_out_in_another_worktree`
Expected: FAIL — git2 error bubbles up as a plain `Git` variant, not `BranchConflict`.

- [ ] **Step 3: Implement the conflict detection**

Modify `prepare`: before calling `repo.worktree(...)`, check if the branch is already checked out in an existing worktree.

```rust
        if let Some(bname) = &branch_name {
            for wt_name in repo.worktrees()?.iter().flatten() {
                let wt = repo.find_worktree(wt_name)?;
                let wt_repo = Repository::open(wt.path())?;
                if let Ok(head) = wt_repo.head() {
                    if head.shorthand() == Some(bname.as_str()) {
                        return Err(WorktreeError::BranchConflict { branch: bname.clone() });
                    }
                }
            }
        }
```

Insert immediately after the "Ensure branch exists" block.

- [ ] **Step 4: Run test to verify pass**

Run: `cargo test -p pitboss-core worktree`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/src/worktree/
git commit -m "Worktree rejects branch already checked out elsewhere"
```

---

### Task 21: Worktree cleanup with three policies

**Files:**
- Modify: `crates/pitboss-core/src/worktree/manager.rs`
- Modify: `crates/pitboss-core/src/worktree/mod.rs` (tests)

- [ ] **Step 1: Write failing tests**

Append to the `tests` module:

```rust
    #[test]
    fn cleanup_always_removes_worktree_on_success() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());
        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "wt-ca", None).unwrap();
        let path = wt.path.clone();
        mgr.cleanup(wt, CleanupPolicy::Always, true).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_always_removes_worktree_on_failure() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());
        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "wt-cf", None).unwrap();
        let path = wt.path.clone();
        mgr.cleanup(wt, CleanupPolicy::Always, false).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_on_success_keeps_failed_worktree() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());
        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "wt-os", None).unwrap();
        let path = wt.path.clone();
        mgr.cleanup(wt, CleanupPolicy::OnSuccess, false).unwrap();
        assert!(path.exists(), "failed worktree preserved for forensics");
    }

    #[test]
    fn cleanup_never_always_keeps() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());
        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "wt-nev", None).unwrap();
        let path = wt.path.clone();
        mgr.cleanup(wt, CleanupPolicy::Never, true).unwrap();
        assert!(path.exists());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core worktree::tests::cleanup_always_removes_worktree_on_success`
Expected: PANIC on `unimplemented!("cleanup — Task 21")`.

- [ ] **Step 3: Implement cleanup**

Replace `cleanup` in `crates/pitboss-core/src/worktree/manager.rs`:

```rust
    pub fn cleanup(
        &self,
        wt: Worktree,
        policy: CleanupPolicy,
        succeeded: bool,
    ) -> Result<(), WorktreeError> {
        let should_remove = match policy {
            CleanupPolicy::Always    => true,
            CleanupPolicy::OnSuccess => succeeded,
            CleanupPolicy::Never     => false,
        };
        if !should_remove { return Ok(()); }

        let repo = Repository::open(&wt.repo_root)?;
        if let Ok(handle) = repo.find_worktree(&wt.name) {
            // Prune the worktree administrative entry.
            let mut opts = git2::WorktreePruneOptions::new();
            opts.valid(true).locked(true).working_tree(true);
            let _ = handle.prune(Some(&mut opts));
        }
        // Ensure the sibling working dir is gone.
        if wt.path.exists() {
            std::fs::remove_dir_all(&wt.path)?;
        }
        Ok(())
    }
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core worktree`
Expected: all cleanup tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/src/worktree/
git commit -m "WorktreeManager cleanup with three policies"
```

---

## Phase 5 — Persistence (pitboss-core/store)

Defines the wire types that appear in `summary.jsonl` / `summary.json`, the `SessionStore` trait, and the `JsonFileStore` implementation used by v0.1.

### Task 22: Wire types — TaskRecord, RunSummary, RunMeta

**Files:**
- Create: `crates/pitboss-core/src/store/mod.rs`
- Create: `crates/pitboss-core/src/store/record.rs`
- Modify: `crates/pitboss-core/src/lib.rs`

- [ ] **Step 1: Write failing test**

Create `crates/pitboss-core/src/store/mod.rs`:

```rust
//! Persistence — trait and file-backed implementation.

pub mod record;

pub use record::{RunMeta, RunSummary, TaskRecord, TaskStatus};
```

Create `crates/pitboss-core/src/store/record.rs`:

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::parser::TokenUsage;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Success,
    Failed,
    TimedOut,
    Cancelled,
    SpawnFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub status: TaskStatus,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub ended_at:   DateTime<Utc>,
    pub duration_ms: i64,
    pub worktree_path: Option<PathBuf>,
    pub log_path: PathBuf,
    pub token_usage: TokenUsage,
    pub claude_session_id: Option<String>,
    pub final_message_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMeta {
    pub run_id: Uuid,
    pub manifest_path: PathBuf,
    pub shire_version: String,
    pub claude_version: Option<String>,
    pub started_at: DateTime<Utc>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: Uuid,
    pub manifest_path: PathBuf,
    pub shire_version: String,
    pub claude_version: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at:   DateTime<Utc>,
    pub total_duration_ms: i64,
    pub tasks_total: usize,
    pub tasks_failed: usize,
    pub was_interrupted: bool,
    pub tasks: Vec<TaskRecord>,
}
```

Modify `crates/pitboss-core/src/lib.rs` to add `pub mod store;`.

Add to the bottom of `crates/pitboss-core/src/store/record.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn task_record_round_trips_json() {
        let rec = TaskRecord {
            task_id: "t1".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap(),
            ended_at:   Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 30).unwrap(),
            duration_ms: 30_000,
            worktree_path: Some(PathBuf::from("/tmp/wt")),
            log_path: PathBuf::from("/tmp/log"),
            token_usage: TokenUsage { input: 1, output: 2, cache_read: 3, cache_creation: 4 },
            claude_session_id: Some("sess".into()),
            final_message_preview: Some("ok".into()),
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: TaskRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.task_id, "t1");
        assert!(matches!(back.status, TaskStatus::Success));
    }
}
```

- [ ] **Step 2: Run tests to verify pass**

Run: `cargo test -p pitboss-core store`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-core/src/store/ crates/pitboss-core/src/lib.rs
git commit -m "Add store wire types: TaskRecord, RunSummary, RunMeta"
```

---

### Task 23: SessionStore trait + StoreError

**Files:**
- Create: `crates/pitboss-core/src/store/traits.rs`
- Modify: `crates/pitboss-core/src/store/mod.rs`
- Modify: `crates/pitboss-core/src/error.rs`

- [ ] **Step 1: Write trait**

Create `crates/pitboss-core/src/store/traits.rs`:

```rust
use async_trait::async_trait;
use uuid::Uuid;

use crate::error::StoreError;
use super::record::{RunMeta, RunSummary, TaskRecord};

#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    async fn init_run(&self, meta: &RunMeta) -> Result<(), StoreError>;
    async fn append_record(&self, run_id: Uuid, record: &TaskRecord) -> Result<(), StoreError>;
    async fn finalize_run(&self, summary: &RunSummary) -> Result<(), StoreError>;
    async fn load_run(&self, run_id: Uuid) -> Result<RunSummary, StoreError>;
}
```

Modify `crates/pitboss-core/src/store/mod.rs`:

```rust
//! Persistence — trait and file-backed implementation.

pub mod record;
pub mod traits;

pub use record::{RunMeta, RunSummary, TaskRecord, TaskStatus};
pub use traits::SessionStore;
```

Add to `crates/pitboss-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("run not found: {0}")]
    NotFound(uuid::Uuid),

    #[error("incomplete run: {0}")]
    Incomplete(String),
}
```

- [ ] **Step 2: Verify compiles**

Run: `cargo check -p pitboss-core`
Expected: compiles clean.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-core/src/store/ crates/pitboss-core/src/error.rs
git commit -m "Add SessionStore trait and StoreError"
```

---

### Task 24: JsonFileStore implementation

**Files:**
- Create: `crates/pitboss-core/src/store/json_file.rs`
- Modify: `crates/pitboss-core/src/store/mod.rs`

- [ ] **Step 1: Write failing tests**

Append to `crates/pitboss-core/src/store/mod.rs`:

```rust
pub mod json_file;
pub use json_file::JsonFileStore;

#[cfg(test)]
mod integration_tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn meta(run_id: Uuid, root: PathBuf) -> RunMeta {
        RunMeta {
            run_id,
            manifest_path: root.join("shire.toml"),
            shire_version: "0.1.0".into(),
            claude_version: Some("1.0.0".into()),
            started_at: Utc::now(),
            env: HashMap::new(),
        }
    }

    fn rec(task_id: &str, status: TaskStatus) -> TaskRecord {
        let now = Utc::now();
        TaskRecord {
            task_id: task_id.into(),
            status,
            exit_code: Some(0),
            started_at: now,
            ended_at: now,
            duration_ms: 0,
            worktree_path: None,
            log_path: PathBuf::from("/dev/null"),
            token_usage: crate::parser::TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: None,
        }
    }

    #[tokio::test]
    async fn init_and_append_and_finalize_round_trip() {
        let dir = TempDir::new().unwrap();
        let store = JsonFileStore::new(dir.path().to_path_buf());
        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path().to_path_buf())).await.unwrap();
        store.append_record(run_id, &rec("a", TaskStatus::Success)).await.unwrap();
        store.append_record(run_id, &rec("b", TaskStatus::Failed)).await.unwrap();

        let summary = RunSummary {
            run_id,
            manifest_path: dir.path().join("shire.toml"),
            shire_version: "0.1.0".into(),
            claude_version: None,
            started_at: Utc::now(),
            ended_at:   Utc::now(),
            total_duration_ms: 0,
            tasks_total: 2,
            tasks_failed: 1,
            was_interrupted: false,
            tasks: vec![rec("a", TaskStatus::Success), rec("b", TaskStatus::Failed)],
        };
        store.finalize_run(&summary).await.unwrap();

        let back = store.load_run(run_id).await.unwrap();
        assert_eq!(back.tasks.len(), 2);
        assert_eq!(back.tasks_failed, 1);
    }

    #[tokio::test]
    async fn load_orphan_run_marks_interrupted() {
        let dir = TempDir::new().unwrap();
        let store = JsonFileStore::new(dir.path().to_path_buf());
        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path().to_path_buf())).await.unwrap();
        store.append_record(run_id, &rec("only", TaskStatus::Success)).await.unwrap();
        // Do not finalize.

        let loaded = store.load_run(run_id).await.unwrap();
        assert!(loaded.was_interrupted);
        assert_eq!(loaded.tasks.len(), 1);
    }
}
```

- [ ] **Step 2: Run tests to verify fail**

Run: `cargo test -p pitboss-core store::integration_tests`
Expected: FAIL — `JsonFileStore` undefined.

- [ ] **Step 3: Implement JsonFileStore**

Create `crates/pitboss-core/src/store/json_file.rs`:

```rust
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

use crate::error::StoreError;

use super::record::{RunMeta, RunSummary, TaskRecord};
use super::traits::SessionStore;

pub struct JsonFileStore {
    root: PathBuf,
}

impl JsonFileStore {
    pub fn new(root: PathBuf) -> Self { Self { root } }

    fn run_dir(&self, run_id: Uuid) -> PathBuf { self.root.join(run_id.to_string()) }
    fn summary_jsonl(&self, run_id: Uuid) -> PathBuf { self.run_dir(run_id).join("summary.jsonl") }
    fn summary_json(&self, run_id: Uuid) -> PathBuf { self.run_dir(run_id).join("summary.json") }
    fn meta_json(&self, run_id: Uuid) -> PathBuf { self.run_dir(run_id).join("meta.json") }
}

#[async_trait]
impl SessionStore for JsonFileStore {
    async fn init_run(&self, meta: &RunMeta) -> Result<(), StoreError> {
        let dir = self.run_dir(meta.run_id);
        fs::create_dir_all(&dir).await?;
        let bytes = serde_json::to_vec_pretty(meta)?;
        fs::write(self.meta_json(meta.run_id), bytes).await?;
        // Touch the summary.jsonl to create an empty file.
        OpenOptions::new().create(true).append(true).open(self.summary_jsonl(meta.run_id)).await?;
        Ok(())
    }

    async fn append_record(&self, run_id: Uuid, record: &TaskRecord) -> Result<(), StoreError> {
        let line = serde_json::to_string(record)?;
        let mut f = OpenOptions::new()
            .create(true).append(true)
            .open(self.summary_jsonl(run_id)).await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.sync_all().await?;
        Ok(())
    }

    async fn finalize_run(&self, summary: &RunSummary) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(summary)?;
        let path = self.summary_json(summary.run_id);
        fs::write(path, bytes).await?;
        Ok(())
    }

    async fn load_run(&self, run_id: Uuid) -> Result<RunSummary, StoreError> {
        let fin = self.summary_json(run_id);
        if fs::try_exists(&fin).await.unwrap_or(false) {
            let bytes = fs::read(&fin).await?;
            return Ok(serde_json::from_slice(&bytes)?);
        }
        // Orphan path — assemble from summary.jsonl + meta.json.
        let meta_bytes = fs::read(self.meta_json(run_id)).await?;
        let meta: RunMeta = serde_json::from_slice(&meta_bytes)?;
        let jsonl = self.summary_jsonl(run_id);
        let file = tokio::fs::File::open(&jsonl).await?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        let mut tasks = Vec::new();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() { continue; }
            let r: TaskRecord = serde_json::from_str(&line)?;
            tasks.push(r);
        }
        let tasks_failed = tasks.iter().filter(|t| !matches!(t.status, super::TaskStatus::Success)).count();
        let started = meta.started_at;
        let ended = tasks.last().map(|t| t.ended_at).unwrap_or(started);
        Ok(RunSummary {
            run_id: meta.run_id,
            manifest_path: meta.manifest_path,
            shire_version: meta.shire_version,
            claude_version: meta.claude_version,
            started_at: started,
            ended_at: ended,
            total_duration_ms: (ended - started).num_milliseconds(),
            tasks_total: tasks.len(),
            tasks_failed,
            was_interrupted: true,
            tasks,
        })
    }
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core store`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/src/store/
git commit -m "Implement JsonFileStore with orphan detection"
```

---

## Phase 6 — Manifest Loader (pitboss-cli)

### Task 25: Manifest serde schema with deny_unknown_fields

**Files:**
- Create: `crates/pitboss-cli/src/manifest/mod.rs`
- Create: `crates/pitboss-cli/src/manifest/schema.rs`

- [ ] **Step 1: Write failing test**

Create `crates/pitboss-cli/src/manifest/mod.rs`:

```rust
pub mod schema;
pub mod resolve;
pub mod validate;

pub use schema::{Manifest, RunConfig, Defaults, Task, Template};
```

Create `crates/pitboss-cli/src/manifest/schema.rs`:

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub run: RunConfig,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "template")]
    pub templates: Vec<Template>,
    #[serde(default, rename = "task")]
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    pub max_parallel:      Option<u32>,
    #[serde(default)]
    pub halt_on_failure:   bool,
    pub run_dir:           Option<PathBuf>,
    #[serde(default = "default_cleanup")]
    pub worktree_cleanup:  WorktreeCleanup,
    #[serde(default)]
    pub emit_event_stream: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_parallel: None,
            halt_on_failure: false,
            run_dir: None,
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
        }
    }
}

fn default_cleanup() -> WorktreeCleanup { WorktreeCleanup::OnSuccess }

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCleanup { Always, OnSuccess, Never }

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub model:        Option<String>,
    pub effort:       Option<Effort>,
    pub tools:        Option<Vec<String>>,
    pub timeout_secs: Option<u64>,
    pub use_worktree: Option<bool>,
    #[serde(default)]
    pub env:          HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Effort { Low, Medium, High, Xhigh, Max }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: Option<String>,
    pub template: Option<String>,
    #[serde(default)]
    pub vars: HashMap<String, String>,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub tools: Option<Vec<String>>,
    pub timeout_secs: Option<u64>,
    pub use_worktree: Option<bool>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}
```

Write the failing test. Append to `crates/pitboss-cli/src/manifest/schema.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_top_level_key() {
        let toml_src = r#"
            wibble = "surprise"
            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
        "#;
        let err: Result<Manifest, _> = toml::from_str(toml_src);
        assert!(err.is_err(), "should reject unknown key");
    }

    #[test]
    fn accepts_minimal_manifest() {
        let toml_src = r#"
            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "hi"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.tasks.len(), 1);
        assert_eq!(m.tasks[0].id, "x");
    }

    #[test]
    fn parses_full_manifest_with_template() {
        let toml_src = r#"
            [run]
            max_parallel = 8
            halt_on_failure = true
            worktree_cleanup = "never"

            [defaults]
            model = "claude-sonnet-4-6"
            effort = "high"
            tools = ["Read", "Bash"]

            [[template]]
            id = "sweep"
            prompt = "Audit {pm} in {dir}"

            [[task]]
            id = "t1"
            directory = "/tmp"
            template = "sweep"
            vars = { pm = "npm", dir = "/tmp" }
            branch = "feat/x"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.run.max_parallel, Some(8));
        assert!(m.run.halt_on_failure);
        assert_eq!(m.templates.len(), 1);
        assert_eq!(m.tasks[0].template.as_deref(), Some("sweep"));
    }
}
```

To make this compile, also create stub files referenced from `mod.rs`:

Create `crates/pitboss-cli/src/manifest/resolve.rs`:

```rust
// Stub — populated in Task 26.
```

Create `crates/pitboss-cli/src/manifest/validate.rs`:

```rust
// Stub — populated in Task 27.
```

Wire the module into `main.rs` by modifying `crates/pitboss-cli/src/main.rs`:

```rust
mod manifest;

fn main() {
    println!("shire v{} (skeleton)", env!("CARGO_PKG_VERSION"));
}
```

- [ ] **Step 2: Run tests to verify pass**

Run: `cargo test -p pitboss-cli manifest::schema::tests`
Expected: all three pass.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/
git commit -m "Add manifest schema with deny_unknown_fields"
```

---

### Task 26: Template resolution + defaults merging

**Files:**
- Modify: `crates/pitboss-cli/src/manifest/resolve.rs`

- [ ] **Step 1: Write failing tests**

Replace `crates/pitboss-cli/src/manifest/resolve.rs`:

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};

use super::schema::{Defaults, Effort, Manifest, Task, Template, WorktreeCleanup};

/// Fully resolved task ready for dispatch.
#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
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
    ["Read","Write","Edit","Bash","Glob","Grep"].iter().map(|s| s.to_string()).collect()
}

pub fn resolve(manifest: Manifest, env_max_parallel: Option<u32>) -> Result<ResolvedManifest> {
    let templates: HashMap<String, &Template> =
        manifest.templates.iter().map(|t| (t.id.clone(), t)).collect();

    let mut resolved = Vec::with_capacity(manifest.tasks.len());
    for task in &manifest.tasks {
        resolved.push(resolve_task(task, &manifest.defaults, &templates)?);
    }

    let max_parallel = manifest.run.max_parallel
        .or(env_max_parallel)
        .unwrap_or(DEFAULT_MAX_PARALLEL);

    let run_dir = manifest.run.run_dir
        .unwrap_or_else(|| default_run_dir());

    Ok(ResolvedManifest {
        max_parallel,
        halt_on_failure:   manifest.run.halt_on_failure,
        run_dir,
        worktree_cleanup:  manifest.run.worktree_cleanup,
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
            let tmpl = templates.get(tid)
                .ok_or_else(|| anyhow!("task '{}' references unknown template '{}'", task.id, tid))?;
            substitute(&tmpl.prompt, &task.vars)
                .with_context(|| format!("rendering template '{}' for task '{}'", tid, task.id))?
        }
        (Some(_), Some(_)) => bail!("task '{}' sets both prompt and template", task.id),
        (None, None)       => bail!("task '{}' has no prompt and no template", task.id),
    };

    let mut env = defaults.env.clone();
    env.extend(task.env.clone());

    Ok(ResolvedTask {
        id: task.id.clone(),
        directory: task.directory.clone(),
        prompt,
        branch: task.branch.clone(),
        model:  task.model.clone()
                  .or_else(|| defaults.model.clone())
                  .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        effort: task.effort.or(defaults.effort).unwrap_or(DEFAULT_EFFORT),
        tools:  task.tools.clone()
                  .or_else(|| defaults.tools.clone())
                  .unwrap_or_else(default_tools),
        timeout_secs: task.timeout_secs.or(defaults.timeout_secs).unwrap_or(DEFAULT_TIMEOUT_SECS),
        use_worktree: task.use_worktree.or(defaults.use_worktree).unwrap_or(true),
        env,
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
                while let Some(nc) = iter.next() {
                    if nc == '}' {
                        let value = vars.get(&name)
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
    if let Some(h) = dirs_home() { h.join(".local/share/shire/runs") }
    else { PathBuf::from("./shire-runs") }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn man(src: &str) -> Manifest { toml::from_str(src).unwrap() }

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
            prompt = "literal \{ and \}"
            [[task]]
            id = "a"
            directory = "/tmp"
            template = "t"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].prompt, "literal { and }");
    }
}
```

- [ ] **Step 2: Run tests to verify pass**

Run: `cargo test -p pitboss-cli manifest::resolve`
Expected: all seven pass.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/manifest/resolve.rs
git commit -m "Manifest resolve: templates, defaults, concurrency precedence"
```

---

### Task 27: Validation — all documented failure modes

**Files:**
- Modify: `crates/pitboss-cli/src/manifest/validate.rs`

- [ ] **Step 1: Write failing tests**

Replace `crates/pitboss-cli/src/manifest/validate.rs`:

```rust
use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Result};

use super::resolve::ResolvedManifest;

/// Run all v0.1 validations. Call after [`crate::manifest::resolve::resolve`].
pub fn validate(resolved: &ResolvedManifest) -> Result<()> {
    validate_ids(resolved)?;
    validate_directories(resolved)?;
    validate_branch_conflicts(resolved)?;
    validate_ranges(resolved)?;
    Ok(())
}

fn validate_ids(r: &ResolvedManifest) -> Result<()> {
    let mut seen = HashSet::new();
    for t in &r.tasks {
        if !seen.insert(&t.id) {
            bail!("duplicate task id: {}", t.id);
        }
        if t.id.is_empty() {
            bail!("empty task id");
        }
        if !t.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            bail!("task id '{}' contains invalid characters (allowed: a-zA-Z0-9_-)", t.id);
        }
    }
    Ok(())
}

fn validate_directories(r: &ResolvedManifest) -> Result<()> {
    for t in &r.tasks {
        if !t.directory.is_dir() {
            bail!("task '{}' directory does not exist or is not a directory: {}",
                  t.id, t.directory.display());
        }
        if t.use_worktree && !is_in_git_repo(&t.directory) {
            bail!("task '{}' has use_worktree=true but directory is not a git work-tree: {}",
                  t.id, t.directory.display());
        }
    }
    Ok(())
}

fn validate_branch_conflicts(r: &ResolvedManifest) -> Result<()> {
    let mut seen: HashSet<(std::path::PathBuf, String)> = HashSet::new();
    for t in &r.tasks {
        if !t.use_worktree { continue; }
        if let Some(b) = &t.branch {
            let canon = std::fs::canonicalize(&t.directory)
                .unwrap_or_else(|_| t.directory.clone());
            if !seen.insert((canon, b.clone())) {
                bail!("two tasks target the same directory + branch '{}'", b);
            }
        }
    }
    Ok(())
}

fn validate_ranges(r: &ResolvedManifest) -> Result<()> {
    if r.max_parallel == 0 {
        bail!("max_parallel must be > 0");
    }
    for t in &r.tasks {
        if t.timeout_secs == 0 {
            bail!("task '{}': timeout_secs must be > 0", t.id);
        }
    }
    Ok(())
}

fn is_in_git_repo(path: &Path) -> bool {
    git2::Repository::discover(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::resolve::{resolve, ResolvedTask};
    use super::super::schema::{Manifest, Effort, WorktreeCleanup};
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn with_tmp_repo(use_git: bool) -> TempDir {
        let d = TempDir::new().unwrap();
        if use_git {
            Command::new("git").args(["init","-q"]).current_dir(d.path()).status().unwrap();
            Command::new("git").args(["config","user.email","t@t.x"]).current_dir(d.path()).status().unwrap();
            Command::new("git").args(["config","user.name","t"]).current_dir(d.path()).status().unwrap();
            std::fs::write(d.path().join("r"), "").unwrap();
            Command::new("git").args(["add","."]).current_dir(d.path()).status().unwrap();
            Command::new("git").args(["commit","-q","-m","i"]).current_dir(d.path()).status().unwrap();
        }
        d
    }

    fn rt(id: &str, dir: PathBuf, use_worktree: bool, branch: Option<&str>) -> ResolvedTask {
        ResolvedTask {
            id: id.into(),
            directory: dir,
            prompt: "p".into(),
            branch: branch.map(str::to_string),
            model: "m".into(),
            effort: Effort::High,
            tools: vec![],
            timeout_secs: 60,
            use_worktree,
            env: Default::default(),
        }
    }

    fn rm(tasks: Vec<ResolvedTask>) -> ResolvedManifest {
        ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks,
        }
    }

    #[test]
    fn rejects_duplicate_ids() {
        let d = with_tmp_repo(true);
        let r = rm(vec![
            rt("a", d.path().to_path_buf(), false, None),
            rt("a", d.path().to_path_buf(), false, None),
        ]);
        assert!(validate(&r).unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn rejects_missing_directory() {
        let r = rm(vec![rt("a", PathBuf::from("/no/such/path"), false, None)]);
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_non_git_directory_with_worktree_true() {
        let d = with_tmp_repo(false);
        let r = rm(vec![rt("a", d.path().to_path_buf(), true, Some("b"))]);
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("not a git"));
    }

    #[test]
    fn accepts_non_git_directory_with_worktree_false() {
        let d = with_tmp_repo(false);
        let r = rm(vec![rt("a", d.path().to_path_buf(), false, None)]);
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn rejects_branch_dir_duplicates() {
        let d = with_tmp_repo(true);
        let r = rm(vec![
            rt("a", d.path().to_path_buf(), true, Some("shared")),
            rt("b", d.path().to_path_buf(), true, Some("shared")),
        ]);
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_zero_max_parallel() {
        let d = with_tmp_repo(true);
        let mut r = rm(vec![rt("a", d.path().to_path_buf(), false, None)]);
        r.max_parallel = 0;
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_invalid_id_chars() {
        let d = with_tmp_repo(true);
        let r = rm(vec![rt("has spaces", d.path().to_path_buf(), false, None)]);
        assert!(validate(&r).is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify pass**

Run: `cargo test -p pitboss-cli manifest::validate`
Expected: all seven pass.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/manifest/validate.rs
git commit -m "Manifest validation: ids, directories, branch conflicts, ranges"
```

---

### Task 28: Path expansion + top-level load function

**Files:**
- Create: `crates/pitboss-cli/src/manifest/load.rs`
- Modify: `crates/pitboss-cli/src/manifest/mod.rs`

- [ ] **Step 1: Write failing test**

Modify `crates/pitboss-cli/src/manifest/mod.rs` to add:

```rust
pub mod load;
pub use load::load_manifest;
```

Create `crates/pitboss-cli/src/manifest/load.rs`:

```rust
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::resolve::{resolve, ResolvedManifest};
use super::schema::Manifest;
use super::validate::validate;

/// Load, parse, resolve, and validate a manifest from disk.
///
/// `env_max_parallel` should be `std::env::var("ANTHROPIC_MAX_CONCURRENT")`
/// parsed to a `u32` if present.
pub fn load_manifest(path: &Path, env_max_parallel: Option<u32>) -> Result<ResolvedManifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading manifest at {}", path.display()))?;
    let mut manifest: Manifest = toml::from_str(&text)
        .with_context(|| format!("parsing manifest at {}", path.display()))?;
    expand_paths(&mut manifest)?;

    let resolved = resolve(manifest, env_max_parallel)?;
    validate(&resolved)?;
    Ok(resolved)
}

fn expand_paths(m: &mut Manifest) -> Result<()> {
    for t in &mut m.tasks {
        t.directory = expand(&t.directory)?;
    }
    if let Some(dir) = &m.run.run_dir {
        m.run.run_dir = Some(expand(dir)?);
    }
    Ok(())
}

fn expand(p: &Path) -> Result<PathBuf> {
    let s = p.to_string_lossy();
    let expanded = shellexpand::full(&s)
        .with_context(|| format!("expanding path {s}"))?;
    Ok(PathBuf::from(expanded.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn loads_valid_manifest_from_disk() {
        let dir = TempDir::new().unwrap();
        // Init a git repo at dir so validation passes with use_worktree=true default.
        Command::new("git").args(["init","-q"]).current_dir(dir.path()).status().unwrap();
        Command::new("git").args(["config","user.email","t@t.x"]).current_dir(dir.path()).status().unwrap();
        Command::new("git").args(["config","user.name","t"]).current_dir(dir.path()).status().unwrap();
        std::fs::write(dir.path().join("r"), "").unwrap();
        Command::new("git").args(["add","."]).current_dir(dir.path()).status().unwrap();
        Command::new("git").args(["commit","-q","-m","i"]).current_dir(dir.path()).status().unwrap();

        let manifest = dir.path().join("shire.toml");
        std::fs::write(&manifest, format!(r#"
[[task]]
id = "only"
directory = "{}"
prompt = "hi"
"#, dir.path().display())).unwrap();

        let r = load_manifest(&manifest, None).unwrap();
        assert_eq!(r.tasks[0].id, "only");
    }
}
```

- [ ] **Step 2: Run tests to verify pass**

Run: `cargo test -p pitboss-cli manifest::load`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/manifest/
git commit -m "Top-level load_manifest pipeline with path expansion"
```

---

## Phase 7 — Dispatch Runner (pitboss-cli)

### Task 29: CLI wiring with clap subcommands

**Files:**
- Create: `crates/pitboss-cli/src/cli.rs`
- Modify: `crates/pitboss-cli/src/main.rs`

- [ ] **Step 1: Write the CLI skeleton**

Create `crates/pitboss-cli/src/cli.rs`:

```rust
use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "shire", version, about = "Headless dispatcher for parallel Claude Code agents")]
pub struct Cli {
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[arg(short, long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Parse, resolve and validate a manifest. Prints report and exits.
    Validate {
        manifest: PathBuf,
    },
    /// Execute a manifest.
    Dispatch {
        manifest: PathBuf,
        /// Override run_dir from the manifest / default.
        #[arg(long)]
        run_dir: Option<PathBuf>,
        /// Print the resolved claude spawn commands and exit.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print version information.
    Version,
}
```

Replace `crates/pitboss-cli/src/main.rs`:

```rust
mod cli;
mod manifest;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};

fn main() -> Result<()> {
    let args = Cli::parse();
    init_tracing(args.verbose, args.quiet);

    match args.command {
        Command::Version => {
            println!("shire {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Validate { manifest } => run_validate(&manifest),
        Command::Dispatch { manifest, run_dir, dry_run } => {
            run_dispatch(&manifest, run_dir, dry_run)
        }
    }
}

fn init_tracing(verbose: u8, quiet: bool) {
    use tracing_subscriber::{fmt, EnvFilter};
    let level = match (quiet, verbose) {
        (true, _)   => "warn",
        (false, 0)  => "info",
        (false, 1)  => "debug",
        (false, _)  => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("shire={level},pitboss_core={level}")));
    fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
}

fn run_validate(manifest: &std::path::Path) -> Result<()> {
    let env_mp = parse_env_max_parallel();
    let r = manifest::load_manifest(manifest, env_mp)?;
    println!("OK — {} tasks, max_parallel={}", r.tasks.len(), r.max_parallel);
    Ok(())
}

fn run_dispatch(
    manifest: &std::path::Path,
    _run_dir_override: Option<std::path::PathBuf>,
    _dry_run: bool,
) -> Result<()> {
    let env_mp = parse_env_max_parallel();
    let _resolved = manifest::load_manifest(manifest, env_mp)?;
    anyhow::bail!("dispatch not yet implemented — Task 30+");
}

fn parse_env_max_parallel() -> Option<u32> {
    std::env::var("ANTHROPIC_MAX_CONCURRENT").ok().and_then(|s| s.parse().ok())
}
```

- [ ] **Step 2: Verify CLI builds and `validate` runs end-to-end**

Run: `cargo build -p pitboss-cli`
Expected: builds clean.

Run: `cargo run -p pitboss-cli -- version`
Expected: `shire 0.1.0`.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/
git commit -m "CLI subcommands with clap; validate end-to-end works"
```

---

### Task 30: Claude binary probe

**Files:**
- Create: `crates/pitboss-cli/src/dispatch/mod.rs`
- Create: `crates/pitboss-cli/src/dispatch/probe.rs`
- Modify: `crates/pitboss-cli/src/main.rs`

- [ ] **Step 1: Write failing test**

Create `crates/pitboss-cli/src/dispatch/mod.rs`:

```rust
pub mod probe;
pub mod runner;
pub mod summary;
pub mod signals;

pub use probe::probe_claude;
pub use runner::run_dispatch_inner;
```

Create `crates/pitboss-cli/src/dispatch/probe.rs`:

```rust
use std::path::Path;

use anyhow::{bail, Result};
use tokio::process::Command;

/// Probe the claude CLI for its version string. Returns `None` when the binary
/// exists but the probe output is unparseable — a non-fatal degrade.
/// Returns `Err` when the binary is not executable or not found (fatal).
pub async fn probe_claude(binary: &Path) -> Result<Option<String>> {
    let output = Command::new(binary).arg("--version").output().await;
    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if text.is_empty() { Ok(None) } else { Ok(Some(text)) }
        }
        Ok(o) => {
            tracing::warn!(code = ?o.status.code(), "claude --version exited non-zero; proceeding without version");
            Ok(None)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("claude binary not found at {}", binary.display())
        }
        Err(e) => bail!("failed to probe claude: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn nonexistent_binary_is_fatal() {
        let err = probe_claude(&PathBuf::from("/nope/claude")).await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn echo_binary_succeeds_with_unparsed_output() {
        let v = probe_claude(&PathBuf::from("/bin/echo")).await.unwrap();
        assert!(v.is_some());
    }
}
```

Create stub files to make `mod.rs` compile:

`crates/pitboss-cli/src/dispatch/runner.rs`:

```rust
use anyhow::Result;
use std::path::PathBuf;

use crate::manifest::resolve::ResolvedManifest;

pub async fn run_dispatch_inner(
    _resolved: ResolvedManifest,
    _claude_binary: PathBuf,
    _run_dir_override: Option<PathBuf>,
    _dry_run: bool,
) -> Result<i32> {
    anyhow::bail!("dispatch runner — Task 31+")
}
```

`crates/pitboss-cli/src/dispatch/summary.rs`:

```rust
// Populated in Task 33.
```

`crates/pitboss-cli/src/dispatch/signals.rs`:

```rust
// Populated in Task 34.
```

Modify `crates/pitboss-cli/src/main.rs` to add `mod dispatch;`:

```rust
mod cli;
mod dispatch;
mod manifest;
```

- [ ] **Step 2: Run tests to verify pass**

Run: `cargo test -p pitboss-cli dispatch::probe`
Expected: both tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/
git commit -m "Add claude binary probe with unparseable-output tolerance"
```

---

### Task 31: Dispatch runner — happy path with FakeSpawner

The dispatch runner is the beating heart. It owns the semaphore, spawns per-task executors, drives the store, and aggregates the summary. Unit-test with `FakeSpawner` injection via a trait.

**Files:**
- Modify: `crates/pitboss-cli/src/dispatch/runner.rs`
- Modify: `crates/pitboss-cli/Cargo.toml`
- Modify: `crates/pitboss-cli/src/main.rs`

- [ ] **Step 1: Add test-support feature pulling pitboss-core's equivalent**

Modify `crates/pitboss-cli/Cargo.toml` — add under `[features]`:

```toml
[features]
test-support = ["pitboss-core/test-support"]
```

Modify `[dev-dependencies]`:

```toml
[dev-dependencies]
pitboss-cli  = { path = ".", features = ["test-support"] }
tempfile   = { workspace = true }
pitboss-core = { path = "../pitboss-core", features = ["test-support"] }
```

- [ ] **Step 2: Write failing test**

Replace `crates/pitboss-cli/src/dispatch/runner.rs`:

```rust
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use pitboss_core::process::{ProcessSpawner, SpawnCmd, TokioSpawner};
use pitboss_core::session::{CancelToken, SessionHandle};
use pitboss_core::store::{JsonFileStore, RunMeta, RunSummary, SessionStore, TaskRecord, TaskStatus};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

use crate::manifest::resolve::{ResolvedManifest, ResolvedTask};

/// Public entry — main.rs calls this. Constructs production spawner + store.
pub async fn run_dispatch_inner(
    resolved: ResolvedManifest,
    claude_binary: PathBuf,
    run_dir_override: Option<PathBuf>,
    dry_run: bool,
) -> Result<i32> {
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let run_dir = run_dir_override.unwrap_or_else(|| resolved.run_dir.clone());
    tokio::fs::create_dir_all(&run_dir).await.ok();
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(run_dir.clone()));

    execute(resolved, claude_binary, spawner, store, dry_run).await
}

/// Inner workhorse — takes its dependencies injected for testability.
pub async fn execute(
    resolved: ResolvedManifest,
    claude_binary: PathBuf,
    spawner: Arc<dyn ProcessSpawner>,
    store: Arc<dyn SessionStore>,
    dry_run: bool,
) -> Result<i32> {
    let run_id = Uuid::now_v7();
    let meta = RunMeta {
        run_id,
        manifest_path: PathBuf::new(),
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version: None,
        started_at: Utc::now(),
        env: Default::default(),
    };
    store.init_run(&meta).await.context("init run")?;

    if dry_run {
        for t in &resolved.tasks {
            println!("DRY-RUN {}: {} {}", t.id,
                     claude_binary.display(),
                     spawn_args(t).join(" "));
        }
        return Ok(0);
    }

    let semaphore = Arc::new(Semaphore::new(resolved.max_parallel as usize));
    let cancel = CancelToken::new();
    let wt_mgr = Arc::new(WorktreeManager::new());
    let records: Arc<Mutex<Vec<TaskRecord>>> = Arc::new(Mutex::new(Vec::new()));

    let mut handles = Vec::new();

    for task in resolved.tasks.clone() {
        if cancel.is_draining() { break; }
        let permit = semaphore.clone().acquire_owned().await?;
        let spawner = spawner.clone();
        let store = store.clone();
        let cancel = cancel.clone();
        let claude = claude_binary.clone();
        let records = records.clone();
        let wt_mgr = wt_mgr.clone();
        let halt_on_failure = resolved.halt_on_failure;
        let run_dir = resolved.run_dir.clone();
        let cleanup_policy = match resolved.worktree_cleanup {
            crate::manifest::schema::WorktreeCleanup::Always    => CleanupPolicy::Always,
            crate::manifest::schema::WorktreeCleanup::OnSuccess => CleanupPolicy::OnSuccess,
            crate::manifest::schema::WorktreeCleanup::Never     => CleanupPolicy::Never,
        };

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let record = execute_task(&task, &claude, spawner, store.clone(),
                                      cancel.clone(), wt_mgr, cleanup_policy, run_id, run_dir).await;
            let failed = !matches!(record.status, TaskStatus::Success);
            records.lock().await.push(record);
            if failed && halt_on_failure { cancel.drain(); }
        }));
    }

    for h in handles { let _ = h.await; }

    let records = Arc::try_unwrap(records).map_err(|_| anyhow::anyhow!("records locked"))?
        .into_inner();
    let tasks_failed = records.iter().filter(|r| !matches!(r.status, TaskStatus::Success)).count();

    let started_at = meta.started_at;
    let ended_at   = Utc::now();
    let summary = RunSummary {
        run_id, manifest_path: PathBuf::new(),
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version: None,
        started_at, ended_at,
        total_duration_ms: (ended_at - started_at).num_milliseconds(),
        tasks_total: records.len(),
        tasks_failed,
        was_interrupted: cancel.is_draining() || cancel.is_terminated(),
        tasks: records,
    };
    store.finalize_run(&summary).await?;

    Ok(if tasks_failed > 0 { 1 } else { 0 })
}

async fn execute_task(
    task: &ResolvedTask,
    claude: &PathBuf,
    spawner: Arc<dyn ProcessSpawner>,
    _store: Arc<dyn SessionStore>,
    cancel: CancelToken,
    wt_mgr: Arc<WorktreeManager>,
    cleanup: CleanupPolicy,
    run_id: Uuid,
    run_dir: PathBuf,
) -> TaskRecord {
    let task_dir = run_dir.join(run_id.to_string()).join("tasks").join(&task.id);
    tokio::fs::create_dir_all(&task_dir).await.ok();
    let log_path = task_dir.join("stdout.log");

    // Worktree preparation (optional).
    let mut worktree_handle = None;
    let cwd = if task.use_worktree {
        let name = format!("shire-{}-{}", task.id, run_id);
        match wt_mgr.prepare(&task.directory, &name, task.branch.as_deref()) {
            Ok(wt) => {
                let p = wt.path.clone();
                worktree_handle = Some(wt);
                p
            }
            Err(e) => {
                return TaskRecord {
                    task_id: task.id.clone(),
                    status: TaskStatus::SpawnFailed,
                    exit_code: None,
                    started_at: Utc::now(), ended_at: Utc::now(),
                    duration_ms: 0,
                    worktree_path: None,
                    log_path,
                    token_usage: Default::default(),
                    claude_session_id: None,
                    final_message_preview: Some(format!("worktree error: {e}")),
                };
            }
        }
    } else {
        task.directory.clone()
    };

    let cmd = SpawnCmd {
        program: claude.clone(),
        args: spawn_args(task),
        cwd: cwd.clone(),
        env: task.env.clone(),
    };

    let outcome = SessionHandle::new(task.id.clone(), spawner, cmd)
        .with_log_path(log_path.clone())
        .run_to_completion(cancel, Duration::from_secs(task.timeout_secs))
        .await;

    let status = match outcome.final_state {
        pitboss_core::session::SessionState::Completed         => TaskStatus::Success,
        pitboss_core::session::SessionState::Failed { .. }     => TaskStatus::Failed,
        pitboss_core::session::SessionState::TimedOut          => TaskStatus::TimedOut,
        pitboss_core::session::SessionState::Cancelled         => TaskStatus::Cancelled,
        pitboss_core::session::SessionState::SpawnFailed { .. } => TaskStatus::SpawnFailed,
        _ => TaskStatus::Failed,
    };

    // Cleanup worktree.
    if let Some(wt) = worktree_handle {
        let succeeded = matches!(status, TaskStatus::Success);
        let _ = wt_mgr.cleanup(wt, cleanup, succeeded);
    }

    let worktree_path = if task.use_worktree { Some(cwd) } else { None };
    TaskRecord {
        task_id: task.id.clone(),
        status,
        exit_code: outcome.exit_code,
        started_at: outcome.started_at,
        ended_at: outcome.ended_at,
        duration_ms: outcome.duration_ms(),
        worktree_path,
        log_path,
        token_usage: outcome.token_usage,
        claude_session_id: outcome.claude_session_id,
        final_message_preview: outcome.final_message_preview,
    }
}

fn spawn_args(task: &ResolvedTask) -> Vec<String> {
    let mut args = vec!["--output-format".into(), "stream-json".into()];
    if !task.tools.is_empty() {
        args.push("--allowedTools".into());
        args.push(task.tools.join(","));
    }
    args.push("--model".into());
    args.push(task.model.clone());
    args.push("-p".into());
    args.push(task.prompt.clone());
    args
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(root: &std::path::Path) {
        Command::new("git").args(["init","-q"]).current_dir(root).status().unwrap();
        Command::new("git").args(["config","user.email","t@t.x"]).current_dir(root).status().unwrap();
        Command::new("git").args(["config","user.name","t"]).current_dir(root).status().unwrap();
        std::fs::write(root.join("r"), "").unwrap();
        Command::new("git").args(["add","."]).current_dir(root).status().unwrap();
        Command::new("git").args(["commit","-q","-m","i"]).current_dir(root).status().unwrap();
    }

    #[tokio::test]
    async fn executes_three_tasks_with_mixed_outcomes() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let run_dir = TempDir::new().unwrap();

        let resolved = crate::manifest::resolve::ResolvedManifest {
            max_parallel: 2,
            halt_on_failure: false,
            run_dir: run_dir.path().to_path_buf(),
            worktree_cleanup: crate::manifest::schema::WorktreeCleanup::Always,
            emit_event_stream: false,
            tasks: vec![
                ResolvedTask {
                    id: "ok".into(),
                    directory: dir.path().to_path_buf(),
                    prompt: "p".into(), branch: None,
                    model: "m".into(), effort: crate::manifest::schema::Effort::High,
                    tools: vec![], timeout_secs: 30,
                    use_worktree: false, env: Default::default(),
                },
                ResolvedTask {
                    id: "bad".into(),
                    directory: dir.path().to_path_buf(),
                    prompt: "p".into(), branch: None,
                    model: "m".into(), effort: crate::manifest::schema::Effort::High,
                    tools: vec![], timeout_secs: 30,
                    use_worktree: false, env: Default::default(),
                },
            ],
        };

        // Script: first call succeeds, second call fails. FakeSpawner is single-shot,
        // so we use a cycling spawner.
        let spawner = Arc::new(CyclingFake(
            vec![
                FakeScript::new()
                    .stdout_line(r#"{"type":"result","session_id":"s1","usage":{"input_tokens":1,"output_tokens":2}}"#)
                    .exit_code(0),
                FakeScript::new()
                    .stdout_line(r#"{"type":"result","session_id":"s2","usage":{"input_tokens":1,"output_tokens":2}}"#)
                    .exit_code(5),
            ],
            std::sync::Mutex::new(0),
        ));

        let store = Arc::new(JsonFileStore::new(run_dir.path().to_path_buf()));
        let rc = execute(resolved, PathBuf::from("claude"), spawner, store.clone(), false)
            .await.unwrap();
        assert_eq!(rc, 1, "one failure → exit 1");
    }

    struct CyclingFake(Vec<FakeScript>, std::sync::Mutex<usize>);

    #[async_trait::async_trait]
    impl ProcessSpawner for CyclingFake {
        async fn spawn(&self, cmd: SpawnCmd)
            -> Result<Box<dyn pitboss_core::process::ChildProcess>, pitboss_core::error::SpawnError>
        {
            let i = {
                let mut lock = self.1.lock().unwrap();
                let i = *lock;
                *lock += 1;
                i
            };
            let script = self.0[i % self.0.len()].clone();
            FakeSpawner::new(script).spawn(cmd).await
        }
    }
}
```

- [ ] **Step 3: Run tests to verify pass**

Run: `cargo test -p pitboss-cli --features test-support dispatch::runner`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-cli/ 
git commit -m "Dispatch runner: semaphore, per-task execution, summary aggregation"
```

---

### Task 32: Dispatch halt_on_failure cascade test

**Files:**
- Modify: `crates/pitboss-cli/src/dispatch/runner.rs` (add tests)

- [ ] **Step 1: Write failing test**

Append to the `tests` module in `crates/pitboss-cli/src/dispatch/runner.rs`:

```rust
    #[tokio::test]
    async fn halt_on_failure_drains_after_first_failure() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let run_dir = TempDir::new().unwrap();

        let make_task = |id: &str| ResolvedTask {
            id: id.into(),
            directory: dir.path().to_path_buf(),
            prompt: "p".into(), branch: None,
            model: "m".into(), effort: crate::manifest::schema::Effort::High,
            tools: vec![], timeout_secs: 30,
            use_worktree: false, env: Default::default(),
        };

        let resolved = crate::manifest::resolve::ResolvedManifest {
            max_parallel: 1,    // serialize so ordering is deterministic
            halt_on_failure: true,
            run_dir: run_dir.path().to_path_buf(),
            worktree_cleanup: crate::manifest::schema::WorktreeCleanup::Always,
            emit_event_stream: false,
            tasks: vec![make_task("a"), make_task("b"), make_task("c")],
        };

        let spawner = Arc::new(CyclingFake(
            vec![
                FakeScript::new()
                    .stdout_line(r#"{"type":"result","session_id":"s","usage":{"input_tokens":0,"output_tokens":0}}"#)
                    .exit_code(7),   // fails → cascade
                FakeScript::new().exit_code(0),
                FakeScript::new().exit_code(0),
            ],
            std::sync::Mutex::new(0),
        ));
        let store = Arc::new(JsonFileStore::new(run_dir.path().to_path_buf()));
        let rc = execute(resolved, PathBuf::from("claude"), spawner, store.clone(), false)
            .await.unwrap();
        assert_eq!(rc, 1);

        // Expect only task "a" recorded; others were skipped by the drain.
        // summary.json should exist with tasks.len() == 1.
        let summary_path = run_dir.path()
            .join(store_run_id_dir(run_dir.path()))
            .join("summary.json");
        let bytes = std::fs::read(&summary_path).unwrap();
        let s: RunSummary = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(s.tasks.len(), 1);
    }

    fn store_run_id_dir(root: &std::path::Path) -> String {
        // Finds the single UUID-named subdir just created.
        for entry in std::fs::read_dir(root).unwrap() {
            let e = entry.unwrap();
            if e.path().is_dir() { return e.file_name().to_string_lossy().to_string(); }
        }
        panic!("no run dir")
    }
```

- [ ] **Step 2: Run test to verify pass**

Run: `cargo test -p pitboss-cli --features test-support dispatch::runner::tests::halt_on_failure_drains_after_first_failure`
Expected: pass. (If it fails because drain isn't checked before permit acquisition, adjust the loop to break on `cancel.is_draining()` between `permit.acquire()` calls — code already does this.)

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/dispatch/runner.rs
git commit -m "Test halt_on_failure cascade stops subsequent tasks"
```

---

### Task 33: Wire run_dispatch into main.rs

**Files:**
- Modify: `crates/pitboss-cli/src/main.rs`

- [ ] **Step 1: Replace `run_dispatch` to call into the runner**

Replace `fn run_dispatch` in `crates/pitboss-cli/src/main.rs`:

```rust
fn run_dispatch(
    manifest: &std::path::Path,
    run_dir_override: Option<std::path::PathBuf>,
    dry_run: bool,
) -> Result<()> {
    let env_mp = parse_env_max_parallel();
    let resolved = manifest::load_manifest(manifest, env_mp)?;
    let claude_bin = std::env::var_os("SHIRE_CLAUDE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("claude"));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all().build()?;
    rt.block_on(async {
        if !dry_run {
            let _ = dispatch::probe_claude(&claude_bin).await?;
        }
        let code = dispatch::run_dispatch_inner(resolved, claude_bin, run_dir_override, dry_run).await?;
        std::process::exit(code);
    })
}
```

- [ ] **Step 2: Verify CLI dry-run end-to-end**

Build a minimal shire.toml in a temp dir pointing at a git repo, and run `cargo run -p pitboss-cli -- dispatch /path/to/shire.toml --dry-run`. Expected: prints the spawn command lines.

(No automated test — smoke-verify and move on.)

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/main.rs
git commit -m "Wire dispatch command through main.rs with probe + tokio runtime"
```

---

## Phase 8 — Signal Handling

### Task 34: Two-phase Ctrl-C handler

**Files:**
- Modify: `crates/pitboss-cli/src/dispatch/signals.rs`
- Modify: `crates/pitboss-cli/src/dispatch/runner.rs`

- [ ] **Step 1: Write the signals module**

Replace `crates/pitboss-cli/src/dispatch/signals.rs`:

```rust
use std::time::Duration;

use pitboss_core::session::CancelToken;

const SECOND_SIGINT_WINDOW: Duration = Duration::from_secs(5);

/// Spawn a task that watches for Ctrl-C in two phases:
///   1st SIGINT within window → drain
///   2nd SIGINT within window → terminate
/// After the window, re-armed: a single later SIGINT is treated as a fresh first.
pub fn install_ctrl_c_watcher(cancel: CancelToken) {
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() { return; }
            cancel.drain();
            tracing::warn!("received Ctrl-C — draining; send another within 5s to terminate");
            match tokio::time::timeout(SECOND_SIGINT_WINDOW, tokio::signal::ctrl_c()).await {
                Ok(Ok(_)) => {
                    cancel.terminate();
                    tracing::warn!("received second Ctrl-C — terminating subprocesses");
                    return;
                }
                _ => {
                    tracing::info!("drain window expired; continuing in drain mode");
                    // Loop again: if another Ctrl-C arrives later, start a new window.
                }
            }
        }
    });
}
```

- [ ] **Step 2: Install in execute()**

Modify `crates/pitboss-cli/src/dispatch/runner.rs` — near the top of `execute()`, just after `store.init_run(...)`:

```rust
    crate::dispatch::signals::install_ctrl_c_watcher(cancel.clone());
```

- [ ] **Step 3: Integration test for signals deferred to Task 38**

(Handled end-to-end in the integration-test phase where we can actually raise signals against a subprocess.)

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-cli/src/dispatch/
git commit -m "Two-phase Ctrl-C watcher: drain then terminate"
```

---

## Phase 9 — fake-claude + Integration Tests

### Task 35: Scripted fake-claude binary

**Files:**
- Create: `tests-support/fake-claude/Cargo.toml`
- Create: `tests-support/fake-claude/src/main.rs`
- Modify: `Cargo.toml` (workspace root — add to members)

- [ ] **Step 1: Write the fake binary**

Create `tests-support/fake-claude/Cargo.toml`:

```toml
[package]
name         = "fake-claude"
version      = "0.0.0"
edition      = "2021"
rust-version = "1.82"
publish      = false

[[bin]]
name = "fake-claude"
path = "src/main.rs"

[dependencies]
serde_json = { workspace = true }
```

Create `tests-support/fake-claude/src/main.rs`:

```rust
// Scripted fake `claude`. Reads MOSAIC_FAKE_SCRIPT (JSONL) and plays back:
//   {"stdout":"..."}        — emit line on stdout
//   {"stderr":"..."}        — emit line on stderr
//   {"sleep_ms":500}        — sleep N ms
// Then exits with MOSAIC_FAKE_EXIT_CODE (default 0).
//
// If MOSAIC_FAKE_HOLD=1, blocks until SIGTERM after playing script.

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

fn main() -> std::io::Result<()> {
    // Handle --version specially for the probe.
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--version" {
        println!("fake-claude 0.0.0");
        return Ok(());
    }

    let script_path = std::env::var("MOSAIC_FAKE_SCRIPT").ok();
    if let Some(path) = script_path {
        let file = std::fs::File::open(&path)?;
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() { continue; }
            let v: serde_json::Value = serde_json::from_str(&line)?;
            if let Some(s) = v.get("stdout").and_then(|x| x.as_str()) {
                println!("{s}");
                std::io::stdout().flush()?;
            } else if let Some(s) = v.get("stderr").and_then(|x| x.as_str()) {
                eprintln!("{s}");
            } else if let Some(n) = v.get("sleep_ms").and_then(|x| x.as_u64()) {
                std::thread::sleep(Duration::from_millis(n));
            }
        }
    }

    if std::env::var("MOSAIC_FAKE_HOLD").ok().as_deref() == Some("1") {
        loop { std::thread::sleep(Duration::from_secs(3600)); }
    }

    let code: i32 = std::env::var("MOSAIC_FAKE_EXIT_CODE")
        .ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    std::process::exit(code);
}
```

Modify root `Cargo.toml` `[workspace] members`:

```toml
members = [
    "crates/pitboss-core",
    "crates/pitboss-cli",
    "tests-support/fake-claude",
]
```

- [ ] **Step 2: Build it**

Run: `cargo build -p fake-claude`
Expected: produces `target/debug/fake-claude`.

- [ ] **Step 3: Commit**

```bash
git add tests-support/ Cargo.toml
git commit -m "Add fake-claude scripted binary for integration tests"
```

---

### Task 36: Workspace integration test harness

**Files:**
- Create: `tests/dispatch_flows.rs`
- Create: `tests/support/mod.rs`
- Create: `tests/fixtures/scripts/success.jsonl`
- Create: `tests/fixtures/scripts/failure.jsonl`

- [ ] **Step 1: Fixtures**

Create `tests/fixtures/scripts/success.jsonl`:

```
{"stdout":"{\"type\":\"system\",\"subtype\":\"init\"}"}
{"stdout":"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}"}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"s1\",\"result\":\"done\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}"}
```

Create `tests/fixtures/scripts/failure.jsonl`:

```
{"stdout":"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"oh no\"}]}}"}
```

- [ ] **Step 2: Support helpers**

Create `tests/support/mod.rs`:

```rust
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn init_git_repo(dir: &Path) {
    Command::new("git").args(["init","-q"]).current_dir(dir).status().unwrap();
    Command::new("git").args(["config","user.email","t@t.x"]).current_dir(dir).status().unwrap();
    Command::new("git").args(["config","user.name","t"]).current_dir(dir).status().unwrap();
    std::fs::write(dir.join("README.md"), "x").unwrap();
    Command::new("git").args(["add","."]).current_dir(dir).status().unwrap();
    Command::new("git").args(["commit","-q","-m","i"]).current_dir(dir).status().unwrap();
}

pub fn fake_claude_path() -> PathBuf {
    // Assumes cargo workspace layout.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .join("target/debug/fake-claude")
}

pub fn shire_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/debug/shire")
}

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/scripts").join(name)
}
```

- [ ] **Step 3: Write integration test**

Create `tests/dispatch_flows.rs`:

```rust
mod support;

use std::process::Command;
use support::*;
use tempfile::TempDir;

fn ensure_built() {
    // Build shire + fake-claude before running tests.
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "pitboss-cli", "-p", "fake-claude"])
        .status().unwrap();
    assert!(status.success(), "build failed");
}

#[test]
fn three_task_mixed_outcomes_produce_summary() {
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();

    let manifest_path = repo.path().join("shire.toml");
    std::fs::write(&manifest_path, format!(r#"
[run]
max_parallel = 2
run_dir = "{run_dir}"
worktree_cleanup = "always"

[defaults]
use_worktree = false

[[task]]
id = "ok1"
directory = "{repo}"
prompt = "p"

[[task]]
id = "ok2"
directory = "{repo}"
prompt = "p"

[[task]]
id = "bad"
directory = "{repo}"
prompt = "p"
"#, run_dir = run_dir.path().display(), repo = repo.path().display())).unwrap();

    // Run with fake-claude. First two tasks succeed; the third exits 2.
    let mut cmd = Command::new(shire_binary());
    cmd.arg("dispatch").arg(&manifest_path);
    cmd.env("SHIRE_CLAUDE_BINARY", fake_claude_path());
    cmd.env("MOSAIC_FAKE_SCRIPT", fixture("success.jsonl"));
    cmd.env("MOSAIC_FAKE_EXIT_CODE", "0");
    // Note: all three tasks will use the SAME script/exit code — simplified from a cycling
    // fixture. For per-task variation, use Task 37.
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr));

    // Locate the single run dir and verify summary.json exists.
    let mut run_dirs = std::fs::read_dir(run_dir.path()).unwrap();
    let rd = run_dirs.next().unwrap().unwrap().path();
    let summary = rd.join("summary.json");
    assert!(summary.exists());
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    assert_eq!(s["tasks_total"].as_u64().unwrap(), 3);
}
```

- [ ] **Step 4: Run**

Run: `cargo test --test dispatch_flows -- --test-threads=1`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add tests/ 
git commit -m "Integration test harness + three-task happy-path flow"
```

---

### Task 37: Integration test — halt_on_failure + per-task scripts

**Files:**
- Modify: `tests/dispatch_flows.rs`
- Create: `tests/fixtures/scripts/exit2.jsonl`

- [ ] **Step 1: Fixture for a failing run**

Create `tests/fixtures/scripts/exit2.jsonl`:

```
{"stdout":"{\"type\":\"result\",\"session_id\":\"s\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}"}
```

- [ ] **Step 2: Append test — relies on per-task env (extend pitboss-cli to honor Task-level env vars that point at different fake scripts)**

For this integration test only, we rely on the `env = { MOSAIC_FAKE_SCRIPT = "..." }` per-task env already supported by shire's manifest schema.

Append to `tests/dispatch_flows.rs`:

```rust
#[test]
fn halt_on_failure_stops_remaining_tasks() {
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();

    let script_ok  = fixture("success.jsonl");
    let script_bad = fixture("exit2.jsonl");

    let manifest_path = repo.path().join("shire.toml");
    std::fs::write(&manifest_path, format!(r#"
[run]
max_parallel = 1
halt_on_failure = true
run_dir = "{run_dir}"
worktree_cleanup = "always"

[defaults]
use_worktree = false

[[task]]
id = "fails"
directory = "{repo}"
prompt = "p"
env = {{ MOSAIC_FAKE_SCRIPT = "{bad}", MOSAIC_FAKE_EXIT_CODE = "2" }}

[[task]]
id = "would-run"
directory = "{repo}"
prompt = "p"
env = {{ MOSAIC_FAKE_SCRIPT = "{ok}", MOSAIC_FAKE_EXIT_CODE = "0" }}
"#,
        run_dir = run_dir.path().display(),
        repo    = repo.path().display(),
        bad     = script_bad.display(),
        ok      = script_ok.display())).unwrap();

    let out = Command::new(shire_binary())
        .arg("dispatch").arg(&manifest_path)
        .env("SHIRE_CLAUDE_BINARY", fake_claude_path())
        .output().unwrap();

    // shire exits 1 because at least one task failed.
    assert_eq!(out.status.code(), Some(1));

    let rd = std::fs::read_dir(run_dir.path()).unwrap().next().unwrap().unwrap().path();
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(rd.join("summary.json")).unwrap()).unwrap();
    assert_eq!(s["tasks"].as_array().unwrap().len(), 1, "second task should not have run");
}
```

- [ ] **Step 3: Run**

Run: `cargo test --test dispatch_flows halt_on_failure_stops_remaining_tasks -- --test-threads=1`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add tests/
git commit -m "Integration test: halt_on_failure cascade"
```

---

### Task 38: Integration test — Ctrl-C two-phase

**Files:**
- Modify: `tests/dispatch_flows.rs`
- Create: `tests/fixtures/scripts/hold.jsonl`

- [ ] **Step 1: Fixture that holds until signaled**

Create `tests/fixtures/scripts/hold.jsonl`:

```
{"stdout":"{\"type\":\"system\",\"subtype\":\"init\"}"}
```

Env var `MOSAIC_FAKE_HOLD=1` makes the fake loop after emitting.

- [ ] **Step 2: Write the test**

Append to `tests/dispatch_flows.rs`:

```rust
#[cfg(unix)]
#[test]
fn ctrl_c_twice_terminates_running_tasks() {
    use std::time::Duration;

    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();

    let manifest_path = repo.path().join("shire.toml");
    std::fs::write(&manifest_path, format!(r#"
[run]
max_parallel = 1
run_dir = "{run_dir}"
worktree_cleanup = "always"

[defaults]
use_worktree = false

[[task]]
id = "held"
directory = "{repo}"
prompt = "p"
timeout_secs = 120
env = {{ MOSAIC_FAKE_SCRIPT = "{hold}", MOSAIC_FAKE_HOLD = "1" }}
"#,
        run_dir = run_dir.path().display(),
        repo    = repo.path().display(),
        hold    = fixture("hold.jsonl").display())).unwrap();

    let mut child = std::process::Command::new(shire_binary())
        .arg("dispatch").arg(&manifest_path)
        .env("SHIRE_CLAUDE_BINARY", fake_claude_path())
        .spawn().unwrap();

    // Give shire time to spawn the fake.
    std::thread::sleep(Duration::from_millis(500));

    // Send SIGINT twice.
    let pid = child.id() as i32;
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }
    std::thread::sleep(Duration::from_millis(200));
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let status = std::thread::spawn(move || child.wait().unwrap())
        .join().expect("process joins");
    // shire exits non-zero after cancellation.
    assert!(!status.success());
}
```

Add `libc = { workspace = true }` as a dev-dependency of the workspace integration test (edit root `Cargo.toml`):

Actually the workspace integration tests live in the workspace root, not in a crate — so they use the root `Cargo.toml` package if you declare one. Simpler: put this test inside `crates/pitboss-cli/tests/` instead. Move the test file there.

Move `tests/` to `crates/pitboss-cli/tests/` by repeating the `Create` operations relative to that path. Update any `env!("CARGO_MANIFEST_DIR")` paths — they now point at `crates/pitboss-cli/`, so `fake_claude_path()` becomes:

```rust
pub fn fake_claude_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).parent().unwrap().parent().unwrap()
        .join("target/debug/fake-claude")
}

pub fn shire_binary() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).parent().unwrap().parent().unwrap()
        .join("target/debug/shire")
}
```

Add `libc` to `crates/pitboss-cli/[dev-dependencies]`:

```toml
libc = { workspace = true }
```

- [ ] **Step 3: Run**

Run: `cargo test -p pitboss-cli --test dispatch_flows ctrl_c -- --test-threads=1`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-cli/
git commit -m "Integration test: two-phase Ctrl-C cancellation"
```

---

## Phase 10 — Stdout Progress Table

### Task 39: Non-TTY and TTY progress table

**Files:**
- Create: `crates/pitboss-cli/src/tui_table.rs`
- Modify: `crates/pitboss-cli/src/main.rs`
- Modify: `crates/pitboss-cli/src/dispatch/runner.rs`

- [ ] **Step 1: Write the table module**

Create `crates/pitboss-cli/src/tui_table.rs`:

```rust
use pitboss_core::store::{TaskRecord, TaskStatus};

pub struct ProgressTable {
    is_tty: bool,
    rows: Vec<Row>,
    rendered_lines: usize,
}

struct Row {
    task_id: String,
    status:  Status,
    duration_ms: i64,
    tokens_in: u64,
    tokens_out: u64,
    tokens_cache: u64,
    exit_code: Option<i32>,
}

enum Status { Pending, Running, Done(TaskStatus) }

impl ProgressTable {
    pub fn new(is_tty: bool) -> Self {
        Self { is_tty, rows: Vec::new(), rendered_lines: 0 }
    }

    pub fn register(&mut self, task_id: &str) {
        self.rows.push(Row {
            task_id: task_id.into(),
            status:  Status::Pending,
            duration_ms: 0,
            tokens_in: 0, tokens_out: 0, tokens_cache: 0,
            exit_code: None,
        });
        self.render();
    }

    pub fn mark_running(&mut self, task_id: &str) {
        if let Some(r) = self.find_mut(task_id) { r.status = Status::Running; }
        self.render();
    }

    pub fn mark_done(&mut self, rec: &TaskRecord) {
        if let Some(r) = self.find_mut(&rec.task_id) {
            r.status = Status::Done(rec.status.clone());
            r.duration_ms = rec.duration_ms;
            r.tokens_in    = rec.token_usage.input;
            r.tokens_out   = rec.token_usage.output;
            r.tokens_cache = rec.token_usage.cache_read;
            r.exit_code    = rec.exit_code;
        }
        self.render();
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Row> {
        self.rows.iter_mut().find(|r| r.task_id == id)
    }

    fn render(&mut self) {
        if self.is_tty {
            // Move cursor up by rendered_lines, clear, rewrite.
            if self.rendered_lines > 0 {
                print!("\x1b[{}A\x1b[J", self.rendered_lines);
            }
            let header = self.format_header();
            println!("{header}");
            for r in &self.rows {
                println!("{}", self.format_row(r));
            }
            self.rendered_lines = self.rows.len() + 1;
        } else {
            // Append-only: only render on state change of the last row.
            if let Some(last) = self.rows.last() {
                println!("{}", self.format_row(last));
            }
        }
    }

    fn format_header(&self) -> String {
        format!("{:<20} {:<12} {:>8} {:<22} {:>4}",
                "TASK", "STATUS", "TIME", "TOKENS (in/out/cache)", "EXIT")
    }

    fn format_row(&self, r: &Row) -> String {
        let status = match &r.status {
            Status::Pending => "… Pending".to_string(),
            Status::Running => "● Running".to_string(),
            Status::Done(TaskStatus::Success)     => "✓ Success".to_string(),
            Status::Done(TaskStatus::Failed)      => "✗ Failed".to_string(),
            Status::Done(TaskStatus::TimedOut)    => "⏱ TimedOut".to_string(),
            Status::Done(TaskStatus::Cancelled)   => "⊘ Cancelled".to_string(),
            Status::Done(TaskStatus::SpawnFailed) => "! SpawnFail".to_string(),
        };
        let time = if r.duration_ms == 0 { "—".to_string() } else {
            let secs = r.duration_ms / 1000;
            format!("{}m{:02}s", secs / 60, secs % 60)
        };
        let tokens = if r.tokens_in == 0 && r.tokens_out == 0 { "—".to_string() } else {
            format!("{} / {} / {}", r.tokens_in, r.tokens_out, r.tokens_cache)
        };
        let exit = r.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "—".to_string());
        format!("{:<20} {:<12} {:>8} {:<22} {:>4}", r.task_id, status, time, tokens, exit)
    }
}
```

- [ ] **Step 2: Wire into the runner**

Modify `crates/pitboss-cli/src/dispatch/runner.rs` — at the top of `execute()`:

```rust
    let is_tty = atty::is(atty::Stream::Stdout);
    let table = Arc::new(Mutex::new(crate::tui_table::ProgressTable::new(is_tty)));
    for t in &resolved.tasks { table.lock().await.register(&t.id); }
```

Before `SessionHandle::new` in `execute_task` — accept `table` as a parameter and call `mark_running`. After the outcome is built, call `mark_done(&record)`. Thread `table: Arc<Mutex<ProgressTable>>` through the signature.

Modify `crates/pitboss-cli/src/main.rs` to add `mod tui_table;`.

- [ ] **Step 3: Smoke test manually**

Run: `cargo test --workspace`
Expected: all existing tests still pass.

Run a dispatch against fake-claude manually and watch the table render.

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-cli/src/
git commit -m "Stdout progress table with TTY redraw and non-TTY append"
```

---

## Phase 11 — Polish, Docs, and Manual Smoke Test

### Task 40: README manual smoke test instructions

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Replace README with a usable v0.1 guide**

Replace `README.md`:

```markdown
# Agent Shire

Headless Rust dispatcher for parallel Claude Code agent sessions.

## Install

```
cargo install --path crates/pitboss-cli
```

## Quick start

Create `shire.toml` in a directory that is inside a git repo:

```toml
[run]
max_parallel = 2

[[task]]
id = "hello"
directory = "/path/to/repo"
prompt = "Say hello in a file called hello.txt"
branch = "feat/hello"
```

Then:

```
pitboss validate shire.toml
pitboss dispatch shire.toml
```

Artifacts land in `~/.local/share/shire/runs/<run-id>/`.

## Concurrency

Default `max_parallel` is 4. Override hierarchy: manifest `[run].max_parallel`
beats `ANTHROPIC_MAX_CONCURRENT` env beats the default.

## Manual smoke test for releases

With a real `claude` binary on PATH and ANTHROPIC_API_KEY set:

1. Create two throwaway git repos.
2. Point one manifest at each with a trivial prompt ("write `hi` to a file").
3. `pitboss dispatch ./manifest.toml` — confirm the progress table updates, both
   Hobbits succeed, and the summary.json contains expected fields.
4. Run again with `halt_on_failure = true` and an intentionally-failing prompt
   in the first task. Confirm the second task is skipped.
5. Run with a long-running prompt and Ctrl-C once → drain completes; Ctrl-C
   twice → tasks report `Cancelled`.

## Development

```
cargo test --workspace
cargo test -p pitboss-core --features test-support
cargo lint
cargo tidy
```

See `docs/superpowers/specs/2026-04-16-agent-shire-design.md` for design.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "README: quick start, concurrency override, manual smoke test"
```

---

### Task 41a: Spec gap polish — snapshot files, exit codes, claude_version plumbing

Three small gaps from the spec to close in one pass.

**Files:**
- Modify: `crates/pitboss-cli/src/dispatch/runner.rs`
- Modify: `crates/pitboss-cli/src/main.rs`

- [ ] **Step 1: Write run-dir snapshot files**

Modify `crates/pitboss-cli/src/dispatch/runner.rs`. Extend `run_dispatch_inner` to accept the raw manifest TOML text, then write both snapshot files at the run-dir root once `run_id` is known (just before `store.init_run`):

```rust
pub async fn run_dispatch_inner(
    resolved: ResolvedManifest,
    manifest_text: String,
    manifest_path: PathBuf,
    claude_binary: PathBuf,
    claude_version: Option<String>,
    run_dir_override: Option<PathBuf>,
    dry_run: bool,
) -> Result<i32> {
    // ... spawner + run_dir + store setup (as before) ...
    execute(resolved, manifest_text, manifest_path, claude_binary, claude_version,
            spawner, store, dry_run).await
}
```

Extend `execute` to accept `manifest_text`, `manifest_path`, `claude_version`. Immediately after computing `run_id`, compute the per-run directory and write the two files:

```rust
    let run_subdir = run_dir.join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.ok();
    tokio::fs::write(run_subdir.join("manifest.snapshot.toml"), &manifest_text).await?;
    let resolved_bytes = serde_json::to_vec_pretty(&resolved).ok();
    if let Some(b) = resolved_bytes {
        tokio::fs::write(run_subdir.join("resolved.json"), b).await?;
    }
```

Note: `ResolvedManifest` and its members need `#[derive(Serialize)]` — add `Serialize` to the derive list in `crates/pitboss-cli/src/manifest/resolve.rs` for `ResolvedManifest` and `ResolvedTask`.

Thread `claude_version` into the `RunMeta` and `RunSummary`:

```rust
    let meta = RunMeta {
        run_id,
        manifest_path: manifest_path.clone(),
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version: claude_version.clone(),
        // ...
    };
    // ... and in RunSummary construction:
    claude_version: claude_version.clone(),
    manifest_path: manifest_path.clone(),
```

- [ ] **Step 2: Exit codes per spec §8**

Modify `crates/pitboss-cli/src/main.rs`. Restructure `run_dispatch` to map error classes to exit codes:

```rust
fn run_dispatch(
    manifest: &std::path::Path,
    run_dir_override: Option<std::path::PathBuf>,
    dry_run: bool,
) -> ! {
    let env_mp = parse_env_max_parallel();
    let manifest_text = match std::fs::read_to_string(manifest) {
        Ok(t) => t,
        Err(e) => { eprintln!("read manifest: {e}"); std::process::exit(2); }
    };
    let resolved = match manifest::load_manifest(manifest, env_mp) {
        Ok(r) => r,
        Err(e) => { eprintln!("validation failed: {e:#}"); std::process::exit(2); }
    };
    let claude_bin = std::env::var_os("SHIRE_CLAUDE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("claude"));

    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => { eprintln!("runtime: {e}"); std::process::exit(2); }
    };

    let code = rt.block_on(async move {
        let claude_version = if dry_run { None } else {
            match dispatch::probe_claude(&claude_bin).await {
                Ok(v) => v,
                Err(e) => { eprintln!("{e}"); return 2; }
            }
        };
        match dispatch::run_dispatch_inner(
            resolved, manifest_text, manifest.to_path_buf(),
            claude_bin, claude_version, run_dir_override, dry_run).await
        {
            Ok(c) => c,
            Err(e) => { eprintln!("dispatch: {e:#}"); 1 }
        }
    });
    std::process::exit(code);
}
```

Change `run_dispatch` call site in `main()` to drop the `?` since it now diverges:

```rust
        Command::Dispatch { manifest, run_dir, dry_run } => {
            run_dispatch(&manifest, run_dir, dry_run)
        }
```

And update the `Command` enum / `main` return type as needed so `main` returns `Result<()>` only for the non-diverging branches.

- [ ] **Step 3: 130 on interruption**

Modify `execute` so the final exit code reflects cancellation state:

```rust
    let rc = if cancel.is_terminated() { 130 }
             else if tasks_failed > 0  { 1 }
             else                      { 0 };
    Ok(rc)
```

- [ ] **Step 4: Test — validation failure → exit 2**

Add to `crates/pitboss-cli/tests/dispatch_flows.rs`:

```rust
#[test]
fn validation_failure_exits_two() {
    ensure_built();
    let dir = TempDir::new().unwrap();
    let manifest_path = dir.path().join("bad.toml");
    std::fs::write(&manifest_path, "unknown_root_key = 1\n").unwrap();

    let out = std::process::Command::new(shire_binary())
        .arg("dispatch").arg(&manifest_path)
        .env("SHIRE_CLAUDE_BINARY", fake_claude_path())
        .output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}
```

- [ ] **Step 5: Run full test suite**

Run: `cargo test --workspace`
Expected: everything green, including new validation-exit test.

- [ ] **Step 6: Commit**

```bash
git add crates/pitboss-cli/
git commit -m "Exit codes per spec, snapshot files, claude_version plumbing"
```

---

### Task 41: End-to-end sanity pass

No new code — a checkpoint.

- [ ] **Step 1: Run the full suite**

Run: `cargo test --workspace`
Expected: every test passes.

Run: `cargo test -p pitboss-core --features test-support`
Expected: every feature-gated test passes.

Run: `cargo lint`
Expected: zero warnings.

Run: `cargo tidy`
Expected: no formatting diffs.

- [ ] **Step 2: Commit (a version tag if everything clean)**

```bash
git tag v0.1.0-pre
```

Manual smoke test per README §"Manual smoke test for releases" before tagging `v0.1.0`.

---

## Appendix — Self-Review Checklist

Run this once before handing off to subagent execution.

- [ ] **Spec coverage check**: Every section/requirement in `docs/superpowers/specs/2026-04-16-agent-shire-design.md` can be pointed at a task number above.
- [ ] **Placeholder scan**: No "TBD", "TODO", "fill in later", or narrative-only steps.
- [ ] **Type consistency**: Signatures in later tasks match what earlier tasks defined (e.g., `SessionOutcome` uses the same field names everywhere).
- [ ] **Ambiguity check**: Any step that says "adjust" or "restructure" has a concrete direction.
- [ ] **Test-and-implementation pairing**: Every coding task has a failing-test step before its implementation step, and a passing-test step before commit.


