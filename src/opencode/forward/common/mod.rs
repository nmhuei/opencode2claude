//! Helpers shared by synchronous and streaming forwarding paths.

mod compat;
mod header_parse;
mod json_repair;
mod schema;
mod search;
mod tokens;

#[cfg(test)]
mod tests;

// --- Shared constants ---
const MAX_COMPAT_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_COMPAT_BATCH_ITEMS: usize = 32;
const MAX_COMPAT_CALLS_PER_RESPONSE: usize = 128;

// Re-exported for the original common.rs `use super::*` consumers
// (used by dsml_argument_tests and compat_parser_invariant_tests in tests.rs)
#[cfg(test)]
pub(crate) use crate::handlers::MessagesRequest;

// --- Re-exports: compat markers, detection, extraction, TV/XML parsing ---
// Part of the original module surface: kept re-exported so every historical
// path (`crate::opencode::forward::common::X`) keeps resolving, even though
// current consumers reach these only through the extraction entry points.
#[allow(unused_imports)]
pub(crate) use compat::{
    compat_tool_marker_pending_suffix_len, extract_compat_tool_requests_detailed,
    find_compat_tool_intent_marker_in_context, parse_compat_tool_requests_at_eof,
    parse_compat_tool_requests_with_consumed, CompatToolCall,
};
#[cfg(test)]
pub(crate) use compat::{
    extract_compat_tool_requests, parse_compat_tool_request, parse_compat_tool_request_at_eof,
};
pub(crate) use compat::{find_literal_marker_in_context, CompatMarkdownState};

// --- Re-exports: search-result injection and search helpers ---
pub(crate) use search::{
    inject_search_results, normalize_search_query, prepare_compat_tool_retry,
    prepare_final_search_synthesis, prepare_native_tool_retry, resolve_search_query,
    search_results_with_instruction,
};

// --- Re-exports: schema coercion and tool utilities ---
#[cfg(test)]
pub(crate) use schema::get_correct_tool_name;
pub(crate) use schema::{
    invalid_semantic_tool_argument, looks_like_unverified_tool_success, matching_tool_name,
    normalize_dsml_arguments, tool_call_fingerprint,
};

// --- Re-exports: token estimation (public API) ---
pub use tokens::{estimate_input_tokens, estimate_string_tokens};

// --- Daemon / health helpers (remain in mod.rs) ---

use reqwest::Client;

/// Check if the OpenCode daemon is running and reachable.
pub async fn check_daemon(client: &Client, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}/doc", port);
    client
        .get(&url)
        .timeout(std::time::Duration::from_millis(500))
        .send()
        .await
        .is_ok()
}

/// Read an upstream response body without allowing unbounded allocation.
pub(super) async fn read_bounded_body(
    response: crate::opencode::retry::LeasedResponse,
    max_bytes: usize,
) -> Result<Vec<u8>, crate::error::BridgeError> {
    use futures_util::StreamExt;

    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            crate::error::BridgeError::UpstreamError(format!(
                "Failed reading upstream response: {error}"
            ))
        })?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(crate::error::BridgeError::UpstreamError(format!(
                "Upstream response exceeded configured limit of {max_bytes} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
