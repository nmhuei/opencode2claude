//! OpenCode direct API gateway and format mapping layer.
//!
//! This module bypasses running OpenCode subprocesses entirely and communicates
//! directly with the public upstream completions API to act as a pure,
//! transparent LLM completions provider (supporting tools and streaming).

use crate::error::BridgeError;
use crate::handlers::{ContentVal, MessagesRequest};

use axum::response::sse::Event;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use tracing::{error, info, warn};

// ── Models & Structs for OpenAI Chat Completions API ──

#[derive(Debug, Serialize)]
pub struct OpenAiRequest {
    pub model: String,
    pub messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAiMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAiToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: OpenAiFunctionCall,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAiFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize)]
pub struct OpenAiTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: OpenAiFunction,
}

#[derive(Debug, Serialize)]
pub struct OpenAiFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct OpenAiResponse {
    pub id: String,
    pub model: String,
    pub choices: Vec<OpenAiChoice>,
    pub usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiChoice {
    pub message: OpenAiResponseMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiResponseMessage {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<OpenAiResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiResponseToolCall {
    pub id: String,
    pub function: OpenAiResponseFunctionCall,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiResponseFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

// ── Streaming response structures ──

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamChunk {
    pub choices: Vec<OpenAiStreamChoice>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamChoice {
    pub delta: OpenAiStreamDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamDelta {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<OpenAiStreamToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamToolCall {
    pub index: usize,
    pub id: Option<String>,
    pub function: Option<OpenAiStreamFunctionCall>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiStreamFunctionCall {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

// ── Helper functions ──

/// Check if the OpenCode daemon is running and reachable.
pub async fn check_daemon(client: &Client, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}/doc", port);
    client
        .get(&url)
        .timeout(std::time::Duration::from_millis(500))
        .send()
        .await
        .is_ok()
}

fn extract_system_prompt(system_val: &serde_json::Value) -> String {
    if let Some(s) = system_val.as_str() {
        return s.to_string();
    }
    if let Some(arr) = system_val.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(obj) = item.as_object() {
                if obj.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                        parts.push(text);
                    }
                }
            }
        }
        return parts.join("\n");
    }
    String::new()
}

fn is_web_search_tool(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    name_lower == "websearch"
        || name_lower == "web_search"
        || name_lower == "webfetch"
        || name_lower == "web_fetch"
}

fn url_decode(s: &str) -> String {
    let mut decoded = String::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(c1), Some(c2)) = (h1, h2) {
                if let Ok(val) = u8::from_str_radix(&format!("{}{}", c1, c2), 16) {
                    decoded.push(val as char);
                    continue;
                }
            }
        }
        decoded.push(ch);
    }
    decoded
}

fn urlencoding_simple(query: &str) -> String {
    let mut encoded = String::new();
    for b in query.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            b' ' => {
                encoded.push('+');
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
}

fn strip_html_tags(html: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            output.push(c);
        }
    }
    output
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

async fn perform_duckduckgo_search(query: &str) -> String {
    let client = reqwest::Client::new();
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding_simple(query)
    );
    let res = match client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .send()
        .await {
            Ok(r) => r,
            Err(e) => return format!("Search failed: {}", e),
        };

    let html = match res.text().await {
        Ok(t) => t,
        Err(e) => return format!("Failed to read search body: {}", e),
    };

    let mut results = Vec::new();
    let mut remaining = &html[..];
    while let Some(start_pos) = remaining.find("<a class=\"result__snippet\"") {
        remaining = &remaining[start_pos..];
        if let Some(href_start) = remaining.find("href=\"") {
            let href_content = &remaining[href_start + 6..];
            if let Some(href_end) = href_content.find("\"") {
                let link = &href_content[..href_end];
                let url = if let Some(uddg_pos) = link.find("uddg=") {
                    let uddg_val = &link[uddg_pos + 5..];
                    let end_pos = uddg_val.find('&').unwrap_or(uddg_val.len());
                    url_decode(&uddg_val[..end_pos])
                } else {
                    format!("https:{}", link)
                };

                if let Some(tag_end) = remaining.find(">") {
                    let text_content = &remaining[tag_end + 1..];
                    if let Some(anchor_end) = text_content.find("</a>") {
                        let snippet = &text_content[..anchor_end];
                        let clean_snippet = strip_html_tags(snippet);
                        results.push(format!("URL: {}\nSnippet: {}\n", url, clean_snippet));
                    }
                }
            }
        }
        if let Some(next_pos) = remaining.find("</a>") {
            remaining = &remaining[next_pos + 4..];
        } else {
            break;
        }
        if results.len() >= 5 {
            break;
        }
    }

    if results.is_empty() {
        "No results found.".to_string()
    } else {
        results.join("\n")
    }
}

async fn perform_tavily_search(query: &str, api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "api_key": api_key,
        "query": query,
        "include_answer": false,
        "max_results": 5
    });

    let res = client
        .post("https://api.tavily.com/search")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = res.status();
    if !status.is_success() {
        let err_body = res.text().await.unwrap_or_default();
        return Err(format!("Tavily status {}: {}", status, err_body));
    }

    let parsed: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    if let Some(arr) = parsed.get("results").and_then(|v| v.as_array()) {
        for item in arr {
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
            results.push(format!(
                "URL: {}\nTitle: {}\nSnippet: {}\n",
                url, title, content
            ));
        }
    }

    if results.is_empty() {
        Ok("No results found on Tavily.".to_string())
    } else {
        Ok(results.join("\n"))
    }
}

