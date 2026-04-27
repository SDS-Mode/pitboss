//! Auto-generates `docs/manifest-map.md` from the [`super::metadata`] registry.
//!
//! For every `[section]` in the v0.9 schema, the generator emits a markdown
//! section containing per-field code references back to the Rust definitions
//! in `super::schema`. The struct + field line numbers are resolved by a
//! one-shot scan over a compile-time `include_str!` of `schema.rs` — this
//! avoids the unstable `proc_macro2::Span::source_file()` API while still
//! producing real, navigable line numbers.
//!
//! See `pitboss schema --help` for the user-facing CLI surface.

use std::collections::HashMap;
use std::fmt::Write as _;

use pitboss_schema::{FieldDescriptor, FormType, SchemaSection};

use super::metadata::sections;

/// Compile-time copy of the schema source. Lets us resolve `(struct, field)
/// → line number` without needing the source tree at runtime.
const SCHEMA_SOURCE: &str = include_str!("schema.rs");

/// Path that ends up in every `[file:line]` link. The doc lives at
/// `docs/manifest-map.md`, so links are `../`-relative to walk up to repo
/// root and back into `crates/`.
const SCHEMA_PATH: &str = "../crates/pitboss-cli/src/manifest/schema.rs";

/// Render the full `manifest-map.md` document body. Pure function — same
/// inputs always produce identical output, which is what the `--check` mode
/// in [`super::super::cli`] relies on.
pub fn render() -> String {
    let line_index = build_line_index(SCHEMA_SOURCE);
    let mut out = String::new();
    write_header(&mut out);
    for section in sections() {
        render_section(&mut out, &section, &line_index);
    }
    out
}

fn write_header(out: &mut String) {
    out.push_str(
        "# Pitboss manifest map\n\
         \n\
         > **Auto-generated. Do not edit by hand.** Regenerate with:\n\
         > ```\n\
         > pitboss schema --format=map > docs/manifest-map.md\n\
         > ```\n\
         > CI verifies the checked-in file matches the generator output via\n\
         > `pitboss schema --format=map --check docs/manifest-map.md`.\n\
         \n\
         This document maps every TOML field in the v0.9 manifest schema to its\n\
         Rust struct field and source location. For schema *explanations* see\n\
         [`book/src/operator-guide/manifest-schema.md`](../book/src/operator-guide/manifest-schema.md).\n\
         The annotated example lives at [`pitboss.example.toml`](../pitboss.example.toml).\n\
         \n",
    );
}

fn render_section(out: &mut String, section: &SchemaSection, lines: &LineIndex) {
    let struct_line = lines.struct_line(section.type_name);
    let _ = writeln!(
        out,
        "## `{}` — `{}`\n",
        section.toml_path, section.type_name
    );
    if let Some(line) = struct_line {
        let _ = writeln!(
            out,
            "Defined at [`{path}:{line}`]({path}#L{line}).\n",
            path = SCHEMA_PATH,
            line = line,
        );
    }

    out.push_str("| Field | Type | Required | Help | Source |\n");
    out.push_str("|---|---|---|---|---|\n");
    for f in section.fields {
        render_field_row(out, section.type_name, f, lines);
    }
    out.push('\n');
}

fn render_field_row(out: &mut String, type_name: &str, f: &FieldDescriptor, lines: &LineIndex) {
    let req = if f.required { "**yes**" } else { "no" };
    let ty = format_type_cell(f);
    let help = if f.help.is_empty() {
        "—".to_string()
    } else {
        escape_pipes(f.help)
    };
    let src = match lines.field_line(type_name, f.name) {
        Some(line) => format!(
            "[`{path}#L{line}`]({path}#L{line})",
            path = SCHEMA_PATH,
            line = line,
        ),
        None => "—".to_string(),
    };
    let _ = writeln!(
        out,
        "| `{}` | {} | {} | {} | {} |",
        f.name, ty, req, help, src
    );
}

