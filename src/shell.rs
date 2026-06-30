//! Shell command execution with configurable safety policy.
//!
//! The bridge can intercept prompts starting with `!` and execute them
//! directly on the local machine, bypassing the LLM entirely.
//! This module handles both the security policy and execution.

use std::collections::HashSet;

/// Defines how the bridge handles `!` shell commands.
#[derive(Debug, Clone)]
pub enum ShellPolicy {
    /// Shell commands are completely disabled (safest).
    Disabled,
    /// Only commands whose base name is in the set are allowed.
    AllowList(HashSet<String>),
    /// All shell commands are allowed (must opt-in explicitly).
    Unrestricted,
}

/// Check if a shell command string contains metacharacters that could
/// bypass the allowlist by chaining multiple commands or performing
/// command substitution/redirection.
fn has_shell_metacharacters(cmd_str: &str) -> bool {
    let metachars = [';', '&', '|', '`', '$', '>', '<', '\n'];
    cmd_str.contains(metachars)
}

impl ShellPolicy {
    /// Check if a given shell command string is allowed under this policy.
    /// Returns Ok(()) if allowed, Err(reason) if blocked.
    pub fn check(&self, cmd_str: &str) -> Result<(), String> {
        match self {
            ShellPolicy::Disabled => Err("Shell commands are disabled by policy".to_string()),
            ShellPolicy::AllowList(allowed) => {
                // Reject shell metacharacters that would allow bypassing the allowlist
                if has_shell_metacharacters(cmd_str) {
                    return Err(format!(
                        "Command contains shell metacharacters that are not permitted in allowlist mode: '{}'",
                        cmd_str
                    ));
                }
                let base_cmd = extract_base_command(cmd_str);
                if allowed.contains(&base_cmd) {
                    Ok(())
                } else {
                    Err(format!(
                        "Command '{}' is not in the allowlist. Allowed: {}",
                        base_cmd,
                        allowed.iter().cloned().collect::<Vec<_>>().join(", ")
                    ))
                }
            }
            ShellPolicy::Unrestricted => Ok(()),
        }
    }

    /// Human-readable description of the current policy.
    pub fn description(&self) -> String {
        match self {
            ShellPolicy::Disabled => "disabled".to_string(),
            ShellPolicy::AllowList(set) => {
                format!(
                    "allowlist ({})",
                    set.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            }
            ShellPolicy::Unrestricted => "unrestricted".to_string(),
        }
    }
}

/// Extract the base command name from a shell command string.
/// e.g., "git status" → "git", "ls -la" → "ls"
fn extract_base_command(cmd_str: &str) -> String {
    cmd_str.split_whitespace().next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_base_command() {
        assert_eq!(extract_base_command("git status"), "git");
        assert_eq!(extract_base_command("ls -la /tmp"), "ls");
        assert_eq!(extract_base_command("  pwd  "), "pwd");
        assert_eq!(extract_base_command(""), "");
    }

    #[test]
    fn test_shell_policy_disabled() {
        let policy = ShellPolicy::Disabled;
        assert!(policy.check("ls").is_err());
        assert!(policy.check("git status").is_err());
    }

    #[test]
    fn test_shell_policy_allowlist() {
        let allowed: HashSet<String> = vec!["git", "ls", "pwd"]
            .into_iter()
            .map(String::from)
            .collect();
        let policy = ShellPolicy::AllowList(allowed);

        assert!(policy.check("git status").is_ok());
        assert!(policy.check("ls -la").is_ok());
        assert!(policy.check("pwd").is_ok());
        assert!(policy.check("rm -rf /").is_err());
        assert!(policy.check("curl evil.com").is_err());
    }

    #[test]
    fn test_has_shell_metacharacters() {
        assert!(!has_shell_metacharacters("git status"));
        assert!(!has_shell_metacharacters("ls -la"));
        assert!(has_shell_metacharacters("git status; rm -rf /"));
        assert!(has_shell_metacharacters("ls && whoami"));
        assert!(has_shell_metacharacters("ls || whoami"));
        assert!(has_shell_metacharacters("echo `id`"));
        assert!(has_shell_metacharacters("echo $(whoami)"));
        assert!(has_shell_metacharacters("ls | whoami"));
        assert!(has_shell_metacharacters("echo foo > /tmp/bar"));
        assert!(has_shell_metacharacters("cat < /etc/passwd"));
        assert!(has_shell_metacharacters("cmd\nother_cmd"));
    }

    #[test]
    fn test_allowlist_rejects_metacharacters() {
        let allowed: HashSet<String> = vec!["git", "ls", "pwd"]
            .into_iter()
            .map(String::from)
            .collect();
        let policy = ShellPolicy::AllowList(allowed);

        // Base command is allowed but metacharacters present → rejected
        assert!(policy.check("git status; rm -rf /").is_err());
        assert!(policy.check("ls && whoami").is_err());
        assert!(policy.check("pwd || curl evil.com").is_err());
        assert!(policy.check("git | whoami").is_err());
        assert!(policy.check("ls `whoami`").is_err());
        assert!(policy.check("git $(curl evil.com)").is_err());
        assert!(policy.check("ls > /tmp/foo").is_err());
        assert!(policy.check("cat < /etc/passwd").is_err());
        assert!(policy.check("git\nrm -rf /").is_err());

        // Normal allowed commands still work
        assert!(policy.check("git status").is_ok());
        assert!(policy.check("ls -la").is_ok());

        // Blocked commands still blocked
        assert!(policy.check("rm -rf /").is_err());
    }

    #[test]
    fn test_unrestricted_does_not_check_metacharacters() {
        let policy = ShellPolicy::Unrestricted;
        // Unrestricted allows everything, even with metacharacters
        assert!(policy.check("anything").is_ok());
        assert!(policy.check("rm -rf /").is_ok());
        assert!(policy.check("ls; whoami").is_ok());
        assert!(policy.check("ls && whoami").is_ok());
    }

    #[test]
    fn test_policy_description() {
        assert_eq!(ShellPolicy::Disabled.description(), "disabled");
        assert_eq!(ShellPolicy::Unrestricted.description(), "unrestricted");
    }
}
