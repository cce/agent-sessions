//! iTerm2 API client for querying window/tab/session layout
//!
//! Connects to iTerm2's Unix socket using WebSocket protocol and protobuf messages.
//! Requires cookie authentication via AppleScript.

use futures_util::{SinkExt, StreamExt};
use prost::Message;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use tokio::net::UnixStream;
use tokio_tungstenite::{tungstenite, WebSocketStream};

// Include the generated protobuf code
pub mod iterm2_proto {
    include!(concat!(env!("OUT_DIR"), "/iterm2.rs"));
}

use iterm2_proto::*;
use iterm2_proto::split_tree_link;
use iterm2_proto::variable_request;

/// Request an authentication cookie from iTerm2 via AppleScript
fn request_cookie() -> Result<String, String> {
    eprintln!("[iTerm2 API] Requesting cookie via AppleScript...");
    let output = Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "iTerm2" to request cookie"#)
        .output()
        .map_err(|e| {
            eprintln!("[iTerm2 API] ERROR: Failed to run AppleScript: {}", e);
            format!("Failed to run AppleScript: {}", e)
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("[iTerm2 API] ERROR: AppleScript failed: {}", stderr);
        return Err(format!("AppleScript failed: {}", stderr));
    }

    let cookie = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cookie.is_empty() {
        eprintln!("[iTerm2 API] ERROR: iTerm2 returned empty cookie");
        return Err("iTerm2 returned empty cookie".to_string());
    }

    eprintln!("[iTerm2 API] Got cookie successfully");
    Ok(cookie)
}

/// Information about an iTerm2 window
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItermWindow {
    pub window_id: String,
    pub tabs: Vec<ItermTab>,
}

/// Information about a tab within an iTerm2 window
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItermTab {
    pub tab_id: String,
    pub sessions: Vec<ItermSessionInfo>,
}

/// Information about a session (terminal pane) in iTerm2
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItermSessionInfo {
    pub session_id: String,
    pub tty: String,
    pub name: String,
}

/// Complete iTerm2 layout information
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItermLayout {
    pub windows: Vec<ItermWindow>,
}

/// Response from get_iterm_layout command
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItermLayoutResponse {
    pub windows: Vec<ItermWindow>,
    /// Maps TTY (e.g., "ttys042") to window ID for easy lookup
    pub session_to_window: HashMap<String, String>,
}

/// Get the iTerm2 socket path
fn get_socket_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let socket_path = home.join("Library/Application Support/iTerm2/private/socket");
    if socket_path.exists() {
        Some(socket_path)
    } else {
        None
    }
}

/// Connect to iTerm2 via WebSocket over Unix socket with cookie authentication
async fn connect_to_iterm() -> Result<WebSocketStream<UnixStream>, String> {
    log::debug!("Looking for iTerm2 socket...");
    let socket_path = get_socket_path()
        .ok_or_else(|| {
            log::error!("iTerm2 socket not found");
            "iTerm2 socket not found. Is iTerm2 running with API enabled?".to_string()
        })?;
    log::debug!("Found socket at {:?}", socket_path);

    // Request authentication cookie via AppleScript
    let cookie = request_cookie()?;

    log::debug!("Connecting to iTerm2 socket...");
    let stream = UnixStream::connect(&socket_path)
        .await
        .map_err(|e| {
            log::error!("Failed to connect to iTerm2 socket: {}", e);
            format!("Failed to connect to iTerm2 socket: {}", e)
        })?;

    // Create WebSocket connection with required headers including cookie
    // Based on iTerm2's Python library: origin, subprotocol, and cookie are required
    let request = http::Request::builder()
        .uri("ws://localhost/")
        .header(http::header::HOST, "localhost")
        .header(http::header::ORIGIN, "ws://localhost/")
        .header("x-iterm2-library-version", "rust 0.1")
        .header("x-iterm2-cookie", &cookie)
        .header("x-iterm2-disable-auth-ui", "true")
        .header(
            http::header::SEC_WEBSOCKET_KEY,
            tungstenite::handshake::client::generate_key(),
        )
        .header(http::header::SEC_WEBSOCKET_PROTOCOL, "api.iterm2.com")
        .header(http::header::CONNECTION, "Upgrade")
        .header(http::header::UPGRADE, "websocket")
        .header(http::header::SEC_WEBSOCKET_VERSION, "13")
        .body(())
        .map_err(|e| format!("Failed to build request: {}", e))?;

    let (ws_stream, _response) = tokio_tungstenite::client_async(request, stream)
        .await
        .map_err(|e| format!("WebSocket handshake failed: {}", e))?;

    Ok(ws_stream)
}

