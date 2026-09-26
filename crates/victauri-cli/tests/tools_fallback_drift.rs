//! Drift guard: the bridge's baked fallback tool list (`src/tools_fallback.json`, served
//! while the app is down) must name exactly the tools the plugin registers with `#[tool(`
//! in `victauri-plugin/src/mcp/mod.rs`. A tool added to or removed from the plugin without
//! regenerating the fallback would otherwise ship a stale cold-start tool list.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Extract the MCP tool names from the plugin source: for each `#[tool(` attribute, the
/// name is an explicit `name = "..."` inside the attribute if present, else the identifier
/// of the next `fn`.
fn plugin_tool_names(source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut rest = source;
    while let Some(start) = rest.find("#[tool(") {
        rest = &rest[start + "#[tool(".len()..];
        let Some(fn_pos) = rest.find("fn ") else {
            break;
        };
        let attr = &rest[..fn_pos];
        let explicit = attr.lines().find_map(|line| {
            let value = line.trim().strip_prefix("name")?.trim_start();
            let value = value.strip_prefix('=')?.trim_start().strip_prefix('"')?;
            Some(value[..value.find('"')?].to_string())
        });
        let ident: String = rest[fn_pos + 3..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        names.insert(explicit.unwrap_or(ident));
    }
    names
}

#[test]
fn fallback_tool_list_matches_plugin_tools() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let plugin_src = manifest.join("../victauri-plugin/src/mcp/mod.rs");
    let Ok(source) = std::fs::read_to_string(&plugin_src) else {
        // Packaged crate (crates.io tarball): the sibling plugin source isn't present.
        eprintln!("skipping: {} not found", plugin_src.display());
        return;
    };
    let plugin = plugin_tool_names(&source);
    assert!(
        plugin.len() >= 30,
        "parsed only {} #[tool( names from {} — did the attribute layout change?",
        plugin.len(),
        plugin_src.display()
    );

    let fallback_json = std::fs::read_to_string(manifest.join("src/tools_fallback.json"))
        .expect("read tools_fallback.json");
    let fallback: Vec<serde_json::Value> =
        serde_json::from_str(&fallback_json).expect("tools_fallback.json is a JSON array");
    let fallback_names: BTreeSet<String> = fallback
        .iter()
        .map(|t| {
            t["name"]
                .as_str()
                .expect("every fallback tool has a string name")
                .to_string()
        })
        .collect();
    assert_eq!(
        fallback_names.len(),
        fallback.len(),
        "duplicate names in tools_fallback.json"
    );

    let missing: Vec<_> = plugin.difference(&fallback_names).collect();
    let extra: Vec<_> = fallback_names.difference(&plugin).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "tools_fallback.json drifted from the plugin's #[tool( set.\n  \
         missing from fallback: {missing:?}\n  not in plugin: {extra:?}\n  \
         Regenerate crates/victauri-cli/src/tools_fallback.json from the #[tool] annotations."
    );
}

#[test]
fn parser_handles_explicit_names_and_plain_fns() {
    let src = r#"
    #[tool(
        description = "a",
        annotations(read_only_hint = true)
    )]
    async fn alpha(&self) {}

    #[tool(
        name = "renamed",
        description = "b"
    )]
    async fn beta(&self) {}
    "#;
    let names = plugin_tool_names(src);
    assert_eq!(
        names.into_iter().collect::<Vec<_>>(),
        vec!["alpha".to_string(), "renamed".to_string()]
    );
}
