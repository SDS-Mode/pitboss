//! Field-level metadata for pitboss manifest schema structs.
//!
//! Re-exports the [`FieldMetadata`] derive macro from `pitboss-schema-derive`
//! and defines the runtime types it emits.
//!
//! # Example
//!
//! ```ignore
//! use pitboss_schema::FieldMetadata;
//!
//! #[derive(FieldMetadata)]
//! struct RunConfig {
//!     #[field(label = "Max parallel tasks", help = "Concurrency cap for flat-mode tasks.")]
//!     max_parallel_tasks: Option<u32>,
//! }
//!
//! let meta = RunConfig::field_metadata();
//! assert_eq!(meta[0].name, "max_parallel_tasks");
//! assert!(!meta[0].required);                 // Option<T> ⇒ optional
//! ```
//!
//! Downstream consumers (PR 1.C `manifest-map.md` generator, PR 1.D complete
//! example TOML emitter, PR 1.E `pitboss scaffold`) walk descriptors returned
//! by the per-struct `field_metadata()` method (emitted by the derive).
//!
//! # Generated `field_metadata()` method
//!
//! `#[derive(FieldMetadata)]` emits an inherent `pub fn field_metadata()` on
//! the target struct. The function returns a `&'static [FieldDescriptor]`
//! laid out in source order — one entry per non-`#[field(skip)]` named
//! field. Because the slice is `'static`, lookups don't allocate and the
//! descriptor table can live in `.rodata`.
//!
//! Required-ness is inferred via:
//! `required = !is_optional(&ty) && !has_serde_default(&attrs)`,
//! overridable with `#[field(required = true|false)]`. `Option<T>` and
//! `#[serde(default)]` / `#[serde(default = "fn")]` both flip a field to
//! optional in form output without changing the underlying serde shape.

pub use pitboss_schema_derive::FieldMetadata;

/// Per-field descriptor emitted by `#[derive(FieldMetadata)]`.
///
/// All fields are `&'static` so the descriptor table can live in `.rodata` —
/// no allocation at lookup time.
///
/// `Serialize` is provided so descriptors can be exported directly to JSON
/// (n8n form definitions, schema documentation, manifest-map tooling)
/// without consumers having to manually reconstruct the wire shape.
/// `Deserialize` is intentionally not derived — every field is
/// `&'static str`, so a round-trip would require borrowing from a
/// caller-owned buffer with non-trivial lifetime semantics. Form
/// builders that want to ingest the descriptor JSON should define their
/// own owned mirror struct with `String` fields.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct FieldDescriptor {
    /// Field identifier as it appears in the TOML key (after serde renames).
    pub name: &'static str,
    /// Human-readable label suitable for a form input.
    pub label: &'static str,
    /// Long-form help text. Empty if not supplied.
    pub help: &'static str,
    /// Form-builder hint (text input vs textarea vs enum select, etc.).
    pub form_type: FormType,
    /// `true` when the field must appear in the TOML for the manifest to
    /// validate. Inferred from `Option<T>` / `#[serde(default)]` unless
    /// overridden via `#[field(required = true)]`.
    pub required: bool,
    /// Allowed values for `FormType::EnumSelect`. Empty otherwise.
    pub enum_values: &'static [&'static str],
}

/// Hint to a form-builder UI about which input widget to render for a field.
///
/// The derive macro infers a sensible default from the Rust type:
///
/// | Rust type                     | Inferred `FormType`        |
/// |-------------------------------|----------------------------|
/// | `String`, `&str`              | `Text`                     |
/// | `PathBuf`, `Path`             | `Path`                     |
/// | `bool`                        | `Boolean`                  |
/// | `u8..u64`, `i8..i64`, `usize` | `Integer`                  |
/// | `f32`, `f64`                  | `Float`                    |
/// | `Vec<String>`                 | `StringList`               |
/// | `HashMap<String, String>`     | `KeyValueMap`              |
/// | other / generics              | `Text` (override required) |
///
/// `Option<T>` is unwrapped before inference. Override with
/// `#[field(form_type = "long_text" | "enum_select" | ...)]`.
///
/// `#[non_exhaustive]` lets us add new variants (e.g. `Url`, `Date`) in a
/// minor release without breaking exhaustive `match` in downstream crates.
/// Pair with `_ => …` arms when matching outside this crate.
///
/// Derives `Hash`, `PartialOrd`, `Ord` so descriptor consumers can use
/// `FormType` as a `HashMap` key or sort descriptor lists by widget kind
/// without a workaround.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FormType {
    /// Single-line text input.
    Text,
    /// Multi-line textarea (e.g. `Lead.prompt`).
    LongText,
    /// Integer numeric input.
    Integer,
    /// Floating-point numeric input.
    Float,
    /// Boolean checkbox / toggle.
    Boolean,
    /// Filesystem path picker.
    Path,
    /// Single-select with `enum_values` populating the options.
    EnumSelect,
    /// Repeated string entries (e.g. `tools`, `args`).
    StringList,
    /// Repeated `key = value` entries (e.g. `env`).
    KeyValueMap,
}

