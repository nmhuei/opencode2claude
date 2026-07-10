//! Web-search fallback chain.

mod client;
mod providers;
mod types;
mod util;

pub use client::SearchClient;
pub use types::{SearchProviderKind, SearchResult};
pub use util::{strip_html_tags, url_decode, urlencoding_simple};

#[cfg(test)]
mod tests;