async fn perform_exa_search(query: &str, api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "query": query,
        "numResults": 5,
        "useAutoprompt": true
    });

    let res = client
        .post("https://api.exa.ai/search")
        .header("x-api-key", api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = res.status();
    if !status.is_success() {
        let err_body = res.text().await.unwrap_or_default();
        return Err(format!("Exa status {}: {}", status, err_body));
    }

    let parsed: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    if let Some(arr) = parsed.get("results").and_then(|v| v.as_array()) {
        for item in arr {
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");

            let mut snippet = String::new();
            if let Some(highlights) = item.get("highlights").and_then(|v| v.as_array()) {
                let h_texts: Vec<&str> = highlights.iter().filter_map(|h| h.as_str()).collect();
                if !h_texts.is_empty() {
                    snippet = h_texts.join(" ... ");
                }
            }
            if snippet.is_empty() {
                snippet = item
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            if snippet.len() > 300 {
                snippet = format!("{}...", &snippet[..300]);
            }

            results.push(format!(
                "URL: {}\nTitle: {}\nSnippet: {}\n",
                url, title, snippet
            ));
        }
    }

    if results.is_empty() {
        Ok("No results found on Exa.".to_string())
    } else {
        Ok(results.join("\n"))
    }
}

async fn perform_serper_search(query: &str, api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "q": query
    });

    let res = client
        .post("https://google.serper.dev/search")
        .header("X-API-KEY", api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = res.status();
    if !status.is_success() {
        let err_body = res.text().await.unwrap_or_default();
        return Err(format!("Serper status {}: {}", status, err_body));
    }

    let parsed: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let mut results = Vec::new();

    if let Some(answer_box) = parsed.get("answerBox") {
        if let Some(snippet) = answer_box.get("snippet").and_then(|v| v.as_str()) {
            results.push(format!("Answer Box (Direct Answer):\n{}\n", snippet));
        }
    }

    if let Some(arr) = parsed.get("organic").and_then(|v| v.as_array()) {
        for item in arr {
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = item.get("link").and_then(|v| v.as_str()).unwrap_or("");
            let snippet = item.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
            results.push(format!(
                "URL: {}\nTitle: {}\nSnippet: {}\n",
                url, title, snippet
            ));
        }
    }

    if results.is_empty() {
        Ok("No results found on Serper.dev.".to_string())
    } else {
        Ok(results.join("\n"))
    }
}

async fn perform_searxng_search(query: &str, base_url: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let url = if base_url.contains('?') {
        format!("{}&q={}&format=json", base_url, urlencoding_simple(query))
    } else {
        format!(
            "{}/search?q={}&format=json",
            base_url.trim_end_matches('/'),
            urlencoding_simple(query)
        )
    };

    let res = client.get(&url).send().await.map_err(|e| e.to_string())?;

    let status = res.status();
    if !status.is_success() {
        let err_body = res.text().await.unwrap_or_default();
        return Err(format!("SearXNG status {}: {}", status, err_body));
    }

    let parsed: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    if let Some(arr) = parsed.get("results").and_then(|v| v.as_array()) {
        for item in arr {
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
            results.push(format!(
                "URL: {}\nTitle: {}\nSnippet: {}\n",
                url, title, content
            ));
        }
    }

    if results.is_empty() {
        Ok("No results found on SearXNG.".to_string())
    } else {
        Ok(results.join("\n"))
    }
}

pub async fn perform_search_fallback(query: &str, config: &crate::config::BridgeConfig) -> String {
    // 1. Tavily
    if let Some(ref api_key) = config.tavily_api_key {
        info!("Attempting Tavily search...");
        match perform_tavily_search(query, api_key).await {
            Ok(results) => return results,
            Err(e) => warn!("Tavily search failed: {}. Falling back...", e),
        }
    }

    // 2. Exa
    if let Some(ref api_key) = config.exa_api_key {
        info!("Attempting Exa search...");
        match perform_exa_search(query, api_key).await {
            Ok(results) => return results,
            Err(e) => warn!("Exa search failed: {}. Falling back...", e),
        }
    }

    // 3. Serper
    if let Some(ref api_key) = config.serper_api_key {
        info!("Attempting Serper.dev search...");
        match perform_serper_search(query, api_key).await {
            Ok(results) => return results,
            Err(e) => warn!("Serper search failed: {}. Falling back...", e),
        }
    }

    // 4. SearXNG
    if let Some(ref url) = config.searxng_url {
        info!("Attempting SearXNG search...");
        match perform_searxng_search(query, url).await {
            Ok(results) => return results,
            Err(e) => warn!("SearXNG search failed: {}. Falling back...", e),
        }
    }

    // 5. DuckDuckGo as default fallback
    info!("Attempting DuckDuckGo search...");
    perform_duckduckgo_search(query).await
}

