use opencode2api::router::accounts::{
    Account, AccountPool, AccountSelectionStrategy,
};
use std::time::Duration;

#[test]
fn test_account_creation_and_filtering() {
    let mut pool = AccountPool::new(AccountSelectionStrategy::FillFirst);
    let acc1 = Account {
        id: "cline-1".to_string(),
        name: "Cline Primary".to_string(),
        provider: "cline".to_string(),
        api_key: None,
        access_token: Some("token1".to_string()),
        refresh_token: None,
        email: Some("user1@example.com".to_string()),
        expires_at: None,
        priority: 1,
        is_active: true,
    };
    let acc2 = Account {
        id: "cline-2".to_string(),
        name: "Cline Backup".to_string(),
        provider: "cline".to_string(),
        api_key: None,
        access_token: Some("token2".to_string()),
        refresh_token: None,
        email: Some("user2@example.com".to_string()),
        expires_at: None,
        priority: 2,
        is_active: true,
    };

    pool.add_account(acc1.clone());
    pool.add_account(acc2.clone());

    // Initially, acc1 (priority 1) should be selected
    let selected = pool.select_account("cline", "z-ai/glm-5.3-flash", &[]);
    assert_eq!(selected.unwrap().id, "cline-1");

    // If cline-1 is excluded, cline-2 should be selected
    let selected2 = pool.select_account("cline", "z-ai/glm-5.3-flash", &["cline-1".to_string()]);
    assert_eq!(selected2.unwrap().id, "cline-2");
}

#[test]
fn test_per_model_lock_isolation() {
    let mut pool = AccountPool::new(AccountSelectionStrategy::FillFirst);
    let acc1 = Account {
        id: "cline-1".to_string(),
        name: "Cline Primary".to_string(),
        provider: "cline".to_string(),
        api_key: None,
        access_token: Some("token1".to_string()),
        refresh_token: None,
        email: None,
        expires_at: None,
        priority: 1,
        is_active: true,
    };
    let acc2 = Account {
        id: "cline-2".to_string(),
        name: "Cline Backup".to_string(),
        provider: "cline".to_string(),
        api_key: None,
        access_token: Some("token2".to_string()),
        refresh_token: None,
        email: None,
        expires_at: None,
        priority: 2,
        is_active: true,
    };

    pool.add_account(acc1);
    pool.add_account(acc2);

    // Lock model A on account 1
    pool.lock_tracker()
        .lock_model("cline-1", "z-ai/glm-5.3-flash", Duration::from_secs(60));

    // For model A: cline-1 is locked, so cline-2 should be selected
    let sel_model_a = pool.select_account("cline", "z-ai/glm-5.3-flash", &[]);
    assert_eq!(sel_model_a.unwrap().id, "cline-2");

    // For model B: cline-1 is NOT locked! (Per-model lock isolation)
    let sel_model_b = pool.select_account("cline", "deepseek-chat", &[]);
    assert_eq!(sel_model_b.unwrap().id, "cline-1");
}

#[test]
fn test_sticky_round_robin_rotation() {
    let mut pool = AccountPool::new(AccountSelectionStrategy::StickyRoundRobin { limit: 2 });
    let acc1 = Account {
        id: "acc-1".to_string(),
        name: "A1".to_string(),
        provider: "cline".to_string(),
        api_key: None,
        access_token: Some("t1".to_string()),
        refresh_token: None,
        email: None,
        expires_at: None,
        priority: 1,
        is_active: true,
    };
    let acc2 = Account {
        id: "acc-2".to_string(),
        name: "A2".to_string(),
        provider: "cline".to_string(),
        api_key: None,
        access_token: Some("t2".to_string()),
        refresh_token: None,
        email: None,
        expires_at: None,
        priority: 2,
        is_active: true,
    };

    pool.add_account(acc1);
    pool.add_account(acc2);

    // Call 1: acc-1
    let c1 = pool.select_account("cline", "glm", &[]).unwrap();
    assert_eq!(c1.id, "acc-1");
    pool.record_usage(&c1.id);

    // Call 2: still acc-1 (limit is 2)
    let c2 = pool.select_account("cline", "glm", &[]).unwrap();
    assert_eq!(c2.id, "acc-1");
    pool.record_usage(&c2.id);

    // Call 3: reaches limit -> switches to acc-2!
    let c3 = pool.select_account("cline", "glm", &[]).unwrap();
    assert_eq!(c3.id, "acc-2");
}