fn format_type_cell(f: &FieldDescriptor) -> String {
    let base = match f.form_type {
        FormType::Text => "text".to_string(),
        FormType::LongText => "text (multi-line)".to_string(),
        FormType::Integer => "integer".to_string(),
        FormType::Float => "float".to_string(),
        FormType::Boolean => "boolean".to_string(),
        FormType::Path => "path".to_string(),
        FormType::EnumSelect => "enum".to_string(),
        FormType::StringList => "string list".to_string(),
        FormType::KeyValueMap => "key-value map".to_string(),
        // FormType is `#[non_exhaustive]` so a future variant added in
        // `pitboss-schema` doesn't silently break this generator.
        _ => f.form_type.as_str().to_string(),
    };
    if !f.enum_values.is_empty() {
        let opts = f
            .enum_values
            .iter()
            .map(|v| format!("`{v}`"))
            .collect::<Vec<_>>()
            .join(" \\| ");
        format!("{base} ({opts})")
    } else {
        base
    }
}

/// Replace `|` with `\|` so help text containing pipes doesn't break the
/// surrounding markdown table. Intentionally minimal — the field metadata
/// is curated, not user-supplied.
fn escape_pipes(s: &str) -> String {
    s.replace('|', "\\|")
}

// ─── Line resolution ────────────────────────────────────────────────────────

/// Maps `(struct_name, field_name)` to source line numbers within
/// `schema.rs`. Built once per render call.
struct LineIndex {
    structs: HashMap<String, usize>,
    fields: HashMap<(String, String), usize>,
}

impl LineIndex {
    fn struct_line(&self, name: &str) -> Option<usize> {
        self.structs.get(name).copied()
    }
    fn field_line(&self, struct_name: &str, field_name: &str) -> Option<usize> {
        self.fields
            .get(&(struct_name.to_string(), field_name.to_string()))
            .copied()
    }
}

/// Parse `schema.rs` once to build a struct→line and (struct, field)→line
/// index. The grammar handled is intentionally narrow — only `pub struct
/// Name { … }` blocks with `pub field:` declarations. `#[serde(rename = …)]`
/// is honored so the TOML key (which is what the metadata registry uses)
/// resolves to the line of the underlying Rust field.
fn build_line_index(src: &str) -> LineIndex {
    let mut structs = HashMap::new();
    let mut fields = HashMap::new();

    let mut current_struct: Option<String> = None;
    let mut brace_depth: i32 = 0;
    let mut pending_rename: Option<String> = None;

    for (idx, raw_line) in src.lines().enumerate() {
        let lineno = idx + 1;
        let line = raw_line.trim();

        if let Some(struct_name) = parse_struct_decl(line) {
            current_struct = Some(struct_name.clone());
            structs.insert(struct_name, lineno);
            brace_depth = if line.contains('{') { 1 } else { 0 };
            pending_rename = None;
            continue;
        }

        if current_struct.is_some() {
            // Skip doc-comment / line-comment lines entirely so a `///` block
            // containing braces (e.g. an example showing a struct literal)
            // doesn't disturb depth tracking and falsely close the body. The
            // narrow grammar this scanner accepts already excludes comments
            // from being struct or field declarations, so the only effect of
            // counting braces inside them was to corrupt the depth state.
            let is_comment_line = line.starts_with("//") || line.starts_with("///");
            if !is_comment_line {
                // Track depth so we know when to leave the struct body.
                for c in line.chars() {
                    match c {
                        '{' => brace_depth += 1,
                        '}' => brace_depth -= 1,
                        _ => {}
                    }
                }
            }
            if brace_depth <= 0 {
                current_struct = None;
                pending_rename = None;
                brace_depth = 0;
                continue;
            }

            if let Some(rename) = parse_serde_rename(line) {
                pending_rename = Some(rename);
                continue;
            }

            if let Some(field_name) = parse_field_decl(line) {
                let key_name = pending_rename.take().unwrap_or(field_name);
                if let Some(s) = current_struct.as_ref() {
                    fields.insert((s.clone(), key_name), lineno);
                }
            }
        }
    }

    LineIndex { structs, fields }
}

