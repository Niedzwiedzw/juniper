//! Code generation for `#[derive(GraphQLInterface)]` macro.

use proc_macro2::TokenStream;
use quote::{format_ident, ToTokens as _};
use syn::{ext::IdentExt as _, parse_quote, spanned::Spanned};

use crate::{
    common::{field, parse::TypeExt as _, scalar},
    result::GraphQLScope,
    util::{span_container::SpanContainer, RenameRule},
};

use super::{Attr, Definition};

/// [`GraphQLScope`] of errors for `#[derive(GraphQLInterface)]` macro.
const ERR: GraphQLScope = GraphQLScope::InterfaceDerive;

/// Expands `#[derive(GraphQLInterface)]` macro into generated code.
pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let ast = syn::parse2::<syn::DeriveInput>(input)?;
    let attr = Attr::from_attrs("graphql", &ast.attrs)?;

    let data = if let syn::Data::Struct(data) = &ast.data {
        data
    } else {
        return Err(ERR.custom_error(ast.span(), "can only be derived on structs"));
    };

    let struct_ident = &ast.ident;
    let struct_span = ast.span();

    let name = attr
        .name
        .clone()
        .map(SpanContainer::into_inner)
        .unwrap_or_else(|| struct_ident.unraw().to_string());
    if !attr.is_internal && name.starts_with("__") {
        ERR.no_double_underscore(
            attr.name
                .as_ref()
                .map(SpanContainer::span_ident)
                .unwrap_or_else(|| struct_ident.span()),
        );
    }

    let scalar = scalar::Type::parse(attr.scalar.as_deref(), &ast.generics);

    proc_macro_error::abort_if_dirty();

    let renaming = attr
        .rename_fields
        .as_deref()
        .copied()
        .unwrap_or(RenameRule::CamelCase);

    let fields = data
        .fields
        .iter()
        .filter_map(|f| parse_field(f, &renaming))
        .collect::<Vec<_>>();

    proc_macro_error::abort_if_dirty();

    if fields.is_empty() {
        ERR.emit_custom(struct_span, "must have at least one field");
    }
    if !field::all_different(&fields) {
        ERR.emit_custom(struct_span, "must have a different name for each field");
    }

    proc_macro_error::abort_if_dirty();

    let context = attr
        .context
        .as_deref()
        .cloned()
        .or_else(|| {
            fields.iter().find_map(|f| {
                f.arguments.as_ref().and_then(|f| {
                    f.iter()
                        .find_map(field::MethodArgument::context_ty)
                        .cloned()
                })
            })
        })
        .unwrap_or_else(|| parse_quote! { () });

    let enum_alias_ident = attr
        .r#enum
        .as_deref()
        .cloned()
        .unwrap_or_else(|| format_ident!("{}Value", struct_ident.to_string()));
    let enum_ident = attr.r#enum.as_ref().map_or_else(
        || format_ident!("{}ValueEnum", struct_ident.to_string()),
        |c| format_ident!("{}Enum", c.inner().to_string()),
    );

    Ok(Definition {
        generics: ast.generics.clone(),
        vis: ast.vis.clone(),
        enum_ident,
        enum_alias_ident,
        name,
        description: attr.description.as_deref().cloned(),
        context,
        scalar,
        fields,
        implemented_for: attr
            .implemented_for
            .iter()
            .map(|c| c.inner().clone())
            .collect(),
    }
    .into_token_stream())
}

/// Parses a [`field::Definition`] from the given struct field definition.
///
/// Returns [`None`] if the parsing fails, or the struct field is ignored.
#[must_use]
fn parse_field(field: &syn::Field, renaming: &RenameRule) -> Option<field::Definition> {
    let field_ident = field
        .ident
        .as_ref()
        .or_else(|| err_unnamed_field(&field.span()))?;

    let attr = field::Attr::from_attrs("graphql", &field.attrs)
        .map_err(|e| proc_macro_error::emit_error!(e))
        .ok()?;

    if attr.ignore.is_some() {
        return None;
    }

    let name = attr
        .name
        .as_ref()
        .map(|m| m.as_ref().value())
        .unwrap_or_else(|| renaming.apply(&field_ident.unraw().to_string()));
    if name.starts_with("__") {
        ERR.no_double_underscore(
            attr.name
                .as_ref()
                .map(SpanContainer::span_ident)
                .unwrap_or_else(|| field_ident.span()),
        );
        return None;
    }

    let mut ty = field.ty.clone();
    ty.lifetimes_anonymized();

    let description = attr.description.as_ref().map(|d| d.as_ref().value());
    let deprecated = attr
        .deprecated
        .as_deref()
        .map(|d| d.as_ref().map(syn::LitStr::value));

    Some(field::Definition {
        name,
        ty,
        description,
        deprecated,
        ident: field_ident.clone(),
        arguments: None,
        has_receiver: false,
        is_async: false,
    })
}

/// Emits "expected named struct field" [`syn::Error`] pointing to the given
/// `span`.
fn err_unnamed_field<T, S: Spanned>(span: &S) -> Option<T> {
    ERR.emit_custom(span.span(), "expected named struct field");
    None
}
