//! Upstream forwarding split by execution mode.

pub(crate) mod common;
pub(super) mod fallback_intent;
#[cfg(test)]
mod parity_tests;
mod stream;
mod sync;

pub use common::{check_daemon, estimate_input_tokens, estimate_string_tokens};
pub use stream::forward_to_llm_stream;
pub use sync::forward_to_llm_sync;
