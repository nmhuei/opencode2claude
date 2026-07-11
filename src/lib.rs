//! OpenCode2Claude — A local proxy that translates Anthropic Messages API
//! requests into OpenAI-compatible API calls.
//!
//! This library is re-exported by the binary for integration testing.
//! All public API items are exposed through their respective modules.

pub mod app;
pub mod cli;
pub mod config;
pub mod dashboard;
pub mod docker;
pub mod doctor;
pub mod error;
pub mod handlers;
pub mod infrastructure;
pub mod init;
pub mod management;
pub mod middleware;
pub mod observability;
pub mod opencode;
pub mod output;
pub mod pidfile;
pub mod proxy_pool;
pub mod rest_api;
pub mod runtime;
pub mod server;
pub mod shell;
pub mod sse;
pub mod state;
pub mod stream_tracker;
pub mod supervisor;
pub mod tui;
pub mod update;
pub mod workers;
