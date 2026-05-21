//! Wire-compat content-guard test (closes #577).
//!
//! Mechanizes one corner of the convention documented at the top of
//! `crates/pitboss-cli/src/control/protocol.rs` (lines 8-34): every
//! field of a **nested payload struct** embedded in a `ControlOp` /
//! `ControlEvent` variant must carry one of:
//!
//!   - `#[serde(default)]`
//!   - `#[serde(default = "fn")]`
//!   - `#[serde(default, skip_serializing_if = "...")]`
//!   - `#[serde(skip)]` (the field is never on the wire)
//!   - `#[serde(flatten)]` (delegates to the inner type's discriminator)
//!
//! ## Why nested-struct scope, not inline-variant-field scope
//!
//! The contract's prose targets every new field, including inline
//! variant fields like `SubleadSpawned.read_down` (PR #566). But the
//! pre-contract baseline of inline variant fields is large
//! (~30 fields) and grandfathering them all behind an allowlist
//! creates a high-maintenance review surface that contributors will
//! eventually game.
//!
//! The recurrence pattern that drove this guard (see #577) is
//! specifically nested-payload structs (`RunFinishedSummary`,
//! `WorkerSnapshotEntry`, `ApprovalPlanWire`) where the field is in
//! a separate type definition, requiring a separate file-read to
//! inspect — exactly the slippage that reviewers and audit tickets
//! keep catching late. Inline variant fields are at least visible at
//! the variant declaration site.
//!
//! Inline-variant-field omissions are still caught by reviewers and
//! historical audit tickets — this test guards the harder-to-spot
//! class.
//!
//! ## How it works
//!
//! 1. Parse `protocol.rs` via `syn::parse_file`.
//! 2. Find every enum decorated with `#[serde(tag = "...")]` (the
//!    wire-protocol enums).
//! 3. For each variant, **don't check the variant's own fields**
//!    (those are the inline-variant scope, out of this test's
//!    remit) — instead, walk each field's type and recurse into
//!    any locally-defined struct it references.
//! 4. For each discovered struct, check that every named field
//!    carries one of the acceptable serde attributes.
//! 5. Identity-bearing fields that must fail-loudly on absence
//!    (e.g. `ActorActivityEntry.actor_id`) live in `EXEMPT_FIELDS`
//!    with a documented reason.
//!
//! ## Companion check: enum-typed field types (#580 FU-5)
//!
//! `every_nested_struct_field_in_wire_variants_has_back_compat_attr`
//! verifies the field has a back-compat attribute. It does NOT
//! verify the field's type can actually decode when absent — for
//! enum-typed fields, "decodes when absent" requires the enum to
//! be `Default`-reachable (`#[derive(Default)]` + a `#[default]`
//! variant). A second test,
//! `enums_used_as_wire_field_types_are_default_reachable`, closes
//! this gap by walking every locally-defined enum in `protocol.rs`
//! and asserting it's `Default`-reachable whenever it's referenced
//! as a field type on a struct or an inline variant. Fields wrapped
//! in `Option<E>` (`None` is the default) and fields carrying
//! `#[serde(default = "fn")]` (explicit defaulter) are exempt.

use std::collections::{BTreeMap, BTreeSet};

use syn::{Attribute, Field, Fields, File, GenericArgument, Item, ItemEnum, ItemStruct, Type};

const PROTOCOL_RS: &str = include_str!("../src/control/protocol.rs");

/// Field paths intentionally exempt from the contract.
///
/// Each entry is `"StructName.field_name"` plus a one-line reason.
/// Add a new exemption only when the field has a documented design
/// reason for failing-loudly on absence rather than defaulting
/// silently — e.g. identity-bearing fields where `""` or `0` would
/// be a silent misclassification.
const EXEMPT_FIELDS: &[(&str, &str)] = &[
    (
        "ActorActivityEntry.actor_id",
        "identity-bearing — must fail-loud on absence rather than default to empty string",
    ),
    (
        "ResourceSampleEntry.actor_id",
        "identity-bearing — same rationale as ActorActivityEntry.actor_id (#553)",
    ),
];

