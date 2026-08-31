//! CLI-to-server argument bridge.

#[derive(Default, Debug, Clone)]
pub struct ServeArgsBridge {
    pub port: Option<u16>,
    pub host: Option<String>,
    pub config: Option<String>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub max_body_size: Option<usize>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
    pub egress_mode: Option<String>,
    pub upstream_base_url: Option<String>,
    pub upstream_api_key: Option<String>,
}

impl From<ServeArgsBridge> for crate::config::CliOverrides {
    fn from(args: ServeArgsBridge) -> Self {
        let clear_upstream_api_key =
            args.upstream_base_url.is_some() && args.upstream_api_key.is_none();
        Self {
            bridge_port: args.port,
            host: args.host,
            model: args.model,
            shell_policy: args.shell_policy,
            config_path: args.config,
            max_body_size: args.max_body_size,
            tavily_api_key: args.tavily_api_key,
            exa_api_key: args.exa_api_key,
            serper_api_key: args.serper_api_key,
            searxng_url: args.searxng_url,
            searxng_api_key: args.searxng_api_key,
            egress_mode: args.egress_mode,
            upstream_base_url: args.upstream_base_url,
            upstream_api_key: args.upstream_api_key,
            clear_upstream_api_key,
        }
    }
}
