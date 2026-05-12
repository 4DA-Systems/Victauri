use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rmcp::model::{CallToolResult, RawContent};
use serde::Serialize;

use super::VictauriMcpHandler;

#[derive(Serialize)]
struct ToolInfo {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

pub enum ToolCallError {
    UnknownTool(String),
    InvalidParams(String),
}

pub fn router(handler: VictauriMcpHandler) -> axum::Router {
    axum::Router::new()
        .route("/", axum::routing::get(list_tools))
        .route("/{tool_name}", axum::routing::post(call_tool))
        .with_state(handler)
}

async fn list_tools(State(handler): State<VictauriMcpHandler>) -> Json<Vec<ToolInfo>> {
    let tools = VictauriMcpHandler::tool_router()
        .list_all()
        .into_iter()
        .filter(|t| handler.is_tool_enabled(t.name.as_ref()))
        .map(|t| ToolInfo {
            name: t.name.to_string(),
            description: t.description.as_deref().map(String::from),
        })
        .collect();
    Json(tools)
}

async fn call_tool(
    State(handler): State<VictauriMcpHandler>,
    Path(tool_name): Path<String>,
    body: String,
) -> Response {
    let args: serde_json::Value = if body.trim().is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("invalid JSON: {e}") })),
                )
                    .into_response();
            }
        }
    };

    match handler.execute_tool(&tool_name, args).await {
        Ok(result) => (StatusCode::OK, Json(to_rest_json(result))).into_response(),
        Err(ToolCallError::UnknownTool(name)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("unknown tool: {name}") })),
        )
            .into_response(),
        Err(ToolCallError::InvalidParams(msg)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("invalid parameters: {msg}") })),
        )
            .into_response(),
    }
}

fn to_rest_json(result: CallToolResult) -> serde_json::Value {
    let is_error = result.is_error.unwrap_or(false);

    if is_error {
        let msg = result
            .content
            .iter()
            .find_map(|c| match &c.raw {
                RawContent::Text(tc) => Some(tc.text.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "unknown error".to_string());
        return serde_json::json!({ "error": msg });
    }

    if result.content.len() == 1 {
        return match &result.content[0].raw {
            RawContent::Text(tc) => {
                let parsed = serde_json::from_str::<serde_json::Value>(&tc.text)
                    .unwrap_or_else(|_| serde_json::Value::String(tc.text.clone()));
                serde_json::json!({ "result": parsed })
            }
            RawContent::Image(ic) => serde_json::json!({
                "result": {
                    "type": "image",
                    "data": ic.data,
                    "mimeType": ic.mime_type,
                }
            }),
            _ => serde_json::json!({ "result": null }),
        };
    }

    let items: Vec<serde_json::Value> = result
        .content
        .iter()
        .filter_map(|c| match &c.raw {
            RawContent::Text(tc) => Some(serde_json::json!({ "type": "text", "text": tc.text })),
            RawContent::Image(ic) => Some(serde_json::json!({
                "type": "image",
                "data": ic.data,
                "mimeType": ic.mime_type,
            })),
            _ => None,
        })
        .collect();
    serde_json::json!({ "result": items })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::Content;

    #[test]
    fn text_result_parsed_as_json() {
        let result = CallToolResult::success(vec![Content::text(
            r#"{"title":"4DA","version":"1.0"}"#,
        )]);
        let json = to_rest_json(result);
        assert_eq!(json["result"]["title"], "4DA");
        assert_eq!(json["result"]["version"], "1.0");
    }

    #[test]
    fn text_result_plain_string() {
        let result = CallToolResult::success(vec![Content::text("hello world")]);
        let json = to_rest_json(result);
        assert_eq!(json["result"], "hello world");
    }

    #[test]
    fn error_result() {
        let mut result = CallToolResult::success(vec![Content::text("eval timed out after 30s")]);
        result.is_error = Some(true);
        let json = to_rest_json(result);
        assert_eq!(json["error"], "eval timed out after 30s");
        assert!(json.get("result").is_none());
    }

    #[test]
    fn image_result() {
        let result = CallToolResult::success(vec![Content::image("aWJhc2U2NA==", "image/png")]);
        let json = to_rest_json(result);
        assert_eq!(json["result"]["type"], "image");
        assert_eq!(json["result"]["data"], "aWJhc2U2NA==");
        assert_eq!(json["result"]["mimeType"], "image/png");
    }

    #[test]
    fn empty_content() {
        let result = CallToolResult::success(vec![]);
        let json = to_rest_json(result);
        assert_eq!(json["result"], serde_json::json!([]));
    }
}
