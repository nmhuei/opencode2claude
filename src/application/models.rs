//! Curated OpenCode Zen free-model catalog and model selection policy.

use crate::management::{config_apply, dto};
use crate::state::AppState;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct ModelProfile {
    pub id: &'static str,
    pub label: &'static str,
    pub provider: &'static str,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub supports_thinking: bool,
    pub anthropic_alias: &'static str,
}

impl ModelProfile {
    #[inline]
    pub const fn auto_compact_window(&self) -> usize {
        (self.context_window * 80) / 100
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct FreeModel {
    pub id: &'static str,
    pub label: &'static str,
    pub provider: &'static str,
    pub protocol: &'static str,
    pub limited_time: bool,
    pub privacy_notice: &'static str,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub supports_thinking: bool,
}

impl FreeModel {
    #[inline]
    pub const fn auto_compact_window(&self) -> usize {
        (self.context_window * 80) / 100
    }

    pub fn to_profile(&self) -> ModelProfile {
        ModelProfile {
            id: self.id,
            label: self.label,
            provider: self.provider,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            supports_thinking: self.supports_thinking,
            anthropic_alias: if self.context_window >= 1_000_000 {
                "claude-opus-5"
            } else {
                "claude-sonnet-5"
            },
        }
    }
}

pub const FREE_MODELS: &[FreeModel] = &[
    FreeModel {
        id: "opencode/mimo-v2.5-free",
        label: "MiMo-V2.5 Free",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Free-period prompts may be retained and used to improve the model.",
        context_window: 256_000,
        max_output_tokens: 64_000,
        supports_thinking: true,
    },
    FreeModel {
        id: "opencode/nemotron-3-ultra-free",
        label: "Nemotron 3 Ultra Free",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Trial endpoint; do not submit personal or confidential information.",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_thinking: true,
    },
    FreeModel {
        id: "opencode/nemotron-3.5-lightning-free",
        label: "Nemotron 3.5 Lightning Free",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Trial endpoint; do not submit personal or confidential information.",
        context_window: 256_000,
        max_output_tokens: 64_000,
        supports_thinking: true,
    },
    FreeModel {
        id: "opencode/deepseek-v4-flash-free",
        label: "DeepSeek V4 Flash Free",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Free-period prompts may be retained and used to improve the model.",
        context_window: 1_000_000,
        max_output_tokens: 384_000,
        supports_thinking: true,
    },
    FreeModel {
        id: "opencode/big-pickle",
        label: "Big Pickle",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Free-period prompts may be retained and used to improve the model.",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_thinking: true,
    },
    FreeModel {
        id: "opencode/hy3-free",
        label: "HY3 Free",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Free-period prompts may be retained and used to improve the model.",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_thinking: false,
    },
    FreeModel {
        id: "opencode/ling-3.0-flash-fin-free",
        label: "Ling 3.0 Flash Fin Free",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Free-period prompts may be retained and used to improve the model.",
        context_window: 256_000,
        max_output_tokens: 64_000,
        supports_thinking: false,
    },
    FreeModel {
        id: "opencode/laguna-s-2.1-free",
        label: "Laguna S 2.1 Free",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Free-period prompts may be retained and used to improve the model.",
        context_window: 64_000,
        max_output_tokens: 16_384,
        supports_thinking: false,
    },
    FreeModel {
        id: "opencode/muse-spark-1.2-contributor-free",
        label: "Muse Spark 1.2 Contributor Free",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Contributor free tier.",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_thinking: false,
    },
    FreeModel {
        id: "opencode/north-mini-code-free",
        label: "North Mini Code Free",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice:
            "Do not submit personal or confidential information during the free period.",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_thinking: false,
    },
    FreeModel {
        id: "opencode/x-preview-f-free",
        label: "OpenCode X-Preview-F (OX Alpha)",
        provider: "OpenCode",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Preview endpoint; do not submit personal or confidential information.",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        supports_thinking: true,
    },
];

pub const API_MODEL_PROFILES: &[ModelProfile] = &[
    ModelProfile {
        id: "deepseek-v4-flash",
        label: "DeepSeek V4 Flash",
        provider: "DeepSeek API",
        context_window: 1_000_000,
        max_output_tokens: 384_000,
        supports_thinking: true,
        anthropic_alias: "claude-opus-5",
    },
    ModelProfile {
        id: "deepseek-v4-flash-vision-exp",
        label: "DeepSeek V4 Flash Vision Exp",
        provider: "DeepSeek API",
        context_window: 1_000_000,
        max_output_tokens: 384_000,
        supports_thinking: true,
        anthropic_alias: "claude-opus-5",
    },
    ModelProfile {
        id: "glm-5.3-flash",
        label: "GLM 5.3 Flash",
        provider: "Zhipu API",
        context_window: 1_000_000,
        max_output_tokens: 131_072,
        supports_thinking: true,
        anthropic_alias: "claude-opus-5",
    },
    ModelProfile {
        id: "qwen3.8-flash",
        label: "Qwen 3.8 Flash",
        provider: "b.ai API",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_thinking: true,
        anthropic_alias: "claude-sonnet-5",
    },
];
pub fn model_pricing(model: &str) -> &'static str {
    let clean = model.strip_prefix("opencode/").unwrap_or(model);
    match clean {
        "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp" | "glm-5.3-flash" => {
            "Free (0 Credits)"
        }
        "qwen3.8-flash" => "Free (0 Credits)",
        _ if is_supported_free_model(model) => "Free tier",
        _ => "Provider-defined",
    }
}

pub fn model_default_output_tokens(model: &str) -> Option<usize> {
    let clean = model.strip_prefix("opencode/").unwrap_or(model);
    match clean {
        "glm-5.3-flash" => Some(65_536),
        _ => None,
    }
}

pub fn free_models() -> &'static [FreeModel] {
    FREE_MODELS
}

