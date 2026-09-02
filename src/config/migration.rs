//! Versioned configuration migrations shared by loader and management apply.

use serde::Serialize;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MigrationReport {
    pub from_version: u32,
    pub to_version: u32,
    pub renamed_keys: Vec<String>,
    pub changed: bool,
}

const V0_ALIASES: &[(&str, &str)] = &[
    ("bridge_port", "port"),
    ("auth_token", "auth_tokens"),
    ("dashboard_token", "dashboard_admin_token"),
    ("rest_token", "rest_api_token"),
    ("proxy_urls", "primary_proxies"),
    ("standby_proxies", "warm_standby_proxies"),
    ("proxy_count", "active_proxy_count"),
    ("upstream_url", "upstream_base_url"),
    ("metrics", "metrics_enabled"),
];

/// Keys retired from every operator-facing surface (env, TOML, template,
/// display) because they were enforced nowhere at runtime. Migration drops
/// them silently — mirroring how unknown keys are tolerated — which keeps
/// legacy documents loading and lets management's known-key allowlist stop
/// listing them without rejecting pre-existing files.
const RETIRED_KEYS: &[&str] = &["max_provider_attempts"];

pub fn migrate_document(content: &str) -> Result<(String, MigrationReport), String> {
    let value = content
        .parse::<toml::Value>()
        .map_err(|error| format!("Invalid TOML: {error}"))?;
    let (value, report) = migrate_value(value)?;
    let output = toml::to_string_pretty(&value)
        .map_err(|error| format!("Failed to serialize migrated config: {error}"))?;
    Ok((output, report))
}

pub fn migrate_value(mut value: toml::Value) -> Result<(toml::Value, MigrationReport), String> {
    let table = value
        .as_table_mut()
        .ok_or_else(|| "Configuration root must be a TOML table".to_string())?;
    let from_version = table
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .unwrap_or(0);
    if from_version < 0 {
        return Err("schema_version must be non-negative".to_string());
    }
    let from_version = from_version as u32;
    if from_version > CURRENT_SCHEMA_VERSION && !matches!(from_version, 2 | 3) {
        return Err(format!(
            "Configuration schema version {from_version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
        ));
    }

    for retired in RETIRED_KEYS {
        table.remove(*retired);
    }

    // Provider management owns schema v2 sections. The legacy loader still
    // reports v1 as its current schema, but must preserve a validated v2
    // document so `active_alias` and provider tables reach the resolver.
    if matches!(from_version, 2 | 3) {
        return Ok((
            value,
            MigrationReport {
                from_version,
                to_version: from_version,
                renamed_keys: Vec::new(),
                changed: false,
            },
        ));
    }

    let mut renamed_keys = Vec::new();
    if from_version == 0 {
        for (legacy, current) in V0_ALIASES {
            if table.contains_key(*legacy) && table.contains_key(*current) {
                return Err(format!(
                    "Configuration contains both legacy key '{legacy}' and current key '{current}'"
                ));
            }
            if let Some(value) = table.remove(*legacy) {
                table.insert((*current).to_string(), value);
                renamed_keys.push(format!("{legacy}->{current}"));
            }
        }
    }

    let version_changed = from_version != CURRENT_SCHEMA_VERSION;
    table.insert(
        "schema_version".to_string(),
        toml::Value::Integer(i64::from(CURRENT_SCHEMA_VERSION)),
    );
    let changed = version_changed || !renamed_keys.is_empty();
    Ok((
        value,
        MigrationReport {
            from_version,
            to_version: CURRENT_SCHEMA_VERSION,
            renamed_keys,
            changed,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_keys_and_sets_schema_version() {
        let (document, report) =
            migrate_document("bridge_port = 4111\ndashboard_token = \"secret\"\nproxy_count = 2\n")
                .unwrap();
        assert!(report.changed);
        assert_eq!(report.from_version, 0);
        assert!(document.contains("schema_version = 1"));
        assert!(document.contains("port = 4111"));
        assert!(document.contains("dashboard_admin_token = \"secret\""));
        assert!(document.contains("active_proxy_count = 2"));
        assert!(!document.contains("bridge_port"));
    }

    #[test]
    fn rejects_conflicting_legacy_and_current_keys() {
        let error = migrate_document("bridge_port=4000\nport=4001\n").unwrap_err();
        assert!(error.contains("both legacy key"));
    }

    #[test]
    fn rejects_future_schema() {
        let error = migrate_document("schema_version=99\n").unwrap_err();
        assert!(error.contains("newer than supported"));
    }

    #[test]
    fn current_schema_is_idempotent() {
        let (first, first_report) = migrate_document("schema_version=1\nport=4000\n").unwrap();
        let (second, second_report) = migrate_document(&first).unwrap();
        assert!(!first_report.changed);
        assert!(!second_report.changed);
        assert_eq!(first, second);
    }

    #[test]
    fn strips_retired_keys_from_any_schema_version() {
        // Current-schema documents are stripped in place.
        let (document, _) =
            migrate_document("schema_version = 1\nmax_provider_attempts = 7\nport = 4000\n")
                .unwrap();
        assert!(!document.contains("max_provider_attempts"));
        assert!(document.contains("port = 4000"));

        // Legacy documents go through the same retirement on their way up.
        let (document, _) = migrate_document("max_provider_attempts = 7\n").unwrap();
        assert!(!document.contains("max_provider_attempts"));

        // Stripping is idempotent: re-migrating a stripped document is a no-op.
        let (second, report) = migrate_document(&document).unwrap();
        assert!(!report.changed);
        assert_eq!(second, document);
    }

    #[test]
    fn provider_schema_v2_is_preserved_for_provider_loader() {
        let (document, report) = migrate_document(
            "schema_version = 2\nactive_alias = \"free-1m\"\n[[providers]]\nid = \"kilo\"\n",
        )
        .unwrap();
        assert_eq!(report.from_version, 2);
        assert_eq!(report.to_version, 2);
        assert!(document.contains("active_alias"));
        assert!(document.contains("[[providers]]"));
    }

    #[test]
    fn provider_schema_v3_is_preserved_for_provider_loader() {
        let (document, report) = migrate_document(
            "schema_version = 3\n[router]\nactive_alias = \"free-1m\"\n[providers.bai]\nbase_url = \"https://api.b.ai/v1\"\n",
        )
        .unwrap();
        assert_eq!(report.from_version, 3);
        assert_eq!(report.to_version, 3);
        assert!(document.contains("schema_version = 3"));
        assert!(document.contains("[router]"));
        assert!(document.contains("[providers.bai]"));
    }
}