impl FormType {
    /// Parse the string used by `#[field(form_type = "...")]`. Unknown values
    /// fall back to `Text` to keep the build green; the derive macro itself
    /// validates known values at compile time.
    ///
    /// **Prefer [`FormType::try_from_str`] in non-macro callers** — this
    /// fall-back-to-`Text` behaviour will silently mis-classify a typo'd
    /// form-type string and is only safe for the macro's compile-time-
    /// validated path.
    pub const fn from_str(s: &str) -> Self {
        match Self::try_from_str(s) {
            Some(v) => v,
            None => FormType::Text,
        }
    }

    /// Strict parse: returns `None` for any string outside the known set.
    ///
    /// Use this from non-macro call sites so a misspelled `form_type`
    /// surfaces as an error rather than silently degrading to `Text`.
    /// (#158)
    pub const fn try_from_str(s: &str) -> Option<Self> {
        // Const-fn `match` on string slices isn't stable, so this is a
        // hand-rolled byte comparison. Keep in sync with the derive's
        // `KNOWN_FORM_TYPES` table.
        match s.as_bytes() {
            b"text" => Some(FormType::Text),
            b"long_text" => Some(FormType::LongText),
            b"integer" => Some(FormType::Integer),
            b"float" => Some(FormType::Float),
            b"boolean" => Some(FormType::Boolean),
            b"path" => Some(FormType::Path),
            b"enum_select" => Some(FormType::EnumSelect),
            b"string_list" => Some(FormType::StringList),
            b"key_value_map" => Some(FormType::KeyValueMap),
            _ => None,
        }
    }

    /// Stable string identifier for tooling (matches the parser above).
    pub const fn as_str(self) -> &'static str {
        match self {
            FormType::Text => "text",
            FormType::LongText => "long_text",
            FormType::Integer => "integer",
            FormType::Float => "float",
            FormType::Boolean => "boolean",
            FormType::Path => "path",
            FormType::EnumSelect => "enum_select",
            FormType::StringList => "string_list",
            FormType::KeyValueMap => "key_value_map",
        }
    }
}

/// One entry in the global registry of all manifest schema sections.
///
/// `Serialize` is provided alongside [`FieldDescriptor`] so n8n form
/// exporters and schema docs can JSON-serialize the section directly.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct SchemaSection {
    /// TOML path to the section (e.g. `[run]`, `[[task]]`, `[lead]`).
    pub toml_path: &'static str,
    /// Rust type name (e.g. `"RunConfig"`).
    pub type_name: &'static str,
    /// Per-field descriptors for the section.
    pub fields: &'static [FieldDescriptor],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_from_str_returns_none_for_unknown() {
        assert_eq!(FormType::try_from_str("nope"), None);
        assert_eq!(FormType::try_from_str(""), None);
        // Also case-sensitive — "Text" is not "text".
        assert_eq!(FormType::try_from_str("Text"), None);
    }

    #[test]
    fn try_from_str_round_trips_with_as_str() {
        for v in [
            FormType::Text,
            FormType::LongText,
            FormType::Integer,
            FormType::Float,
            FormType::Boolean,
            FormType::Path,
            FormType::EnumSelect,
            FormType::StringList,
            FormType::KeyValueMap,
        ] {
            assert_eq!(FormType::try_from_str(v.as_str()), Some(v));
        }
    }

    #[test]
    fn from_str_falls_back_to_text_for_back_compat() {
        // Documented degraded behaviour — preserved so the macro's
        // compile-time-validated path isn't broken by the strict variant.
        assert_eq!(FormType::from_str("nope"), FormType::Text);
    }

    #[test]
    fn form_type_serializes_snake_case() {
        let s = serde_json::to_string(&FormType::LongText).unwrap();
        assert_eq!(s, "\"long_text\"");
        let s = serde_json::to_string(&FormType::KeyValueMap).unwrap();
        assert_eq!(s, "\"key_value_map\"");
    }

    #[test]
    fn form_type_is_hashable_and_orderable() {
        // Compile-time guarantees: the new derives must hold.
        use std::collections::HashMap;
        let mut m: HashMap<FormType, &'static str> = HashMap::new();
        m.insert(FormType::Text, "t");
        m.insert(FormType::Integer, "i");
        assert_eq!(m.get(&FormType::Text), Some(&"t"));

        let mut v = [FormType::Path, FormType::Boolean, FormType::Text];
        v.sort();
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn field_descriptor_serializes() {
        let d = FieldDescriptor {
            name: "max_workers",
            label: "Max workers",
            help: "",
            form_type: FormType::Integer,
            required: false,
            enum_values: &[],
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("\"name\":\"max_workers\""));
        assert!(s.contains("\"form_type\":\"integer\""));
        assert!(s.contains("\"required\":false"));
    }

    #[test]
    fn schema_section_serializes() {
        let descs: &[FieldDescriptor] = &[FieldDescriptor {
            name: "a",
            label: "a",
            help: "",
            form_type: FormType::Text,
            required: true,
            enum_values: &[],
        }];
        let s = SchemaSection {
            toml_path: "[run]",
            type_name: "RunConfig",
            fields: descs,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"toml_path\":\"[run]\""));
        assert!(json.contains("\"type_name\":\"RunConfig\""));
        assert!(json.contains("\"fields\":["));
    }
}
