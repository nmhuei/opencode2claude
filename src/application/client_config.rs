//! Deterministic client-configuration presets for the dashboard API workspace.

use super::integration::IntegrationEnvironment;
use serde::Serialize;
use std::str::FromStr;

const CLAUDE_CODE_COMPAT_MODEL: &str = "claude-opus-5";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientConfigFormat {
    Env,
    ClaudeCode,
    OpenAiPython,
    AnthropicPython,
    Curl,
}

impl ClientConfigFormat {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::ClaudeCode => "claude-code",
            Self::OpenAiPython => "openai-python",
            Self::AnthropicPython => "anthropic-python",
            Self::Curl => "curl",
        }
    }
}

impl FromStr for ClientConfigFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "env" | "dotenv" => Ok(Self::Env),
            "claude-code" | "claude" | "settings-json" => Ok(Self::ClaudeCode),
            "openai-python" | "openai" => Ok(Self::OpenAiPython),
            "anthropic-python" | "anthropic" => Ok(Self::AnthropicPython),
            "curl" | "shell" => Ok(Self::Curl),
            other => Err(format!("unsupported client config format: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GeneratedClientConfig {
    pub format: &'static str,
    pub filename: &'static str,
    pub content_type: &'static str,
    pub content: String,
    pub contains_secret: bool,
}

pub fn generate(
    format: ClientConfigFormat,
    environment: &IntegrationEnvironment,
    api_key: &str,
    contains_secret: bool,
) -> GeneratedClientConfig {
    let model = environment
        .model
        .as_deref()
        .unwrap_or(super::integration::OX_ALPHA_MODEL);
    let content = match format {
        ClientConfigFormat::Env => dotenv(environment, api_key, model),
        ClientConfigFormat::ClaudeCode => claude_code(environment, api_key, model),
        ClientConfigFormat::OpenAiPython => openai_python(environment, api_key, model),
        ClientConfigFormat::AnthropicPython => anthropic_python(environment, api_key, model),
        ClientConfigFormat::Curl => curl_script(environment, api_key, model),
    };
    let (filename, content_type) = match format {
        ClientConfigFormat::Env => ("opencode2api.env", "text/plain; charset=utf-8"),
        ClientConfigFormat::ClaudeCode => (
            "claude-code-settings.json",
            "application/json; charset=utf-8",
        ),
        ClientConfigFormat::OpenAiPython => {
            ("openai-opencode2api.py", "text/x-python; charset=utf-8")
        }
        ClientConfigFormat::AnthropicPython => {
            ("anthropic-opencode2api.py", "text/x-python; charset=utf-8")
        }
        ClientConfigFormat::Curl => ("curl-opencode2api.sh", "text/x-shellscript; charset=utf-8"),
    };
    GeneratedClientConfig {
        format: format.id(),
        filename,
        content_type,
        content,
        contains_secret,
    }
}

fn dotenv(environment: &IntegrationEnvironment, api_key: &str, model: &str) -> String {
    format!(
        "# OpenCode2API client environment\nOPENAI_API_KEY={}\nOPENAI_BASE_URL={}\nANTHROPIC_API_KEY={}\nANTHROPIC_BASE_URL={}\nOPENCODE_MODEL={}\n",
        dotenv_value(api_key),
        dotenv_value(&environment.openai_base_url),
        dotenv_value(api_key),
        dotenv_value(&environment.anthropic_base_url),
        dotenv_value(model),
    )
}

fn claude_code(environment: &IntegrationEnvironment, api_key: &str, model: &str) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "$schema": "https://json.schemastore.org/claude-code-settings.json",
        "env": {
            "ANTHROPIC_API_KEY": api_key,
            "ANTHROPIC_BASE_URL": environment.anthropic_base_url,
            "OPENCODE_MODEL": model
        },
        "model": CLAUDE_CODE_COMPAT_MODEL,
        "ultracode": true,
        "alwaysThinkingEnabled": true
    }))
    .unwrap_or_else(|_| "{}".to_string())
        + "\n"
}

