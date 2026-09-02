use clap::Parser;
use opencode2api::cli::{Cli, Command};

#[test]
fn global_config_parses_before_or_after_a_command() {
    let before = Cli::try_parse_from([
        "opencode2api",
        "--config",
        "custom.toml",
        "provider",
        "list",
    ])
    .unwrap();
    let after = Cli::try_parse_from([
        "opencode2api",
        "provider",
        "list",
        "--config",
        "custom.toml",
    ])
    .unwrap();
    assert_eq!(before.config.as_deref(), Some("custom.toml"));
    assert_eq!(after.config.as_deref(), Some("custom.toml"));
}

#[test]
fn modern_provider_nouns_are_top_level_commands() {
    for argv in [
        &["opencode2api", "credential", "list"][..],
        &["opencode2api", "alias", "list"][..],
        &["opencode2api", "route", "explain", "free-1m"][..],
        &["opencode2api", "config", "show", "--effective"][..],
    ] {
        assert!(
            Cli::try_parse_from(argv).is_ok(),
            "failed to parse {argv:?}"
        );
    }
    let parsed = Cli::try_parse_from(["opencode2api", "route", "explain", "free-1m"]).unwrap();
    assert!(matches!(parsed.command, Some(Command::Route(_))));
}

#[test]
fn provider_management_supports_enable_disable_test_and_alias_use() {
    for argv in [
        &["opencode2api", "provider", "enable", "bai"][..],
        &["opencode2api", "provider", "disable", "bai"][..],
        &["opencode2api", "provider", "show", "bai"][..],
        &["opencode2api", "provider", "test", "bai"][..],
        &["opencode2api", "credential", "test", "bai", "main"][..],
        &["opencode2api", "alias", "use", "free-1m"][..],
        &["opencode2api", "model", "discover", "bai"][..],
        &["opencode2api", "model", "verify", "bai", "deepseek-1m"][..],
    ] {
        assert!(
            Cli::try_parse_from(argv).is_ok(),
            "failed to parse {argv:?}"
        );
    }
}

#[test]
fn json_and_quiet_remain_mutually_exclusive() {
    assert!(
        Cli::try_parse_from(["opencode2api", "--json", "--quiet", "provider", "list",]).is_err()
    );
}
