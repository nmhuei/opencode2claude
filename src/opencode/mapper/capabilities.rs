//! Typed model capability and alias policy used by the Anthropic -> OpenAI mapper.
//!
//! This module is deliberately descriptive rather than restrictive for unknown
//! models: generic OpenAI-compatible providers keep pass-through behavior, while
//! families with bridge-specific quirks opt into explicit policy here.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    /// The bridge has model-specific support/translation for this capability.
    Native,
    /// Preserve the request shape and let the upstream provider decide.
    Passthrough,
    /// The bridge must not send this capability in its native upstream form.
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenBehavior {
    Preserve,
    ReasoningStreamFloor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputBehavior {
    Passthrough,
    JsonObjectOnly,
    PromptSchema,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFamily {
    Generic,
    ReasoningHeavy,
    DeepSeekV4,
    DeepSeekV4FlashFree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModelCapabilities {
    pub family: ModelFamily,
    pub tools: CapabilitySupport,
    pub reasoning: CapabilitySupport,
    pub streaming: CapabilitySupport,
    pub images: CapabilitySupport,
    pub token_behavior: TokenBehavior,
    pub structured_output: StructuredOutputBehavior,
}

impl ModelCapabilities {
    pub fn reasoning_heavy(self) -> bool {
        self.token_behavior == TokenBehavior::ReasoningStreamFloor
    }
}

/// Resolve client-facing aliases into the namespace actually sent upstream.
///
/// Prefix stripping happens before alias rules so policy/allowlist checks and
/// the forwarder reason about exactly the same canonical identifier.
pub fn uses_opencode_model_aliases(base_url: &str) -> bool {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "opencode.ai" || host.ends_with(".opencode.ai"))
}

pub fn canonical_model_name(model: &str) -> String {
    let name = model.strip_prefix("opencode/").unwrap_or(model);
    match name {
        "deepseek-v4-flash" => "deepseek-v4-flash-free".to_string(),
        "nemotron-3-ultra" => "nemotron-3-ultra-free".to_string(),
        "x-preview" | "x-preview-f" | "ox-alpha" | "sonnet[1m]" => "x-preview-f-free".to_string(),
        _ if name.starts_with("claude-") => "x-preview-f-free".to_string(),
        _ => name.to_string(),
    }
}

pub fn model_capabilities(model: &str) -> ModelCapabilities {
    let name = canonical_model_name(model).to_ascii_lowercase();
    if name.contains("deepseek-v4-flash-free") {
        return ModelCapabilities {
            family: ModelFamily::DeepSeekV4FlashFree,
            tools: CapabilitySupport::Native,
            reasoning: CapabilitySupport::Native,
            streaming: CapabilitySupport::Native,
            images: CapabilitySupport::Passthrough,
            token_behavior: TokenBehavior::ReasoningStreamFloor,
            structured_output: StructuredOutputBehavior::PromptSchema,
        };
    }
    if name.contains("deepseek-v4-flash") || name.contains("deepseek-v4-pro") {
        return ModelCapabilities {
            family: ModelFamily::DeepSeekV4,
            tools: CapabilitySupport::Native,
            reasoning: CapabilitySupport::Native,
            streaming: CapabilitySupport::Native,
            images: CapabilitySupport::Passthrough,
            token_behavior: TokenBehavior::ReasoningStreamFloor,
            structured_output: StructuredOutputBehavior::JsonObjectOnly,
        };
    }
    if (name.contains("deepseek") && (name.contains("r1") || name.contains("reasoner")))
        || name.contains("reasoning")
        || name.contains("-r1")
    {
        return ModelCapabilities {
            family: ModelFamily::ReasoningHeavy,
            tools: CapabilitySupport::Passthrough,
            reasoning: CapabilitySupport::Passthrough,
            streaming: CapabilitySupport::Passthrough,
            images: CapabilitySupport::Passthrough,
            token_behavior: TokenBehavior::ReasoningStreamFloor,
            structured_output: StructuredOutputBehavior::Passthrough,
        };
    }
    ModelCapabilities {
        family: ModelFamily::Generic,
        tools: CapabilitySupport::Passthrough,
        reasoning: CapabilitySupport::Passthrough,
        streaming: CapabilitySupport::Passthrough,
        images: CapabilitySupport::Passthrough,
        token_behavior: TokenBehavior::Preserve,
        structured_output: StructuredOutputBehavior::Passthrough,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_before_capability_classification() {
        assert_eq!(
            canonical_model_name("opencode/deepseek-v4-flash"),
            "deepseek-v4-flash-free"
        );
        assert_eq!(canonical_model_name("claude-opus-5"), "x-preview-f-free");
        assert_eq!(canonical_model_name("opencode/gpt-4o"), "gpt-4o");
        assert_eq!(
            model_capabilities("deepseek-v4-flash").family,
            ModelFamily::DeepSeekV4FlashFree
        );
    }

    #[test]
    fn capability_profiles_preserve_existing_mapper_policy() {
        let free = model_capabilities("deepseek-v4-flash-free");
        assert_eq!(free.reasoning, CapabilitySupport::Native);
        assert_eq!(free.token_behavior, TokenBehavior::ReasoningStreamFloor);
        assert_eq!(
            free.structured_output,
            StructuredOutputBehavior::PromptSchema
        );

        let pro = model_capabilities("deepseek-v4-pro");
        assert_eq!(pro.family, ModelFamily::DeepSeekV4);
        assert_eq!(
            pro.structured_output,
            StructuredOutputBehavior::JsonObjectOnly
        );

        let generic = model_capabilities("gpt-4o");
        assert_eq!(generic.family, ModelFamily::Generic);
        assert_eq!(generic.token_behavior, TokenBehavior::Preserve);
        assert_eq!(generic.images, CapabilitySupport::Passthrough);
    }
}