#[test]
fn every_nested_struct_field_in_wire_variants_has_back_compat_attr() {
    let parsed: File = syn::parse_file(PROTOCOL_RS).expect("parse protocol.rs as a Rust file");

    let structs_by_name: BTreeMap<String, &ItemStruct> = parsed
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Struct(s) = item {
                Some((s.ident.to_string(), s))
            } else {
                None
            }
        })
        .collect();

    let wire_enums: Vec<&ItemEnum> = parsed
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Enum(e) = item {
                if has_serde_tag(&e.attrs) {
                    Some(e)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    assert!(
        wire_enums.len() >= 2,
        "expected at least ControlOp + ControlEvent tagged enums in protocol.rs, found {}; \
         did the wire-protocol module move?",
        wire_enums.len()
    );

    // Validate every EXEMPT_FIELDS entry points at a real field. Without
    // this, a typo (e.g. `ActorActivityEntry.actorid` instead of
    // `actor_id`) would silently become a no-op exemption — the real
    // field would then either be flagged (best case) or, if also typo'd
    // in the source, missed entirely. The exemption mechanism exists
    // precisely as a deliberate review touchpoint; a typo'd exemption
    // defeats that purpose.
    for (path, _reason) in EXEMPT_FIELDS {
        let (struct_name, field_name) = path.split_once('.').unwrap_or_else(|| {
            panic!("EXEMPT_FIELDS entry {path:?} must be `StructName.field_name`")
        });
        let s = structs_by_name.get(struct_name).unwrap_or_else(|| {
            panic!(
                "EXEMPT_FIELDS: struct {struct_name:?} not found in protocol.rs \
                 (did the file move or the struct get renamed?)"
            )
        });
        let Fields::Named(named) = &s.fields else {
            panic!("EXEMPT_FIELDS: struct {struct_name:?} has no named fields");
        };
        assert!(
            named
                .named
                .iter()
                .any(|f| f.ident.as_ref().is_some_and(|i| i == field_name)),
            "EXEMPT_FIELDS: field {field_name:?} not found on struct {struct_name:?}"
        );
    }

    let exempt: BTreeSet<String> = EXEMPT_FIELDS.iter().map(|(p, _)| p.to_string()).collect();

    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut violations: Vec<String> = Vec::new();

    // Walk every variant of every wire enum. We deliberately DON'T
    // check the variant's own fields (those are inline-variant
    // scope) — only follow their types into local structs.
    for wire_enum in &wire_enums {
        for variant in &wire_enum.variants {
            let fields_iter: Box<dyn Iterator<Item = &Field>> = match &variant.fields {
                Fields::Named(named) => Box::new(named.named.iter()),
                Fields::Unnamed(unnamed) => Box::new(unnamed.unnamed.iter()),
                Fields::Unit => continue,
            };
            for field in fields_iter {
                follow_type(
                    &field.ty,
                    &structs_by_name,
                    &exempt,
                    &mut visited,
                    &mut violations,
                );
            }
        }
    }

    if !violations.is_empty() {
        let lines = violations
            .iter()
            .map(|v| format!("  - {v}"))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "\nwire-compat violations found in nested payload structs ({}):\n\n{lines}\n\n\
             Contract (control/protocol.rs:8-34): every field of a struct embedded in a\n  \
             ControlEvent/ControlOp variant must carry one of:\n    \
             #[serde(default)] | #[serde(default = \"fn\")] | #[serde(default, skip_serializing_if = \"...\")]\n    \
             #[serde(skip)] | #[serde(flatten)]\n\n\
             To exempt an identity-bearing field that must fail-loudly on absence, add an entry\n  \
             to EXEMPT_FIELDS at the top of this test with a documented reason.\n",
            violations.len(),
        );
    }
}

