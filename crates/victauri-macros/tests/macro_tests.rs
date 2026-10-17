#![allow(dead_code)]
// Most tests read the schema metadata, which `#[inspectable]` compiles in only under
// `debug_assertions`; under `cargo test --release` they are compiled out.
#![cfg_attr(not(debug_assertions), allow(unused_imports))]

use victauri_core::registry::CommandInfo;

mod fake_tauri_types {
    pub struct AppHandle;
    pub struct State<T>(std::marker::PhantomData<T>);
    pub struct Window;
    pub struct Webview;
}

#[victauri_macros::inspectable(description = "Save an API key")]
fn save_api_key(provider: String, key: String) -> Result<(), String> {
    let _ = (provider, key);
    Ok(())
}

#[victauri_macros::inspectable(description = "Load user data")]
async fn load_user(user_id: u64, include_profile: Option<bool>) -> Result<String, String> {
    let _ = (user_id, include_profile);
    Ok("user".to_string())
}

#[victauri_macros::inspectable]
fn simple_command() -> String {
    "hello".to_string()
}

#[victauri_macros::inspectable(
    description = "Persist user prefs",
    intent = "save user preferences to persistent storage",
    category = "settings",
    example = "save my settings",
    example = "persist preferences"
)]
fn save_user_preferences(prefs: String) -> Result<(), String> {
    let _ = prefs;
    Ok(())
}

#[cfg(debug_assertions)] // metadata is debug-only (see `release_builds_register_nothing`)
#[test]
fn inspectable_generates_schema_fn() {
    let info: CommandInfo = save_api_key__schema();
    assert_eq!(info.name, "save_api_key");
    assert_eq!(info.description.as_deref(), Some("Save an API key"));
    assert!(!info.is_async);
    assert_eq!(info.args.len(), 2);
    assert_eq!(info.args[0].name, "provider");
    assert_eq!(info.args[0].type_name, "String");
    assert!(info.args[0].required);
    assert_eq!(info.args[1].name, "key");
    assert!(info.args[1].required);
    assert!(info.intent.is_none());
    assert!(info.category.is_none());
    assert!(info.examples.is_empty());
}

#[cfg(debug_assertions)] // metadata is debug-only (see `release_builds_register_nothing`)
#[test]
fn inspectable_async_fn() {
    let info: CommandInfo = load_user__schema();
    assert_eq!(info.name, "load_user");
    assert!(info.is_async);
    assert_eq!(info.args.len(), 2);
    assert_eq!(info.args[0].name, "user_id");
    assert_eq!(info.args[0].type_name, "u64");
    assert!(info.args[0].required);
    assert_eq!(info.args[1].name, "include_profile");
    assert!(!info.args[1].required);
}

#[cfg(debug_assertions)] // metadata is debug-only (see `release_builds_register_nothing`)
#[test]
fn inspectable_default_description() {
    let info: CommandInfo = simple_command__schema();
    assert_eq!(info.description.as_deref(), Some("simple command"));
    assert_eq!(info.args.len(), 0);
}

#[cfg(debug_assertions)] // metadata is debug-only (see `release_builds_register_nothing`)
#[test]
fn inspectable_with_intent_annotations() {
    let info: CommandInfo = save_user_preferences__schema();
    assert_eq!(info.description.as_deref(), Some("Persist user prefs"));
    assert_eq!(
        info.intent.as_deref(),
        Some("save user preferences to persistent storage")
    );
    assert_eq!(info.category.as_deref(), Some("settings"));
    assert_eq!(info.examples.len(), 2);
    assert_eq!(info.examples[0], "save my settings");
    assert_eq!(info.examples[1], "persist preferences");
    assert_eq!(info.args.len(), 1);
    assert_eq!(info.args[0].name, "prefs");
}

// Auto-discovery registration is emitted only under `debug_assertions` (in the consumer's
// profile), so these hold in debug builds; `release_builds_register_nothing` pins the other side.
#[cfg(debug_assertions)]
#[test]
fn auto_discovery_finds_inspectable_commands() {
    let discovered = victauri_core::auto_discovered_commands();
    let names: Vec<&str> = discovered.iter().map(|c| c.name.as_str()).collect();

    assert!(
        names.contains(&"save_api_key"),
        "auto_discover should find save_api_key, got: {names:?}"
    );
    assert!(
        names.contains(&"simple_command"),
        "auto_discover should find simple_command, got: {names:?}"
    );
    assert!(
        names.contains(&"save_user_preferences"),
        "auto_discover should find save_user_preferences, got: {names:?}"
    );
    assert!(
        names.contains(&"load_user"),
        "auto_discover should find load_user, got: {names:?}"
    );
    assert!(
        discovered.len() >= 4,
        "should discover at least 4 commands, got {}",
        discovered.len()
    );
}

