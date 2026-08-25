//! Persistent shell integration for commands that must mutate the parent shell.
//!
//! A child process cannot modify its parent's environment. The installed shell
//! hook therefore turns bare `opencode2api` into a Claude Code launcher:
//! it evaluates the canonical bridge environment in the current interactive shell
//! and launches `claude`. Starting the bridge remains an explicit manual action.
//! `opencode2api set env` remains available for env-only use.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

pub const HOOK_BEGIN: &str = "# >>> opencode2api shell integration >>>";
pub const HOOK_END: &str = "# <<< opencode2api shell integration <<<";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
}

impl ShellKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }

    fn default_rc_path(self) -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set; pass --rc explicitly"))?;
        match self {
            Self::Bash => Ok(home.join(".bashrc")),
            Self::Zsh => {
                let base = std::env::var_os("ZDOTDIR")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or(home);
                Ok(base.join(".zshrc"))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct HookInstallResult {
    pub shell: ShellKind,
    pub path: PathBuf,
    pub changed: bool,
}

pub fn resolve_shell(requested: &str) -> Result<ShellKind> {
    match requested.trim().to_ascii_lowercase().as_str() {
        "bash" => Ok(ShellKind::Bash),
        "zsh" => Ok(ShellKind::Zsh),
        "auto" | "" => {
            let shell = std::env::var("SHELL").unwrap_or_default();
            let name = Path::new(&shell)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match name.as_str() {
                "bash" => Ok(ShellKind::Bash),
                "zsh" => Ok(ShellKind::Zsh),
                _ => Err(anyhow!(
                    "could not detect a supported shell from SHELL={shell:?}; use --shell bash or --shell zsh"
                )),
            }
        }
        other => Err(anyhow!(
            "unsupported shell {other:?}; supported shells are bash and zsh"
        )),
    }
}

pub fn render_hook() -> String {
    format!(
        r#"{HOOK_BEGIN}
opencode2api() {{
    if [ "$#" -eq 0 ]; then
        local _opencode2api_env _opencode2api_status
        if ! command -v claude >/dev/null 2>&1; then
            printf '%s\n' 'opencode2api: Claude Code is not installed or not available in PATH.' >&2
            return 127
        fi
        local _opencode2api_server_status
        _opencode2api_server_status="$(command opencode2api --quiet server status 2>/dev/null)" || return $?
        if [ "$_opencode2api_server_status" != "running" ]; then
            printf '%s\n' 'opencode2api: bridge is not running. Start it first with: opencode2api server start' >&2
            return 1
        fi
        _opencode2api_env="$(command opencode2api --quiet env)" || return $?
        eval "$_opencode2api_env"
        _opencode2api_status=$?
        [ "$_opencode2api_status" -eq 0 ] || return "$_opencode2api_status"
        command claude
        return $?
    fi
    if [ "$#" -eq 2 ] && [ "$1" = "set" ] && [ "$2" = "env" ]; then
        local _opencode2api_env _opencode2api_status
        _opencode2api_env="$(command opencode2api --quiet env)" || return $?
        eval "$_opencode2api_env"
        _opencode2api_status=$?
        return $_opencode2api_status
    fi
    command opencode2api "$@"
}}
{HOOK_END}"#
    )
}

fn remove_managed_block_text(input: &str) -> (String, bool) {
    let mut text = input.to_string();
    let mut changed = false;

    while let Some(start) = text.find(HOOK_BEGIN) {
        let Some(end_rel) = text[start..].find(HOOK_END) else {
            break;
        };
        let end_marker_end = start + end_rel + HOOK_END.len();
        let end = if text.as_bytes().get(end_marker_end) == Some(&b'\n') {
            end_marker_end + 1
        } else {
            end_marker_end
        };
        text.replace_range(start..end, "");
        changed = true;
    }

    (text, changed)
}

pub fn upsert_hook_text(input: &str) -> String {
    let hook = render_hook();
    let (without_old, _) = remove_managed_block_text(input);
    let base = without_old.trim_end_matches('\n');
    if base.is_empty() {
        format!("{hook}\n")
    } else {
        format!("{base}\n\n{hook}\n")
    }
}

pub fn install_hook(
    requested_shell: &str,
    rc_override: Option<&Path>,
) -> Result<HookInstallResult> {
    let shell = resolve_shell(requested_shell)?;
    let path = match rc_override {
        Some(path) => path.to_path_buf(),
        None => shell.default_rc_path()?,
    };
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    let updated = upsert_hook_text(&existing);
    let changed = updated != existing;
    if changed {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, updated)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(HookInstallResult {
        shell,
        path,
        changed,
    })
}

pub fn uninstall_hook(
    requested_shell: &str,
    rc_override: Option<&Path>,
) -> Result<HookInstallResult> {
    let shell = resolve_shell(requested_shell)?;
    let path = match rc_override {
        Some(path) => path.to_path_buf(),
        None => shell.default_rc_path()?,
    };
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    let (updated, changed) = remove_managed_block_text(&existing);
    if changed {
        std::fs::write(&path, updated)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(HookInstallResult {
        shell,
        path,
        changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_launches_claude_for_bare_command_and_preserves_manual_env_mode() {
        let hook = render_hook();
        assert!(hook.contains("[ \"$#\" -eq 0 ]"));
        assert!(hook.contains("command -v claude"));
        assert!(hook.contains("command opencode2api --quiet server status"));
        assert!(!hook.contains("command opencode2api --quiet server start"));
        assert!(hook.contains("command opencode2api --quiet env"));
        assert!(hook.contains("command claude"));
        assert!(hook.contains("[ \"$1\" = \"set\" ]"));
        assert!(hook.contains("[ \"$2\" = \"env\" ]"));
        assert!(hook.contains("command opencode2api \"$@\""));
        assert!(!hook.contains("OPENCODE_MODEL="));
        assert!(!hook.contains("CLAUDE_CODE_EFFORT_LEVEL="));
    }

    #[test]
    fn hook_upsert_is_idempotent_and_preserves_user_content() {
        let first = upsert_hook_text("export KEEP_ME=1\n");
        let second = upsert_hook_text(&first);
        assert_eq!(first, second);
        assert!(second.starts_with("export KEEP_ME=1\n"));
        assert_eq!(second.matches(HOOK_BEGIN).count(), 1);
        assert_eq!(second.matches(HOOK_END).count(), 1);
    }
}