fn has_serde_tag(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("serde") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("tag") {
                found = true;
            }
            if let Ok(value) = meta.value() {
                let _ = value.parse::<syn::Expr>();
            }
            Ok(())
        });
        found
    })
}

fn follow_type(
    ty: &Type,
    structs_by_name: &BTreeMap<String, &ItemStruct>,
    exempt: &BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    violations: &mut Vec<String>,
) {
    let mut names: Vec<String> = Vec::new();
    collect_struct_names(ty, structs_by_name, &mut names);
    for name in names {
        if !visited.insert(name.clone()) {
            continue;
        }
        if let Some(s) = structs_by_name.get(&name) {
            check_struct(s, structs_by_name, exempt, visited, violations);
        }
    }
}

fn collect_struct_names(
    ty: &Type,
    structs_by_name: &BTreeMap<String, &ItemStruct>,
    out: &mut Vec<String>,
) {
    if let Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            let name = seg.ident.to_string();
            if structs_by_name.contains_key(&name) {
                out.push(name);
            }
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                for arg in &args.args {
                    if let GenericArgument::Type(inner) = arg {
                        collect_struct_names(inner, structs_by_name, out);
                    }
                }
            }
        }
    }
}

fn check_struct(
    s: &ItemStruct,
    structs_by_name: &BTreeMap<String, &ItemStruct>,
    exempt: &BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    violations: &mut Vec<String>,
) {
    let struct_name = s.ident.to_string();
    if let Fields::Named(named) = &s.fields {
        for field in &named.named {
            let field_name = field
                .ident
                .as_ref()
                .map(|i| i.to_string())
                .unwrap_or_default();
            let path = format!("{struct_name}.{field_name}");
            check_field(&path, field, exempt, violations);
            follow_type(&field.ty, structs_by_name, exempt, visited, violations);
        }
    }
}

fn check_field(path: &str, field: &Field, exempt: &BTreeSet<String>, violations: &mut Vec<String>) {
    if exempt.contains(path) {
        return;
    }
    if !has_back_compat_attr(&field.attrs) {
        violations.push(format!(
            "{path} has no acceptable serde attribute (default / skip / flatten / skip_serializing_if)"
        ));
    }
}

fn has_back_compat_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("serde") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("default")
                || meta.path.is_ident("skip_serializing_if")
                || meta.path.is_ident("skip")
                || meta.path.is_ident("flatten")
            {
                found = true;
            }
            if let Ok(value) = meta.value() {
                let _ = value.parse::<syn::Expr>();
            }
            Ok(())
        });
        found
    })
}

/// Sanity check that the guard would actually catch a regression: parse
/// a synthetic snippet that mirrors the F-PROTO-3 bug shape and verify
/// the violation surfaces. Without this, the main test could quietly
/// degrade to a no-op (e.g. if a future refactor renames `ControlEvent`
/// or removes the `#[serde(tag)]` attribute) and the regression class
/// would silently slip through CI again.
#[test]
fn guard_catches_synthetic_regression() {
    let synthetic = r#"
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum SyntheticEvent {
    Finished { summary: SyntheticSummary },
}

#[derive(Serialize, Deserialize)]
pub struct SyntheticSummary {
    pub total: usize,   // <-- intentionally missing #[serde(default)]
    pub failed: usize,  // <-- intentionally missing #[serde(default)]
}
"#;

    let parsed: File = syn::parse_file(synthetic).expect("parse synthetic snippet");

    let structs_by_name: BTreeMap<String, &ItemStruct> = parsed
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Struct(s) => Some((s.ident.to_string(), s)),
            _ => None,
        })
        .collect();

    let wire_enums: Vec<&ItemEnum> = parsed
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Enum(e) if has_serde_tag(&e.attrs) => Some(e),
            _ => None,
        })
        .collect();

    assert_eq!(wire_enums.len(), 1, "expected one tagged enum in synthetic");

    let exempt: BTreeSet<String> = BTreeSet::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut violations: Vec<String> = Vec::new();

    for variant in &wire_enums[0].variants {
        if let Fields::Named(named) = &variant.fields {
            for field in &named.named {
                follow_type(
                    &field.ty,
                    &structs_by_name,
                    &exempt,
                    &mut visited,
                    &mut violations,
                );
            }
        }
    }

    assert_eq!(
        violations.len(),
        2,
        "guard should flag both missing fields, got {violations:?}"
    );
    assert!(
        violations
            .iter()
            .any(|v| v.contains("SyntheticSummary.total")),
        "guard should name SyntheticSummary.total; got {violations:?}"
    );
    assert!(
        violations
            .iter()
            .any(|v| v.contains("SyntheticSummary.failed")),
        "guard should name SyntheticSummary.failed; got {violations:?}"
    );
}

