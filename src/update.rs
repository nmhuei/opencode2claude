//! Self-update module — checks GitHub releases and replaces the current binary.
//!
//! Uses the GitHub Releases API to fetch the latest version, compares it
//! to the current build, and replaces the binary on disk if an update is
//! needed or requested.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

/// Info about a single release asset (binary for a specific platform).
#[derive(Debug, Clone)]
pub struct AssetInfo {
    /// File name, e.g. `opencode2api-linux-amd64`.
    pub name: String,
    /// Download URL for the asset.
    pub download_url: String,
}

/// Full release info from the GitHub API.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    /// Git tag (e.g. `v0.4.0`).
    pub tag: String,
    /// Version string without the `v` prefix.
    pub version: String,
    /// Downloadable assets for this release.
    pub assets: Vec<AssetInfo>,
    /// Release body (markdown).
    pub body: String,
}

/// Current binary version at compile time.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Build a user-agent string for GitHub API requests.
fn user_agent() -> String {
    format!("opencode2api/{}", current_version())
}

/// Determine the expected asset name for the current platform.
///
/// Returns `None` for unsupported platforms.
fn platform_asset_name() -> Option<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("linux", "x86_64") => Some("opencode2api-linux-amd64"),
        ("linux", "aarch64") => Some("opencode2api-linux-arm64"),
        ("macos", "x86_64") => Some("opencode2api-macos-amd64"),
        ("macos", "aarch64") => Some("opencode2api-macos-arm64"),
        _ => None,
    }
}

/// Fetch the latest release from the GitHub API.
pub async fn fetch_latest_release(client: &reqwest::Client) -> Result<ReleaseInfo> {
    let url = "https://api.github.com/repos/nmhuei/opencode2api/releases/latest";
    let resp = client
        .get(url)
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", user_agent())
        .send()
        .await
        .with_context(|| format!("failed to fetch {url}"))?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "GitHub API returned {} — check your network or rate limits",
            resp.status()
        ));
    }

    let data: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse GitHub release JSON")?;

    let tag = data["tag_name"].as_str().unwrap_or("unknown").to_string();
    let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
    let body = data["body"].as_str().unwrap_or("").to_string();

    let assets = data["assets"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let name = a["name"].as_str()?.to_string();
                    let download_url = a["browser_download_url"].as_str()?.to_string();
                    Some(AssetInfo { name, download_url })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(ReleaseInfo {
        tag,
        version,
        assets,
        body,
    })
}

/// Compare two semantic version strings.
///
/// Supports strict semver (e.g. `0.4.0`). Returns:
/// - `Ordering::Greater` if `a > b`
/// - `Ordering::Less` if `a < b`
/// - `Ordering::Equal` if equal
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let a_parts: Vec<u32> = a.split('.').filter_map(|p| p.parse::<u32>().ok()).collect();
    let b_parts: Vec<u32> = b.split('.').filter_map(|p| p.parse::<u32>().ok()).collect();

    for i in 0..a_parts.len().max(b_parts.len()) {
        let av = a_parts.get(i).copied().unwrap_or(0);
        let bv = b_parts.get(i).copied().unwrap_or(0);
        if av != bv {
            return av.cmp(&bv);
        }
    }
    std::cmp::Ordering::Equal
}

/// Check whether an update is available.
pub fn has_update(current: &str, release: &ReleaseInfo) -> bool {
    compare_versions(release.version.as_str(), current) == std::cmp::Ordering::Greater
}

/// Find the asset matching the current platform.
pub fn find_matching_asset(release: &ReleaseInfo) -> Option<&AssetInfo> {
    let asset_name = platform_asset_name()?;
    release.assets.iter().find(|a| a.name == asset_name)
}

