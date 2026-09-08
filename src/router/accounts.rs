use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub priority: u32,
    pub is_active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountSelectionStrategy {
    FillFirst,
    StickyRoundRobin { limit: usize },
}

impl Default for AccountSelectionStrategy {
    fn default() -> Self {
        Self::FillFirst
    }
}

/// Tracks per-model cooldowns: `(account_id, model_id) -> Instant`
#[derive(Debug, Default, Clone)]
pub struct ModelLockTracker {
    locks: Arc<Mutex<HashMap<(String, String), Instant>>>,
}

impl ModelLockTracker {
    pub fn new() -> Self {
        Self {
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn lock_model(&self, account_id: &str, model: &str, cooldown: Duration) {
        let mut locks = self.locks.lock().unwrap();
        let until = Instant::now() + cooldown;
        locks.insert((account_id.to_string(), model.to_string()), until);
    }

    pub fn is_model_locked(&self, account_id: &str, model: &str) -> bool {
        let mut locks = self.locks.lock().unwrap();
        let key = (account_id.to_string(), model.to_string());
        if let Some(&until) = locks.get(&key) {
            if Instant::now() < until {
                return true;
            } else {
                locks.remove(&key);
            }
        }
        false
    }

    pub fn clear_model_lock(&self, account_id: &str, model: &str) {
        let mut locks = self.locks.lock().unwrap();
        locks.remove(&(account_id.to_string(), model.to_string()));
    }
}

#[derive(Debug, Clone)]
pub struct AccountPool {
    accounts: Vec<Account>,
    strategy: AccountSelectionStrategy,
    lock_tracker: ModelLockTracker,
    usage_counts: Arc<Mutex<HashMap<String, usize>>>,
    current_index: Arc<Mutex<usize>>,
}

impl AccountPool {
    pub fn new(strategy: AccountSelectionStrategy) -> Self {
        Self {
            accounts: Vec::new(),
            strategy,
            lock_tracker: ModelLockTracker::new(),
            usage_counts: Arc::new(Mutex::new(HashMap::new())),
            current_index: Arc::new(Mutex::new(0)),
        }
    }

    pub fn add_account(&mut self, account: Account) {
        // Keep sorted by priority ascending (1 = highest priority)
        self.accounts.retain(|a| a.id != account.id);
        self.accounts.push(account);
        self.accounts.sort_by_key(|a| a.priority);
    }

    pub fn remove_account(&mut self, id: &str) -> bool {
        let initial_len = self.accounts.len();
        self.accounts.retain(|a| a.id != id);
        self.accounts.len() < initial_len
    }

    pub fn get_accounts(&self) -> &[Account] {
        &self.accounts
    }

    pub fn lock_tracker(&self) -> &ModelLockTracker {
        &self.lock_tracker
    }

    pub fn select_account(
        &self,
        provider: &str,
        model: &str,
        exclude_ids: &[String],
    ) -> Option<Account> {
        let available: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|a| {
                a.is_active
                    && a.provider.eq_ignore_ascii_case(provider)
                    && !exclude_ids.contains(&a.id)
                    && !self.lock_tracker.is_model_locked(&a.id, model)
            })
            .collect();

        if available.is_empty() {
            return None;
        }

        match self.strategy {
            AccountSelectionStrategy::FillFirst => available.first().copied().cloned(),
            AccountSelectionStrategy::StickyRoundRobin { limit } => {
                let mut idx_lock = self.current_index.lock().unwrap();
                let mut usage_lock = self.usage_counts.lock().unwrap();

                let cur_idx = *idx_lock % available.len();
                let candidate = available[cur_idx];
                let count = usage_lock.get(&candidate.id).copied().unwrap_or(0);

                if count >= limit {
                    // Reset count and advance to next available account
                    usage_lock.insert(candidate.id.clone(), 0);
                    let next_idx = (cur_idx + 1) % available.len();
                    *idx_lock = next_idx;
                    Some(available[next_idx].clone())
                } else {
                    Some(candidate.clone())
                }
            }
        }
    }

    pub fn record_usage(&self, account_id: &str) {
        let mut usage = self.usage_counts.lock().unwrap();
        *usage.entry(account_id.to_string()).or_insert(0) += 1;
    }

    pub fn default_storage_path() -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home)
            .join(".config")
            .join("opencode2api")
            .join("accounts.json")
    }

    pub fn load_from_file(path: &std::path::Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read accounts file: {e}"))?;
        let accounts: Vec<Account> = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse accounts JSON: {e}"))?;
        let mut pool = Self::new(AccountSelectionStrategy::FillFirst);
        for acc in accounts {
            pool.add_account(acc);
        }
        Ok(pool)
    }

    pub fn save_to_file(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(&self.accounts)
            .map_err(|e| format!("Failed to serialize accounts: {e}"))?;
        std::fs::write(path, json).map_err(|e| format!("Failed to write accounts file: {e}"))
    }

    pub fn load_or_init() -> Self {
        let path = Self::default_storage_path();
        if path.exists() {
            if let Ok(pool) = Self::load_from_file(&path) {
                return pool;
            }
        }

        let mut pool = Self::new(AccountSelectionStrategy::FillFirst);

        // Auto-import cline account if local cline is authenticated
        if let Ok(token) = crate::application::cline::find_cline_token() {
            pool.add_account(Account {
                id: "cline-local".to_string(),
                name: "Cline Local Account".to_string(),
                provider: "cline".to_string(),
                api_key: None,
                access_token: Some(token),
                refresh_token: None,
                email: None,
                expires_at: None,
                priority: 1,
                is_active: true,
            });
        }

        // Auto-import OpenCode local server account
        pool.add_account(Account {
            id: "opencode-local".to_string(),
            name: "OpenCode Local Daemon".to_string(),
            provider: "opencode".to_string(),
            api_key: None,
            access_token: None,
            refresh_token: None,
            email: None,
            expires_at: None,
            priority: 1,
            is_active: true,
        });

        // Best-effort save initial imported accounts
        let _ = pool.save_to_file(&path);
        pool
    }
}
