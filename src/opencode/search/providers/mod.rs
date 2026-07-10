//! Individual search-provider adapters.

mod duckduckgo;
mod exa;
mod searxng;
mod serper;
mod tavily;

pub(super) use duckduckgo::search as duckduckgo;
pub(super) use exa::search as exa;
pub(super) use searxng::search as searxng;
pub(super) use serper::search as serper;
pub(super) use tavily::search as tavily;

pub(super) fn format_results(results: Vec<String>, empty_message: &str) -> String {
    if results.is_empty() {
        empty_message.to_string()
    } else {
        results.join("\n")
    }
}
