//! Integration tests for `#[derive(FieldMetadata)]` exercising the bug
//! fixes from #159: `serde(default)` token-walk false positives,
//! duplicate-key detection, and direct-variant emission.

use pitboss_schema::{FieldMetadata, FormType};

#[derive(FieldMetadata)]
#[allow(dead_code)]
struct Basic {
    /// Optional field — should be marked non-required without an explicit
    /// `#[field(required = false)]`.
    opt: Option<String>,
    /// Required field — String + no serde-default.
    req: String,
}

#[test]
fn option_fields_are_optional() {
    let meta = Basic::field_metadata();
    assert_eq!(meta[0].name, "opt");
    assert!(!meta[0].required, "Option<T> ⇒ optional");
    assert_eq!(meta[1].name, "req");
    assert!(meta[1].required, "String + no serde default ⇒ required");
}

#[test]
fn form_type_emitted_as_direct_variant() {
    // After the #159 fix, the macro emits `FormType::Path` directly
    // rather than `FormType::from_str("path")`. Verify the variant is
    // the inferred one, not the silent Text fallback.
    let meta = WithPath::field_metadata();
    assert_eq!(meta[0].form_type, FormType::Path);
}

#[derive(FieldMetadata)]
#[allow(dead_code)]
struct WithPath {
    p: std::path::PathBuf,
}

// ── #159: `serde(default)` token-walk false positives ──────────────────

/// Reproduces the regression: `serde(skip_serializing_if = "...")` carries
/// the predicate as a *string literal*, but in earlier macro builds the
/// raw token-tree walk also picked up identifiers from `serde(rename = ...)`
/// or paths in adjacent meta items. The fixed parser parses serde's
/// nested meta as a real comma-separated `Meta` list and only matches
/// top-level `default` paths.
///
/// Construct a serde attribute whose contents include a `default`
/// substring inside a string-literal predicate AND a non-default sibling
/// directive — both should be ignored.
#[derive(FieldMetadata, serde::Serialize)]
#[allow(dead_code)]
struct DefaultStringInValueDoesNotTriggerDefault {
    #[serde(skip_serializing_if = "default_predicate", rename = "p")]
    payload: String,
}

#[allow(dead_code)]
fn default_predicate(_: &String) -> bool {
    false
}

#[test]
fn serde_string_with_default_substring_does_not_count_as_default() {
    let meta = DefaultStringInValueDoesNotTriggerDefault::field_metadata();
    assert!(
        meta[0].required,
        "string literals containing the word \"default\" inside other \
         serde directives must NOT be treated as `#[serde(default)]`; \
         the field stays required"
    );
}

#[derive(FieldMetadata, serde::Deserialize)]
#[allow(dead_code)]
struct WithBareDefault {
    #[serde(default)]
    flag: bool,
}

#[test]
fn bare_serde_default_marks_field_optional() {
    let meta = WithBareDefault::field_metadata();
    assert!(!meta[0].required, "#[serde(default)] ⇒ optional");
}

#[derive(FieldMetadata, serde::Deserialize)]
#[allow(dead_code)]
struct WithFnDefault {
    #[serde(default = "WithFnDefault::default_count")]
    count: u32,
}

impl WithFnDefault {
    fn default_count() -> u32 {
        4
    }
}

#[test]
fn fn_serde_default_marks_field_optional() {
    let meta = WithFnDefault::field_metadata();
    assert!(!meta[0].required, "#[serde(default = \"fn\")] ⇒ optional");
}

#[derive(FieldMetadata, serde::Deserialize)]
#[allow(dead_code)]
struct DefaultMixedWithRename {
    #[serde(default, rename = "name")]
    title: String,
}

#[test]
fn combined_default_with_rename_still_optional() {
    let meta = DefaultMixedWithRename::field_metadata();
    assert!(!meta[0].required);
}
