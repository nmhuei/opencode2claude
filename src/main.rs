use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use futures_util::stream::{self, Stream};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{convert::Infallible, env, net::SocketAddr, process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Deserialize)]
struct MessageContent {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ContentVal {
    Single(String),
    Multiple(Vec<MessageContent>),
}

#[derive(Debug, Deserialize)]
struct Message {
    role: String,
    content: ContentVal,
}

#[derive(Debug, Deserialize)]
struct MessagesRequest {
    model: Option<String>,
    messages: Vec<Message>,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, Clone)]
struct AppState {
    bridge_port: u16,
    opencode_port: u16,
    opencode_model: Option<String>,
}

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let bridge_port: u16 = env::var("BRIDGE_PORT")
        .unwrap_or_else(|_| "4000".to_string())
        .parse()
        .unwrap_or(4000);

    let opencode_port: u16 = env::var("OPENCODE_PORT")
        .unwrap_or_else(|_| "4096".to_string())
        .parse()
        .unwrap_or(4096);

    let opencode_model = env::var("OPENCODE_MODEL").ok();

    let state = AppState {
        bridge_port,
        opencode_port,
        opencode_model,
    };

    let app = Router::new()
        .route("/v1/messages", post(handle_messages))
        .with_state(state.clone());

    let addr = SocketAddr::from(([127, 0, 0, 1], bridge_port));
    info!("OpenCode2Claude Bridge listening on http://{}", addr);
    info!("To redirect Claude Code, run: export ANTHROPIC_API_URL=\"http://{}/v1\"", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn handle_messages(
    State(state): State<AppState>,
    Json(payload): Json<MessagesRequest>,
) -> impl IntoResponse {
    let mut prompt = String::new();
    for msg in &payload.messages {
        if msg.role == "user" {
            match &msg.content {
                ContentVal::Single(text) => {
                    prompt.push_str(text);
                }
                ContentVal::Multiple(parts) => {
                    for part in parts {
                        if part.content_type == "text" {
                            if let Some(ref t) = part.text {
                                prompt.push_str(t);
                            }
                        }
                    }
                }
            }
        }
    }
    let prompt = prompt.trim().to_string();
    let req_model = payload.model.clone().unwrap_or_else(|| "claude-3-5-sonnet".to_string());

    info!("Incoming prompt ({} chars) [Model: {}]", prompt.len(), req_model);

    if prompt.starts_with('!') {
        let shell_cmd = prompt[1..].trim().to_string();
        info!("Intercepted local shell command: '{}'", shell_cmd);
        
        if payload.stream {
            let sse_stream = run_shell_stream(shell_cmd, req_model);
            Sse::new(sse_stream).keep_alive(KeepAlive::default()).into_response()
        } else {
            let output_text = run_shell_sync(&shell_cmd).await;
            let response = json!({
                "id": "msg_local_shell",
                "type": "message",
                "role": "assistant",
                "model": req_model,
                "content": [{"type": "text", "text": output_text}],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            });
            Json(response).into_response()
        }
    } else {
        // OpenCode path
        let is_daemon_running = check_daemon(state.opencode_port).await;
        let mut cmd = Command::new("opencode");
        cmd.arg("run");

        if is_daemon_running {
            let daemon_url = format!("http://127.0.0.1:{}", state.opencode_port);
            cmd.arg("--attach").arg(&daemon_url);
            info!("Attached to active OpenCode Daemon on port {}", state.opencode_port);
        } else {
            warn!("No active OpenCode Daemon found. Running in standalone mode.");
        }

        if let Some(ref model) = state.opencode_model {
            cmd.arg("-m").arg(model);
            info!("Using model override: {}", model);
        }

        cmd.arg("--dangerously-skip-permissions").arg(&prompt);

        if payload.stream {
            let sse_stream = run_opencode_stream(cmd, req_model);
            Sse::new(sse_stream).keep_alive(KeepAlive::default()).into_response()
        } else {
            let output_text = run_opencode_sync(cmd).await;
            let response = json!({
                "id": "msg_opencode",
                "type": "message",
                "role": "assistant",
                "model": req_model,
                "content": [{"type": "text", "text": output_text}],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            });
            Json(response).into_response()
        }
    }
}

async fn check_daemon(port: u16) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap();
    let url = format!("http://127.0.0.1:{}/doc", port);
    client.get(&url).send().await.is_ok()
}

async fn run_shell_sync(cmd_str: &str) -> String {
    match Command::new("sh")
        .arg("-c")
        .arg(cmd_str)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            format!("{}{}", stdout, stderr)
        }
        Err(e) => format!("Local Shell Error: {}", e),
    }
}

async fn run_opencode_sync(mut cmd: Command) -> String {
    match cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
    {
        Ok(output) => {
            if output.status.success() {
                String::from_utf8_lossy(&output.stdout).to_string()
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                format!("Error returned code {}:\n{}", output.status.code().unwrap_or(-1), stderr)
            }
        }
        Err(e) => format!("Bridge Execution Error: {}", e),
    }
}

