//! Curated OpenCode Zen free-model catalog and model selection policy.

use crate::management::{config_apply, dto};
use crate::state::AppState;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct FreeModel {
    pub id: &'static str,
    pub label: &'static str,
    pub provider: &'static str,
    pub protocol: &'static str,
    pub limited_time: bool,
    pub privacy_notice: &'static str,
}

pub const FREE_MODELS: &[FreeModel] = &[
    FreeModel {
        id: "opencode/x-preview-f-free",
        label: "OpenCode X-Preview-F (OX Alpha)",
        provider: "OpenCode Zen",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Preview endpoint; do not submit personal or confidential information.",
    },
    FreeModel {
        id: "opencode/deepseek-v4-flash-free",
        label: "DeepSeek V4 Flash Free",
        provider: "OpenCode Zen",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Free-period prompts may be retained and used to improve the model.",
    },
    FreeModel {
        id: "opencode/nemotron-3-ultra-free",
        label: "Nemotron 3 Ultra Free",
        provider: "OpenCode Zen",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Trial endpoint; do not submit personal or confidential information.",
    },
    FreeModel {
        id: "opencode/mimo-v2.5-free",
        label: "MiMo-V2.5 Free",
        provider: "OpenCode Zen",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Free-period prompts may be retained and used to improve the model.",
    },
    FreeModel {
        id: "opencode/north-mini-code-free",
        label: "North Mini Code Free",
        provider: "OpenCode Zen",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice:
            "Do not submit personal or confidential information during the free period.",
    },
    FreeModel {
        id: "opencode/big-pickle",
        label: "Big Pickle",
        provider: "OpenCode Zen",
        protocol: "openai_chat_completions",
        limited_time: true,
        privacy_notice: "Free-period prompts may be retained and used to improve the model.",
    },
];

pub fn free_models() -> &'static [FreeModel] {
    FREE_MODELS
}

pub fn is_supported_free_model(model: &str) -> bool {
    FREE_MODELS.iter().any(|candidate| candidate.id == model)
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
        assert_eq!(FREE_MODELS.len(), 6);
        for model in FREE_MODELS {
            assert!(model.id.starts_with("opencode/"));
            assert!(ids.insert(model.id));
            assert!(model.limited_time);
        }
    }
}
