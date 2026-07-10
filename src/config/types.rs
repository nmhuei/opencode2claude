//! Public configuration data structures.

use crate::shell::ShellPolicy;
use std::net::IpAddr;

#[derive(Debug, Default)]
pub struct CliOverrides {
    pub bridge_port: Option<u16>,
    pub host: Option<String>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub config_path: Option<String>,
    pub max_body_size: Option<usize>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub host: IpAddr,
    pub bridge_port: u16,
    pub opencode_port: u16,
    pub model: Option<String>,
    pub shell_policy: ShellPolicy,
    pub auth_tokens: Option<Vec<String>>,
    pub max_body_size: usize,
    pub stream_buffer_size: usize,
    pub channel_capacity: usize,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
    pub max_search_loops: u32,
    #[allow(dead_code)]
    pub proxies: Option<Vec<String>>,
    pub primary_proxies: Option<Vec<String>>,
    pub warm_standby_proxies: Option<Vec<String>>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".parse().expect("hardcoded loopback is valid"),
            bridge_port: 0,
            opencode_port: 0,
            model: None,
            shell_policy: ShellPolicy::Disabled,
            auth_tokens: None,
            max_body_size: super::DEFAULT_MAX_BODY_SIZE,
            stream_buffer_size: super::DEFAULT_STREAM_BUFFER_SIZE,
            channel_capacity: super::DEFAULT_CHANNEL_CAPACITY,
            tavily_api_key: None,
            exa_api_key: None,
            serper_api_key: None,
            searxng_url: None,
            searxng_api_key: None,
            max_search_loops: 10,
            proxies: None,
            primary_proxies: None,
            warm_standby_proxies: None,
        }
    }
}
