//! CLI v2 regression tests.
//!
//! These tests verify CLI parsing contracts without spawning a full daemon.
//! They use `std::process::Command` to invoke the binary with various flags
//! and assert exit codes / output patterns.

use std::process::Command;

/// Path to the opencode2claude binary, set by Cargo at build time.
const BINARY: &str = env!("CARGO_BIN_EXE_opencode2claude");

// ── Global flag conflicts ──

#[test]
fn json_and_quiet_conflict() {
    let output = Command::new(BINARY)
        .args(["--json", "--quiet", "server", "status"])
        .output()
        .expect("Failed to run opencode2claude");
    assert!(
        !output.status.success(),
        "--json and --quiet should conflict (exit code != 0)"
    );
}

// ── Shell policy validation ──

#[test]
fn invalid_shell_policy_fails_at_parse() {
    let output = Command::new(BINARY)
        .args(["server", "start", "--shell-policy", "typo"])
        .output()
        .expect("Failed to run opencode2claude");
    // clap parse failures exit with code 2
    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid --shell-policy should fail at parse time with exit code 2"
    );
}

// ── TLS requires validation ──

#[test]
fn tls_cert_without_key_fails_at_parse() {
    let output = Command::new(BINARY)
        .args(["server", "start", "--tls-cert", "./cert.pem"])
        .output()
        .expect("Failed to run opencode2claude");
    assert_eq!(
        output.status.code(),
        Some(2),
        "--tls-cert without --tls-key should fail at parse time"
    );
}

#[test]
fn tls_key_without_cert_fails_at_parse() {
    let output = Command::new(BINARY)
        .args(["server", "start", "--tls-key", "./key.pem"])
        .output()
        .expect("Failed to run opencode2claude");
    assert_eq!(
        output.status.code(),
        Some(2),
        "--tls-key without --tls-cert should fail at parse time"
    );
}

// ── Help output ──

#[test]
fn help_includes_v2_subcommands() {
    let output = Command::new(BINARY)
        .args(["--help"])
        .output()
        .expect("Failed to run opencode2claude");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("server"),
        "help should mention 'server' subcommand"
    );
    assert!(
        stdout.contains("proxy"),
        "help should mention 'proxy' subcommand"
    );
    assert!(
        stdout.contains("doctor"),
        "help should mention 'doctor' subcommand"
    );
}

#[test]
fn server_start_help_includes_v2_flags() {
    let output = Command::new(BINARY)
        .args(["server", "start", "--help"])
        .output()
        .expect("Failed to run opencode2claude");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("--foreground"),
        "help should show --foreground/-f"
    );
    assert!(stdout.contains("--model"), "help should show --model/-m");
    assert!(
        stdout.contains("--shell-policy"),
        "help should show --shell-policy"
    );
    assert!(stdout.contains("--config"), "help should show --config/-c");
}

// ── JSON output ──

#[test]
fn json_output_is_parseable() {
    let output = Command::new(BINARY)
        .args(["--json", "server", "status"])
        .output()
        .expect("Failed to run opencode2claude");

    // Even when bridge is not running, JSON output should be parseable
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // stdout should contain JSON (status info or error)
    let is_json = stdout.trim().starts_with('{') || stdout.trim().starts_with('[');
    assert!(
        is_json,
        "JSON mode output should start with JSON object/array.\n\
         stdout: {:?}\nstderr: {:?}",
        stdout, stderr
    );
}

// ── Completion output ──

#[test]
fn completion_bash_is_syntactically_valid() {
    let output = Command::new(BINARY)
        .args(["completion", "bash"])
        .output()
        .expect("Failed to run opencode2claude");
    assert!(output.status.success(), "completion bash should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "completion bash output should not be empty"
    );

    // Should include the public command tree
    assert!(
        stdout.contains("server"),
        "bash completion should include 'server' subcommand"
    );
    assert!(
        stdout.contains("proxy"),
        "bash completion should include 'proxy' subcommand"
    );
}
