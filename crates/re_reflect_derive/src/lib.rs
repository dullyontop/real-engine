//! `#[derive(Reflect)]`: implements `re_reflect::Reflect` for a struct with
//! named fields, the derive form of `re_reflect::component!` (ADR-0032).
//!
//! The component's name is the struct's name, its doc and each field's doc
//! come from their `///` comments, and each field's default is the field's
//! value in the type's `Default`. The struct also derives (or implements)
//! bevy's `Component`, `Clone` and `Default` itself, as `Reflect` requires.
//!
//! Attributes:
//! * `#[reflect(skip)]` on a field leaves it out of the schema: scene files,
//!   commands and scripts never see it, and it keeps its `Default` value on
//!   load (for caches and handles that are not data);
//! * `#[reflect(crate = "path")]` on the struct names the `re_reflect` crate
//!   when it is not a direct dependency (for example `re_scene::reflect`).

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr, Path, parse_macro_input};

/// Implements `re_reflect::Reflect`; see the crate documentation.
#[proc_macro_derive(Reflect, attributes(reflect))]
pub fn derive_reflect(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// The `///` lines of `attrs`, as string literal expressions.
fn doc_lines(attrs: &[syn::Attribute]) -> Vec<syn::Expr> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => Some(nv.value.clone()),
            _ => None,
        })
        .collect()
}

/// Whether a field carries `#[reflect(skip)]`.
fn skipped(field: &syn::Field) -> syn::Result<bool> {
    let mut skip = false;
    for attr in field.attrs.iter().filter(|a| a.path().is_ident("reflect")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                skip = true;
                Ok(())
            } else {
                Err(meta.error("expected `skip` (the only field attribute)"))
            }
        })?;
    }
    Ok(skip)
}

/// The `re_reflect` path: `#[reflect(crate = "...")]`, or `::re_reflect`.
fn crate_path(input: &DeriveInput) -> syn::Result<Path> {
    let mut path: Path = syn::parse_quote!(::re_reflect);
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("reflect")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                let lit: LitStr = meta.value()?.parse()?;
                path = lit.parse()?;
                Ok(())
            } else {
                Err(meta.error("expected `crate = \"path\"` (the only struct attribute)"))
            }
        })?;
    }
    Ok(path)
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            Fields::Unit => {
                return Err(syn::Error::new_spanned(
                    name,
                    "a unit struct has no fields to reflect: give it named fields (a marker component can have none: `struct Tag {}`)",
                ));
            }
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    name,
                    "#[derive(Reflect)] needs named fields: files and scripts name every field",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                name,
                "#[derive(Reflect)] works on structs; for a field enum use `field_enum!`",
            ));
        }
    };
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "a reflected component cannot be generic: its name is its type's name",
        ));
    }
    let krate = crate_path(input)?;
    let doc = doc_lines(&input.attrs);
    let mut idents = Vec::new();
    let mut types = Vec::new();
    let mut docs = Vec::new();
    for f in fields {
        if skipped(f)? {
            continue;
        }
        idents.push(f.ident.clone().expect("named field"));
        types.push(f.ty.clone());
        docs.push(doc_lines(&f.attrs));
    }
    let names: Vec<String> = idents.iter().map(ToString::to_string).collect();
    let indices: Vec<usize> = (0..idents.len()).collect();
    let field_docs = docs.iter().map(|d| quote!(&[#(#d),*]));
    Ok(quote! {
        impl #krate::registry::Reflect for #name {
            const NAME: &'static str = stringify!(#name);

            fn schema() -> #krate::registry::ComponentSchema {
                let default = <Self as ::core::default::Default>::default();
                #krate::registry::ComponentSchema {
                    name: stringify!(#name).to_string(),
                    doc: #krate::registry::join_doc(&[#(#doc),*]),
                    fields: vec![#(
                        #krate::registry::FieldSchema {
                            name: #names.to_string(),
                            ty: <#types as #krate::value::FieldValue>::field_type(),
                            doc: #krate::registry::join_doc(#field_docs),
                            default: #krate::value::FieldValue::to_value(&default.#idents),
                        }
                    ),*],
                    origin: #krate::registry::ComponentOrigin::Native,
                }
            }

            fn get_field(&self, index: usize) -> #krate::value::Value {
                match index {
                    #(#indices => #krate::value::FieldValue::to_value(&self.#idents),)*
                    _ => panic!("{}: field index {index} out of range", stringify!(#name)),
                }
            }

            fn set_field(&mut self, index: usize, value: &#krate::value::Value) {
                match index {
                    #(#indices => {
                        self.#idents = <#types as #krate::value::FieldValue>::from_value(value)
                            .expect("value was coerced to the field type");
                    })*
                    _ => panic!("{}: field index {index} out of range", stringify!(#name)),
                }
            }
        }
    })
}
