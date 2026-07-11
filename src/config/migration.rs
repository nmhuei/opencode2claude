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
    if from_version > CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "Configuration schema version {from_version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
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
}
