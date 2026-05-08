#![allow(dead_code)]

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

#[test]
fn inspectable_default_description() {
    let info: CommandInfo = simple_command__schema();
    assert_eq!(info.description.as_deref(), Some("simple command"));
    assert_eq!(info.args.len(), 0);
}

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
