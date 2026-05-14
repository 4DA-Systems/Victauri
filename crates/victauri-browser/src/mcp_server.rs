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
    Tool {
        name: name.into(),
        description: Some(description.into()),
        input_schema: serde_json::from_value(schema).unwrap_or_default(),
        ..Default::default()
    }
}
