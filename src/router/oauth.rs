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
