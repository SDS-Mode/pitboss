//! Auto-generates `docs/manifest-reference.toml` from the [`super::metadata`]
//! registry.
//!
//! Unlike the hand-curated [`pitboss.example.toml`] at the repo root (which is
//! organised pedagogically and uses commented-out fields to highlight defaults),
//! this artifact emits every field as an uncommented `key = value` assignment
//! using a placeholder derived from the field's [`FormType`]. Operators copy it
//! as a starting point and replace placeholders with real values.
//!
//! Drift is verified by [`tests::checked_in_doc_matches_generator`] and by the
//! CLI's `pitboss schema --format=example --check docs/manifest-reference.toml`
//! invocation in CI.

use std::fmt::Write as _;

use pitboss_schema::{FieldDescriptor, FormType, SchemaSection};

use super::metadata::sections;

/// Render the full reference-TOML body. Pure function — same input always
/// produces identical output, which the `--check` mode relies on.
pub fn render() -> String {
    let mut out = String::new();
    write_header(&mut out);
    for section in sections() {
        render_section(&mut out, &section);
    }
    out
}

fn write_header(out: &mut String) {
    out.push_str(
        "# Pitboss manifest — complete reference\n\
         #\n\
         # Auto-generated. Do not edit by hand. Regenerate with:\n\
         #   pitboss schema --format=example > docs/manifest-reference.toml\n\
         #\n\
         # Every field declared in the v0.9 schema is present below as an\n\
         # uncommented `key = value` assignment using a placeholder derived\n\
         # from the field's form type. This file demonstrates *structure*; it\n\
         # is NOT a runnable manifest:\n\
         #   * `[lead]` and `[[task]]` are mutually exclusive — pick one.\n\
         #   * `[[mcp_server]]` / `[[approval_policy]]` / `[[template]]` are\n\
         #     repeating sections; one example entry is shown.\n\
         #   * Sub-sections that aren't surfaced through the FieldMetadata\n\
         #     registry yet (e.g. `[approval_policy.match]`, `[[notification]]`)\n\
         #     are omitted; see `pitboss.example.toml` for hand-curated\n\
         #     coverage and `book/src/operator-guide/manifest-schema.md` for\n\
         #     prose explanations.\n\
         #\n\
         # Each row's source-of-truth lives in\n\
         # `crates/pitboss-cli/src/manifest/schema.rs`. The companion field\n\
         # map at `docs/manifest-map.md` provides clickable file:line refs.\n\
         \n",
    );
}

fn render_section(out: &mut String, section: &SchemaSection) {
    let _ = writeln!(out, "{}", section.toml_path);
    for f in section.fields {
        render_field_line(out, f);
    }
    out.push('\n');
}

fn render_field_line(out: &mut String, f: &FieldDescriptor) {
    let value = render_placeholder(f);
    let _ = writeln!(out, "{} = {}", f.name, value);
}