fn run_shell_stream(
    cmd_str: String,
    model: String,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let (tx, rx) = tokio::sync::mpsc::channel(100);

    tokio::spawn(async move {
        // Send message_start
        let _ = tx.send(Event::default().event("message_start").json_data(json!({
            "type": "message_start",
            "message": {
                "id": "msg_local_shell",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }
        })).unwrap()).await;

        // Send content_block_start
        let _ = tx.send(Event::default().event("content_block_start").json_data(json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        })).unwrap()).await;

        match Command::new("sh")
            .arg("-c")
            .arg(&cmd_str)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let stdout = child.stdout.take().unwrap();
                let stderr = child.stderr.take().unwrap();
                let mut reader = BufReader::new(stdout);
                let mut err_reader = BufReader::new(stderr);
                let mut out_buffer = [0; 64];
                let mut err_buffer = [0; 64];

                loop {
                    tokio::select! {
                        res = reader.read(&mut out_buffer) => {
                            match res {
                                Ok(0) => break,
                                Ok(n) => {
                                    let text = String::from_utf8_lossy(&out_buffer[..n]).to_string();
                                    let _ = tx.send(Event::default().event("content_block_delta").json_data(json!({
                                        "type": "content_block_delta",
                                        "index": 0,
                                        "delta": {"type": "text_delta", "text": text}
                                    })).unwrap()).await;
                                }
                                Err(_) => break,
                            }
                        }
                        res = err_reader.read(&mut err_buffer) => {
                            match res {
                                Ok(0) => {},
                                Ok(n) => {
                                    let text = String::from_utf8_lossy(&err_buffer[..n]).to_string();
                                    let _ = tx.send(Event::default().event("content_block_delta").json_data(json!({
                                        "type": "content_block_delta",
                                        "index": 0,
                                        "delta": {"type": "text_delta", "text": text}
                                    })).unwrap()).await;
                                }
                                Err(_) => {},
                            }
                        }
                    }
                }
                let _ = child.wait().await;
            }
            Err(e) => {
                let _ = tx.send(Event::default().event("content_block_delta").json_data(json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": format!("\n[Local Shell Error]: {}", e)}
                })).unwrap()).await;
            }
        }

        // Send closing events
        let _ = tx.send(Event::default().event("content_block_stop").json_data(json!({
            "type": "content_block_stop",
            "index": 0
        })).unwrap()).await;

        let _ = tx.send(Event::default().event("message_delta").json_data(json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 0}
        })).unwrap()).await;

        let _ = tx.send(Event::default().event("message_stop").json_data(json!({
            "type": "message_stop"
        })).unwrap()).await;
    });

    tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok)
}

fn run_opencode_stream(
    mut cmd: Command,
    model: String,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let (tx, rx) = tokio::sync::mpsc::channel(100);

    tokio::spawn(async move {
        // Send message_start
        let _ = tx.send(Event::default().event("message_start").json_data(json!({
            "type": "message_start",
            "message": {
                "id": "msg_opencode",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }
        })).unwrap()).await;

        // Send content_block_start
        let _ = tx.send(Event::default().event("content_block_start").json_data(json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        })).unwrap()).await;

        match cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take().unwrap();
                let stderr = child.stderr.take().unwrap();
                let mut reader = BufReader::new(stdout);
                let mut err_reader = BufReader::new(stderr);
                let mut out_buffer = [0; 64];
                let mut err_buffer = [0; 64];

                loop {
                    tokio::select! {
                        res = reader.read(&mut out_buffer) => {
                            match res {
                                Ok(0) => break,
                                Ok(n) => {
                                    let text = String::from_utf8_lossy(&out_buffer[..n]).to_string();
                                    let _ = tx.send(Event::default().event("content_block_delta").json_data(json!({
                                        "type": "content_block_delta",
                                        "index": 0,
                                        "delta": {"type": "text_delta", "text": text}
                                    })).unwrap()).await;
                                }
                                Err(_) => break,
                            }
                        }
                        res = err_reader.read(&mut err_buffer) => {
                            match res {
                                Ok(0) => {},
                                Ok(n) => {
                                    let text = String::from_utf8_lossy(&err_buffer[..n]).to_string();
                                    let _ = tx.send(Event::default().event("content_block_delta").json_data(json!({
                                        "type": "content_block_delta",
                                        "index": 0,
                                        "delta": {"type": "text_delta", "text": format!("\n[OpenCode Stderr]: {}", text)}
                                    })).unwrap()).await;
                                }
                                Err(_) => {},
                            }
                        }
                    }
                }
                let _ = child.wait().await;
            }
            Err(e) => {
                let _ = tx.send(Event::default().event("content_block_delta").json_data(json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": format!("\n[Bridge Error]: {}", e)}
                })).unwrap()).await;
            }
        }

        // Send closing events
        let _ = tx.send(Event::default().event("content_block_stop").json_data(json!({
            "type": "content_block_stop",
            "index": 0
        })).unwrap()).await;

        let _ = tx.send(Event::default().event("message_delta").json_data(json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 0}
        })).unwrap()).await;

        let _ = tx.send(Event::default().event("message_stop").json_data(json!({
            "type": "message_stop"
        })).unwrap()).await;
    });

    tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok)
}
