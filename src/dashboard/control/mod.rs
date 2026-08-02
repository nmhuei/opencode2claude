//! Authenticated dashboard control-plane endpoints.

mod access;
mod api_keys;
mod catalog;
mod history;
mod network;
mod system;

pub use access::{
    handler_audit, handler_client_config, handler_completion, handler_config_init,
    handler_config_template, handler_doctor, handler_environment, handler_metrics,
};
pub use api_keys::{
    handler_api_key_detail, handler_api_keys, handler_delete_api_key, handler_generate_keys,
    handler_revoke_keys, handler_rotate_api_key, handler_update_api_key, handler_verify_api_key,
};
pub use catalog::{handler_capabilities, handler_models, handler_select_model};
pub use history::{
    handler_history_content, handler_history_delete, handler_history_detail,
    handler_history_export, handler_history_list, handler_history_purge, handler_history_settings,
    handler_history_settings_update, handler_history_stats,
};
pub use network::{
    handler_proxy_logs, handler_proxy_plan, handler_proxy_purge, handler_proxy_restart_all,
};
pub use system::{
    handler_server_logs, handler_server_restart, handler_server_stop, handler_update_apply,
    handler_update_check,
};

use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

pub(super) fn dashboard_error(
    status: StatusCode,
    message: impl Into<String>,
) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({"status":"error","message":message.into()})),
    )
}
