use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub default_base_url: &'static str,
    pub auth_header: &'static str,
    pub auth_scheme: &'static str,
    pub has_oauth: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    providers: HashMap<String, ProviderDescriptor>,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        let mut registry = Self {
            providers: HashMap::new(),
        };

        registry.register(ProviderDescriptor {
            id: "opencode",
            name: "OpenCode Local",
            default_base_url: "http://127.0.0.1:4096",
            auth_header: "Authorization",
            auth_scheme: "Bearer",
            has_oauth: false,
        });

        registry.register(ProviderDescriptor {
            id: "cline",
            name: "Cline",
            default_base_url: "https://api.cline.bot/api/v1",
            auth_header: "Authorization",
            auth_scheme: "Bearer",
            has_oauth: true,
        });

        registry.register(ProviderDescriptor {
            id: "deepseek",
            name: "DeepSeek",
            default_base_url: "https://api.deepseek.com/v1",
            auth_header: "Authorization",
            auth_scheme: "Bearer",
            has_oauth: false,
        });

        registry.register(ProviderDescriptor {
            id: "google",
            name: "Google Gemini",
            default_base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
            auth_header: "Authorization",
            auth_scheme: "Bearer",
            has_oauth: true,
        });

        registry.register(ProviderDescriptor {
            id: "openai",
            name: "OpenAI",
            default_base_url: "https://api.openai.com/v1",
            auth_header: "Authorization",
            auth_scheme: "Bearer",
            has_oauth: true,
        });

        registry.register(ProviderDescriptor {
            id: "openrouter",
            name: "OpenRouter",
            default_base_url: "https://openrouter.ai/api/v1",
            auth_header: "Authorization",
            auth_scheme: "Bearer",
            has_oauth: false,
        });

        registry.register(ProviderDescriptor {
            id: "groq",
            name: "Groq",
            default_base_url: "https://api.groq.com/openai/v1",
            auth_header: "Authorization",
            auth_scheme: "Bearer",
            has_oauth: false,
        });

        registry.register(ProviderDescriptor {
            id: "ollama",
            name: "Ollama Local",
            default_base_url: "http://127.0.0.1:11434/v1",
            auth_header: "Authorization",
            auth_scheme: "Bearer",
            has_oauth: false,
        });

        registry
    }
}

impl ProviderRegistry {
    pub fn register(&mut self, descriptor: ProviderDescriptor) {
        self.providers.insert(descriptor.id.to_string(), descriptor);
    }

    pub fn get_provider(&self, id: &str) -> Option<&ProviderDescriptor> {
        self.providers.get(&id.to_lowercase())
    }

    pub fn get(&self, id: &str) -> Option<&ProviderDescriptor> {
        self.get_provider(id)
    }

    pub fn list_providers(&self) -> Vec<&ProviderDescriptor> {
        let mut list: Vec<&ProviderDescriptor> = self.providers.values().collect();
        list.sort_by_key(|p| p.id);
        list
    }
}
