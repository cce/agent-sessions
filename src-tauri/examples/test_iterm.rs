//! Standalone test for iTerm2 API connection
//! Run with: cargo run --example test_iterm

use futures_util::{SinkExt, StreamExt};
use prost::Message;
use std::path::PathBuf;
use std::process::Command;
use tokio::net::UnixStream;
use tokio_tungstenite::{tungstenite, WebSocketStream};

// Include the generated protobuf code
pub mod iterm2_proto {
    include!(concat!(env!("OUT_DIR"), "/iterm2.rs"));
}

use iterm2_proto::*;
use iterm2_proto::variable_request;

fn request_cookie() -> Result<String, String> {
    println!("Requesting cookie via AppleScript...");
    let output = Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "iTerm2" to request cookie"#)
        .output()
        .map_err(|e| format!("Failed to run AppleScript: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("AppleScript failed: {}", stderr));
    }

    let cookie = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cookie.is_empty() {
        return Err("iTerm2 returned empty cookie".to_string());
    }

    println!("Got cookie: {}...", &cookie[..20.min(cookie.len())]);
    Ok(cookie)
}

fn get_socket_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let socket_path = home.join("Library/Application Support/iTerm2/private/socket");
    if socket_path.exists() {
        Some(socket_path)
    } else {
        None
    }
}

async fn connect_to_iterm() -> Result<WebSocketStream<UnixStream>, String> {
    let socket_path = get_socket_path()
        .ok_or_else(|| "iTerm2 socket not found".to_string())?;
    println!("Found socket at {:?}", socket_path);

    let cookie = request_cookie()?;

    println!("Connecting to WebSocket...");
    let stream = UnixStream::connect(&socket_path)
        .await
        .map_err(|e| format!("Failed to connect: {}", e))?;

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

    println!("WebSocket connected!");
    Ok(ws_stream)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== iTerm2 API Test ===\n");

    let mut ws = connect_to_iterm().await?;

    // Send ListSessionsRequest
    let request = ClientOriginatedMessage {
        id: Some(1),
        submessage: Some(client_originated_message::Submessage::ListSessionsRequest(
            ListSessionsRequest {},
        )),
    };

    let mut buf = Vec::new();
    request.encode(&mut buf)?;
    println!("Sending ListSessionsRequest ({} bytes)...", buf.len());

    ws.send(tungstenite::Message::Binary(buf.into())).await?;

    // Receive response
    println!("Waiting for response...");
    let response_msg = ws
        .next()
        .await
        .ok_or("No response")??;

    let response_data = match response_msg {
        tungstenite::Message::Binary(data) => {
            println!("Got binary response ({} bytes)", data.len());
            // Print hex dump of first 100 bytes for debugging
            let preview: Vec<String> = data.iter().take(100).map(|b| format!("{:02x}", b)).collect();
            println!("First 100 bytes: {}", preview.join(" "));
            data
        }
        other => {
            return Err(format!("Unexpected message type: {:?}", other).into());
        }
    };

    // Try to decode
    println!("\nDecoding response...");
    match ServerOriginatedMessage::decode(response_data.as_ref()) {
        Ok(server_msg) => {
            println!("Decoded successfully! id={:?}", server_msg.id);
            match server_msg.submessage {
                Some(server_originated_message::Submessage::ListSessionsResponse(resp)) => {
                    println!("\nGot ListSessionsResponse with {} windows", resp.windows.len());
                    for window in &resp.windows {
                        println!("\n  Window: {:?}", window.window_id);
                        let mut all_session_ids = Vec::new();
                    for tab in &window.tabs {
                            println!("    Tab: {:?} (has_root={})", tab.tab_id, tab.root.is_some());
                            if let Some(root) = &tab.root {
                                all_session_ids.extend(print_node(root, 6));
                            } else {
                                println!("      (no root node)");
                            }
                        }

                    // Query TTY for first 3 sessions in this window
                    if !all_session_ids.is_empty() {
                        println!("\n    Querying TTYs for first 3 sessions...");
                        let mut req_id = 100i64;
                        for sid in all_session_ids.iter().take(3) {
                            if let Some(tty) = query_tty(&mut ws, sid, req_id).await {
                                println!("      {} -> {}", &sid[..8], tty);
                            } else {
                                println!("      {} -> (no tty)", &sid[..8]);
                            }
                            req_id += 1;
                        }
                    }
                    }
                }
                Some(server_originated_message::Submessage::Error(err)) => {
                    println!("Got error: {}", err);
                }
                Some(server_originated_message::Submessage::Notification(_)) => {
                    println!("Got notification (unexpected)");
                }
                Some(server_originated_message::Submessage::VariableResponse(_)) => {
                    println!("Got variable response (unexpected)");
                }
                None => {
                    println!("Empty submessage");
                }
            }
        }
        Err(e) => {
            println!("Failed to decode: {}", e);
        }
    }

    let _ = ws.close(None).await;
    println!("\nDone!");
    Ok(())
}

fn print_node(node: &SplitTreeNode, indent: usize) -> Vec<String> {
    let pad = " ".repeat(indent);
    println!("{}SplitTreeNode (vertical={:?})", pad, node.vertical);
    let mut session_ids = Vec::new();
    for link in &node.links {
        if let Some(child) = &link.child {
            match child {
                split_tree_link::Child::Session(s) => {
                    println!("{}  Session: id={:?} title={:?}", pad, s.unique_identifier, s.title);
                    if let Some(id) = &s.unique_identifier {
                        session_ids.push(id.clone());
                    }
                }
                split_tree_link::Child::Node(n) => {
                    session_ids.extend(print_node(n, indent + 2));
                }
            }
        }
    }
    session_ids
}

async fn query_tty(ws: &mut WebSocketStream<UnixStream>, session_id: &str, req_id: i64) -> Option<String> {
    use iterm2_proto::variable_request;

    let request = ClientOriginatedMessage {
        id: Some(req_id),
        submessage: Some(client_originated_message::Submessage::VariableRequest(
            VariableRequest {
                scope: Some(variable_request::Scope::SessionId(session_id.to_string())),
                get: vec!["tty".to_string()],
            },
        )),
    };

    let mut buf = Vec::new();
    request.encode(&mut buf).ok()?;
    ws.send(tungstenite::Message::Binary(buf.into())).await.ok()?;

    let response = ws.next().await?.ok()?;
    let data = match response {
        tungstenite::Message::Binary(d) => d,
        _ => return None,
    };

    let server_msg = ServerOriginatedMessage::decode(data.as_ref()).ok()?;
    match server_msg.submessage {
        Some(server_originated_message::Submessage::VariableResponse(resp)) => {
            println!("        [DEBUG] VariableResponse: status={:?} values={:?}", resp.status, resp.values);
            // values is repeated, one per requested variable (we only request "tty")
            if let Some(value_json) = resp.values.first() {
                // Value is JSON encoded, e.g. "\"/dev/ttys042\"" or "null"
                if value_json != "null" {
                    // Parse the JSON string
                    if let Ok(v) = serde_json::from_str::<String>(value_json) {
                        return Some(v);
                    }
                }
            }
            None
        }
        Some(_) => {
            println!("        [DEBUG] Got unexpected submessage type");
            None
        }
        None => {
            println!("        [DEBUG] No submessage");
            None
        }
    }
}