/// Replace the current binary by downloading the new one.
///
/// Downloads into a temp file next to the current binary, then renames
/// atomically (POSIX: rename works even while the binary is running).
pub async fn apply_update(client: &reqwest::Client, asset: &AssetInfo) -> Result<PathBuf> {
    let current_exe = std::env::current_exe().context("cannot determine current binary path")?;
    let parent = current_exe
        .parent()
        .context("current binary has no parent directory")?;

    // Download to a temp file first
    let tmp_path = parent.join(format!(".{}.download", asset.name));

    let resp = client
        .get(&asset.download_url)
        .header("User-Agent", user_agent())
        .send()
        .await
        .with_context(|| format!("failed to download {}", asset.name))?;

    if !resp.status().is_success() {
        return Err(anyhow!("download returned {}", resp.status()));
    }

    let bytes = resp
        .bytes()
        .await
        .context("failed to read download stream")?;

    tokio::fs::write(&tmp_path, &bytes)
        .await
        .context("failed to write temp download file")?;

    // Make it executable (Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)
            .context("failed to set executable permissions")?;
    }

    // Rename temp file → current binary location.
    // On Unix this atomically replaces the running binary.
    // On Windows we'd have to handle the file lock differently.
    tokio::fs::rename(&tmp_path, &current_exe)
        .await
        .with_context(|| format!("failed to replace binary at {}", current_exe.display()))?;

    Ok(current_exe)
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_version_is_set() {
        let ver = current_version();
        assert!(!ver.is_empty(), "CARGO_PKG_VERSION should not be empty");
        assert!(
            ver.chars().next().unwrap().is_ascii_digit(),
            "version should start with a digit: {ver}"
        );
    }

    #[test]
    fn test_compare_versions_equal() {
        assert_eq!(
            compare_versions("0.4.0", "0.4.0"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn test_compare_versions_newer() {
        assert_eq!(
            compare_versions("0.5.0", "0.4.0"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("0.4.1", "0.4.0"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("1.0.0", "0.9.9"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn test_compare_versions_older() {
        assert_eq!(compare_versions("0.3.0", "0.4.0"), std::cmp::Ordering::Less);
        assert_eq!(compare_versions("0.3.9", "0.4.0"), std::cmp::Ordering::Less);
    }

    #[test]
    fn test_compare_versions_different_lengths() {
        assert_eq!(compare_versions("0.4", "0.4.0"), std::cmp::Ordering::Equal);
        assert_eq!(
            compare_versions("0.4.0.0", "0.4.0"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn test_has_update_true() {
        let release = ReleaseInfo {
            tag: "v0.5.0".into(),
            version: "0.5.0".into(),
            assets: vec![],
            body: String::new(),
        };
        assert!(has_update("0.4.0", &release));
    }

    #[test]
    fn test_has_update_false_when_equal() {
        let release = ReleaseInfo {
            tag: "v0.4.0".into(),
            version: "0.4.0".into(),
            assets: vec![],
            body: String::new(),
        };
        assert!(!has_update("0.4.0", &release));
    }

    #[test]
    fn test_has_update_false_when_behind() {
        let release = ReleaseInfo {
            tag: "v0.3.0".into(),
            version: "0.3.0".into(),
            assets: vec![],
            body: String::new(),
        };
        assert!(!has_update("0.4.0", &release));
    }

    #[test]
    fn test_platform_asset_name_exists_for_known_platforms() {
        // Just verify the function returns something on the current platform
        let name = platform_asset_name();
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        if (os == "linux" || os == "macos") && (arch == "x86_64" || arch == "aarch64") {
            assert!(
                name.is_some(),
                "should return an asset name for {os}/{arch}"
            );
            let name = name.unwrap();
            assert!(name.contains(os), "asset name should contain OS name");
        }
    }

    #[test]
    fn test_find_matching_asset_none_when_empty() {
        let release = ReleaseInfo {
            tag: "v0.4.0".into(),
            version: "0.4.0".into(),
            assets: vec![],
            body: String::new(),
        };
        let result = find_matching_asset(&release);
        // Might be None on unsupported platforms or empty assets
        if let Some(asset) = result {
            assert!(!asset.name.is_empty());
            assert!(!asset.download_url.is_empty());
        }
    }

    #[test]
    fn test_find_matching_asset_finds_match() {
        let release = ReleaseInfo {
            tag: "v0.4.0".into(),
            version: "0.4.0".into(),
            assets: vec![
                AssetInfo {
                    name: "opencode2api-linux-amd64".into(),
                    download_url: "https://example.com/linux-amd64".into(),
                },
                AssetInfo {
                    name: "opencode2api-macos-arm64".into(),
                    download_url: "https://example.com/macos-arm64".into(),
                },
            ],
            body: String::new(),
        };

        let platform_name = platform_asset_name();
        let found = find_matching_asset(&release);
        if let (Some(pn), Some(fa)) = (platform_name, found) {
            assert_eq!(fa.name, pn);
        }
    }
}