// =================================================================
// #580 FU-5 — enum-typed wire field guard
// =================================================================

/// Enums intentionally exempt from the Default-reachability check.
///
/// Each entry is `"EnumName"` plus a one-line reason. Add an entry
/// only when an enum has a documented design reason for failing-loudly
/// when absent rather than defaulting silently.
const EXEMPT_ENUMS: &[(&str, &str)] = &[
    // (currently empty — every payload enum in protocol.rs is
    // Default-reachable. Entries are added here as the rare exception,
    // not the rule.)
];

/// Every locally-defined enum that appears as a field type on a
/// `ControlOp` / `ControlEvent` variant (inline) or on a nested
/// payload struct must be **Default-reachable**: `#[derive(Default)]`
/// somewhere on its declaration AND a variant marked `#[default]`.
///
/// Without this guard, a contributor could declare:
///
/// ```ignore
/// enum NewKind { A, B }            // no Default
/// struct WirePayload {
///     #[serde(default)]            // ❌ won't compile, but…
///     kind: NewKind,
/// }
/// ```
///
/// …or worse, leave the field bare (no `#[serde(default)]`) so the
/// field becomes required on the wire — silently breaking every
/// older client. The sibling guard above checks fields-have-attrs;
/// this guard checks that the attrs would actually work for the
/// field's type.
///
/// Fields whose type is `Option<E>` are skipped — `None` is the
/// default. Fields carrying `#[serde(default = "fn")]` are skipped
/// — the explicit defaulter handles the absent case.
#[test]
fn enums_used_as_wire_field_types_are_default_reachable() {
    let parsed: File = syn::parse_file(PROTOCOL_RS).expect("parse protocol.rs as a Rust file");

    // Locally-defined enums minus the wire-tagged enums (ControlOp /
    // ControlEvent): tagged enums reject unknown discriminators
    // cleanly via a serde error — "defaulting" them is not a concept.
    let payload_enums: BTreeMap<String, &ItemEnum> = parsed
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Enum(e) if !has_serde_tag(&e.attrs) => Some((e.ident.to_string(), e)),
            _ => None,
        })
        .collect();

    let wire_enums: Vec<&ItemEnum> = parsed
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Enum(e) if has_serde_tag(&e.attrs) => Some(e),
            _ => None,
        })
        .collect();
    let structs: Vec<&ItemStruct> = parsed
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Struct(s) = item {
                Some(s)
            } else {
                None
            }
        })
        .collect();

    let exempt: BTreeSet<String> = EXEMPT_ENUMS.iter().map(|(n, _)| n.to_string()).collect();
    let mut violations: Vec<String> = Vec::new();

    // Nested struct fields.
    for s in &structs {
        if let Fields::Named(named) = &s.fields {
            for field in &named.named {
                let field_name = field
                    .ident
                    .as_ref()
                    .map(|i| i.to_string())
                    .unwrap_or_default();
                check_enum_field(
                    field,
                    &format!("{}.{field_name}", s.ident),
                    &payload_enums,
                    &exempt,
                    &mut violations,
                );
            }
        }
    }

    // Inline variant fields on wire enums — this is the surface the
    // sibling guard intentionally skips, and is exactly where the
    // motivating `level: PressureLevel` case lives.
    for we in &wire_enums {
        for variant in &we.variants {
            let fields_iter: Box<dyn Iterator<Item = &Field>> = match &variant.fields {
                Fields::Named(named) => Box::new(named.named.iter()),
                Fields::Unnamed(unnamed) => Box::new(unnamed.unnamed.iter()),
                Fields::Unit => continue,
            };
            for field in fields_iter {
                let field_name = field
                    .ident
                    .as_ref()
                    .map(|i| i.to_string())
                    .unwrap_or_default();
                check_enum_field(
                    field,
                    &format!("{}::{}.{field_name}", we.ident, variant.ident),
                    &payload_enums,
                    &exempt,
                    &mut violations,
                );
            }
        }
    }

    if !violations.is_empty() {
        let lines = violations
            .iter()
            .map(|v| format!("  - {v}"))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "\nnon-Default-reachable enum field types found ({}):\n\n{lines}\n\n\
             Every enum referenced as a wire field type must be Default-reachable\n  \
             (`#[derive(Default)]` on the enum AND a variant marked `#[default]`)\n  \
             so absence on the wire decodes cleanly. Alternatives:\n    \
             - wrap the field as `Option<E>` (None is the default), or\n    \
             - add `#[serde(default = \"fn\")]` to the field for an explicit defaulter, or\n    \
             - exempt the enum via EXEMPT_ENUMS with a documented fail-loud reason.\n",
            violations.len(),
        );
    }
}

