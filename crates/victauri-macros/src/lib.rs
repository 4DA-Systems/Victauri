#![forbid(unsafe_code)]
//! Procedural macros for Victauri — currently provides the `#[inspectable]` attribute
//! for auto-registering Tauri commands with the MCP introspection layer.

use heck::{ToLowerCamelCase, ToSnakeCase};
use proc_macro::TokenStream;
use quote::quote;
use syn::ext::IdentExt;
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
/// # Argument keys
///
/// Tauri renames command arguments to camelCase by default: a `size_kb` argument is
/// invoked as `invoke("cmd", { sizeKb })`. Each registered argument records that IPC
/// key (`CommandArg::key`), so an agent reading the registry passes the key the
/// command actually accepts. `#[tauri::command]` consumes its own attribute before
/// this macro runs, so when the command uses `rename_all = "snake_case"` or
/// `rename = "..."`, repeat it here — `#[inspectable(rename_all = "snake_case")]`,
/// `#[inspectable(rename = "...")]`. (Written *above* `#[tauri::command(...)]`,
/// `#[inspectable]` reads those options itself.)
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
    let command_options = tauri_command_options(&input.attrs);

    let fn_name = &input.sig.ident;
    let schema_fn_name = syn::Ident::new(&format!("{fn_name}__schema"), fn_name.span());
    // The name the frontend invokes: Tauri's `rename`, else the bare identifier
    // (`r#type` is invoked as `type`).
    let command_name = attrs
        .rename
        .clone()
        .or(command_options.rename)
        .unwrap_or_else(|| fn_name.unraw().to_string());
    let is_async = input.sig.asyncness.is_some();

    let description = attrs
        .description
        .unwrap_or_else(|| command_name.replace('_', " "));

    let snake_case = attrs
        .snake_case
        .or(command_options.snake_case)
        .unwrap_or(false);
    let args_info = extract_args(&input.sig);
    let arg_tokens: Vec<_> = args_info
        .iter()
        .map(|(name, type_str, required)| {
            let key = invoke_key(name, snake_case);
            quote! {
                victauri_core::registry::CommandArg::new(#name, #type_str, #required)
                    .with_key(#key)
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
            victauri_core::registry::CommandInfo::new(#command_name)
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
    /// `rename = "..."`: the name the command is invoked by.
    rename: Option<String>,
    /// `rename_all = "snake_case" | "camelCase"`: the argument key case.
    snake_case: Option<bool>,
}

/// The options of a `#[tauri::command(...)]` still present on the function, which
/// happens only when `#[inspectable]` is written above it.
#[derive(Default)]
struct CommandOptions {
    rename: Option<String>,
    snake_case: Option<bool>,
}

fn parse_rename_all(lit: &syn::LitStr) -> syn::Result<bool> {
    match lit.value().as_str() {
        "snake_case" => Ok(true),
        "camelCase" => Ok(false),
        _ => Err(syn::Error::new(
            lit.span(),
            "expected \"camelCase\" or \"snake_case\"",
        )),
    }
}

fn tauri_command_options(fn_attrs: &[syn::Attribute]) -> CommandOptions {
    let mut out = CommandOptions::default();
    for attr in fn_attrs {
        let path = attr.path();
        let is_command = path.segments.last().is_some_and(|s| s.ident == "command")
            && (path.segments.len() == 1 || path.segments[0].ident == "tauri");
        if !is_command || !matches!(attr.meta, syn::Meta::List(_)) {
            continue;
        }
        // Best effort: Tauri validates its own attribute (and `async` is a keyword
        // `parse_nested_meta` refuses), so a parse failure here is never an error.
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                out.snake_case = Some(parse_rename_all(&meta.value()?.parse()?)?);
            } else if meta.path.is_ident("rename") {
                out.rename = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.input.peek(syn::Token![=]) {
                // Other options (`root = "..."`) are Tauri's business.
                let _: syn::Expr = meta.value()?.parse()?;
            }
            Ok(())
        });
    }
    out
}

/// The IPC key Tauri derives for an argument name (tauri-macros `ArgumentCase`).
fn invoke_key(name: &str, snake_case: bool) -> String {
    if snake_case {
        name.to_snake_case()
    } else {
        name.to_lower_camel_case()
    }
}

fn parse_attrs(attr: TokenStream) -> syn::Result<InspectableAttrs> {
    let mut attrs = InspectableAttrs {
        description: None,
        intent: None,
        category: None,
        examples: Vec::new(),
        rename: None,
        snake_case: None,
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
        } else if meta.path.is_ident("rename") {
            attrs.rename = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("rename_all") {
            attrs.snake_case = Some(parse_rename_all(&meta.value()?.parse()?)?);
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
                    syn::Pat::Ident(ident) => ident.ident.unraw().to_string(),
                    _ => return None,
                };

                let ty = &*pat_type.ty;
                if is_tauri_framework_type(ty) {
                    return None;
                }
                let type_str = quote!(#ty).to_string();

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

/// Whether Tauri injects this argument itself (the `CommandArg` impls that do not
/// read the invoke payload), so it never appears in the frontend's arguments.
fn is_tauri_framework_type(ty: &syn::Type) -> bool {
    const FRAMEWORK_TYPES: &[&str] = &[
        "AppHandle",
        "State",
        "Window",
        "Webview",
        "WebviewWindow",
        "Request",
        "CommandScope",
        "GlobalScope",
    ];
    let ty = match ty {
        syn::Type::Reference(r) => &*r.elem,
        syn::Type::Paren(p) => &*p.elem,
        syn::Type::Group(g) => &*g.elem,
        other => other,
    };
    // Judge by the path's LAST segment, never by splitting its tokens as a string:
    // `State<'_, my::Pool>` has a `::` inside its generics.
    let syn::Type::Path(path) = ty else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|seg| FRAMEWORK_TYPES.iter().any(|t| seg.ident == t))
}

fn extract_return_type(sig: &syn::Signature) -> String {
    match &sig.output {
        syn::ReturnType::Default => "()".to_string(),
        syn::ReturnType::Type(_, ty) => quote!(#ty).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fn_attrs(src: &str) -> Vec<syn::Attribute> {
        syn::parse_str::<ItemFn>(src).unwrap().attrs
    }

    #[test]
    fn reads_rename_options_from_a_tauri_command_attribute() {
        let o = tauri_command_options(&fn_attrs(
            "#[tauri::command(rename_all = \"snake_case\", rename = \"x\")] fn f() {}",
        ));
        assert_eq!(o.snake_case, Some(true));
        assert_eq!(o.rename.as_deref(), Some("x"));
    }

    #[test]
    fn tolerates_the_rest_of_tauris_attribute_grammar() {
        for src in [
            "#[tauri::command(async)] fn f() {}",
            "#[command(root = \"crate\", rename_all = \"snake_case\")] fn f() {}",
            "#[tauri::command] fn f() {}",
            "#[other::command(rename = \"no\")] fn f() {}",
        ] {
            let o = tauri_command_options(&fn_attrs(src));
            assert!(o.rename.is_none() || src.contains("root"), "{src}");
        }
        let o = tauri_command_options(&fn_attrs(
            "#[command(root = \"crate\", rename_all = \"snake_case\")] fn f() {}",
        ));
        assert_eq!(o.snake_case, Some(true));
    }

    #[test]
    fn keys_match_tauri_argument_case() {
        assert_eq!(invoke_key("size_kb", false), "sizeKb");
        assert_eq!(invoke_key("a1_b", false), "a1B");
        assert_eq!(invoke_key("name", false), "name");
        assert_eq!(invoke_key("size_kb", true), "size_kb");
    }
}