/// Map a [`FieldDescriptor`] to a TOML right-hand-side literal.
///
/// `enum_values` (when present) wins — the first variant is emitted as a
/// quoted string. Otherwise the placeholder is derived from `form_type`.
fn render_placeholder(f: &FieldDescriptor) -> String {
    if !f.enum_values.is_empty() {
        return format!("\"{}\"", f.enum_values[0]);
    }
    match f.form_type {
        FormType::Text => "\"<value>\"".to_string(),
        FormType::LongText => "\"\"\"\n  Replace with the actual prompt body.\n\"\"\"".to_string(),
        FormType::Integer => "0".to_string(),
        FormType::Float => "0.0".to_string(),
        FormType::Boolean => "false".to_string(),
        FormType::Path => "\"/path/to/value\"".to_string(),
        // Reachable only when enum_values is empty — keep a sentinel so a
        // missing `enum_values` annotation surfaces visibly in the output.
        FormType::EnumSelect => "\"<enum>\"".to_string(),
        FormType::StringList => "[]".to_string(),
        FormType::KeyValueMap => "{ EXAMPLE_KEY = \"value\" }".to_string(),
        // `FormType` is `#[non_exhaustive]`; a new variant should land
        // here with an explicit placeholder rather than silently emitting
        // a generic `<value>` token.
        _ => format!("\"<{}>\"", f.form_type.as_str()),
    }
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
                "rendered reference missing section header for {}",
                section.toml_path
            );
        }
    }

    /// The emitted doc must contain every registered field as a `key =`
    /// assignment. Catches regressions where a section is rendered with the
    /// header but no field rows.
    #[test]
    fn render_contains_every_field() {
        let doc = render();
        let mut missing: Vec<String> = Vec::new();
        for section in sections() {
            for f in section.fields {
                let needle = format!("\n{} = ", f.name);
                if !doc.contains(&needle) {
                    missing.push(format!("{}.{}", section.type_name, f.name));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "fields missing from reference output: {:?}",
            missing
        );
    }

    /// Enum fields must use the first declared `enum_values` variant (quoted)
    /// rather than the generic `"<enum>"` sentinel. Drift guard against the
    /// `enum_values` priority branch in `render_placeholder` regressing.
    #[test]
    fn enum_fields_use_first_variant() {
        let doc = render();
        // worktree_cleanup declares ["always", "on_success", "never"].
        assert!(
            doc.contains("worktree_cleanup = \"always\""),
            "expected worktree_cleanup to render as \"always\", got:\n{}",
            doc
        );
        // permission_routing declares ["path_a", "path_b"].
        assert!(
            doc.contains("permission_routing = \"path_a\""),
            "expected permission_routing to render as \"path_a\""
        );
    }

    /// `Lead.prompt` is `LongText` — the emitter must use a triple-quoted
    /// multi-line string so newlines in the placeholder don't break the
    /// surrounding TOML.
    #[test]
    fn long_text_uses_triple_quoted_block() {
        let doc = render();
        assert!(
            doc.contains("prompt = \"\"\"\n"),
            "expected prompt to use triple-quoted block; got:\n{}",
            doc
        );
    }

    /// `HashMap<String, String>` fields must emit an inline table — not an
    /// empty `{}` and not a separate `[parent.env]` sub-section (which would
    /// disrupt the linear emission order).
    #[test]
    fn key_value_maps_render_as_inline_tables() {
        let doc = render();
        assert!(
            doc.contains("env = { EXAMPLE_KEY = \"value\" }"),
            "expected env field to render as inline table"
        );
    }

    /// The full output must be valid TOML so operators can copy a section
    /// out and parse it directly. We chunk per-section because `[lead]` and
    /// `[[task]]` are mutually exclusive (the doc explains this in the
    /// header) and toml-rs will accept both as a single document only by
    /// coincidence — we want a stronger guarantee here.
    #[test]
    fn whole_document_parses_as_toml() {
        let doc = render();
        // The header comment + all sections together must parse. The
        // mutually-exclusive `[lead]` / `[[task]]` sections coexist as
        // valid TOML even if `pitboss validate` would reject the
        // combination as a manifest — that's fine for a reference doc.
        let _: toml::Value = toml::from_str(&doc)
            .unwrap_or_else(|e| panic!("reference doc must parse as TOML: {e}\n---\n{doc}"));
    }

    /// Drift guard: the checked-in `docs/manifest-reference.toml` must
    /// match what the generator currently emits. Mirrored at the CLI
    /// surface as `pitboss schema --format=example --check
    /// docs/manifest-reference.toml`.
    ///
    /// Comparison strips CR before LF in the checked-in file so a
    /// `core.autocrlf=true` checkout doesn't false-positive every run. The
    /// generator only emits LF; any CR appearing here came from git, not
    /// from drift.
    #[test]
    fn checked_in_doc_matches_generator() {
        const CHECKED_IN: &str = include_str!("../../../../docs/manifest-reference.toml");
        let generated = render();
        let normalized: String = CHECKED_IN.replace("\r\n", "\n");
        if generated != normalized {
            panic!(
                "docs/manifest-reference.toml is stale.\n\
                 Regenerate with:\n\
                 \n    cargo run -q --release -p pitboss-cli -- schema --format=example > docs/manifest-reference.toml\n"
            );
        }
    }
}