fn check_enum_field(
    field: &Field,
    host_path: &str,
    payload_enums: &BTreeMap<String, &ItemEnum>,
    exempt: &BTreeSet<String>,
    violations: &mut Vec<String>,
) {
    if field_is_option(&field.ty) {
        return;
    }
    if has_explicit_default_fn(&field.attrs) {
        return;
    }
    let mut refs: BTreeSet<String> = BTreeSet::new();
    collect_enum_refs(&field.ty, payload_enums, &mut refs);
    for enum_name in refs {
        if exempt.contains(&enum_name) {
            continue;
        }
        let e = payload_enums
            .get(&enum_name)
            .expect("collect_enum_refs only emits keys present in payload_enums");
        if !enum_is_default_reachable(e) {
            violations.push(format!(
                "{host_path} uses non-Default-reachable enum {enum_name}"
            ));
        }
    }
}

fn enum_is_default_reachable(e: &ItemEnum) -> bool {
    let has_default_derive = e.attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("Default") {
                found = true;
            }
            Ok(())
        });
        found
    });
    let has_default_variant = e
        .variants
        .iter()
        .any(|v| v.attrs.iter().any(|a| a.path().is_ident("default")));
    has_default_derive && has_default_variant
}

fn field_is_option(ty: &Type) -> bool {
    if let Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            return seg.ident == "Option";
        }
    }
    false
}

/// Detect `#[serde(default = "fn_name")]` — the explicit-defaulter
/// form. Distinct from bare `#[serde(default)]`, which uses the
/// type's own `Default` impl (and so still requires the enum to be
/// Default-reachable — compile-time enforced, no need to check here).
fn has_explicit_default_fn(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("serde") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("default") {
                // Only `default = "fn"` carries a value; the bare
                // `default` form doesn't.
                if let Ok(value) = meta.value() {
                    let _ = value.parse::<syn::Expr>();
                    found = true;
                }
                return Ok(());
            }
            if let Ok(value) = meta.value() {
                let _ = value.parse::<syn::Expr>();
            }
            Ok(())
        });
        found
    })
}

