//! Tests for the api_key module (moved verbatim from the original single-file layout).

use super::*;
use crate::config::{BridgeConfig, ManagementConfig, TomlConfig};
use crate::infrastructure::file_store::AtomicFileStore;
use std::fs;
use std::path::PathBuf;

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "opencode2api-api-key-{name}-{}-{}",
        std::process::id(),
        unix_timestamp()
    ))
}

#[test]
fn generated_keys_have_prefix_entropy_and_are_distinct() {
    let keys = generate_api_keys(2, 32, "sk-oc2-").unwrap();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|key| key.starts_with("sk-oc2-")));
    assert!(keys.iter().all(|key| key.len() == 7 + 64));
    assert_ne!(keys[0], keys[1]);
}

#[test]
fn merge_preserves_existing_tokens_and_comments() {
    let existing = "# retained comment\nschema_version = 1\nauth_tokens = [\"old\"]\n";
    let merged = merge_auth_tokens(existing, &["new".to_string()], false).unwrap();
    assert!(merged.contains("# retained comment"));
    let parsed: TomlConfig = toml::from_str(&merged).unwrap();
    assert_eq!(
        parsed.auth_tokens.unwrap().into_vec(),
        vec!["old".to_string(), "new".to_string()]
    );
}

#[test]
fn replace_discards_existing_tokens() {
    let merged = merge_auth_tokens(
        "schema_version = 1\nauth_tokens = \"old-a,old-b\"\n",
        &["new".to_string()],
        true,
    )
    .unwrap();
    let parsed: TomlConfig = toml::from_str(&merged).unwrap();
    assert_eq!(parsed.auth_tokens.unwrap().into_vec(), vec!["new"]);
}

#[test]
fn revoke_preserves_comments_and_removes_selected_index() {
    let existing = "# keep me\nschema_version = 1\nauth_tokens = [\"one\", \"two\", \"three\"]\n";
    let updated = remove_auth_tokens(existing, &[1]).unwrap();
    assert!(updated.contains("# keep me"));
    let parsed: TomlConfig = toml::from_str(&updated).unwrap();
    assert_eq!(
        parsed.auth_tokens.unwrap().into_vec(),
        vec!["one".to_string(), "three".to_string()]
    );
}

#[test]
fn inventory_is_secret_safe_and_marks_active_key() {
    let tokens = vec![
        "sk-oc2-111111111111111111111111".to_string(),
        "sk-oc2-222222222222222222222222".to_string(),
    ];
    let inventory = key_inventory(&tokens, Some(&tokens[1]));
    assert_eq!(inventory.len(), 2);
    assert!(!inventory[0]
        .fingerprint
        .contains("111111111111111111111111"));
    assert!(!inventory[0].active);
    assert!(inventory[1].active);
}

