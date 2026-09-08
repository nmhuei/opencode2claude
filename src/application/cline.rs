use crate::application::models::FreeModel;
use std::path::{Path, PathBuf};

pub const CLINE_BASE_URL: &str = "https://api.cline.bot/api/v1";
pub const CLINE_DEFAULT_MODEL: &str = "z-ai/glm-5.3-flash";

pub const CLINE_FREE_MODELS: &[FreeModel] = &[
    FreeModel {
        id: "z-ai/glm-5.3-flash",
        label: "GLM 5.3 Flash",
        provider: "Cline / Z.ai",
        protocol: "openai_chat_completions",
        limited_time: false,
        privacy_notice: "Cline free tier.",
        context_window: 1_000_000,
        max_output_tokens: 131_072,
        supports_thinking: true,
    },
    FreeModel {
        id: "inclusionai/ling-3.0-flash-fin:free",
        label: "Ling 3.0 Flash Fin Free",
        provider: "Cline / InclusionAI",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Cline free tier.",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_thinking: false,
    },
    FreeModel {
        id: "nvidia/nemotron-3.5-lightning:free",
        label: "Nemotron 3.5 Lightning Free",
        provider: "Cline / Nvidia",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Cline free tier.",
        context_window: 256_000,
        max_output_tokens: 32_768,
        supports_thinking: true,
    },
    FreeModel {
        id: "cohere/north-mini-code:free",
        label: "North Mini Code Free",
        provider: "Cline / Cohere",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Cline free tier.",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_thinking: false,
    },
];

pub fn normalize_cline_model(raw: &str) -> String {
    let clean = raw.trim();
    if clean.eq_ignore_ascii_case("glm-5.3-flash")
        || clean.eq_ignore_ascii_case("glm5.3-flash")
        || clean.eq_ignore_ascii_case("glm5.3 flash")
        || clean.eq_ignore_ascii_case("glm")
    {
        "z-ai/glm-5.3-flash".to_string()
    } else {
        clean.to_string()
    }
}

pub fn cline_settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".cline/data/settings/providers.json"))
}

pub fn find_cline_token() -> Result<String, String> {
    let path = cline_settings_path().ok_or_else(|| {
        "could not determine home directory (neither HOME nor USERPROFILE is set)".to_string()
    })?;
    read_cline_token_from_path(&path)
}

pub fn read_cline_token_from_path(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Err(format!(
            "Cline settings file not found at {}. Ensure Cline is installed and authenticated, or pass --api-key-stdin",
            path.display()
        ));
    }
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;

    let token = json
        .pointer("/providers/cline/settings/auth/accessToken")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            format!(
                "no accessToken found at /providers/cline/settings/auth/accessToken in {}. Please log in via Cline or pass --api-key-stdin",
                path.display()
            )
        })?;

    Ok(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_cline_model() {
        assert_eq!(normalize_cline_model("glm-5.3-flash"), "z-ai/glm-5.3-flash");
        assert_eq!(normalize_cline_model("glm5.3-flash"), "z-ai/glm-5.3-flash");
        assert_eq!(normalize_cline_model("glm5.3 flash"), "z-ai/glm-5.3-flash");
        assert_eq!(normalize_cline_model("glm"), "z-ai/glm-5.3-flash");
        assert_eq!(
            normalize_cline_model("z-ai/glm-5.3-flash"),
            "z-ai/glm-5.3-flash"
        );
        assert_eq!(
            normalize_cline_model("nvidia/nemotron-3.5-lightning:free"),
            "nvidia/nemotron-3.5-lightning:free"
        );
    }

    #[test]
    fn test_read_cline_token_from_path() {
        let temp_dir = std::env::temp_dir().join("cline_test");
        let _ = std::fs::create_dir_all(&temp_dir);
        let test_file = temp_dir.join("providers.json");
        let content = r#"{
            "providers": {
                "cline": {
                    "settings": {
                        "auth": {
                            "accessToken": "test-jwt-token-123"
                        }
                    }
                }
            }
        }"#;
        std::fs::write(&test_file, content).unwrap();

        let token = read_cline_token_from_path(&test_file).unwrap();
        assert_eq!(token, "test-jwt-token-123");

        let _ = std::fs::remove_file(test_file);
    }
}
