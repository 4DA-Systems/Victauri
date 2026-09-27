#![forbid(unsafe_code)]
//! Procedural macros for Victauri — currently provides the `#[inspectable]` attribute
//! for auto-registering Tauri commands with the MCP introspection layer.

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

/// Marks a `#[tauri::command]` as inspectable by Victauri.
///
/// Generates a companion `<fn_name>__schema()` function that returns a
/// `victauri_core::CommandInfo` with the command's name, description,
/// argument types, return type, and NL-resolution metadata.
/// Call the schema function at setup time to register the command in the
/// Victauri `CommandRegistry`.
///
/// # Dependency requirement
///
/// The generated code names `victauri_core::...` by absolute path, so the crate
/// using this attribute must list `victauri-core` in its own `[dependencies]`
/// (this holds whether the macro is imported from `victauri_macros` or via the
/// `victauri_plugin::inspectable` re-export). Without it, compilation fails with
/// `E0433: failed to resolve: use of undeclared crate victauri_core`.
/// `victauri init` adds the dependency automatically.
///
/// # Example
///
/// ```rust,ignore
/// #[tauri::command]
/// #[inspectable(description = "Save API key for a provider")]
/// async fn save_api_key(provider: String, key: String) -> Result<(), String> {
///     // ...
/// }
///
/// // At setup:
/// state.registry.register(save_api_key__schema());
/// ```
#[proc_macro_attribute]
pub fn inspectable(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let attrs = match parse_attrs(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let schema_fn_name = syn::Ident::new(&format!("{fn_name_str}__schema"), fn_name.span());
    let is_async = input.sig.asyncness.is_some();

    let description = attrs
        .description
        .unwrap_or_else(|| fn_name_str.replace('_', " "));

    let args_info = extract_args(&input.sig);
    let arg_tokens: Vec<_> = args_info
        .iter()
        .map(|(name, type_str, required)| {
            quote! {
                victauri_core::registry::CommandArg::new(#name, #type_str, #required)
            }
        })
        .collect();

    let return_type = extract_return_type(&input.sig);

    let intent_token = attrs.intent.as_ref().map(|i| quote! { .with_intent(#i) });

    let category_token = attrs
        .category
        .as_ref()
        .map(|c| quote! { .with_category(#c) });

    let example_tokens: Vec<_> = attrs
        .examples
        .iter()
        .map(|e| quote! { #e.to_string() })
        .collect();

    let expanded = quote! {
        #input

        #[allow(dead_code, non_snake_case)]
        fn #schema_fn_name() -> victauri_core::registry::CommandInfo {
            // Built via constructors, not a struct literal: `CommandInfo` and
            // `CommandArg` are `#[non_exhaustive]`, and this code expands in the
            // user's crate.
            victauri_core::registry::CommandInfo::new(#fn_name_str)
                .with_description(#description)
                .with_args(vec![#(#arg_tokens),*])
                .with_return_type(#return_type)
                .with_async(#is_async)
                #intent_token
                #category_token
                .with_examples(vec![#(#example_tokens),*])
        }

        victauri_core::inventory::submit! {
            victauri_core::registry::CommandInfoFactory(#schema_fn_name)
        }
    };

    TokenStream::from(expanded)
}

struct InspectableAttrs {
    description: Option<String>,
    intent: Option<String>,
    category: Option<String>,
    examples: Vec<String>,
}

fn parse_attrs(attr: TokenStream) -> syn::Result<InspectableAttrs> {
    let mut attrs = InspectableAttrs {
        description: None,
        intent: None,
        category: None,
        examples: Vec::new(),
    };

    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("description") {
            attrs.description = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("intent") {
            attrs.intent = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("category") {
            attrs.category = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("example") {
            attrs
                .examples
                .push(meta.value()?.parse::<syn::LitStr>()?.value());
        } else {
            return Err(meta.error("unknown #[inspectable] attribute"));
        }
        Ok(())
    });

    syn::parse::Parser::parse(parser, attr)?;
    Ok(attrs)
}

fn extract_args(sig: &syn::Signature) -> Vec<(String, String, bool)> {
    sig.inputs
        .iter()
        .filter_map(|arg| {
            if let syn::FnArg::Typed(pat_type) = arg {
                let name = match &*pat_type.pat {
                    syn::Pat::Ident(ident) => ident.ident.to_string(),
                    _ => return None,
                };

                let ty = &*pat_type.ty;
                let type_str = quote!(#ty).to_string();
                if is_tauri_framework_type(&type_str) {
                    return None;
                }

                let is_option = type_str.starts_with("Option")
                    || type_str.starts_with("Option <")
                    || type_str.contains(":: Option");
                let type_name = type_str;

                Some((name, type_name, !is_option))
            } else {
                None
            }
        })
        .collect()
}

fn is_tauri_framework_type(type_str: &str) -> bool {
    const FRAMEWORK_TYPES: &[&str] = &["AppHandle", "State", "Window", "Webview", "WebviewWindow"];
    let last_segment = type_str
        .rsplit("::")
        .next()
        .unwrap_or(type_str)
        .split('<')
        .next()
        .unwrap_or(type_str)
        .trim();
    FRAMEWORK_TYPES.contains(&last_segment)
}

fn extract_return_type(sig: &syn::Signature) -> String {
    match &sig.output {
        syn::ReturnType::Default => "()".to_string(),
        syn::ReturnType::Type(_, ty) => quote!(#ty).to_string(),
    }
}