pub fn is_supported_free_model(model: &str) -> bool {
    let clean = model.strip_prefix("opencode/").unwrap_or(model);
    FREE_MODELS.iter().any(|candidate| {
        let cand_clean = candidate
            .id
            .strip_prefix("opencode/")
            .unwrap_or(candidate.id);
        candidate.id == model || cand_clean == clean
    })
}

pub fn resolve_model_profile(model: &str) -> ModelProfile {
    let clean = model.strip_prefix("opencode/").unwrap_or(model);
    for candidate in FREE_MODELS {
        let cand_clean = candidate
            .id
            .strip_prefix("opencode/")
            .unwrap_or(candidate.id);
        if candidate.id == model || cand_clean == clean {
            return candidate.to_profile();
        }
    }

    if let Some(profile) = API_MODEL_PROFILES
        .iter()
        .find(|profile| profile.id.eq_ignore_ascii_case(clean))
    {
        return *profile;
    }

    let lower = clean.to_ascii_lowercase();
    if lower.contains("gemini") {
        ModelProfile {
            id: "gemini-3.7-flash",
            label: "Gemini Flash",
            provider: "Google",
            context_window: 1_000_000,
            max_output_tokens: 64_000,
            supports_thinking: true,
            anthropic_alias: "claude-opus-5",
        }
    } else if lower.contains("claude") {
        ModelProfile {
            id: "claude-opus-5",
            label: "Claude Opus 5",
            provider: "Anthropic",
            context_window: 200_000,
            max_output_tokens: 64_000,
            supports_thinking: true,
            anthropic_alias: "claude-opus-5",
        }
    } else if lower.contains("deepseek") {
        ModelProfile {
            id: "deepseek-v4-pro",
            label: "DeepSeek V4",
            provider: "DeepSeek",
            context_window: 1_000_000,
            max_output_tokens: 384_000,
            supports_thinking: true,
            anthropic_alias: "claude-opus-5",
        }
    } else if lower.contains("glm") {
        ModelProfile {
            id: "glm-5.3-flash",
            label: "GLM 5.3 Flash",
            provider: "Zhipu AI",
            context_window: 1_000_000,
            max_output_tokens: 131_072,
            supports_thinking: true,
            anthropic_alias: "claude-opus-5",
        }
    } else if lower.contains("gpt-5.2") {
        ModelProfile {
            id: "gpt-5.2",
            label: "GPT-5.2",
            provider: "OpenAI",
            context_window: 400_000,
            max_output_tokens: 128_000,
            supports_thinking: true,
            anthropic_alias: "claude-sonnet-5",
        }
    } else if lower.contains("qwen") {
        ModelProfile {
            id: "qwen3.8-flash",
            label: "Qwen 3.8 Flash",
            provider: "Alibaba",
            context_window: 128_000,
            max_output_tokens: 16_384,
            supports_thinking: true,
            anthropic_alias: "claude-sonnet-5",
        }
    } else if lower.contains("mimo") {
        ModelProfile {
            id: "mimo-v2.5",
            label: "MiMo V2.5",
            provider: "Xiaomi",
            context_window: 256_000,
            max_output_tokens: 64_000,
            supports_thinking: true,
            anthropic_alias: "claude-sonnet-5",
        }
    } else {
        ModelProfile {
            id: "generic-model",
            label: "Generic Model",
            provider: "OpenAI-Compatible",
            context_window: 128_000,
            max_output_tokens: 16_384,
            supports_thinking: false,
            anthropic_alias: "claude-sonnet-5",
        }
    }
}

