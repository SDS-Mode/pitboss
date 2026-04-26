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
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse_macro_input, Data, DeriveInput, Expr, ExprArray, ExprLit, Fields, GenericArgument, Lit,
    PathArguments, Type,
};

const KNOWN_FORM_TYPES: &[&str] = &[
    "text",
    "long_text",
    "integer",
    "float",
    "boolean",
    "path",
    "enum_select",
    "string_list",
    "key_value_map",
];

#[proc_macro_derive(FieldMetadata, attributes(field))]
pub fn derive_field_metadata(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return compile_error(
                    "FieldMetadata only supports structs with named fields",
                    struct_name.span(),
                );
            }
        },
        _ => {
            return compile_error("FieldMetadata only supports structs", struct_name.span());
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

    for attr in &field.attrs {
        if !attr.path().is_ident("field") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("label") {
                label = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("help") {
                help = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("form_type") {
                let v = meta.value()?.parse::<syn::LitStr>()?.value();
                if !KNOWN_FORM_TYPES.contains(&v.as_str()) {
                    return Err(meta.error(format!(
                        "unknown form_type {:?}; expected one of {:?}",
                        v, KNOWN_FORM_TYPES
                    )));
                }
                form_type = Some(v);
            } else if meta.path.is_ident("enum_values") {
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
            } else if meta.path.is_ident("required") {
                required_override = Some(meta.value()?.parse::<syn::LitBool>()?.value);
            } else if meta.path.is_ident("skip") {
                skip = true;
            } else {
                return Err(meta.error("unknown #[field(...)] attribute"));
            }
            Ok(())
        })?;
    }

    if skip {
        return Ok(None);
    }

    let is_optional = is_option_type(&field.ty);
    let inferred = if !enum_values.is_empty() {
        "enum_select"
    } else {
        infer_form_type(&field.ty)
    };
    let form_type_str = form_type.unwrap_or_else(|| inferred.to_string());
    let required = required_override.unwrap_or(!is_optional);

    let label_str = label.unwrap_or_else(|| name_str.clone());
    let help_str = help.unwrap_or_default();

    let enum_values_lits = enum_values.iter().map(|v| quote! { #v });

    Ok(Some(quote! {
        ::pitboss_schema::FieldDescriptor {
            name: #name_str,
            label: #label_str,
            help: #help_str,
            form_type: ::pitboss_schema::FormType::from_str(#form_type_str),
            required: #required,
            enum_values: &[ #( #enum_values_lits ),* ],
        }
    }))
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
