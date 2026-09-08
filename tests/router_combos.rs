use opencode2api::router::combos::{ComboResolver, ComboStrategy, ModelCombo};

#[test]
fn test_combo_registration_and_fallback_resolution() {
    let mut resolver = ComboResolver::new();
    let combo = ModelCombo {
        name: "dev-combo".to_string(),
        models: vec![
            "cline/z-ai/glm-5.3-flash".to_string(),
            "deepseek/deepseek-chat".to_string(),
            "opencode/free".to_string(),
        ],
        strategy: ComboStrategy::Fallback,
    };
    resolver.register_combo(combo);

    // Resolving combo returns all models in sequential order
    let resolved = resolver.resolve("dev-combo");
    assert_eq!(resolved.len(), 3);
    assert_eq!(resolved[0].provider, "cline");
    assert_eq!(resolved[0].model, "z-ai/glm-5.3-flash");
    assert_eq!(resolved[1].provider, "deepseek");
    assert_eq!(resolved[1].model, "deepseek-chat");
    assert_eq!(resolved[2].provider, "opencode");
    assert_eq!(resolved[2].model, "free");
}

#[test]
fn test_single_model_resolution() {
    let resolver = ComboResolver::new();

    // Model with provider prefix
    let resolved = resolver.resolve("cline/z-ai/glm-5.3-flash");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].provider, "cline");
    assert_eq!(resolved[0].model, "z-ai/glm-5.3-flash");

    // Model without prefix (standalone)
    let resolved2 = resolver.resolve("claude-3-5-sonnet");
    assert_eq!(resolved2.len(), 1);
    assert_eq!(resolved2[0].model, "claude-3-5-sonnet");
}

#[test]
fn test_round_robin_combo_rotation() {
    let mut resolver = ComboResolver::new();
    let combo = ModelCombo {
        name: "rr-combo".to_string(),
        models: vec![
            "cline/m1".to_string(),
            "cline/m2".to_string(),
            "cline/m3".to_string(),
        ],
        strategy: ComboStrategy::RoundRobin,
    };
    resolver.register_combo(combo);

    // First call: starts at m1
    let r1 = resolver.resolve("rr-combo");
    assert_eq!(r1[0].model, "m1");

    // Second call: starts at m2
    let r2 = resolver.resolve("rr-combo");
    assert_eq!(r2[0].model, "m2");

    // Third call: starts at m3
    let r3 = resolver.resolve("rr-combo");
    assert_eq!(r3[0].model, "m3");

    // Fourth call: wraps around to m1
    let r4 = resolver.resolve("rr-combo");
    assert_eq!(r4[0].model, "m1");
}
