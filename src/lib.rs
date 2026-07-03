//! OpenCode2Claude — A local proxy that translates Anthropic Messages API
//! requests into OpenAI-compatible API calls.
//!
//! This library is re-exported by the binary for integration testing.
//! All public API items are exposed through their respective modules.

pub mod cli;
pub mod config;
pub mod docker;
pub mod doctor;
pub mod error;
pub mod handlers;
pub mod init;
pub mod middleware;
pub mod opencode;
pub mod output;
pub mod pidfile;
pub mod proxy_pool;
pub mod runtime;
pub mod shell;
pub mod sse;
pub mod state;
pub mod stream_tracker;
pub mod supervisor;
pub mod update;
