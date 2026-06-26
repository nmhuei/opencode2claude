//! OpenCode direct API gateway and format mapping layer.
//!
//! This module bypasses running OpenCode subprocesses entirely and communicates
//! directly with the public upstream completions API to act as a pure,
//! transparent LLM completions provider (supporting tools and streaming).

pub mod types;
pub mod search;
pub mod mapper;
pub mod forward;

// Re-exports so that code using `crate::opencode::check_daemon`,
// `crate::opencode::forward_to_llm_sync`, etc. continues to work.
#[allow(unused_imports)]
pub use self::types::*;
#[allow(unused_imports)]
pub use self::search::*;
#[allow(unused_imports)]
pub use self::mapper::*;
pub use self::forward::*;
