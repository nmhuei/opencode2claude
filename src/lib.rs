//! OpenCode2Claude — A local proxy that translates Anthropic Messages API
//! requests into OpenAI-compatible API calls.
//!
//! This library is re-exported by the binary for integration testing.
//! All public API items are exposed through their respective modules.

pub mod cli;
pub mod config;
pub mod docker;
pub mod error;
pub mod handlers;
pub mod middleware;
pub mod opencode;
pub mod pidfile;
pub mod proxy_pool;
pub mod runtime;
pub mod shell;
pub mod sse;
pub mod state;
pub mod supervisor;
