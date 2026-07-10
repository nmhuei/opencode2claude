//! Search-domain types.

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchProviderKind {
    Tavily,
    Exa,
    Serper,
    SearXng,
    DuckDuckGo,
}
