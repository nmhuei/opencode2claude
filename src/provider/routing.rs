use super::registry::{ProviderRegistry, RegistryError};
use super::types::{AttemptTarget, ProviderRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    RetrySameTarget,
    NextTarget,
    QuarantineCredential,
    QuarantineProvider,
    Fail,
}

#[derive(Debug, Clone)]
pub struct RoutePlanner<'a> {
    registry: &'a ProviderRegistry,
}

impl<'a> RoutePlanner<'a> {
    pub fn new(registry: &'a ProviderRegistry) -> Self {
        Self { registry }
    }
    pub fn plan(
        &self,
        request: &ProviderRequest,
        alias: impl AsRef<str>,
    ) -> Result<Vec<AttemptTarget>, RegistryError> {
        let targets = self.registry.resolve_alias(alias)?;
        let required = request.max_output_tokens.unwrap_or(0);
        Ok(targets
            .into_iter()
            .filter(|target| target.context_window >= required)
            .collect())
    }
    pub fn plan_for_context(
        &self,
        alias: impl AsRef<str>,
        required_context: usize,
    ) -> Result<Vec<AttemptTarget>, RegistryError> {
        Ok(self
            .registry
            .resolve_alias(alias)?
            .into_iter()
            .filter(|target| target.context_window >= required_context)
            .collect())
    }
}
