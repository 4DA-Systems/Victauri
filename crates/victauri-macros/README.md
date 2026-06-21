# victauri-macros

Proc macros for [Victauri](https://github.com/4DA-Systems/victauri) -- auto-instrumentation of Tauri commands for AI agent introspection.

## The `#[inspectable]` Macro

Annotate any `#[tauri::command]` to make it discoverable by Victauri's MCP server:

```rust
use victauri_macros::inspectable;

#[tauri::command]
#[inspectable(
    description = "Greet the user",
    intent = "say hello",
    category = "social",
    example = "greet someone"
)]
async fn greet(name: String) -> String {
    format!("Hello, {name}!")
}
```

This generates a companion function `greet__schema()` that returns a `CommandInfo` struct containing the command's name, parameters, and all metadata. Victauri's MCP tools use this for command discovery, natural language resolution, and ghost command detection.

## Attributes

| Attribute | Required | Description |
|---|---|---|
| `description` | Yes | What the command does |
| `intent` | No | Natural language intent for NL-to-command resolution |
| `category` | No | Grouping label (e.g., "settings", "data", "ui") |
| `example` | No | Example usage phrase |

All code generation happens at compile time with zero runtime cost.

## Documentation

Full API docs: [docs.rs/victauri-macros](https://docs.rs/victauri-macros)

## License

Apache-2.0 -- see [LICENSE](../../LICENSE)

Part of [Victauri](https://github.com/4DA-Systems/victauri). Built by [4DA Systems](https://4da.ai).