fn openai_python(environment: &IntegrationEnvironment, api_key: &str, model: &str) -> String {
    format!(
        "from openai import OpenAI\n\nclient = OpenAI(\n    api_key={},\n    base_url={},\n)\n\nresponse = client.chat.completions.create(\n    model={},\n    messages=[{{\"role\": \"user\", \"content\": \"Hello from OpenCode2API\"}}],\n)\nprint(response.choices[0].message.content)\n",
        python_string(api_key),
        python_string(&environment.openai_base_url),
        python_string(model),
    )
}

fn anthropic_python(environment: &IntegrationEnvironment, api_key: &str, model: &str) -> String {
    format!(
        "from anthropic import Anthropic\n\nclient = Anthropic(\n    api_key={},\n    base_url={},\n)\n\nmessage = client.messages.create(\n    model={},\n    max_tokens=1024,\n    messages=[{{\"role\": \"user\", \"content\": \"Hello from OpenCode2API\"}}],\n)\nprint(message.content[0].text)\n",
        python_string(api_key),
        python_string(&environment.anthropic_base_url),
        python_string(model),
    )
}

fn curl_script(environment: &IntegrationEnvironment, api_key: &str, model: &str) -> String {
    format!(
        "#!/usr/bin/env sh\nset -eu\n\nOPENCODE2API_KEY={}\nOPENAI_BASE_URL={}\nMODEL={}\n\ncurl --fail-with-body --silent --show-error \\\n  \"$OPENAI_BASE_URL/chat/completions\" \\\n  -H \"Authorization: Bearer $OPENCODE2API_KEY\" \\\n  -H \"Content-Type: application/json\" \\\n  -d \"{{\\\"model\\\":\\\"$MODEL\\\",\\\"messages\\\":[{{\\\"role\\\":\\\"user\\\",\\\"content\\\":\\\"Hello from OpenCode2API\\\"}}]}}\"\n",
        shell_quote(api_key),
        shell_quote(&environment.openai_base_url),
        shell_quote(model),
    )
}

fn dotenv_value(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn python_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment() -> IntegrationEnvironment {
        IntegrationEnvironment {
            anthropic_base_url: "http://127.0.0.1:4000".to_string(),
            openai_base_url: "http://127.0.0.1:4000/v1".to_string(),
            api_key: "active-secret".to_string(),
            model: Some("opencode/deepseek-v4-flash-free".to_string()),
            shell_exports: Vec::new(),
        }
    }

    #[test]
    fn all_formats_have_stable_names_and_expected_base_urls() {
        for format in [
            ClientConfigFormat::Env,
            ClientConfigFormat::ClaudeCode,
            ClientConfigFormat::OpenAiPython,
            ClientConfigFormat::AnthropicPython,
            ClientConfigFormat::Curl,
        ] {
            let generated = generate(format, &environment(), "sk-oc2-REPLACE_ME", false);
            assert!(!generated.filename.is_empty());
            assert!(generated.content.contains("127.0.0.1:4000"));
            assert!(generated.content.contains("sk-oc2-REPLACE_ME"));
        }
    }

    #[test]
    fn claude_code_preset_is_valid_json_with_env_object() {
        let generated = generate(
            ClientConfigFormat::ClaudeCode,
            &environment(),
            "sk-oc2-test",
            true,
        );
        let parsed: serde_json::Value = serde_json::from_str(&generated.content).unwrap();
        assert_eq!(parsed["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:4000");
        assert_eq!(
            parsed["$schema"],
            "https://json.schemastore.org/claude-code-settings.json"
        );
        assert_eq!(parsed["model"], "claude-opus-5");
        assert_eq!(parsed["ultracode"], true);
    }

    #[test]
    fn placeholder_output_does_not_include_active_secret() {
        let generated = generate(
            ClientConfigFormat::Env,
            &environment(),
            "sk-oc2-REPLACE_ME",
            false,
        );
        assert!(!generated.content.contains("active-secret"));
        assert!(!generated.contains_secret);
    }
}