#[cfg(debug_assertions)]
#[test]
fn auto_discovery_preserves_metadata() {
    let discovered = victauri_core::auto_discovered_commands();
    let prefs_cmd = discovered
        .iter()
        .find(|c| c.name == "save_user_preferences")
        .expect("save_user_preferences should be auto-discovered");

    assert_eq!(prefs_cmd.description.as_deref(), Some("Persist user prefs"));
    assert_eq!(prefs_cmd.category.as_deref(), Some("settings"));
    assert_eq!(prefs_cmd.examples.len(), 2);
    assert_eq!(prefs_cmd.args.len(), 1);
    assert_eq!(prefs_cmd.args[0].name, "prefs");
}

#[cfg(debug_assertions)]
#[test]
fn registry_from_auto_discovery() {
    let registry = victauri_core::CommandRegistry::from_auto_discovery();
    assert!(
        registry.count() >= 4,
        "registry should have at least 4 auto-discovered commands, got {}",
        registry.count()
    );
    assert!(
        registry.get("save_api_key").is_some(),
        "registry should contain save_api_key"
    );
    assert!(
        registry.get("load_user").is_some(),
        "registry should contain load_user"
    );
}

/// `cargo test --release`: a release build carries no `#[inspectable]` registration (no
/// startup constructor, no metadata strings), while the `__schema()` companions still work.
#[cfg(not(debug_assertions))]
#[test]
fn release_builds_register_nothing() {
    assert!(
        victauri_core::auto_discovered_commands().is_empty(),
        "release builds must not register #[inspectable] commands"
    );
    let info = save_api_key__schema();
    assert_eq!(info.name, "save_api_key");
    assert!(info.description.is_none(), "metadata is debug-only");
    assert!(info.args.is_empty(), "metadata is debug-only");
    assert!(save_user_preferences__schema().examples.is_empty());
}

// ── IPC argument keys (audit: the registry advertised Rust names, but Tauri invokes
//    commands with camelCase keys by default, so an agent following it failed) ──

#[victauri_macros::inspectable]
fn large_payload(size_kb: usize, r#type: String) -> String {
    let _ = (size_kb, r#type);
    String::new()
}

#[victauri_macros::inspectable(rename_all = "snake_case")]
fn snake_keys(size_kb: usize) -> usize {
    size_kb
}

#[victauri_macros::inspectable(rename = "renamed_cmd")]
fn original_name(user_id: u64) -> u64 {
    user_id
}

/// A path-qualified `State` whose generics contain `::` is still an injected arg.
#[victauri_macros::inspectable]
fn with_injected(
    app: fake_tauri_types::AppHandle,
    state: fake_tauri_types::State<std::sync::Mutex<u8>>,
    window: &fake_tauri_types::Window,
    page_size: u32,
) -> u32 {
    let _ = (app, state, window);
    page_size
}

#[cfg(debug_assertions)] // metadata is debug-only (see `release_builds_register_nothing`)
#[test]
fn arg_keys_follow_tauris_camel_case_default() {
    let info = large_payload__schema();
    assert_eq!(info.args[0].name, "size_kb");
    assert_eq!(info.args[0].invoke_key(), "sizeKb");
    // Raw identifiers are invoked without the `r#` prefix.
    assert_eq!(info.args[1].name, "type");
    assert_eq!(info.args[1].key, None);
    // Unchanged keys stay implicit (`key: None`), as for single-word args.
    assert_eq!(save_api_key__schema().args[0].key, None);
}

#[cfg(debug_assertions)] // metadata is debug-only (see `release_builds_register_nothing`)
#[test]
fn rename_all_snake_case_keeps_rust_names() {
    let info = snake_keys__schema();
    assert_eq!(info.args[0].invoke_key(), "size_kb");
    assert_eq!(info.args[0].key, None);
}

#[cfg(debug_assertions)] // metadata is debug-only (see `release_builds_register_nothing`)
#[test]
fn rename_sets_the_invoked_command_name() {
    let info = original_name__schema();
    assert_eq!(info.name, "renamed_cmd");
    assert_eq!(info.args[0].invoke_key(), "userId");
}

#[cfg(debug_assertions)] // metadata is debug-only (see `release_builds_register_nothing`)
#[test]
fn injected_framework_args_are_skipped_even_with_paths_in_generics() {
    let info = with_injected__schema();
    let names: Vec<&str> = info.args.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["page_size"]);
    assert_eq!(info.args[0].invoke_key(), "pageSize");
}
