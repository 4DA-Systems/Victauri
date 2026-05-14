mod auth;
mod bridge_dispatch;
mod installer;
mod mcp_handler;
mod mcp_server;
mod native_messaging;
mod server;
mod tab_state;

use std::net::SocketAddr;
use std::sync::Arc;

use bridge_dispatch::BridgeDispatch;
use mcp_handler::VictauriBrowserHandler;
use tab_state::TabManager;

const DEFAULT_PORT: u16 = 7474;
const PORT_RANGE: u16 = 10;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map_or("serve", String::as_str);

    match command {
        "install" => {
            let extension_id = args.get(2).map_or("EXTENSION_ID", String::as_str);
            let binary = std::env::current_exe()?
                .to_string_lossy()
                .to_string();
            let path = installer::install(&binary, extension_id)?;
            println!("Native messaging host registered at: {path}");
            println!("Extension ID: {extension_id}");
            println!("\nAdd to your .mcp.json:");
            println!(
                r#"{{
  "mcpServers": {{
    "victauri-browser": {{
      "url": "http://127.0.0.1:{DEFAULT_PORT}/mcp"
    }}
  }}
}}"#
            );
            Ok(())
        }
        "uninstall" => {
            installer::uninstall()?;
            println!("Native messaging host unregistered.");
            Ok(())
        }
        "version" => {
            println!("victauri-browser-host {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => serve().await,
    }
}

async fn serve() -> anyhow::Result<()> {
    let port = std::env::var("VICTAURI_BROWSER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let auth_token = std::env::var("VICTAURI_BROWSER_AUTH_TOKEN").ok().or_else(|| {
        let token = auth::generate_token();
        tracing::info!("Generated auth token: {token}");
        Some(token)
    });

    let tab_manager = Arc::new(TabManager::new());
    let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));

    spawn_native_reader(Arc::clone(&dispatch), Arc::clone(&tab_manager));

    let handler = VictauriBrowserHandler::new(Arc::clone(&tab_manager), dispatch);
    let app = server::build_app(handler, auth_token);

    let addr = try_bind(port).await?;
    tracing::info!("victauri-browser listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn spawn_native_reader(dispatch: Arc<BridgeDispatch>, tab_manager: Arc<TabManager>) {
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        loop {
            let msg = match native_messaging::read_message(&mut stdin).await {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::warn!("native messaging read error: {e}");
                    dispatch.cancel_all().await;
                    break;
                }
            };

            let msg_type = msg.get("type").and_then(serde_json::Value::as_str).unwrap_or("");
            match msg_type {
                "response" => {
                    let id = msg.get("id").and_then(serde_json::Value::as_str).unwrap_or("");
                    let data = msg.get("data").cloned();
                    let error = msg
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .map(String::from);
                    dispatch.on_response(id, data, error).await;
                }
                "tab_created" => {
                    if let (Some(tab_id), Some(url), Some(title)) = (
                        msg.get("tab_id").and_then(serde_json::Value::as_u64).map(|v| v as u32),
                        msg.get("url").and_then(serde_json::Value::as_str),
                        msg.get("title").and_then(serde_json::Value::as_str),
                    ) {
                        tab_manager.on_tab_created(tab_id, url, title).await;
                    }
                }
                "tab_closed" => {
                    if let Some(tab_id) =
                        msg.get("tab_id").and_then(serde_json::Value::as_u64).map(|v| v as u32)
                    {
                        tab_manager.on_tab_closed(tab_id).await;
                    }
                }
                "tab_activated" => {
                    if let Some(tab_id) =
                        msg.get("tab_id").and_then(serde_json::Value::as_u64).map(|v| v as u32)
                    {
                        tab_manager.on_tab_activated(tab_id).await;
                    }
                }
                "tab_updated" => {
                    if let Some(tab_id) =
                        msg.get("tab_id").and_then(serde_json::Value::as_u64).map(|v| v as u32)
                    {
                        let url = msg.get("url").and_then(serde_json::Value::as_str);
                        let title = msg.get("title").and_then(serde_json::Value::as_str);
                        tab_manager.on_tab_updated(tab_id, url, title).await;
                    }
                }
                "bridge_ready" => {
                    if let Some(tab_id) =
                        msg.get("tab_id").and_then(serde_json::Value::as_u64).map(|v| v as u32)
                    {
                        tab_manager.on_bridge_ready(tab_id).await;
                        tracing::info!("bridge ready on tab {tab_id}");
                    }
                }
                _ => {
                    tracing::debug!("unknown message type: {msg_type}");
                }
            }
        }
    });
}

async fn try_bind(preferred: u16) -> anyhow::Result<SocketAddr> {
    for offset in 0..=PORT_RANGE {
        let port = preferred + offset;
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                drop(listener);
                if offset > 0 {
                    tracing::info!("Port {preferred} taken, using {port}");
                }
                return Ok(addr);
            }
            Err(_) => continue,
        }
    }
    anyhow::bail!(
        "no available port in range {preferred}-{}",
        preferred + PORT_RANGE
    )
}
