//! Centralized registry of every section in the v0.9 manifest schema.
//!
//! Each schema struct in [`super::schema`] derives [`pitboss_schema::FieldMetadata`],
//! which generates a `field_metadata()` accessor returning a `'static` slice
//! of [`pitboss_schema::FieldDescriptor`]s. The [`SECTIONS`] table below
//! groups those slices with their TOML paths so downstream tools (PR 1.C
//! `manifest-map.md` generator, PR 1.D complete-example emitter, PR 1.E
//! `pitboss scaffold`) can iterate the entire schema from one place.
//!
//! Intentionally thin — this module owns *only* the section table. The
//! per-field descriptors are owned by the structs themselves.

use pitboss_schema::SchemaSection;

use super::schema::{
    ApprovalRuleSpec, ContainerConfig, Defaults, Lead, McpServerSpec, MountSpec, RunConfig,
    SubleadDefaults, Task, Template,
};

/// Walk every section of the v0.9 manifest schema in declaration order.
///
/// The order is meant to match the order of the annotated example
/// (`pitboss.example.toml`) so downstream emitters produce diff-friendly,
/// stably-ordered output.
pub fn sections() -> Vec<SchemaSection> {
    vec![
        SchemaSection {
            toml_path: "[run]",
            type_name: "RunConfig",
            fields: RunConfig::field_metadata(),
        },
        SchemaSection {
            toml_path: "[defaults]",
            type_name: "Defaults",
            fields: Defaults::field_metadata(),
        },
        SchemaSection {
            toml_path: "[container]",
            type_name: "ContainerConfig",
            fields: ContainerConfig::field_metadata(),
        },
        SchemaSection {
            toml_path: "[[container.mount]]",
            type_name: "MountSpec",
            fields: MountSpec::field_metadata(),
        },
        SchemaSection {
            toml_path: "[[task]]",
            type_name: "Task",
            fields: Task::field_metadata(),
        },
        SchemaSection {
            toml_path: "[lead]",
            type_name: "Lead",
            fields: Lead::field_metadata(),
        },
        SchemaSection {
            toml_path: "[sublead_defaults]",
            type_name: "SubleadDefaults",
            fields: SubleadDefaults::field_metadata(),
        },
        SchemaSection {
            toml_path: "[[approval_policy]]",
            type_name: "ApprovalRuleSpec",
            fields: ApprovalRuleSpec::field_metadata(),
        },
        SchemaSection {
            toml_path: "[[mcp_server]]",
            type_name: "McpServerSpec",
            fields: McpServerSpec::field_metadata(),
        },
        SchemaSection {
            toml_path: "[[template]]",
            type_name: "Template",
            fields: Template::field_metadata(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitboss_schema::FormType;

    /// Sanity: every section has at least one field, and field names are
    /// non-empty / non-duplicate within a section.
    #[test]
    fn registry_well_formed() {
        for section in sections() {
            assert!(
                !section.fields.is_empty(),
                "section {} has no fields",
                section.toml_path
            );
            let mut seen = std::collections::HashSet::new();
            for f in section.fields {
                assert!(!f.name.is_empty(), "empty field name in {}", section.toml_path);
                assert!(
                    seen.insert(f.name),
                    "duplicate field {} in {}",
                    f.name,
                    section.toml_path
                );
            }
        }
    }

    /// Drift guard — required fields on `[lead]` must include `id`,
    /// `directory`, `prompt`. If someone makes one of these `Option<T>`
    /// without updating both serde + the form story, this catches it.
    #[test]
    fn lead_required_fields_match_schema() {
        let lead_fields = Lead::field_metadata();
        let req: Vec<&str> = lead_fields
            .iter()
            .filter(|f| f.required)
            .map(|f| f.name)
            .collect();
        for must_have in ["id", "directory", "prompt"] {
            assert!(
                req.contains(&must_have),
                "[lead].{} should be required (got required={:?})",
                must_have,
                req
            );
        }
    }

    /// `Lead.prompt` must render as a textarea, not a one-line text input.
    /// PR 1.D depends on this to emit a multi-line `"""..."""` block.
    #[test]
    fn lead_prompt_is_long_text() {
        let prompt = Lead::field_metadata()
            .iter()
            .find(|f| f.name == "prompt")
            .expect("Lead must declare a `prompt` field");
        assert_eq!(prompt.form_type, FormType::LongText);
    }

    /// `RunConfig.worktree_cleanup` must surface its enum values for form rendering.
    #[test]
    fn worktree_cleanup_exposes_enum_values() {
        let f = RunConfig::field_metadata()
            .iter()
            .find(|f| f.name == "worktree_cleanup")
            .expect("RunConfig must declare `worktree_cleanup`");
        assert_eq!(f.form_type, FormType::EnumSelect);
        assert!(f.enum_values.contains(&"always"));
        assert!(f.enum_values.contains(&"on_success"));
        assert!(f.enum_values.contains(&"never"));
    }

    /// `default_approval_policy` is `Option<ApprovalPolicy>` — the derive
    /// can't introspect the foreign enum's variants, so we supply
    /// `enum_values` explicitly. Verify that worked and that `required`
    /// is correctly inferred as `false` from the `Option<T>` wrap.
    #[test]
    fn default_approval_policy_renders_as_enum_select() {
        let f = RunConfig::field_metadata()
            .iter()
            .find(|f| f.name == "default_approval_policy")
            .expect("RunConfig must declare `default_approval_policy`");
        assert_eq!(f.form_type, FormType::EnumSelect);
        assert!(!f.required, "Option<T> should infer required = false");
        assert!(f.enum_values.contains(&"block"));
        assert!(f.enum_values.contains(&"auto_approve"));
        assert!(f.enum_values.contains(&"auto_reject"));
    }

    /// Drift guard: every section's TOML path must be unique.
    #[test]
    fn section_paths_unique() {
        let mut seen = std::collections::HashSet::new();
        for s in sections() {
            assert!(
                seen.insert(s.toml_path),
                "duplicate section path: {}",
                s.toml_path
            );
        }
    }
}