pub fn select_free_model(
    state: &AppState,
    model: &str,
) -> Result<dto::ConfigApplyResponse, crate::management::service::ManagementError> {
    if !is_supported_free_model(model) {
        return Err(crate::management::service::ManagementError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "unsupported_free_model",
            "Model is not present in the current OpenCode free-model catalog",
        ));
    }
    let content = format!("model = {model:?}\n");
    config_apply::apply_config(state, &content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_prefixed_unique_and_free() {
        let mut ids = std::collections::HashSet::new();
        assert!(!FREE_MODELS.is_empty());
        for model in FREE_MODELS {
            assert!(model.id.starts_with("opencode/"));
            assert!(ids.insert(model.id));
            assert!(model.limited_time);
            assert_eq!(
                model.auto_compact_window(),
                (model.context_window * 80) / 100
            );
        }
    }

    #[test]
    fn test_resolve_model_profile_autocompact_is_80_percent() {
        let mimo = resolve_model_profile("mimo-v2.5-free");
        assert_eq!(mimo.context_window, 256_000);
        assert_eq!(mimo.auto_compact_window(), 204_800);
        assert!(mimo.supports_thinking);

        let nemotron = resolve_model_profile("opencode/nemotron-3-ultra-free");
        assert_eq!(nemotron.context_window, 128_000);
        assert_eq!(nemotron.auto_compact_window(), 102_400);
        assert!(nemotron.supports_thinking);

        let deepseek_free = resolve_model_profile("opencode/deepseek-v4-flash-free");
        assert_eq!(deepseek_free.context_window, 1_000_000);
        assert_eq!(deepseek_free.max_output_tokens, 384_000);
        assert_eq!(deepseek_free.auto_compact_window(), 800_000);

        let deepseek = resolve_model_profile("deepseek-v4-flash");
        assert_eq!(deepseek.context_window, 1_000_000);
        assert_eq!(deepseek.max_output_tokens, 384_000);
        assert_eq!(deepseek.auto_compact_window(), 800_000);

        let vision = resolve_model_profile("deepseek-v4-flash-vision-exp");
        assert_eq!(vision.context_window, 1_000_000);
        assert_eq!(vision.max_output_tokens, 384_000);
        assert_eq!(vision.auto_compact_window(), 800_000);

        let glm = resolve_model_profile("glm-5.3-flash");
        assert_eq!(glm.context_window, 1_000_000);
        assert_eq!(glm.max_output_tokens, 131_072);
        assert_eq!(glm.auto_compact_window(), 800_000);

        let qwen = resolve_model_profile("qwen3.8-flash");
        assert_eq!(qwen.context_window, 128_000);
        assert_eq!(qwen.max_output_tokens, 16_384);
        assert_eq!(qwen.auto_compact_window(), 102_400);

        assert_eq!(model_pricing("deepseek-v4-flash"), "Free (0 Credits)");
        assert_eq!(model_pricing("glm-5.3-flash"), "Free (0 Credits)");
        assert_eq!(model_pricing("qwen3.8-flash"), "Free (0 Credits)");
        assert_eq!(model_default_output_tokens("glm-5.3-flash"), Some(65_536));
        assert_eq!(model_default_output_tokens("deepseek-v4-flash"), None);

        let unknown = resolve_model_profile("unknown-custom-model");
        assert_eq!(unknown.context_window, 128_000);
        assert_eq!(unknown.auto_compact_window(), 102_400);
    }

    #[test]
    fn b_ai_curated_profiles_are_exactly_the_supported_four_models() {
        let ids = API_MODEL_PROFILES
            .iter()
            .map(|profile| profile.id)
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                "deepseek-v4-flash",
                "deepseek-v4-flash-vision-exp",
                "glm-5.3-flash",
                "qwen3.8-flash",
            ]
        );
        assert_eq!(model_pricing("qwen3.8-flash"), "Free (0 Credits)");
    }
}