/// Extract session IDs from a split tree node recursively
fn extract_session_ids_from_node(node: &SplitTreeNode) -> Vec<(String, String)> {
    let mut sessions = Vec::new();

    for link in &node.links {
        if let Some(child) = &link.child {
            match child {
                split_tree_link::Child::Session(session) => {
                    let id = session.unique_identifier.clone().unwrap_or_default();
                    let title = session.title.clone().unwrap_or_default();
                    if !id.is_empty() {
                        sessions.push((id, title));
                    }
                }
                split_tree_link::Child::Node(nested) => {
                    sessions.extend(extract_session_ids_from_node(nested));
                }
            }
        }
    }

    sessions
}

/// Query TTY for a session via VariableRequest
async fn query_session_tty(
    ws: &mut WebSocketStream<UnixStream>,
    session_id: &str,
    request_id: i64,
) -> Result<Option<String>, String> {
    let request = ClientOriginatedMessage {
        id: Some(request_id),
        submessage: Some(client_originated_message::Submessage::VariableRequest(
            VariableRequest {
                scope: Some(variable_request::Scope::SessionId(session_id.to_string())),
                get: vec!["tty".to_string()],
            },
        )),
    };

    let mut buf = Vec::new();
    request
        .encode(&mut buf)
        .map_err(|e| format!("Failed to encode variable request: {}", e))?;

    ws.send(tungstenite::Message::Binary(buf.into()))
        .await
        .map_err(|e| format!("Failed to send variable request: {}", e))?;

    let response_msg = ws
        .next()
        .await
        .ok_or_else(|| "No variable response".to_string())?
        .map_err(|e| format!("Failed to receive variable response: {}", e))?;

    let response_data = match response_msg {
        tungstenite::Message::Binary(data) => data,
        _ => return Ok(None),
    };

    let server_msg = ServerOriginatedMessage::decode(response_data.as_ref())
        .map_err(|e| format!("Failed to decode variable response: {}", e))?;

    match server_msg.submessage {
        Some(server_originated_message::Submessage::VariableResponse(resp)) => {
            // values is repeated, one per requested variable (we only request "tty")
            if let Some(value_json) = resp.values.first() {
                // Value is JSON encoded, e.g. "\"/dev/ttys042\"" or "null"
                if value_json != "null" {
                    if let Ok(v) = serde_json::from_str::<String>(value_json) {
                        return Ok(Some(v));
                    }
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Query iTerm2 for current window/tab/session layout
pub async fn get_iterm_layout() -> Result<ItermLayoutResponse, String> {
    eprintln!("[iTerm2 API] Fetching layout...");
    let mut ws = connect_to_iterm().await.map_err(|e| {
        eprintln!("[iTerm2 API] ERROR: {}", e);
        e
    })?;

    // Build ListSessionsRequest message
    let request = ClientOriginatedMessage {
        id: Some(1),
        submessage: Some(client_originated_message::Submessage::ListSessionsRequest(
            ListSessionsRequest {},
        )),
    };

    // Encode and send the request
    let mut buf = Vec::new();
    request
        .encode(&mut buf)
        .map_err(|e| format!("Failed to encode request: {}", e))?;

    ws.send(tungstenite::Message::Binary(buf.into()))
        .await
        .map_err(|e| format!("Failed to send request: {}", e))?;

    // Receive response
    let response_msg = ws
        .next()
        .await
        .ok_or_else(|| "No response from iTerm2".to_string())?
        .map_err(|e| format!("Failed to receive response: {}", e))?;

    // Decode response
    let response_data = match response_msg {
        tungstenite::Message::Binary(data) => data,
        _ => return Err("Unexpected response type from iTerm2".to_string()),
    };

    let server_msg = ServerOriginatedMessage::decode(response_data.as_ref())
        .map_err(|e| format!("Failed to decode response: {}", e))?;

    // Extract ListSessionsResponse (may need to skip notifications)
    eprintln!("[iTerm2 API] Got server message id={:?}, has_submessage={}", server_msg.id, server_msg.submessage.is_some());
    let list_response = match server_msg.submessage {
        Some(server_originated_message::Submessage::ListSessionsResponse(resp)) => resp,
        Some(server_originated_message::Submessage::Error(err)) => {
            return Err(format!("iTerm2 error: {}", err));
        }
        Some(server_originated_message::Submessage::Notification(_)) => {
            // Got a notification instead of response - read next message
            eprintln!("[iTerm2 API] Got notification, reading next message...");
            let next_msg = ws
                .next()
                .await
                .ok_or_else(|| "No response from iTerm2 after notification".to_string())?
                .map_err(|e| format!("Failed to receive response: {}", e))?;

            let next_data = match next_msg {
                tungstenite::Message::Binary(data) => data,
                _ => return Err("Unexpected response type from iTerm2".to_string()),
            };

            let next_server_msg = ServerOriginatedMessage::decode(next_data.as_ref())
                .map_err(|e| format!("Failed to decode response: {}", e))?;

            match next_server_msg.submessage {
                Some(server_originated_message::Submessage::ListSessionsResponse(resp)) => resp,
                _ => return Err("Expected ListSessionsResponse after notification".to_string()),
            }
        }
        Some(server_originated_message::Submessage::VariableResponse(_)) => {
            return Err("Unexpected VariableResponse".to_string());
        }
        None => return Err("Empty response from iTerm2".to_string()),
    };

    // First pass: collect all session IDs and their window IDs
    let mut session_window_map: Vec<(String, String, String)> = Vec::new(); // (session_id, title, window_id)

    eprintln!("[iTerm2 API] Got {} windows from iTerm2", list_response.windows.len());

    for window in &list_response.windows {
        let window_id = window.window_id.clone().unwrap_or_default();
        for tab in &window.tabs {
            if let Some(root) = &tab.root {
                let session_ids = extract_session_ids_from_node(root);
                for (sid, title) in session_ids {
                    session_window_map.push((sid, title, window_id.clone()));
                }
            }
        }
    }

    eprintln!("[iTerm2 API] Found {} sessions, querying TTYs...", session_window_map.len());

    // Query TTY for each session
    let mut session_to_window = HashMap::new();
    let mut session_infos: HashMap<String, Vec<ItermSessionInfo>> = HashMap::new(); // window_id -> sessions

    let mut request_id = 2i64;
    for (session_id, title, window_id) in &session_window_map {
        if let Ok(Some(tty)) = query_session_tty(&mut ws, session_id, request_id).await {
            let tty_short = tty.trim_start_matches("/dev/").to_string();
            session_to_window.insert(tty_short.clone(), window_id.clone());

            let info = ItermSessionInfo {
                session_id: session_id.clone(),
                tty: tty_short,
                name: title.clone(),
            };
            session_infos.entry(window_id.clone()).or_default().push(info);
        }
        request_id += 1;
    }

    eprintln!("[iTerm2 API] Got {} TTY mappings", session_to_window.len());

    // Build final window/tab structure
    let mut windows = Vec::new();
    for window in &list_response.windows {
        let window_id = window.window_id.clone().unwrap_or_default();
        let mut tabs = Vec::new();

        for tab in &window.tabs {
            let tab_id = tab.tab_id.clone().unwrap_or_default();
            // Get sessions for this window that belong to this tab
            // For simplicity, we put all sessions under the first tab
            let sessions = if tabs.is_empty() {
                session_infos.remove(&window_id).unwrap_or_default()
            } else {
                Vec::new()
            };

            tabs.push(ItermTab {
                tab_id,
                sessions,
            });
        }

        windows.push(ItermWindow { window_id, tabs });
    }

    // Close WebSocket cleanly
    let _ = ws.close(None).await;

    eprintln!("[iTerm2 API] Returning {} windows, {} session mappings", windows.len(), session_to_window.len());
    for (tty, window_id) in &session_to_window {
        eprintln!("[iTerm2 API]   {} -> {}", tty, window_id);
    }

    Ok(ItermLayoutResponse {
        windows,
        session_to_window,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path() {
        // This test will only pass on macOS with iTerm2 installed
        if cfg!(target_os = "macos") {
            // Socket may or may not exist depending on if iTerm2 is running
            let _ = get_socket_path();
        }
    }
}
