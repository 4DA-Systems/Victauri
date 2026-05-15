use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::json;

use crate::mcp_handler::VictauriBrowserHandler;

const SERVER_INSTRUCTIONS: &str = "Victauri Browser — MCP inspection for any website via Chrome \
extension. Tools: eval_js, dom_snapshot, find_elements, interact, input, inspect, css, logs, \
storage, navigate, wait_for, assert_semantic, recording, screenshot, tabs, page_info, cookies, \
get_diagnostics, get_plugin_info, get_memory_stats.";

impl ServerHandler for VictauriBrowserHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_instructions(SERVER_INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = build_tool_definitions();
        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let name = request.name.as_ref();
        let args = request
            .arguments
            .as_ref()
            .map(|m| serde_json::Value::Object(m.clone()))
            .unwrap_or(json!({}));

        match self.execute_tool(name, args).await {
            Ok(value) => {
                let text = match &value {
                    serde_json::Value::String(s) => s.clone(),
                    _ => serde_json::to_string_pretty(&value).unwrap_or_default(),
                };
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        build_tool_definitions()
            .into_iter()
            .find(|t| t.name.as_ref() == name)
    }
}

fn build_tool_definitions() -> Vec<Tool> {
    vec![
        tool_def("eval_js", "Execute JavaScript in the active page and return the result", json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "description": "JavaScript code to execute" },
                "tab_id": { "type": "integer", "description": "Target tab ID (optional, defaults to active)" }
            },
            "required": ["code"]
        })),
        tool_def("dom_snapshot", "Get accessible DOM tree with ref handles for interaction", json!({
            "type": "object",
            "properties": {
                "format": { "type": "string", "enum": ["compact", "json"], "description": "Output format" },
                "tab_id": { "type": "integer" }
            }
        })),
        tool_def("find_elements", "Search DOM elements by text, role, selector, or attribute", json!({
            "type": "object",
            "properties": {
                "query": { "type": "object", "description": "Search query with text/role/selector/attribute fields" },
                "tab_id": { "type": "integer" }
            }
        })),
        tool_def("interact", "Click, hover, focus, scroll, or select elements", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["click", "double_click", "hover", "focus", "scroll", "scroll_into_view", "select"] },
                "ref_id": { "type": "string", "description": "Element ref handle" },
                "timeout_ms": { "type": "integer" },
                "tab_id": { "type": "integer" }
            },
            "required": ["action"]
        })),
        tool_def("input", "Fill, type text, or press keyboard keys", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["fill", "type", "press_key", "clear"] },
                "ref_id": { "type": "string" },
                "value": { "type": "string", "description": "Value for fill" },
                "text": { "type": "string", "description": "Text for type" },
                "key": { "type": "string", "description": "Key for press_key" },
                "timeout_ms": { "type": "integer" },
                "tab_id": { "type": "integer" }
            },
            "required": ["action"]
        })),
        tool_def("inspect", "CSS inspection, visual debug overlays, accessibility audit, performance metrics", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["styles", "bounds", "highlight", "clear_highlights", "accessibility", "performance"] },
                "ref_id": { "type": "string" },
                "ref_ids": { "type": "array", "items": { "type": "string" } },
                "properties": { "type": "array", "items": { "type": "string" } },
                "color": { "type": "string" },
                "label": { "type": "string" },
                "tab_id": { "type": "integer" }
            },
            "required": ["action"]
        })),
        tool_def("css", "Inject or remove custom CSS for debugging/prototyping", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["inject", "remove"] },
                "css": { "type": "string", "description": "CSS to inject" },
                "tab_id": { "type": "integer" }
            },
            "required": ["action"]
        })),
        tool_def("logs", "Console, network, navigation, dialog, and event logs", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["console", "network", "navigation", "dialogs", "events"] },
                "since": { "type": "number", "description": "Timestamp filter" },
                "filter": { "type": "string" },
                "limit": { "type": "integer" },
                "tab_id": { "type": "integer" }
            },
            "required": ["action"]
        })),
        tool_def("storage", "localStorage, sessionStorage, and cookie access", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["get", "set", "delete", "cookies"] },
                "store": { "type": "string", "enum": ["local", "session"] },
                "key": { "type": "string" },
                "value": { "type": "string" },
                "tab_id": { "type": "integer" }
            },
            "required": ["action"]
        })),
        tool_def("navigate", "Navigate pages, go back, manage dialogs", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["go_to", "back", "history", "dialogs"] },
                "url": { "type": "string" },
                "tab_id": { "type": "integer" }
            },
            "required": ["action"]
        })),
        tool_def("wait_for", "Wait for DOM conditions, text, or URL changes", json!({
            "type": "object",
            "properties": {
                "condition": { "type": "string", "enum": ["selector", "selector_gone", "text", "text_gone", "url"] },
                "value": { "type": "string", "description": "Selector, text, or URL pattern to wait for" },
                "timeout_ms": { "type": "integer", "description": "Max wait time (default 10000)" },
                "tab_id": { "type": "integer" }
            },
            "required": ["condition", "value"]
        })),
        tool_def("assert_semantic", "Evaluate an expression and assert a condition on the result", json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string", "description": "JavaScript expression to evaluate" },
                "condition": { "type": "string", "enum": ["equals", "not_equals", "contains", "truthy", "greater_than", "less_than"] },
                "expected": { "type": "string", "description": "Expected value for comparison" },
                "tab_id": { "type": "integer" }
            },
            "required": ["expression", "condition"]
        })),
        tool_def("recording", "Record interactions, create checkpoints, replay", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["start", "stop", "checkpoint", "get_events", "list_checkpoints", "export"] },
                "label": { "type": "string", "description": "Checkpoint label" },
                "since": { "type": "number" },
                "tab_id": { "type": "integer" }
            },
            "required": ["action"]
        })),
        tool_def("screenshot", "Capture page screenshot as PNG (base64)", json!({
            "type": "object",
            "properties": {
                "full_page": { "type": "boolean", "description": "Capture full scrollable page" },
                "tab_id": { "type": "integer" }
            }
        })),
        tool_def("tabs", "List and manage browser tabs", json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list"], "description": "Tab action" }
            }
        })),
        tool_def("page_info", "Get page metadata, URL, title, and resource info", json!({
            "type": "object",
            "properties": {
                "tab_id": { "type": "integer" }
            }
        })),
        tool_def("cookies", "Get cookies for the current page", json!({
            "type": "object",
            "properties": {
                "tab_id": { "type": "integer" }
            }
        })),
        tool_def("get_diagnostics", "Browser extension diagnostics and health info", json!({
            "type": "object",
            "properties": {
                "tab_id": { "type": "integer" }
            }
        })),
        tool_def("get_plugin_info", "Extension and native host version info", json!({
            "type": "object",
            "properties": {}
        })),
        tool_def("get_memory_stats", "JavaScript heap memory statistics", json!({
            "type": "object",
            "properties": {
                "tab_id": { "type": "integer" }
            }
        })),
    ]
}