fn collect_enum_refs(
    ty: &Type,
    payload_enums: &BTreeMap<String, &ItemEnum>,
    out: &mut BTreeSet<String>,
) {
    if let Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            let name = seg.ident.to_string();
            if payload_enums.contains_key(&name) {
                out.insert(name);
            }
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                for arg in &args.args {
                    if let GenericArgument::Type(inner) = arg {
                        collect_enum_refs(inner, payload_enums, out);
                    }
                }
            }
        }
    }
}

/// Sanity-check the new guard catches a future regression. Mirrors
/// `guard_catches_synthetic_regression` for the original guard. A
/// silent no-op here would let the regression class slip through
/// (the same way #577 originally did before the sibling test).
#[test]
fn enum_guard_catches_synthetic_regression() {
    // Three shapes:
    //   - struct field of a non-Default enum (should flag)
    //   - inline variant field of a non-Default enum (should flag)
    //   - Option<non-Default enum> field (should NOT flag — None is default)
    //   - non-Default enum with #[serde(default = "fn")] (should NOT flag)
    let synthetic = r#"
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub enum NoDefaultKind {
    A,
    B,
}

#[derive(Serialize, Deserialize, Default)]
pub enum WithDefault {
    #[default]
    Alpha,
    Beta,
}

#[derive(Serialize, Deserialize)]
pub struct InnerPayload {
    pub bad_field: NoDefaultKind,           // ❌ should flag
    pub good_field: WithDefault,            // ✅ Default-reachable
    pub optional: Option<NoDefaultKind>,    // ✅ Option exempt
    #[serde(default = "explicit_default")]
    pub explicit: NoDefaultKind,            // ✅ explicit defaulter exempt
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum WireEvent {
    Variant {
        kind: NoDefaultKind,                // ❌ should flag (inline variant)
        inner: InnerPayload,
    },
}
"#;

    let parsed: File = syn::parse_file(synthetic).expect("parse synthetic snippet");

    let payload_enums: BTreeMap<String, &ItemEnum> = parsed
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Enum(e) if !has_serde_tag(&e.attrs) => Some((e.ident.to_string(), e)),
            _ => None,
        })
        .collect();
    let wire_enums: Vec<&ItemEnum> = parsed
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Enum(e) if has_serde_tag(&e.attrs) => Some(e),
            _ => None,
        })
        .collect();
    let structs: Vec<&ItemStruct> = parsed
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Struct(s) = item {
                Some(s)
            } else {
                None
            }
        })
        .collect();

    let exempt: BTreeSet<String> = BTreeSet::new();
    let mut violations: Vec<String> = Vec::new();

    for s in &structs {
        if let Fields::Named(named) = &s.fields {
            for field in &named.named {
                let field_name = field
                    .ident
                    .as_ref()
                    .map(|i| i.to_string())
                    .unwrap_or_default();
                check_enum_field(
                    field,
                    &format!("{}.{field_name}", s.ident),
                    &payload_enums,
                    &exempt,
                    &mut violations,
                );
            }
        }
    }
    for we in &wire_enums {
        for variant in &we.variants {
            if let Fields::Named(named) = &variant.fields {
                for field in &named.named {
                    let field_name = field
                        .ident
                        .as_ref()
                        .map(|i| i.to_string())
                        .unwrap_or_default();
                    check_enum_field(
                        field,
                        &format!("{}::{}.{field_name}", we.ident, variant.ident),
                        &payload_enums,
                        &exempt,
                        &mut violations,
                    );
                }
            }
        }
    }

    assert_eq!(
        violations.len(),
        2,
        "expected exactly 2 violations (InnerPayload.bad_field + WireEvent::Variant.kind), \
         got {violations:#?}"
    );
    assert!(
        violations
            .iter()
            .any(|v| v.contains("InnerPayload.bad_field")),
        "expected InnerPayload.bad_field flagged; got {violations:?}"
    );
    assert!(
        violations
            .iter()
            .any(|v| v.contains("WireEvent::Variant.kind")),
        "expected WireEvent::Variant.kind flagged; got {violations:?}"
    );
}
