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
//! example TOML emitter, PR 1.E `pitboss scaffold`) walk the registry returned
//! by [`field_metadata_registry`].

pub use pitboss_schema_derive::FieldMetadata;

/// Per-field descriptor emitted by `#[derive(FieldMetadata)]`.
///
/// All fields are `&'static` so the descriptor table can live in `.rodata` —
/// no allocation at lookup time.
#[derive(Debug, Clone, Copy)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub const fn from_str(s: &str) -> Self {
        // Const-fn `match` on string slices isn't stable, so this is a
        // hand-rolled byte comparison. Keep in sync with the derive's
        // `validate_form_type` table.
        match s.as_bytes() {
            b"text" => FormType::Text,
            b"long_text" => FormType::LongText,
            b"integer" => FormType::Integer,
            b"float" => FormType::Float,
            b"boolean" => FormType::Boolean,
            b"path" => FormType::Path,
            b"enum_select" => FormType::EnumSelect,
            b"string_list" => FormType::StringList,
            b"key_value_map" => FormType::KeyValueMap,
            _ => FormType::Text,
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
#[derive(Debug, Clone, Copy)]
pub struct SchemaSection {
    /// TOML path to the section (e.g. `[run]`, `[[task]]`, `[lead]`).
    pub toml_path: &'static str,
    /// Rust type name (e.g. `"RunConfig"`).
    pub type_name: &'static str,
    /// Per-field descriptors for the section.
    pub fields: &'static [FieldDescriptor],
}