fn tool_def(name: &str, description: &str, schema: serde_json::Value) -> Tool {
    serde_json::from_value(json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    }))
    .expect("tool definition must be valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge_dispatch::BridgeDispatch;
    use crate::tab_state::TabManager;
    use rmcp::ServerHandler;
    use std::sync::Arc;

    fn make_handler() -> VictauriBrowserHandler {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        VictauriBrowserHandler::new(tab_mgr, dispatch)
    }

    #[test]
    fn server_info_has_tools_capability() {
        let handler = make_handler();
        let info = handler.get_info();
        let caps = info.capabilities;
        assert!(caps.tools.is_some());
    }

    #[test]
    fn tool_definitions_are_20() {
        let tools = build_tool_definitions();
        assert_eq!(tools.len(), 20);
    }

    #[test]
    fn all_tools_have_descriptions() {
        let tools = build_tool_definitions();
        for tool in &tools {
            assert!(
                tool.description.is_some(),
                "tool {} missing description",
                tool.name
            );
        }
    }

    #[test]
    fn tool_names_are_unique() {
        let tools = build_tool_definitions();
        let mut names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 20);
    }

    #[test]
    fn get_tool_finds_existing() {
        let handler = make_handler();
        let tool = handler.get_tool("eval_js");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name.as_ref(), "eval_js");
    }

    #[test]
    fn get_tool_returns_none_for_unknown() {
        let handler = make_handler();
        assert!(handler.get_tool("nonexistent").is_none());
    }

    #[test]
    fn all_tools_have_input_schema() {
        let tools = build_tool_definitions();
        for tool in &tools {
            assert!(
                !tool.input_schema.is_empty(),
                "tool {} has empty input schema",
                tool.name
            );
        }
    }

    #[test]
    fn tools_with_required_action_param() {
        let action_tools = [
            "interact", "input", "inspect", "css", "logs", "storage", "navigate", "recording",
        ];
        let tools = build_tool_definitions();
        for name in action_tools {
            let tool = tools.iter().find(|t| t.name.as_ref() == name).unwrap();
            let schema_value = serde_json::Value::Object((*tool.input_schema).clone());
            let required = schema_value.get("required").and_then(|r| r.as_array());
            assert!(
                required.is_some_and(|r| r.iter().any(|v| v == "action")),
                "tool {name} should require 'action' parameter"
            );
        }
    }

    #[test]
    fn eval_js_requires_code_in_schema() {
        let tools = build_tool_definitions();
        let eval = tools.iter().find(|t| t.name.as_ref() == "eval_js").unwrap();
        let schema_value = serde_json::Value::Object((*eval.input_schema).clone());
        let required = schema_value.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|v| v == "code"));
    }

    #[test]
    fn assert_semantic_schema_has_conditions() {
        let tools = build_tool_definitions();
        let tool = tools
            .iter()
            .find(|t| t.name.as_ref() == "assert_semantic")
            .unwrap();
        let schema_value = serde_json::Value::Object((*tool.input_schema).clone());
        let condition_enum = schema_value["properties"]["condition"]["enum"]
            .as_array()
            .unwrap();
        let conditions: Vec<&str> = condition_enum.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(conditions.contains(&"equals"));
        assert!(conditions.contains(&"truthy"));
        assert!(conditions.contains(&"greater_than"));
        assert!(conditions.contains(&"less_than"));
        assert!(conditions.contains(&"contains"));
        assert!(conditions.contains(&"not_equals"));
    }

    #[test]
    fn get_tool_matches_list_tools() {
        let handler = make_handler();
        let tools = build_tool_definitions();
        for tool in &tools {
            let found = handler.get_tool(tool.name.as_ref());
            assert!(found.is_some(), "get_tool should find {}", tool.name);
            assert_eq!(found.unwrap().name, tool.name);
        }
    }

    #[test]
    fn server_instructions_mention_all_tools() {
        let tools = build_tool_definitions();
        for tool in &tools {
            assert!(
                SERVER_INSTRUCTIONS.contains(tool.name.as_ref()),
                "instructions should mention {}",
                tool.name
            );
        }
    }

    // --- Adversarial schema validation tests ---

    #[test]
    fn all_schemas_have_type_object() {
        let tools = build_tool_definitions();
        for tool in &tools {
            let schema = serde_json::Value::Object((*tool.input_schema).clone());
            assert_eq!(
                schema["type"], "object",
                "tool {} schema must be type:object",
                tool.name
            );
        }
    }

    #[test]
    fn all_schemas_have_properties() {
        let tools = build_tool_definitions();
        for tool in &tools {
            let schema = serde_json::Value::Object((*tool.input_schema).clone());
            assert!(
                schema.get("properties").is_some(),
                "tool {} schema must have 'properties'",
                tool.name
            );
        }
    }

    #[test]
    fn tool_names_match_handler_list() {
        let handler = make_handler();
        let handler_tools = handler.list_tools();
        let mcp_tools = build_tool_definitions();

        let mut handler_names: Vec<&str> = handler_tools.iter().map(|t| t.name.as_str()).collect();
        let mut mcp_names: Vec<&str> = mcp_tools.iter().map(|t| t.name.as_ref()).collect();
        handler_names.sort();
        mcp_names.sort();

        assert_eq!(handler_names, mcp_names, "handler tools must match MCP definitions");
    }

    #[test]
    fn tab_id_present_in_most_schemas() {
        let tools = build_tool_definitions();
        let no_tab_tools = ["get_plugin_info", "tabs"];
        for tool in &tools {
            let name = tool.name.as_ref();
            if no_tab_tools.contains(&name) {
                continue;
            }
            let schema = serde_json::Value::Object((*tool.input_schema).clone());
            assert!(
                schema["properties"].get("tab_id").is_some(),
                "tool {name} should have tab_id property"
            );
        }
    }

    #[test]
    fn action_enum_values_match_handler_routing() {
        let tools = build_tool_definitions();

        let expected_actions: std::collections::HashMap<&str, Vec<&str>> = [
            ("interact", vec!["click", "double_click", "hover", "focus", "scroll", "scroll_into_view", "select"]),
            ("input", vec!["fill", "type", "press_key", "clear"]),
            ("inspect", vec!["styles", "bounds", "highlight", "clear_highlights", "accessibility", "performance"]),
            ("css", vec!["inject", "remove"]),
            ("logs", vec!["console", "network", "navigation", "dialogs", "events"]),
            ("storage", vec!["get", "set", "delete", "cookies"]),
            ("navigate", vec!["go_to", "back", "history", "dialogs"]),
            ("recording", vec!["start", "stop", "checkpoint", "get_events", "list_checkpoints", "export"]),
        ]
        .into_iter()
        .collect();

        for (tool_name, expected) in &expected_actions {
            let tool = tools.iter().find(|t| t.name.as_ref() == *tool_name).unwrap();
            let schema = serde_json::Value::Object((*tool.input_schema).clone());
            let enum_vals = schema["properties"]["action"]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{tool_name} missing action enum"));
            let mut actual: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
            let mut expected_sorted = expected.clone();
            actual.sort();
            expected_sorted.sort();
            assert_eq!(
                actual, expected_sorted,
                "action enum mismatch for {tool_name}"
            );
        }
    }

    #[test]
    fn wait_for_requires_condition_and_value() {
        let tools = build_tool_definitions();
        let tool = tools.iter().find(|t| t.name.as_ref() == "wait_for").unwrap();
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let required = schema["required"].as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_names.contains(&"condition"));
        assert!(required_names.contains(&"value"));
    }

    #[test]
    fn assert_semantic_requires_expression_and_condition() {
        let tools = build_tool_definitions();
        let tool = tools
            .iter()
            .find(|t| t.name.as_ref() == "assert_semantic")
            .unwrap();
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let required = schema["required"].as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_names.contains(&"expression"));
        assert!(required_names.contains(&"condition"));
    }

    #[test]
    fn no_tool_has_empty_description() {
        let tools = build_tool_definitions();
        for tool in &tools {
            let desc = tool.description.as_deref().unwrap_or("");
            assert!(
                desc.len() > 10,
                "tool {} has too-short description: {:?}",
                tool.name,
                desc
            );
        }
    }

    #[test]
    fn tool_def_json_roundtrips() {
        let tools = build_tool_definitions();
        for tool in &tools {
            let serialized = serde_json::to_string(&tool).unwrap();
            let deserialized: serde_json::Value = serde_json::from_str(&serialized).unwrap();
            assert_eq!(
                deserialized["name"].as_str().unwrap(),
                tool.name.as_ref(),
                "tool {} failed JSON roundtrip",
                tool.name
            );
        }
    }

    // --- Deep challenger: cross-module consistency ---

    #[tokio::test]
    async fn schema_tool_names_match_handler_tool_list() {
        use crate::mcp_handler::VictauriBrowserHandler;
        use crate::bridge_dispatch::BridgeDispatch;
        use crate::tab_state::TabManager;
        use std::sync::Arc;

        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(tab_mgr, dispatch);

        let schema_tools = build_tool_definitions();
        let handler_tools = handler.list_tools();

        let schema_names: std::collections::HashSet<&str> =
            schema_tools.iter().map(|t| t.name.as_ref()).collect();
        let handler_names: std::collections::HashSet<&str> =
            handler_tools.iter().map(|t| t.name.as_str()).collect();

        // Every schema tool must be in the handler list
        for name in &schema_names {
            assert!(
                handler_names.contains(name),
                "schema defines '{name}' but handler.list_tools() doesn't"
            );
        }

        // Every handler tool must be in the schema
        for name in &handler_names {
            assert!(
                schema_names.contains(name),
                "handler lists '{name}' but schema doesn't define it"
            );
        }

        assert_eq!(schema_names.len(), handler_names.len());
    }

    #[tokio::test]
    async fn all_schema_tools_recognized_by_handler() {
        use crate::mcp_handler::VictauriBrowserHandler;
        use crate::bridge_dispatch::BridgeDispatch;
        use crate::tab_state::TabManager;
        use std::sync::Arc;

        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(Arc::clone(&tab_mgr), Arc::clone(&dispatch));

        // Spawn a responder to resolve any bridge commands immediately
        let d = Arc::clone(&dispatch);
        let responder = tokio::spawn(async move {
            for _ in 0..200 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                let ids = d.pending_ids().await;
                for id in ids {
                    d.on_response(&id, Some(json!({"mock": true, "js_heap": {}})), None).await;
                }
            }
        });

        let tools = build_tool_definitions();
        for tool in &tools {
            let name = tool.name.as_ref();
            let result = handler.execute_tool(name, json!({})).await;
            match result {
                Ok(_) => {}
                Err(e) => {
                    assert!(
                        !e.contains("unknown tool"),
                        "schema tool '{name}' not recognized by handler: {e}"
                    );
                }
            }
        }

        responder.abort();
    }

    #[test]
    fn server_instructions_list_all_tool_names() {
        let tools = build_tool_definitions();
        for tool in &tools {
            let name = tool.name.as_ref();
            assert!(
                SERVER_INSTRUCTIONS.contains(name),
                "tool '{name}' missing from SERVER_INSTRUCTIONS string"
            );
        }
    }

    #[test]
    fn no_duplicate_tool_names_in_schema() {
        let tools = build_tool_definitions();
        let mut seen = std::collections::HashSet::new();
        for tool in &tools {
            assert!(
                seen.insert(tool.name.as_ref()),
                "duplicate tool name in schema: {}",
                tool.name
            );
        }
    }

    #[test]
    fn schema_required_fields_are_in_properties() {
        let tools = build_tool_definitions();
        for tool in &tools {
            let schema = serde_json::Value::Object((*tool.input_schema).clone());
            if let Some(required) = schema["required"].as_array() {
                let properties = schema["properties"].as_object().unwrap_or_else(
                    || panic!("{} has required but no properties", tool.name),
                );
                for req in required {
                    let field = req.as_str().unwrap();
                    assert!(
                        properties.contains_key(field),
                        "{}: required field '{}' not in properties",
                        tool.name,
                        field
                    );
                }
            }
        }
    }
}
