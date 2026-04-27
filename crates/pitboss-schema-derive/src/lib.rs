//! Proc-macro implementation of `#[derive(FieldMetadata)]`.
//!
//! Consumers should depend on `pitboss-schema` (which re-exports the derive)
//! rather than this crate directly.
//!
//! # Emitted code
//!
//! For each `#[derive(FieldMetadata)]` struct, the macro emits:
//!
//! ```ignore
//! impl MyStruct {
//!     pub fn field_metadata() -> &'static [::pitboss_schema::FieldDescriptor] {
//!         &[ /* one entry per field */ ]
//!     }
//! }
//! ```
//!
//! # Attribute reference
//!
//! - `#[field(label = "...")]` — short human label (defaults to the field name).
//! - `#[field(help = "...")]`  — long-form help text (defaults to "").
//! - `#[field(form_type = "...")]` — override the inferred form type. One of:
//!   `text`, `long_text`, `integer`, `float`, `boolean`, `path`, `enum_select`,
//!   `string_list`, `key_value_map`. Compile-time validated.
//! - `#[field(required = true|false)]` — override the inferred requiredness.
//! - `#[field(enum_values = ["a", "b"])]` — populate `FieldDescriptor::enum_values`.
//!   Implies `form_type = "enum_select"` unless an explicit form type was set.
//! - `#[field(skip)]` — exclude this field from the metadata table (e.g. internal
//!   fields that should never appear in a form).

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    parse_macro_input, punctuated::Punctuated, Data, DeriveInput, Expr, ExprArray, ExprLit, Fields,
    GenericArgument, Lit, Meta, PathArguments, Token, Type,
};

const KNOWN_FORM_TYPES: &[(&str, &str)] = &[
    ("text", "Text"),
    ("long_text", "LongText"),
    ("integer", "Integer"),
    ("float", "Float"),
    ("boolean", "Boolean"),
    ("path", "Path"),
    ("enum_select", "EnumSelect"),
    ("string_list", "StringList"),
    ("key_value_map", "KeyValueMap"),
];

fn known_form_type_keys() -> Vec<&'static str> {
    KNOWN_FORM_TYPES.iter().map(|(k, _)| *k).collect()
}

fn form_type_variant(s: &str) -> Option<&'static str> {
    KNOWN_FORM_TYPES.iter().find_map(|(k, v)| (*k == s).then_some(*v))
}

#[proc_macro_derive(FieldMetadata, attributes(field))]
pub fn derive_field_metadata(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => {
                // Use call_site() so the error points at the
                // `#[derive(FieldMetadata)]` line rather than the type
                // identifier, which is where the operator's eye lands.
                return compile_error(
                    "FieldMetadata only supports structs with named fields",
                    Span::call_site(),
                );
            }
        },
        _ => {
            return compile_error(
                "FieldMetadata only supports structs",
                Span::call_site(),
            );
        }
    };

    let mut entries = Vec::<TokenStream2>::new();
    for field in fields {
        match build_entry(field) {
            Ok(Some(tokens)) => entries.push(tokens),
            Ok(None) => {}
            Err(err) => return err.into_compile_error().into(),
        }
    }

    let expanded = quote! {
        impl #struct_name {
            /// Per-field metadata descriptors emitted by `#[derive(FieldMetadata)]`.
            ///
            /// The slice is `'static` and reflects the source-order field
            /// declaration. See [`pitboss_schema::FieldDescriptor`] for the
            /// semantics of each entry.
            #[allow(dead_code)]
            pub fn field_metadata() -> &'static [::pitboss_schema::FieldDescriptor] {
                // `const` promotes the array literal to a static so the
                // returned slice has `'static` lifetime.
                const __FIELDS: &[::pitboss_schema::FieldDescriptor] = &[
                    #( #entries ),*
                ];
                __FIELDS
            }
        }
    };

    expanded.into()
}