/// Return `Some("RunConfig")` for `pub struct RunConfig {`.
fn parse_struct_decl(line: &str) -> Option<String> {
    let s = line.strip_prefix("pub struct ")?;
    let name_end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    let name = &s[..name_end];
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// Return `Some("max_workers")` for `pub max_workers: Option<u32>,`.
/// Skips `#[…]` lines and comments by virtue of the `pub` prefix.
fn parse_field_decl(line: &str) -> Option<String> {
    let s = line.strip_prefix("pub ")?;
    let name_end = s.find(':')?;
    let name = &s[..name_end].trim();
    // Avoid catching `pub struct …` blocks (already handled) and
    // `pub fn …` items.
    if name.is_empty() || name.contains(' ') {
        return None;
    }
    Some(name.to_string())
}

/// Parse `#[serde(rename = "task")]` (and `#[serde(default, rename = "x")]`)
/// to yield `Some("task")`. Returns `None` for unrelated attributes.
fn parse_serde_rename(line: &str) -> Option<String> {
    if !line.starts_with("#[serde(") {
        return None;
    }
    let rename_start = line.find("rename")?;
    let after = &line[rename_start..];
    let eq = after.find('=')?;
    let after_eq = &after[eq + 1..];
    let q1 = after_eq.find('"')?;
    let after_q1 = &after_eq[q1 + 1..];
    let q2 = after_q1.find('"')?;
    Some(after_q1[..q2].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke: render must succeed and contain every section's TOML path.
    #[test]
    fn render_contains_every_section() {
        let doc = render();
        for section in sections() {
            assert!(
                doc.contains(section.toml_path),
                "rendered doc missing section header for {}",
                section.toml_path
            );
        }
    }

    /// Drift guard: the generator must locate at least one source line for
    /// every section's struct. If this fails, the schema scanner regressed.
    #[test]
    fn every_struct_resolves_to_a_line() {
        let lines = build_line_index(SCHEMA_SOURCE);
        for section in sections() {
            assert!(
                lines.struct_line(section.type_name).is_some(),
                "no source line found for struct {}",
                section.type_name
            );
        }
    }

    /// Drift guard: every registered field must resolve to a source line.
    /// Catches: serde renames the macro can't see, fields removed from
    /// schema.rs but left in the registry, scanner-grammar bugs.
    #[test]
    fn every_field_resolves_to_a_line() {
        let lines = build_line_index(SCHEMA_SOURCE);
        let mut missing: Vec<String> = Vec::new();
        for section in sections() {
            for f in section.fields {
                if lines.field_line(section.type_name, f.name).is_none() {
                    missing.push(format!("{}.{}", section.type_name, f.name));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "fields with no source line: {:?}",
            missing
        );
    }

    /// Drift guard: the checked-in `docs/manifest-map.md` must match what
    /// the generator currently emits. Runs as a fast unit test (no
    /// subprocess), and is also wired up to the CLI as
    /// `pitboss schema --format=map --check docs/manifest-map.md` for
    /// CI / contributor use.
    ///
    /// Comparison is done against a CRLF-stripped copy of the checked-in
    /// file: contributors on Windows whose git config has `core.autocrlf=true`
    /// would otherwise see this fail on every clone with a diff that's
    /// invisible in their editor. Drift over actual content is unaffected —
    /// the generator only ever emits LF, so any CR appearing here came from
    /// the checkout, not the source of truth.
    #[test]
    fn checked_in_doc_matches_generator() {
        // `include_str!` resolves relative to *this file's* directory.
        // From `crates/pitboss-cli/src/manifest/map_doc.rs` to
        // `<repo>/docs/manifest-map.md` is six "../" hops.
        const CHECKED_IN: &str = include_str!("../../../../docs/manifest-map.md");
        let generated = render();
        let normalized: String = CHECKED_IN.replace("\r\n", "\n");
        if generated != normalized {
            panic!(
                "docs/manifest-map.md is stale.\n\
                 Regenerate with:\n\
                 \n    cargo run -q --release -p pitboss-cli -- schema --format=map > docs/manifest-map.md\n"
            );
        }
    }

    /// `mounts: Vec<MountSpec>` is `#[serde(rename = "mount")]` — the rename
    /// parser must surface that so the registry's `mount` field name
    /// resolves to the right line.
    #[test]
    fn serde_rename_is_honored() {
        // `mount` is on ContainerConfig.mounts. The registry tags it
        // #[field(skip)] so it doesn't appear in sections() — but the
        // line-index should still see it via the rename path. Probe the
        // helper directly.
        let lines = build_line_index(SCHEMA_SOURCE);
        assert!(
            lines.field_line("ContainerConfig", "mount").is_some(),
            "expected ContainerConfig.mount (renamed from `mounts`) to be indexed"
        );
    }
}
