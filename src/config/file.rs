//! TOML file schema and parsing.

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct TomlConfig {
    pub port: Option<u16>,
    pub host: Option<String>,
    pub opencode_port: Option<u16>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub shell_allowlist: Option<String>,
    pub auth_tokens: Option<String>,
    pub max_body_size: Option<usize>,
    pub stream_buffer_size: Option<usize>,
    pub channel_capacity: Option<usize>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
    pub max_search_loops: Option<u32>,
    pub proxies: Option<Vec<String>>,
}

impl TomlConfig {
    pub fn from_file(path: &str) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        toml::from_str(&content).ok()
    }
}