fn build_entry(field: &syn::Field) -> syn::Result<Option<TokenStream2>> {
    let ident = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new_spanned(field, "field must have an identifier"))?;
    let name_str = ident.to_string();

    let mut label: Option<String> = None;
    let mut help: Option<String> = None;
    let mut form_type: Option<String> = None;
    let mut enum_values: Vec<String> = Vec::new();
    let mut required_override: Option<bool> = None;
    let mut skip = false;

    // Track which keys we've seen so a duplicate (across the same or
    // separate `#[field(...)]` attributes) becomes a hard compile error
    // rather than a silent last-write-wins. (#159)
    let mut seen_keys: std::collections::HashSet<&'static str> =
        std::collections::HashSet::new();

    for attr in &field.attrs {
        if !attr.path().is_ident("field") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            let key: &'static str = if meta.path.is_ident("label") {
                "label"
            } else if meta.path.is_ident("help") {
                "help"
            } else if meta.path.is_ident("form_type") {
                "form_type"
            } else if meta.path.is_ident("enum_values") {
                "enum_values"
            } else if meta.path.is_ident("required") {
                "required"
            } else if meta.path.is_ident("skip") {
                "skip"
            } else {
                return Err(meta.error("unknown #[field(...)] attribute"));
            };
            if !seen_keys.insert(key) {
                return Err(meta.error(format!(
                    "duplicate #[field({key} = ...)] entry; specify each key at most once",
                )));
            }
            match key {
                "label" => {
                    label = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                }
                "help" => {
                    help = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                }
                "form_type" => {
                    let v = meta.value()?.parse::<syn::LitStr>()?.value();
                    if form_type_variant(&v).is_none() {
                        return Err(meta.error(format!(
                            "unknown form_type {:?}; expected one of {:?}",
                            v,
                            known_form_type_keys()
                        )));
                    }
                    form_type = Some(v);
                }
                "enum_values" => {
                    let arr: ExprArray = meta.value()?.parse()?;
                    for e in arr.elems {
                        let s = match e {
                            Expr::Lit(ExprLit {
                                lit: Lit::Str(s), ..
                            }) => s.value(),
                            other => {
                                return Err(syn::Error::new_spanned(
                                    other,
                                    "enum_values entries must be string literals",
                                ));
                            }
                        };
                        enum_values.push(s);
                    }
                }
                "required" => {
                    required_override = Some(meta.value()?.parse::<syn::LitBool>()?.value);
                }
                "skip" => {
                    skip = true;
                }
                _ => unreachable!(),
            }
            Ok(())
        })?;
    }

    if skip {
        return Ok(None);
    }

    let is_optional = is_option_type(&field.ty);
    let has_serde_default = field_has_serde_default(field);
    let inferred = if !enum_values.is_empty() {
        "enum_select"
    } else {
        infer_form_type(&field.ty)
    };
    let form_type_str = form_type.unwrap_or_else(|| inferred.to_string());
    // Required iff (a) not wrapped in Option AND (b) no `#[serde(default)]`
    // / `#[serde(default = "...")]`. The serde-default branch matters for
    // primitives like `bool`/`u32` that have a Rust default but should still
    // render as optional in a form.
    let required = required_override.unwrap_or(!(is_optional || has_serde_default));

    let label_str = label.unwrap_or_else(|| name_str.clone());
    let help_str = help.unwrap_or_default();

    let enum_values_lits = enum_values.iter().map(|v| quote! { #v });

    // Resolve the form_type string to a concrete variant at macro-expand
    // time and emit `::pitboss_schema::FormType::Text` directly. The
    // earlier code emitted a runtime `FormType::from_str(<lit>)` call,
    // which silently degraded to `Text` if KNOWN_FORM_TYPES drifted from
    // the enum. The known-form-type table is now the single source of
    // truth for both the validation set and the variant mapping. (#159)
    let variant_ident = syn::Ident::new(
        form_type_variant(&form_type_str).expect("validated above"),
        proc_macro2::Span::call_site(),
    );

    Ok(Some(quote! {
        ::pitboss_schema::FieldDescriptor {
            name: #name_str,
            label: #label_str,
            help: #help_str,
            form_type: ::pitboss_schema::FormType::#variant_ident,
            required: #required,
            enum_values: &[ #( #enum_values_lits ),* ],
        }
    }))
}

/// `true` when the field carries any flavor of `#[serde(default)]` —
/// either the bare `default` token or `default = "func"`. Parses the
/// nested meta as a comma-separated list of `Meta` items so the check
/// only matches `default` at the top level of each entry, not e.g. a
/// path-segment named `default` inside `skip_serializing_if =
/// "default::is_none"` or a string literal `rename = "default"`. (#159)
fn field_has_serde_default(field: &syn::Field) -> bool {
    for attr in &field.attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let Ok(items) =
            attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        else {
            continue;
        };
        for item in items {
            let path = match &item {
                Meta::Path(p) => p,
                Meta::List(l) => &l.path,
                Meta::NameValue(nv) => &nv.path,
            };
            // `default` must be the *only* segment of the path — guards
            // against `default::is_none` style values surfacing as a hit.
            if path.segments.len() == 1 && path.is_ident("default") {
                return true;
            }
        }
    }
    false
}

/// `true` when the type is `Option<T>` (any path ending in `Option`).
fn is_option_type(ty: &Type) -> bool {
    let Type::Path(tp) = ty else { return false };
    tp.path
        .segments
        .last()
        .map(|s| s.ident == "Option")
        .unwrap_or(false)
}

/// Strip a single `Option<...>` wrapper, returning the inner type.
fn unwrap_option(ty: &Type) -> &Type {
    if let Type::Path(tp) = ty {
        if let Some(seg) = tp.path.segments.last() {
            if seg.ident == "Option" {
                if let PathArguments::AngleBracketed(args) = &seg.arguments {
                    if let Some(GenericArgument::Type(inner)) = args.args.first() {
                        return inner;
                    }
                }
            }
        }
    }
    ty
}

/// Map a Rust type to a default `FormType` string identifier. Used when no
/// explicit `#[field(form_type = "...")]` is supplied.
fn infer_form_type(ty: &Type) -> &'static str {
    let inner = unwrap_option(ty);
    let Type::Path(tp) = inner else {
        return "text";
    };
    let Some(last) = tp.path.segments.last() else {
        return "text";
    };
    let name = last.ident.to_string();
    match name.as_str() {
        "String" | "str" => "text",
        "PathBuf" | "Path" => "path",
        "bool" => "boolean",
        "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize" => {
            "integer"
        }
        "f32" | "f64" => "float",
        "Vec" => {
            // Vec<String> ⇒ string_list; other Vec<T> ⇒ text (the consumer
            // can override with #[field(form_type = "...")]).
            if let PathArguments::AngleBracketed(args) = &last.arguments {
                if let Some(GenericArgument::Type(Type::Path(elem))) = args.args.first() {
                    if let Some(elem_seg) = elem.path.segments.last() {
                        if elem_seg.ident == "String" {
                            return "string_list";
                        }
                    }
                }
            }
            "text"
        }
        "HashMap" | "BTreeMap" => "key_value_map",
        _ => "text",
    }
}

fn compile_error(msg: &str, span: proc_macro2::Span) -> TokenStream {
    syn::Error::new(span, msg).into_compile_error().into()
}