#[tokio::test]
async fn managed_key_is_hashed_persisted_and_hot_authenticates() {
    let root = temp_path("registry");
    fs::create_dir_all(&root).unwrap();
    let config_path = root.join("config.toml");
    let config = BridgeConfig {
        auth_tokens: None,
        management: ManagementConfig {
            config_path: config_path.clone(),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    let store = AtomicFileStore;
    let mut registry = ApiKeyRegistry::load(&config, &store).unwrap();
    let (view, secret) = registry
        .create(
            "Mobile App".to_string(),
            None,
            "production".to_string(),
            None,
            ApiKeyPolicy::default(),
            16,
        )
        .unwrap();
    registry.persist(&store).unwrap();
    let persisted = fs::read_to_string(registry.path()).unwrap();
    assert!(!persisted.contains(&secret));
    assert!(persisted.contains(&view.id));
    let admission = registry
        .match_secret(&secret, "/v1/messages")
        .unwrap()
        .admit()
        .await
        .unwrap();
    assert_eq!(admission.client.key_id, view.id);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn rotate_preserves_runtime_usage_counters() {
    let root = temp_path("rotate-usage");
    fs::create_dir_all(&root).unwrap();
    let config_path = root.join("config.toml");
    let config = BridgeConfig {
        auth_tokens: None,
        management: ManagementConfig {
            config_path: config_path.clone(),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    let store = AtomicFileStore;
    let mut registry = ApiKeyRegistry::load(&config, &store).unwrap();
    let (view, secret) = registry
        .create(
            "Rotate Usage".to_string(),
            None,
            "production".to_string(),
            None,
            ApiKeyPolicy::default(),
            16,
        )
        .unwrap();
    for _ in 0..3 {
        registry
            .match_secret(&secret, "/v1/messages")
            .unwrap()
            .admit()
            .await
            .unwrap();
    }
    let before = registry.get(&view.id).expect("key exists");
    assert_eq!(before.usage.requests, 3);
    let (rotated, _replacement) = registry.rotate(&view.id, 16).unwrap();
    assert_eq!(
        rotated.usage.requests, 3,
        "rotation keeps lifetime requests"
    );
    assert_eq!(
        rotated.usage.minute_requests, 3,
        "rotation keeps minute window"
    );
    assert_eq!(
        rotated.usage.daily_requests, 3,
        "rotation keeps daily quota progress"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn empty_legacy_tokens_are_never_imported_or_authenticatable() {
    let root = temp_path("empty-legacy");
    fs::create_dir_all(&root).unwrap();
    let config_path = root.join("config.toml");
    let config = BridgeConfig {
        auth_tokens: Some(vec![
            crate::config::SecretString::from(""),
            crate::config::SecretString::from("   "),
            crate::config::SecretString::from("sk-oc2-real-legacy-token"),
        ]),
        management: ManagementConfig {
            config_path: config_path.clone(),
            ..BridgeConfig::default().management
        },
        ..Default::default()
    };
    let store = AtomicFileStore;
    let registry = ApiKeyRegistry::load(&config, &store).unwrap();
    assert!(
        registry.match_secret("", "/v1/messages").is_err(),
        "empty token must never authenticate"
    );
    assert!(
        registry.match_secret("   ", "/v1/messages").is_err(),
        "whitespace-only token must never authenticate"
    );
    registry
        .match_secret("sk-oc2-real-legacy-token", "/v1/messages")
        .expect("non-empty legacy tokens still import and authenticate");
    assert_eq!(
        registry.list().len(),
        1,
        "only the non-empty legacy token becomes a record"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn policy_rejects_disallowed_model_and_clamps_when_configured() {
    let mut policy = ApiKeyPolicy {
        allowed_models: vec!["opencode/allowed".to_string()],
        max_output_tokens: Some(4096),
        limit_action: LimitAction::Clamp,
        ..Default::default()
    };
    policy.normalize();
    assert!(policy
        .resolve_model(Some("opencode/blocked"), None, "fallback")
        .is_err());
    assert_eq!(
        policy.enforce_output_tokens(Some(9000)).unwrap(),
        Some(4096)
    );
}

// --- Allowlist namespace (resolved-id semantics) -----------------------
//
// Allowlist matching must happen in the same namespace the forwarder
// ultimately speaks: both the selected name and every allowlist entry go
// through `map_model_name` before comparison. These tests pin that
// contract at the policy layer; the OpenAI-entry end-to-end counterpart
// lives next to `handle_chat_completions_inner`.

fn allowlist_policy(entries: &[&str]) -> ApiKeyPolicy {
    let mut policy = ApiKeyPolicy {
        allowed_models: entries.iter().map(|value| (*value).to_string()).collect(),
        ..Default::default()
    };
    policy.normalize();
    policy
}

#[test]
fn allowlisted_resolved_id_accepts_wire_name_request() {
    // The sibling-audit scenario: the key allows the RESOLVED id, the
    // client sends a WIRE name that normalizes exactly onto it. This must
    // be accepted, never rejected.
    let policy = allowlist_policy(&["deepseek-v4-flash-free"]);
    policy
        .resolve_model(Some("deepseek-v4-flash"), None, "fallback")
        .expect("wire name resolving onto the allowlisted id must be accepted");
    policy
        .resolve_model(Some("opencode/deepseek-v4-flash"), None, "fallback")
        .expect("prefixed spelling of the same resolved class must be accepted");
}

#[test]
fn allowlist_entry_in_wire_form_matches_its_resolved_class() {
    // Inverse-direction pin: an entry written in wire form keeps working
    // and admits every spelling in the same resolved class. Normalize-
    // both can widen within a class but can never newly reject —
    // identical strings always normalize identically.
    let policy = allowlist_policy(&["deepseek-v4-flash"]);
    policy
        .resolve_model(Some("deepseek-v4-flash"), None, "fallback")
        .expect("identical strings stay admitted");
    policy
        .resolve_model(Some("deepseek-v4-flash-free"), None, "fallback")
        .expect("resolved id of the entry's class must be admitted");
    policy
        .resolve_model(Some("opencode/deepseek-v4-flash"), None, "fallback")
        .expect("prefixed spelling of the same class must be admitted");
}

#[test]
fn claude_alias_family_resolves_into_allowlisted_preview_id() {
    // Every claude-* alias collapses onto x-preview-f-free, so an
    // intuitive "allow the preview model" list admits them all.
    let policy = allowlist_policy(&["x-preview-f-free"]);
    for wire in [
        "claude-opus-5",
        "claude-3-5-sonnet",
        "sonnet[1m]",
        "ox-alpha",
        "x-preview",
        "x-preview-f",
    ] {
        policy
            .resolve_model(Some(wire), None, "fallback")
            .unwrap_or_else(|_| panic!("alias {wire} must resolve into the allowed class"));
    }
    // ...while a genuinely different model stays rejected, with the
    // client-sent name carried in the error for debuggability.
    assert!(matches!(
        policy.resolve_model(Some("gpt-4o"), None, "fallback"),
        Err(ApiKeyPolicyError::ModelNotAllowed(name)) if name == "gpt-4o",
    ));
}

#[test]
fn fallback_constant_is_judged_in_resolved_namespace() {
    // With no requested/default/global model, selection falls back to
    // DEFAULT_MODEL ("claude-3-5-sonnet" — itself a wire name). A key
    // allowing the id it resolves onto must admit the modelless request.
    let policy = allowlist_policy(&["x-preview-f-free"]);
    policy
        .resolve_model(None, None, crate::config::DEFAULT_MODEL)
        .expect("the DEFAULT_MODEL fallback resolves onto the allowed class");
}

#[test]
fn empty_allowlist_and_selection_precedence_stay_unchanged() {
    // Empty/unset allowlist = every model allowed (wire names included).
    let open_policy = ApiKeyPolicy::default();
    open_policy
        .resolve_model(Some("gpt-4o"), None, crate::config::DEFAULT_MODEL)
        .expect("empty allowlist must remain permissive");

    // Configured-key default beats the request when overrides are off;
    // the winning name is judged in the resolved namespace.
    let pinned = allowlist_policy(&["nemotron-3-ultra-free"]);
    let pinned = ApiKeyPolicy {
        default_model: Some("opencode/nemotron-3-ultra-free".to_string()),
        allow_model_override: false,
        ..pinned
    };
    pinned
        .resolve_model(Some("gpt-4o"), None, crate::config::DEFAULT_MODEL)
        .expect("override-disabled selection uses the key default, not the request");

    // Overrides on: the requested name wins and is still enforced.
    let overridden = ApiKeyPolicy {
        default_model: Some("opencode/nemotron-3-ultra-free".to_string()),
        allow_model_override: true,
        ..pinned
    };
    assert!(matches!(
        overridden.resolve_model(Some("gpt-4o"), None, crate::config::DEFAULT_MODEL),
        Err(ApiKeyPolicyError::ModelNotAllowed(_))
    ));
}

#[test]
fn both_protocol_entries_render_identical_verdicts() {
    // Parity pin: /v1/messages (handlers/messages.rs) and
    // /v1/chat/completions (handlers/openai.rs) derive their argument in
    // protocol-specific shapes but delegate to this very function, so the
    // verdicts must agree byte-for-byte. The extraction expressions below
    // mirror each call site literally.
    let policy = allowlist_policy(&["opencode/deepseek-v4-flash-free"]);

    // Anthropic shape: `payload.model.as_deref()` over Option<String>.
    let anthropic_payload_model: Option<String> = Some("deepseek-v4-flash".to_string());
    let anthropic = policy.resolve_model(
        anthropic_payload_model.as_deref(),
        None,
        crate::config::DEFAULT_MODEL,
    );

    // OpenAI shape: trimmed-empty collapses to None over String.
    let openai_payload_model = "deepseek-v4-flash".to_string();
    let openai = policy.resolve_model(
        (!openai_payload_model.trim().is_empty()).then_some(openai_payload_model.as_str()),
        None,
        crate::config::DEFAULT_MODEL,
    );

    assert!(
        anthropic.is_ok(),
        "wire name must clear the resolved-id allowlist"
    );
    assert_eq!(
        anthropic, openai,
        "both entries must agree on the identical request"
    );
}
