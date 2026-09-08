use base64::Engine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClineTokenPayload {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub email: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawClineJson {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    email: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<String>,
}

/// Decodes base64 payload from Cline's authorization callback code
pub fn extract_cline_tokens_from_code(code: &str) -> Result<ClineTokenPayload, String> {
    let clean = code.trim();
    // Padding handling for base64
    let mut base64_str = clean.to_string();
    let padding = (4 - (base64_str.len() % 4)) % 4;
    if padding > 0 {
        base64_str.push_str(&"=".repeat(padding));
    }

    let decoded_bytes = base64::engine::general_purpose::STANDARD
        .decode(&base64_str)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(&base64_str))
        .map_err(|e| format!("Base64 decode error: {e}"))?;

    let decoded_str = String::from_utf8(decoded_bytes)
        .map_err(|e| format!("UTF-8 decode error: {e}"))?;

    // Find first '{' and last '}' to extract valid JSON
    let start = decoded_str
        .find('{')
        .ok_or_else(|| "No JSON object found in decoded payload".to_string())?;
    let end = decoded_str
        .rfind('}')
        .ok_or_else(|| "No JSON object end found in decoded payload".to_string())?;

    let json_str = &decoded_str[start..=end];
    let raw: RawClineJson = serde_json::from_str(json_str)
        .map_err(|e| format!("Failed to parse Cline JSON payload: {e}"))?;

    Ok(ClineTokenPayload {
        access_token: raw.access_token,
        refresh_token: raw.refresh_token,
        email: raw.email,
        expires_at: raw.expires_at,
    })
}

pub fn build_cline_auth_url(port: u16) -> String {
    format!(
        "https://api.cline.bot/api/v1/auth/authorize?client_type=extension&callback_url=http://127.0.0.1:{}/callback&redirect_uri=http://127.0.0.1:{}/callback",
        port, port
    )
}

pub async fn wait_for_cline_callback(
    port: u16,
    timeout: std::time::Duration,
) -> Result<ClineTokenPayload, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .map_err(|e| format!("Failed to bind local callback port {port}: {e}"))?;

    let timeout_future = tokio::time::sleep(timeout);
    tokio::pin!(timeout_future);

    loop {
        tokio::select! {
            _ = &mut timeout_future => {
                return Err("Timed out waiting for browser callback (2 minutes limit)".to_string());
            }
            conn = listener.accept() => {
                let (mut stream, _) = conn.map_err(|e| format!("Connection error: {e}"))?;
                let mut buffer = [0u8; 8192];
                let n = stream.read(&mut buffer).await.map_err(|e| format!("Read error: {e}"))?;
                let req_text = String::from_utf8_lossy(&buffer[..n]);

                // Parse GET line
                if let Some(first_line) = req_text.lines().next() {
                    if let Some(path_and_query) = first_line.split_whitespace().nth(1) {
                        if let Some((_, query)) = path_and_query.split_once('?') {
                            let mut code_param = None;
                            for param in query.split('&') {
                                if let Some((k, v)) = param.split_once('=') {
                                    if k == "code" {
                                        code_param = Some(v);
                                        break;
                                    }
                                }
                            }

                            if let Some(code) = code_param {
                                let decoded_code = urlencoding_decode(code);
                                let tokens = extract_cline_tokens_from_code(&decoded_code);

                                let html_response = match &tokens {
                                    Ok(t) => format!(
                                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<!DOCTYPE html><html><body style='font-family:sans-serif;padding:40px;'><h2>Cline Login Successful!</h2><p>Authenticated as: <b>{}</b></p><p>You can close this window and return to your terminal.</p></body></html>",
                                        t.email.as_deref().unwrap_or("Cline User")
                                    ),
                                    Err(e) => format!(
                                        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<!DOCTYPE html><html><body style='font-family:sans-serif;padding:40px;color:red;'><h2>Authentication Failed</h2><p>{e}</p></body></html>"
                                    ),
                                };

                                let _ = stream.write_all(html_response.as_bytes()).await;
                                let _ = stream.flush().await;
                                return tokens;
                            }
                        }
                    }
                }

                // If favicon or other request, return 204
                let not_found = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(not_found.as_bytes()).await;
            }
        }
    }
}

fn urlencoding_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let h1 = bytes.next();
            let h2 = bytes.next();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                if let Ok(val) = u8::from_str_radix(
                    &format!("{}{}", h1 as char, h2 as char),
                    16,
                ) {
                    out.push(val as char);
                    continue;
                }
            }
        } else if b == b'+' {
            out.push(' ');
        } else {
            out.push(b as char);
        }
    }
    out
}
