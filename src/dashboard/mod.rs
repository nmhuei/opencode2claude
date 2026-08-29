//! Browser dashboard transport, split by responsibility.

mod assets;
mod auth;
mod config_file;
mod control;
mod events;
mod overview;
mod time;

pub use assets::{serve_landing, serve_webui};
pub use auth::{handler_auth_status, handler_login, handler_logout};
pub use config_file::{handler_config_preview, handler_config_raw, handler_config_save};
pub use control::{
    handler_api_key_detail, handler_api_keys, handler_audit, handler_capabilities,
    handler_client_config, handler_completion, handler_config_init, handler_config_template,
    handler_delete_api_key, handler_doctor, handler_environment, handler_generate_keys,
    handler_history_content, handler_history_delete, handler_history_detail,
    handler_history_export, handler_history_list, handler_history_purge, handler_history_settings,
    handler_history_settings_update, handler_history_stats, handler_metrics, handler_models,
    handler_proxy_logs, handler_proxy_plan, handler_proxy_purge, handler_proxy_restart_all,
    handler_revoke_keys, handler_rotate_api_key, handler_select_model, handler_server_logs,
    handler_server_restart, handler_server_stop, handler_update_api_key, handler_update_apply,
    handler_update_check, handler_verify_api_key, no_store_middleware,
};
pub use events::{
    handler_events, handler_test_stream_get, handler_test_stream_post, run_heartbeat,
    DashboardEvent,
};
pub use overview::{
    handler_config, handler_dashboard_diagnostics, handler_proxies, handler_proxy_drain,
    handler_proxy_restart, handler_proxy_undrain, handler_rest_status,
};
pub use time::{unix_timestamp, uptime_string};
