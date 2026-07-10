//! Browser dashboard transport, split by responsibility.

mod assets;
mod auth;
mod config_file;
mod events;
mod overview;
mod time;

pub use assets::{serve_landing, serve_webui};
pub use auth::{handler_auth_status, handler_login, handler_logout};
pub use config_file::{handler_config_raw, handler_config_save};
pub use events::{
    handler_events, handler_test_stream_get, handler_test_stream_post, spawn_heartbeat,
    DashboardEvent,
};
pub use overview::{
    handler_config, handler_dashboard_diagnostics, handler_proxies, handler_proxy_restart,
    handler_rest_status,
};
pub use time::{unix_timestamp, uptime_string};
