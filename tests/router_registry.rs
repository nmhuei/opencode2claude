use opencode2api::router::oauth::extract_cline_tokens_from_code;
use opencode2api::router::registry::ProviderRegistry;

#[test]
fn test_provider_registry_defaults() {
    let registry = ProviderRegistry::default();

    // OpenCode
    let opencode = registry.get_provider("opencode").expect("opencode provider");
    assert_eq!(opencode.default_base_url, "http://127.0.0.1:4096");

    // Cline
    let cline = registry.get_provider("cline").expect("cline provider");
    assert_eq!(cline.default_base_url, "https://api.cline.bot/api/v1");

    // DeepSeek
    let deepseek = registry.get_provider("deepseek").expect("deepseek provider");
    assert_eq!(deepseek.default_base_url, "https://api.deepseek.com/v1");
}

#[test]
fn test_cline_base64_code_decoding() {
    // Cline encodes JSON into base64 in the code parameter
    let payload = r#"{"accessToken":"token123","refreshToken":"refresh456","email":"test@example.com","expiresAt":"2026-09-09T00:00:00Z"}"#;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(payload);

    let result = extract_cline_tokens_from_code(&b64).expect("should decode token");
    assert_eq!(result.access_token, "token123");
    assert_eq!(result.refresh_token.as_deref(), Some("refresh456"));
    assert_eq!(result.email.as_deref(), Some("test@example.com"));
}
