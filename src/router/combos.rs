use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComboStrategy {
    Fallback,
    RoundRobin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCombo {
    pub name: String,
    pub models: Vec<String>,
    pub strategy: ComboStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetModel {
    pub provider: String,
    pub model: String,
}

impl TargetModel {
    pub fn parse(raw: &str) -> Self {
        let clean = raw.trim();
        if let Some((provider, model)) = clean.split_once('/') {
            Self {
                provider: provider.trim().to_string(),
                model: model.trim().to_string(),
            }
        } else {
            Self {
                provider: String::new(),
                model: clean.to_string(),
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ComboResolver {
    combos: Arc<Mutex<HashMap<String, ModelCombo>>>,
    rotation_indices: Arc<Mutex<HashMap<String, usize>>>,
}

impl ComboResolver {
    pub fn new() -> Self {
        Self {
            combos: Arc::new(Mutex::new(HashMap::new())),
            rotation_indices: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn register_combo(&mut self, combo: ModelCombo) {
        let mut map = self.combos.lock().unwrap();
        map.insert(combo.name.clone(), combo);
    }

    pub fn get_combo(&self, name: &str) -> Option<ModelCombo> {
        let map = self.combos.lock().unwrap();
        map.get(name).cloned()
    }

    pub fn resolve(&self, model_or_combo: &str) -> Vec<TargetModel> {
        let map = self.combos.lock().unwrap();
        if let Some(combo) = map.get(model_or_combo) {
            if combo.models.is_empty() {
                return vec![TargetModel::parse(model_or_combo)];
            }
            match combo.strategy {
                ComboStrategy::Fallback => combo
                    .models
                    .iter()
                    .map(|m| TargetModel::parse(m))
                    .collect(),
                ComboStrategy::RoundRobin => {
                    let mut indices = self.rotation_indices.lock().unwrap();
                    let cur_idx = indices.get(&combo.name).copied().unwrap_or(0);
                    let mut rotated = Vec::with_capacity(combo.models.len());
                    for i in 0..combo.models.len() {
                        let idx = (cur_idx + i) % combo.models.len();
                        rotated.push(TargetModel::parse(&combo.models[idx]));
                    }
                    // Update rotation index for next call
                    indices.insert(combo.name.clone(), (cur_idx + 1) % combo.models.len());
                    rotated
                }
            }
        } else {
            vec![TargetModel::parse(model_or_combo)]
        }
    }
}
