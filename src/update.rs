//! Self-update module — checks GitHub releases and replaces the current binary.
//!
//! Uses the GitHub Releases API to fetch the latest version, compares it
//! to the current build, and replaces the binary on disk if an update is
//! needed or requested.

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

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

/// Download, verify, smoke-test, atomically replace, and rollback on failure.
///
/// Every release binary must have a companion asset named `<binary>.sha256`.
/// The updater never replaces the current executable without a valid SHA-256
/// and a successful `--version` smoke test.
pub async fn apply_update(client: &reqwest::Client, asset: &AssetInfo) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let _ = (client, asset);
        return Err(anyhow!(
            "self-update is not supported on Windows; install a signed release artifact manually"
        ));
    }

    #[cfg(not(windows))]
    {
        let current_exe =
            std::env::current_exe().context("cannot determine current binary path")?;
        let binary = download_bytes(client, &asset.download_url, &asset.name).await?;
        let checksum_url = format!("{}.sha256", asset.download_url);
        let checksum_text = download_text(client, &checksum_url, "checksum").await?;
        let expected = parse_sha256(&checksum_text).context("invalid checksum asset")?;
        install_candidate(&current_exe, &binary, &expected).await?;
        Ok(current_exe)
    }
}

async fn download_bytes(client: &reqwest::Client, url: &str, label: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .header("User-Agent", user_agent())
        .send()
        .await
        .with_context(|| format!("failed to download {label}"))?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "download of {label} returned {}",
            response.status()
        ));
    }
    Ok(response
        .bytes()
        .await
        .with_context(|| format!("failed to read {label} response body"))?
        .to_vec())
}

async fn download_text(client: &reqwest::Client, url: &str, label: &str) -> Result<String> {
    let bytes = download_bytes(client, url, label).await?;
    String::from_utf8(bytes).with_context(|| format!("{label} is not valid UTF-8"))
}

fn parse_sha256(text: &str) -> Result<String> {
    let hash = text
        .split_whitespace()
        .find(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| anyhow!("checksum file does not contain a 64-character SHA-256"))?;
    Ok(hash.to_ascii_lowercase())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

async fn install_candidate(target: &Path, bytes: &[u8], expected_sha256: &str) -> Result<()> {
    let actual = sha256_hex(bytes);
    if actual != expected_sha256.to_ascii_lowercase() {
        return Err(anyhow!(
            "SHA-256 mismatch: expected {expected_sha256}, received {actual}"
        ));
    }

    let parent = target
        .parent()
        .context("current binary has no parent directory")?;
    let suffix = format!("{}-{}", std::process::id(), uuid::Uuid::new_v4().simple());
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("opencode2api");
    let temp = parent.join(format!(".{file_name}.update-{suffix}"));
    let backup = parent.join(format!(".{file_name}.backup-{suffix}"));

    tokio::fs::write(&temp, bytes)
        .await
        .context("failed to write update candidate")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))
            .await
            .context("failed to set update candidate executable")?;
    }

    if let Err(error) = smoke_binary(&temp).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error.context("downloaded binary failed pre-install smoke test"));
    }

    tokio::fs::rename(target, &backup)
        .await
        .with_context(|| format!("failed to create backup at {}", backup.display()))?;
    if let Err(error) = tokio::fs::rename(&temp, target).await {
        let _ = tokio::fs::rename(&backup, target).await;
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error).context("failed to install update; previous binary restored");
    }

    if let Err(error) = smoke_binary(target).await {
        let _ = tokio::fs::remove_file(target).await;
        let rollback = tokio::fs::rename(&backup, target).await;
        return match rollback {
            Ok(()) => Err(error.context("installed binary failed smoke test; rollback completed")),
            Err(rollback_error) => Err(anyhow!(
                "installed binary failed smoke test ({error}); rollback failed ({rollback_error})"
            )),
        };
    }

    tokio::fs::remove_file(&backup)
        .await
        .context("update succeeded but backup cleanup failed")?;
    Ok(())
}

async fn smoke_binary(path: &Path) -> Result<()> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new(path).arg("--version").output(),
    )
    .await
    .context("binary smoke test timed out")??;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "binary smoke test returned {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
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

    #[test]
    fn checksum_parser_accepts_standard_companion_format() {
        let hash = "a".repeat(64);
        assert_eq!(
            parse_sha256(&format!("{hash}  opencode2api-linux-amd64\n")).unwrap(),
            hash
        );
        assert!(parse_sha256("not-a-checksum").is_err());
    }

    #[cfg(unix)]
    fn update_test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opencode2api-update-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn write_old_binary(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, b"#!/bin/sh\necho old\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checksum_mismatch_never_replaces_existing_binary() {
        let dir = update_test_dir("checksum");
        let target = dir.join("opencode2api");
        write_old_binary(&target);
        let candidate = b"#!/bin/sh\nexit 0\n";
        let error = install_candidate(&target, candidate, &"0".repeat(64))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("SHA-256 mismatch"));
        assert_eq!(std::fs::read(&target).unwrap(), b"#!/bin/sh\necho old\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_preinstall_smoke_leaves_existing_binary_untouched() {
        let dir = update_test_dir("pre-smoke");
        let target = dir.join("opencode2api");
        write_old_binary(&target);
        let candidate = b"#!/bin/sh\nexit 9\n";
        let error = install_candidate(&target, candidate, &sha256_hex(candidate))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("pre-install smoke test"));
        assert_eq!(std::fs::read(&target).unwrap(), b"#!/bin/sh\necho old\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn postinstall_smoke_failure_rolls_back_previous_binary() {
        let dir = update_test_dir("rollback");
        let target = dir.join("opencode2api");
        let marker = dir.join("smoke-marker");
        write_old_binary(&target);
        let candidate = format!(
            "#!/bin/sh\nif [ -f '{}' ]; then exit 7; fi\ntouch '{}'\nexit 0\n",
            marker.display(),
            marker.display()
        )
        .into_bytes();
        let error = install_candidate(&target, &candidate, &sha256_hex(&candidate))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("rollback completed"));
        assert_eq!(std::fs::read(&target).unwrap(), b"#!/bin/sh\necho old\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn valid_candidate_replaces_binary_and_removes_backup() {
        let dir = update_test_dir("success");
        let target = dir.join("opencode2api");
        write_old_binary(&target);
        let candidate = b"#!/bin/sh\necho opencode2api-test\nexit 0\n";
        install_candidate(&target, candidate, &sha256_hex(candidate))
            .await
            .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), candidate);
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("backup"))
            .count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
