//! Provider-scoped model catalogs. Metadata is never keyed by model ID alone.

use super::types::{ContextClass, ModelInfo, ProviderId};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSource {
    Live,
    Configured,
}

#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    models: BTreeMap<(ProviderId, String), ModelInfo>,
    pub source: Option<CatalogSource>,
}

impl ModelCatalog {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn refresh(&mut self, models: impl IntoIterator<Item = ModelInfo>, source: CatalogSource) {
        self.models = models
            .into_iter()
            .map(|model| ((model.provider_id.clone(), model.model_id.clone()), model))
            .collect();
        self.source = Some(source);
    }
    pub fn get(&self, provider: impl AsRef<str>, model: impl AsRef<str>) -> Option<&ModelInfo> {
        self.models.get(&(
            ProviderId::from(provider.as_ref()),
            model.as_ref().to_string(),
        ))
    }
    pub fn models(&self) -> impl Iterator<Item = &ModelInfo> {
        self.models.values()
    }
    pub fn eligible_for_alias(&self, context_window: usize) -> Vec<&ModelInfo> {
        self.models
            .values()
            .filter(|model| {
                matches!(model.context_class(), ContextClass::OneMillion)
                    && model
                        .context_window
                        .is_some_and(|value| value >= context_window)
                    && model.verified_context
            })
            .collect()
    }
}