fn map_model_name(model: &str) -> String {
    let mut name = model.to_string();
    if name.starts_with("opencode/") {
        name = name["opencode/".len()..].to_string();
    }
    match name.as_str() {
        "deepseek-v4-flash" => "deepseek-v4-flash-free".to_string(),
        "nemotron-3-ultra" => "nemotron-3-ultra-free".to_string(),
        _ => name,
    }
}

fn tool_result_content_to_string(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            let mut text_parts = Vec::new();
            for item in arr {
                if let Some(obj) = item.as_object() {
                    if obj.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(text.to_string());
                        }
                    } else {
                        text_parts.push(item.to_string());
                    }
                } else {
                    text_parts.push(item.to_string());
                }
            }
            text_parts.join("\n")
        }
        _ => val.to_string(),
    }
}

/// Map Anthropic request values into standard OpenAI payload.
fn map_anthropic_to_openai(payload: &MessagesRequest, model: String) -> OpenAiRequest {
    let mapped_model = map_model_name(&model);
    let mut openai_messages = Vec::new();

    // Build a map of tool_use_id -> name from previous assistant messages
    let mut tool_name_map = HashMap::new();
    for msg in &payload.messages {
        if msg.role == "assistant" {
            if let ContentVal::Multiple(blocks) = &msg.content {
                for block in blocks {
                    if block.content_type == "tool_use" {
                        if let (Some(id), Some(name)) = (&block.id, &block.name) {
                            tool_name_map.insert(id.clone(), name.clone());
                        }
                    }
                }
            }
        }
    }

    // 1. System Prompt
    if let Some(system_val) = &payload.system {
        let system = extract_system_prompt(system_val);
        if !system.is_empty() {
            openai_messages.push(OpenAiMessage {
                role: "system".to_string(),
                content: Some(system),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }

    // 2. Messages conversation turns
    for msg in &payload.messages {
        match &msg.content {
            ContentVal::Single(text) => {
                openai_messages.push(OpenAiMessage {
                    role: msg.role.clone(),
                    content: Some(text.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
            ContentVal::Multiple(blocks) => {
                if msg.role == "user" {
                    let mut user_text = String::new();
                    for block in blocks {
                        match block.content_type.as_str() {
                            "text" => {
                                if let Some(t) = &block.text {
                                    if !user_text.is_empty() {
                                        user_text.push('\n');
                                    }
                                    user_text.push_str(t);
                                }
                            }
                            "tool_result" => {
                                let name = block.name.clone().or_else(|| {
                                    block
                                        .tool_use_id
                                        .as_ref()
                                        .and_then(|id| tool_name_map.get(id).cloned())
                                });
                                openai_messages.push(OpenAiMessage {
                                    role: "tool".to_string(),
                                    content: block
                                        .content
                                        .as_ref()
                                        .map(tool_result_content_to_string),
                                    tool_calls: None,
                                    tool_call_id: block.tool_use_id.clone(),
                                    name,
                                });
                            }
                            _ => {}
                        }
                    }
                    if !user_text.is_empty() {
                        openai_messages.push(OpenAiMessage {
                            role: "user".to_string(),
                            content: Some(user_text),
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        });
                    }
                } else if msg.role == "assistant" {
                    let mut assistant_text = String::new();
                    let mut tool_calls = Vec::new();
                    for block in blocks {
                        match block.content_type.as_str() {
                            "text" => {
                                if let Some(t) = &block.text {
                                    if !assistant_text.is_empty() {
                                        assistant_text.push('\n');
                                    }
                                    assistant_text.push_str(t);
                                }
                            }
                            "tool_use" => {
                                if let (Some(id), Some(name), Some(input)) =
                                    (&block.id, &block.name, &block.input)
                                {
                                    tool_calls.push(OpenAiToolCall {
                                        id: id.clone(),
                                        tool_type: "function".to_string(),
                                        function: OpenAiFunctionCall {
                                            name: name.clone(),
                                            arguments: serde_json::to_string(input)
                                                .unwrap_or_default(),
                                        },
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                    openai_messages.push(OpenAiMessage {
                        role: "assistant".to_string(),
                        content: if assistant_text.is_empty() {
                            None
                        } else {
                            Some(assistant_text)
                        },
                        tool_calls: if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls)
                        },
                        tool_call_id: None,
                        name: None,
                    });
                }
            }
        }
    }

    // 3. Tools mapping
    let tools = payload.tools.as_ref().map(|t_list| {
        t_list
            .iter()
            .map(|t| OpenAiTool {
                tool_type: "function".to_string(),
                function: OpenAiFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect()
    });

    // 4. Tool Choice mapping
    let tool_choice = payload.tool_choice.as_ref().map(|tc| {
        if let Some(tc_str) = tc.as_str() {
            serde_json::Value::String(tc_str.to_string())
        } else if let Some(tc_obj) = tc.as_object() {
            if let Some(t_type) = tc_obj.get("type").and_then(|t| t.as_str()) {
                match t_type {
                    "auto" => serde_json::Value::String("auto".to_string()),
                    "any" => serde_json::Value::String("required".to_string()),
                    "tool" => {
                        if let Some(t_name) = tc_obj.get("name").and_then(|n| n.as_str()) {
                            serde_json::json!({
                                "type": "function",
                                "function": { "name": t_name }
                            })
                        } else {
                            serde_json::Value::String("auto".to_string())
                        }
                    }
                    _ => serde_json::Value::String("auto".to_string()),
                }
            } else {
                serde_json::Value::String("auto".to_string())
            }
        } else {
            serde_json::Value::String("auto".to_string())
        }
    });

    OpenAiRequest {
        model: mapped_model,
        messages: openai_messages,
        tools,
        tool_choice,
        stream: payload.stream,
        temperature: payload.temperature,
        max_tokens: payload.max_tokens,
    }
}

async fn rotate_warp_ip() {
    info!("Rotating WARP IP address...");
    let _ = tokio::process::Command::new("warp-cli")
        .arg("disconnect")
        .output()
        .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
    let _ = tokio::process::Command::new("warp-cli")
        .arg("connect")
        .output()
        .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(2500)).await;
    info!("WARP IP address rotated successfully.");
}

async fn execute_with_warp_retry(
    client: &Client,
    req_body: &OpenAiRequest,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut retry_count = 0;
    loop {
        let res = client
            .post("https://opencode.ai/zen/v1/chat/completions")
            .json(req_body)
            .send()
            .await;

        match res {
            Ok(response) => {
                let status = response.status();
                // TOO_MANY_REQUESTS is 429, BAD_REQUEST (400) is returned by upstream on free rate limit errors too
                let is_rate_limit = status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status == reqwest::StatusCode::BAD_REQUEST;

                if is_rate_limit && retry_count < 3 {
                    retry_count += 1;
                    warn!(
                        "Upstream rate limit or request error hit (status {}). Attempting to rotate WARP IP (Attempt {}/3)...",
                        status, retry_count
                    );
                    rotate_warp_ip().await;
                    continue;
                }
                return Ok(response);
            }
            Err(e) => {
                if retry_count < 3 {
                    retry_count += 1;
                    warn!(
                        "Network error connecting upstream: {}. Attempting to rotate WARP IP (Attempt {}/3)...",
                        e, retry_count
                    );
                    rotate_warp_ip().await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

// ── API Forwarding Implementations ──

pub async fn forward_to_llm_sync(
    client: &Client,
    mut payload: MessagesRequest,
    model: String,
    config: std::sync::Arc<crate::config::BridgeConfig>,
) -> Result<serde_json::Value, BridgeError> {
    let mut loop_count = 0;
    loop {
        loop_count += 1;
        if loop_count > 5 {
            return Err(BridgeError::UpstreamError(
                "Search loop protection triggered".to_string(),
            ));
        }

        let openai_req = map_anthropic_to_openai(&payload, model.clone());

        info!("Forwarding sync request for model {}", model);

        let res = execute_with_warp_retry(client, &openai_req)
            .await
            .map_err(|e| BridgeError::UpstreamError(e.to_string()))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            error!("Upstream API returned status {}: {}", status, body);
            return Err(BridgeError::UpstreamError(format!(
                "Upstream returned status {}: {}",
                status, body
            )));
        }

        let openai_resp: OpenAiResponse = res
            .json()
            .await
            .map_err(|e| BridgeError::UpstreamError(format!("Failed to parse response: {}", e)))?;

        let choice = openai_resp.choices.first().ok_or_else(|| {
            BridgeError::UpstreamError("No choices returned from upstream".to_string())
        })?;

        // Check if there is an intercepted search tool call
        let mut has_search = false;
        let mut search_tc_id = String::new();
        let mut search_tc_name = String::new();
        let mut search_tc_input = serde_json::Value::Null;
        let mut search_query = String::new();

        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                if is_web_search_tool(&tc.function.name) {
                    has_search = true;
                    search_tc_id = tc.id.clone();
                    search_tc_name = tc.function.name.clone();
                    let input_val: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    search_tc_input = input_val.clone();

                    if let Some(obj) = input_val.as_object() {
                        if let Some(q_val) = obj.get("query").and_then(|v| v.as_str()) {
                            search_query = q_val.to_string();
                        } else if let Some(q_val) = obj.get("q").and_then(|v| v.as_str()) {
                            search_query = q_val.to_string();
                        } else {
                            for (_, v) in obj {
                                if let Some(s) = v.as_str() {
                                    search_query = s.to_string();
                                    break;
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }

        if has_search {
            info!(
                "Intercepted sync search tool call. Query: '{}'",
                search_query
            );
            let search_results = perform_search_fallback(&search_query, &config).await;
            info!("Search completed. Results length: {}", search_results.len());

            // Append assistant's tool call turn
            let mut assistant_content = Vec::new();
            if let Some(reasoning) = &choice.message.reasoning_content {
                if !reasoning.is_empty() {
                    assistant_content.push(
                        serde_json::from_value(serde_json::json!({
                            "type": "text",
                            "text": format!("<thinking>{}</thinking>", reasoning)
                        }))
                        .unwrap(),
                    );
                }
            }
            if let Some(content) = &choice.message.content {
                if !content.is_empty() {
                    assistant_content.push(
                        serde_json::from_value(serde_json::json!({
                            "type": "text",
                            "text": content
                        }))
                        .unwrap(),
                    );
                }
            }
            assistant_content.push(
                serde_json::from_value(serde_json::json!({
                    "type": "tool_use",
                    "id": search_tc_id,
                    "name": search_tc_name,
                    "input": search_tc_input
                }))
                .unwrap(),
            );

            payload.messages.push(crate::handlers::Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(assistant_content),
            });

            // Append tool response turn
            let tool_result_content = vec![serde_json::from_value(serde_json::json!({
                "type": "tool_result",
                "tool_use_id": search_tc_id,
                "name": search_tc_name,
                "content": search_results
            }))
            .unwrap()];
            payload.messages.push(crate::handlers::Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(tool_result_content),
            });

            // Loop again with updated history
            continue;
        }

        // Standard response formatting (no search intercepted or final turn)
        let mut content_blocks = Vec::new();

        // 1. Thinking block (reasoning_content)
        if let Some(reasoning) = &choice.message.reasoning_content {
            if !reasoning.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "thinking",
                    "thinking": reasoning
                }));
            }
        }

        // 2. Text block
        if let Some(text) = &choice.message.content {
            if !text.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": text
                }));
            }
        }

        // 3. Tool calls
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                let input_val: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                content_blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.function.name,
                    "input": input_val
                }));
            }
        }

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("stop") => "end_turn",
            Some("tool_calls") => "tool_use",
            Some("length") => "max_tokens",
            _ => "end_turn",
        };

        let usage = openai_resp.usage.unwrap_or(OpenAiUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
        });

        let anthropic_resp = serde_json::json!({
            "id": format!("msg_opencode_{}", openai_resp.id),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": content_blocks,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": {
                "input_tokens": usage.prompt_tokens,
                "output_tokens": usage.completion_tokens
            }
        });

        return Ok(anthropic_resp);
    }
}

/// Perform a streaming completions request to upstream OpenCode API and stream Anthropic SSE chunks.
pub async fn forward_to_llm_stream(
    client: &Client,
    payload: MessagesRequest,
    model: String,
    channel_capacity: usize,
    config: std::sync::Arc<crate::config::BridgeConfig>,
) -> Result<impl Stream<Item = Result<Event, Infallible>>, BridgeError> {
    let (tx, rx) = tokio::sync::mpsc::channel(channel_capacity);
    let msg_id = format!(
        "msg_opencode_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let builder = crate::sse::SseEventBuilder::new(msg_id, model.clone());
    let client_clone = client.clone();
    let model_clone = model.clone();

    tokio::spawn(async move {
        let mut current_payload = payload;
        let mut loop_count = 0;

        loop {
            loop_count += 1;
            if loop_count > 5 {
                error!("Search loop protection triggered!");
                break;
            }

            let openai_req = map_anthropic_to_openai(&current_payload, model_clone.clone());

            info!(
                "Forwarding stream request for model {} (loop {})",
                model_clone, loop_count
            );

            let res = match execute_with_warp_retry(&client_clone, &openai_req).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Error forwarding upstream request: {}", e);
                    break;
                }
            };

            if !res.status().is_success() {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                error!("Upstream API returned status {}: {}", status, body);
                break;
            }

            let mut bytes_stream = res.bytes_stream();
            let mut line_buffer = Vec::new();

            if loop_count == 1 {
                let _ = tx.send(builder.message_start()).await;
            }

            let mut thinking_block_index: Option<usize> = None;
            let mut text_block_index: Option<usize> = None;
            let mut tool_block_indices: HashMap<usize, (usize, String, String)> = HashMap::new();
            let mut next_content_block_index = 0;
            let mut final_stop_reason = "end_turn".to_string();

            let mut intercepting_search = false;
            let mut search_tc_id = String::new();
            let mut search_tc_name = String::new();
            let mut search_tc_args = String::new();
            let mut accumulated_thinking = String::new();
            let mut accumulated_text = String::new();

            let mut stream_failed = false;

            while let Some(chunk_res) = bytes_stream.next().await {
                let chunk = match chunk_res {
                    Ok(c) => c,
                    Err(e) => {
                        error!("Error reading chunk from upstream: {}", e);
                        stream_failed = true;
                        break;
                    }
                };
                line_buffer.extend_from_slice(&chunk);

                while let Some(pos) = line_buffer.iter().position(|&b| b == b'\n') {
                    let line_bytes = line_buffer.drain(..pos + 1).collect::<Vec<u8>>();
                    let line = String::from_utf8_lossy(&line_bytes);
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }

                    if let Some(stripped) = line.strip_prefix("data:") {
                        let data_str = stripped.trim();
                        if data_str == "[DONE]" {
                            break;
                        }

                        if let Ok(chunk) = serde_json::from_str::<OpenAiStreamChunk>(data_str) {
                            if let Some(choice) = chunk.choices.first() {
                                if let Some(reason) = &choice.finish_reason {
                                    final_stop_reason = match reason.as_str() {
                                        "stop" => "end_turn".to_string(),
                                        "tool_calls" => "tool_use".to_string(),
                                        "length" => "max_tokens".to_string(),
                                        _ => "end_turn".to_string(),
                                    };
                                }

                                // 1. Process reasoning_content (thinking delta)
                                if let Some(reasoning) = &choice.delta.reasoning_content {
                                    if !reasoning.is_empty() {
                                        accumulated_thinking.push_str(reasoning);
                                        if !intercepting_search {
                                            let idx = match thinking_block_index {
                                                Some(i) => i,
                                                None => {
                                                    let i = next_content_block_index;
                                                    next_content_block_index += 1;
                                                    thinking_block_index = Some(i);
                                                    let start_ev = Event::default()
                                                        .event("content_block_start")
                                                        .json_data(serde_json::json!({
                                                            "type": "content_block_start",
                                                            "index": i,
                                                            "content_block": {"type": "thinking", "thinking": ""}
                                                        }))
                                                        .unwrap_or_else(|_| Event::default().data("{}"));
                                                    let _ = tx.send(start_ev).await;
                                                    i
                                                }
                                            };

                                            let delta_ev = Event::default()
                                                .event("content_block_delta")
                                                .json_data(serde_json::json!({
                                                    "type": "content_block_delta",
                                                    "index": idx,
                                                    "delta": {"type": "thinking_delta", "thinking": reasoning}
                                                }))
                                                .unwrap_or_else(|_| Event::default().data("{}"));
                                            let _ = tx.send(delta_ev).await;
                                        }
                                    }
                                }

                                // 2. Process content (text delta)
                                if let Some(content) = &choice.delta.content {
                                    if !content.is_empty() {
                                        accumulated_text.push_str(content);
                                        if !intercepting_search {
                                            // Close thinking block if open
                                            if let Some(idx) = thinking_block_index {
                                                let stop_ev = Event::default()
                                                    .event("content_block_stop")
                                                    .json_data(serde_json::json!({
                                                        "type": "content_block_stop",
                                                        "index": idx
                                                    }))
                                                    .unwrap_or_else(|_| {
                                                        Event::default().data("{}")
                                                    });
                                                let _ = tx.send(stop_ev).await;
                                                thinking_block_index = None;
                                            }

                                            let idx = match text_block_index {
                                                Some(i) => i,
                                                None => {
                                                    let i = next_content_block_index;
                                                    next_content_block_index += 1;
                                                    text_block_index = Some(i);
                                                    let start_ev = Event::default()
                                                        .event("content_block_start")
                                                        .json_data(serde_json::json!({
                                                            "type": "content_block_start",
                                                            "index": i,
                                                            "content_block": {"type": "text", "text": ""}
                                                        }))
                                                        .unwrap_or_else(|_| Event::default().data("{}"));
                                                    let _ = tx.send(start_ev).await;
                                                    i
                                                }
                                            };

                                            let delta_ev = Event::default()
                                                .event("content_block_delta")
                                                .json_data(serde_json::json!({
                                                    "type": "content_block_delta",
                                                    "index": idx,
                                                    "delta": {"type": "text_delta", "text": content}
                                                }))
                                                .unwrap_or_else(|_| Event::default().data("{}"));
                                            let _ = tx.send(delta_ev).await;
                                        }
                                    }
                                }

                                // 3. Process tool calls
                                if let Some(tool_calls) = &choice.delta.tool_calls {
                                    for tc in tool_calls {
                                        let call_idx = tc.index;

                                        // If not created yet and we have tool id & function name
                                        #[allow(clippy::map_entry)]
                                        if !tool_block_indices.contains_key(&call_idx) {
                                            if let (Some(id), Some(func)) = (&tc.id, &tc.function) {
                                                if let Some(name) = &func.name {
                                                    if is_web_search_tool(name) {
                                                        intercepting_search = true;
                                                        search_tc_id = id.clone();
                                                        search_tc_name = name.clone();
                                                    } else {
                                                        // Close thinking block if open
                                                        if let Some(idx) = thinking_block_index {
                                                            let stop_ev = Event::default()
                                                                .event("content_block_stop")
                                                                .json_data(serde_json::json!({
                                                                    "type": "content_block_stop",
                                                                    "index": idx
                                                                }))
                                                                .unwrap_or_else(|_| {
                                                                    Event::default().data("{}")
                                                                });
                                                            let _ = tx.send(stop_ev).await;
                                                            thinking_block_index = None;
                                                        }
                                                        // Close text block if open
                                                        if let Some(idx) = text_block_index {
                                                            let stop_ev = Event::default()
                                                                .event("content_block_stop")
                                                                .json_data(serde_json::json!({
                                                                    "type": "content_block_stop",
                                                                    "index": idx
                                                                }))
                                                                .unwrap_or_else(|_| {
                                                                    Event::default().data("{}")
                                                                });
                                                            let _ = tx.send(stop_ev).await;
                                                            text_block_index = None;
                                                        }

                                                        let idx = next_content_block_index;
                                                        next_content_block_index += 1;
                                                        tool_block_indices.insert(
                                                            call_idx,
                                                            (idx, id.clone(), name.clone()),
                                                        );

                                                        let start_ev = Event::default()
                                                            .event("content_block_start")
                                                            .json_data(serde_json::json!({
                                                                "type": "content_block_start",
                                                                "index": idx,
                                                                "content_block": {
                                                                    "type": "tool_use",
                                                                    "id": id,
                                                                    "name": name,
                                                                    "input": {}
                                                                }
                                                            }))
                                                            .unwrap_or_else(|_| {
                                                                Event::default().data("{}")
                                                            });
                                                        let _ = tx.send(start_ev).await;
                                                    }
                                                }
                                            }
                                        }

                                        // Send arguments delta if present
                                        if intercepting_search {
                                            if let Some(func) = &tc.function {
                                                if let Some(args) = &func.arguments {
                                                    search_tc_args.push_str(args);
                                                }
                                            }
                                        } else {
                                            if let Some((idx, _, _)) =
                                                tool_block_indices.get(&call_idx)
                                            {
                                                if let Some(func) = &tc.function {
                                                    if let Some(args) = &func.arguments {
                                                        if !args.is_empty() {
                                                            let delta_ev = Event::default()
                                                                .event("content_block_delta")
                                                                .json_data(serde_json::json!({
                                                                    "type": "content_block_delta",
                                                                    "index": *idx,
                                                                    "delta": {
                                                                        "type": "input_json_delta",
                                                                        "partial_json": args
                                                                    }
                                                                }))
                                                                .unwrap_or_else(|_| {
                                                                    Event::default().data("{}")
                                                                });
                                                            let _ = tx.send(delta_ev).await;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if stream_failed {
                break;
            }

            if intercepting_search {
                // Perform DuckDuckGo Search
                let input_val: serde_json::Value = serde_json::from_str(&search_tc_args)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                let mut search_query = String::new();
                if let Some(obj) = input_val.as_object() {
                    if let Some(q_val) = obj.get("query").and_then(|v| v.as_str()) {
                        search_query = q_val.to_string();
                    } else if let Some(q_val) = obj.get("q").and_then(|v| v.as_str()) {
                        search_query = q_val.to_string();
                    } else {
                        for (_, v) in obj {
                            if let Some(s) = v.as_str() {
                                search_query = s.to_string();
                                break;
                            }
                        }
                    }
                }

                info!(
                    "Intercepted stream search tool call. Query: '{}'",
                    search_query
                );
                let search_results = perform_search_fallback(&search_query, &config).await;
                info!("Search completed. Results length: {}", search_results.len());

                // Append assistant turn
                let mut assistant_content = Vec::new();
                if !accumulated_thinking.is_empty() {
                    assistant_content.push(
                        serde_json::from_value(serde_json::json!({
                            "type": "text",
                            "text": format!("<thinking>{}</thinking>", accumulated_thinking)
                        }))
                        .unwrap(),
                    );
                }
                if !accumulated_text.is_empty() {
                    assistant_content.push(
                        serde_json::from_value(serde_json::json!({
                            "type": "text",
                            "text": accumulated_text
                        }))
                        .unwrap(),
                    );
                }
                assistant_content.push(
                    serde_json::from_value(serde_json::json!({
                        "type": "tool_use",
                        "id": search_tc_id,
                        "name": search_tc_name,
                        "input": input_val
                    }))
                    .unwrap(),
                );

                current_payload.messages.push(crate::handlers::Message {
                    role: "assistant".to_string(),
                    content: ContentVal::Multiple(assistant_content),
                });

                // Append tool result turn
                let tool_result_content = vec![serde_json::from_value(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": search_tc_id,
                    "name": search_tc_name,
                    "content": search_results
                }))
                .unwrap()];
                current_payload.messages.push(crate::handlers::Message {
                    role: "user".to_string(),
                    content: ContentVal::Multiple(tool_result_content),
                });

                // Loop again with updated history to fetch search-informed response
                continue;
            }

            // Close any remaining active content blocks
            if let Some(idx) = thinking_block_index {
                let stop_ev = Event::default()
                    .event("content_block_stop")
                    .json_data(serde_json::json!({
                        "type": "content_block_stop",
                        "index": idx
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(stop_ev).await;
            }
            if let Some(idx) = text_block_index {
                let stop_ev = Event::default()
                    .event("content_block_stop")
                    .json_data(serde_json::json!({
                        "type": "content_block_stop",
                        "index": idx
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(stop_ev).await;
            }
            for (_, (idx, _, _)) in tool_block_indices {
                let stop_ev = Event::default()
                    .event("content_block_stop")
                    .json_data(serde_json::json!({
                        "type": "content_block_stop",
                        "index": idx
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(stop_ev).await;
            }

            // Send final message_delta and message_stop
            let delta_ev = Event::default()
                .event("message_delta")
                .json_data(serde_json::json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": final_stop_reason,
                        "stop_sequence": null
                    },
                    "usage": {"output_tokens": 0}
                }))
                .unwrap_or_else(|_| Event::default().data("{}"));
            let _ = tx.send(delta_ev).await;

            let stop_ev = Event::default()
                .event("message_stop")
                .json_data(serde_json::json!({
                    "type": "message_stop"
                }))
                .unwrap_or_else(|_| Event::default().data("{}"));
            let _ = tx.send(stop_ev).await;
            break;
        }
    });

    Ok(tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::{AnthropicTool, ContentVal, Message, MessageContent, MessagesRequest};

    #[test]
    fn test_is_web_search_tool() {
        assert!(is_web_search_tool("web_search"));
        assert!(is_web_search_tool("websearch"));
        assert!(is_web_search_tool("web_fetch"));
        assert!(is_web_search_tool("webfetch"));
        assert!(!is_web_search_tool("google_search"));
        assert!(!is_web_search_tool("some_other_tool"));
    }

    #[test]
    fn test_url_decode() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("http%3A%2F%2Fexample.com"), "http://example.com");
        assert_eq!(url_decode("abc"), "abc");
    }

    #[tokio::test]
    async fn test_perform_duckduckgo_search() {
        let results = perform_duckduckgo_search("rust programming").await;
        assert!(!results.is_empty());
    }

    #[test]
    fn test_tool_result_content_to_string() {
        // String variant
        let val_str = serde_json::Value::String("hello world".to_string());
        assert_eq!(tool_result_content_to_string(&val_str), "hello world");

        // Object array variant
        let val_arr = serde_json::json!([
            { "type": "text", "text": "line 1" },
            { "type": "text", "text": "line 2" }
        ]);
        assert_eq!(tool_result_content_to_string(&val_arr), "line 1\nline 2");

        // Non-standard array format
        let val_arr_fallback = serde_json::json!(["hello", 123]);
        assert_eq!(
            tool_result_content_to_string(&val_arr_fallback),
            "\"hello\"\n123"
        );

        // Number/Object fallback
        let val_num = serde_json::Value::Number(42.into());
        assert_eq!(tool_result_content_to_string(&val_num), "42");
    }

    #[test]
    fn test_map_anthropic_to_openai_plain() {
        let payload = MessagesRequest {
            model: Some("claude-3-5-sonnet".to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Single("hello".to_string()),
            }],
            system: Some(serde_json::json!("you are a helpful assistant")),
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.7),
            max_tokens: Some(1024),
        };

        let result = map_anthropic_to_openai(&payload, "claude-3-5-sonnet".to_string());
        assert_eq!(result.model, "claude-3-5-sonnet");
        assert_eq!(result.messages.len(), 2); // 1 system + 1 user
        assert_eq!(result.messages[0].role, "system");
        assert_eq!(
            result.messages[0].content.as_deref(),
            Some("you are a helpful assistant")
        );
        assert_eq!(result.messages[1].role, "user");
        assert_eq!(result.messages[1].content.as_deref(), Some("hello"));
    }

    #[test]
    fn test_map_anthropic_to_openai_tools_and_results() {
        let payload = MessagesRequest {
            model: None,
            messages: vec![
                Message {
                    role: "user".to_string(),
                    content: ContentVal::Single("run command".to_string()),
                },
                Message {
                    role: "assistant".to_string(),
                    content: ContentVal::Multiple(vec![
                        MessageContent {
                            content_type: "text".to_string(),
                            text: Some("Okay, running bash command...".to_string()),
                            ..Default::default()
                        },
                        MessageContent {
                            content_type: "tool_use".to_string(),
                            id: Some("call_123".to_string()),
                            name: Some("bash".to_string()),
                            input: Some(serde_json::json!({ "command": "echo test" })),
                            ..Default::default()
                        },
                    ]),
                },
                Message {
                    role: "user".to_string(),
                    content: ContentVal::Multiple(vec![MessageContent {
                        content_type: "tool_result".to_string(),
                        tool_use_id: Some("call_123".to_string()),
                        content: Some(serde_json::json!([
                            { "type": "text", "text": "test output" }
                        ])),
                        ..Default::default()
                    }]),
                },
            ],
            system: None,
            tools: Some(vec![AnthropicTool {
                name: "bash".to_string(),
                description: "run a command".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
            }]),
            tool_choice: Some(serde_json::json!({ "type": "any" })),
            stream: true,
            temperature: None,
            max_tokens: None,
        };

        let result = map_anthropic_to_openai(&payload, "deepseek-v4-flash".to_string());
        assert_eq!(result.model, "deepseek-v4-flash-free"); // Mapped model name
        assert_eq!(result.messages.len(), 3);

        // First user message
        assert_eq!(result.messages[0].role, "user");
        assert_eq!(result.messages[0].content.as_deref(), Some("run command"));

        // Assistant message with tool_calls
        assert_eq!(result.messages[1].role, "assistant");
        assert_eq!(
            result.messages[1].content.as_deref(),
            Some("Okay, running bash command...")
        );
        let tc = result.messages[1].tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "call_123");
        assert_eq!(tc[0].function.name, "bash");
        assert_eq!(tc[0].function.arguments, "{\"command\":\"echo test\"}");

        // Tool result message mapped to OpenAI's tool role
        assert_eq!(result.messages[2].role, "tool");
        assert_eq!(result.messages[2].tool_call_id.as_deref(), Some("call_123"));
        assert_eq!(result.messages[2].content.as_deref(), Some("test output"));
        // Name should be retrieved from history
        assert_eq!(result.messages[2].name.as_deref(), Some("bash"));

        // Verify tools and tool_choice mappings
        let res_tools = result.tools.unwrap();
        assert_eq!(res_tools.len(), 1);
        assert_eq!(res_tools[0].function.name, "bash");
        assert_eq!(
            result.tool_choice,
            Some(serde_json::Value::String("required".to_string()))
        );
    }
}
