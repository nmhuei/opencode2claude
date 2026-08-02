use crate::config::HistoryCaptureMode;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

use super::types::{HistoryContent, HistoryContentDescriptor};

static BEARER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}").expect("valid bearer regex")
});
static MANAGED_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bsk-oc2-[A-Za-z0-9_-]+\.[A-Za-z0-9_-]{6,}").expect("valid managed key regex")
});
static PROVIDER_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsk-[A-Za-z0-9_-]{16,}").expect("valid provider key regex"));
static JWT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b")
        .expect("valid jwt regex")
});
static PEM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN [^-]*PRIVATE KEY-----.*?-----END [^-]*PRIVATE KEY-----")
        .expect("valid pem regex")
});

const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone)]
pub struct Captured {
    pub body: String,
    pub original_bytes: usize,
    pub stored_bytes: usize,
    pub sha256: String,
    pub redacted: bool,
    pub truncated: bool,
}

pub fn capture_json(value: &Value, mode: HistoryCaptureMode, max_bytes: usize) -> Option<Captured> {
    if matches!(mode, HistoryCaptureMode::Off | HistoryCaptureMode::Metadata) {
        return None;
    }
    let mut value = value.clone();
    let mut redacted = redact_json_keys(&mut value);
    let serialized = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    let (serialized, pattern_redacted) = redact_text(&serialized);
    redacted |= pattern_redacted;
    Some(cap(serialized, max_bytes, redacted))
}

pub fn capture_text(value: &str, mode: HistoryCaptureMode, max_bytes: usize) -> Option<Captured> {
    if matches!(mode, HistoryCaptureMode::Off | HistoryCaptureMode::Metadata) {
        return None;
    }
    let (redacted_text, redacted) = redact_text(value);
    Some(cap(redacted_text, max_bytes, redacted))
}

pub fn as_content(
    kind: impl Into<String>,
    content_type: &str,
    captured: Captured,
) -> HistoryContent {
    HistoryContent {
        descriptor: HistoryContentDescriptor {
            kind: kind.into(),
            content_type: content_type.to_string(),
            original_bytes: captured.original_bytes,
            stored_bytes: captured.stored_bytes,
            sha256: captured.sha256,
            redacted: captured.redacted,
            truncated: captured.truncated,
        },
        body: captured.body,
    }
}

pub fn preview(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut output = normalized.chars().take(max_chars).collect::<String>();
    if normalized.chars().count() > max_chars {
        output.push('…');
    }
    output
}

fn cap(mut body: String, max_bytes: usize, redacted: bool) -> Captured {
    let original_bytes = body.len();
    let mut truncated = false;
    if body.len() > max_bytes {
        body = truncate_utf8(&body, max_bytes.saturating_sub(32));
        body.push_str("\n[TRUNCATED]");
        truncated = true;
    }
    let stored_bytes = body.len();
    let sha256 = Sha256::digest(body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Captured {
        body,
        original_bytes,
        stored_bytes,
        sha256,
        redacted,
        truncated,
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn redact_json_keys(value: &mut Value) -> bool {
    match value {
        Value::Object(map) => {
            let mut changed = false;
            for (key, child) in map.iter_mut() {
                if sensitive_key(key) {
                    *child = Value::String(REDACTED.to_string());
                    changed = true;
                } else {
                    changed |= redact_json_keys(child);
                }
            }
            changed
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= redact_json_keys(item);
            }
            changed
        }
        Value::String(text) => {
            let (replacement, changed) = redact_text(text);
            if changed {
                *text = replacement;
            }
            changed
        }
        _ => false,
    }
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "authorization"
            | "apikey"
            | "accesstoken"
            | "refreshtoken"
            | "password"
            | "passwd"
            | "secret"
            | "cookie"
            | "setcookie"
            | "clientsecret"
            | "privatekey"
            | "dashboardadmintoken"
            | "csrftoken"
    ) || normalized.ends_with("apikey")
        || normalized.ends_with("password")
        || normalized.ends_with("secret")
        || normalized.ends_with("token")
}

fn redact_text(value: &str) -> (String, bool) {
    let mut output = value.to_string();
    let mut changed = false;
    for regex in [
        &*BEARER_RE,
        &*MANAGED_KEY_RE,
        &*PROVIDER_KEY_RE,
        &*JWT_RE,
        &*PEM_RE,
    ] {
        if regex.is_match(&output) {
            output = regex.replace_all(&output, REDACTED).into_owned();
            changed = true;
        }
    }
    (output, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recursively_redacts_keys_and_token_patterns_before_storage() {
        let value = json!({
            "messages": [{"role":"user","content":"use Bearer abcdefghijklmnop"}],
            "api_key": "sk-oc2-client.supersecret",
            "nested": {"password":"hello"}
        });
        let captured = capture_json(&value, HistoryCaptureMode::Redacted, 4096).unwrap();
        assert!(captured.redacted);
        assert!(!captured.body.contains("supersecret"));
        assert!(!captured.body.contains("abcdefghijklmnop"));
        assert!(!captured.body.contains("hello"));
        assert!(captured.body.contains(REDACTED));
    }

    #[test]
    fn caps_content_on_utf8_boundary() {
        let captured =
            capture_text("á".repeat(100).as_str(), HistoryCaptureMode::Redacted, 80).unwrap();
        assert!(captured.truncated);
        assert!(captured.body.is_char_boundary(captured.body.len()));
    }
}
