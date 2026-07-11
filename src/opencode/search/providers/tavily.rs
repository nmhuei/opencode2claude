use super::{map_reqwest_error, read_json_response};
use crate::opencode::search::types::{
    SearchError, SearchErrorKind, SearchPolicy, SearchProviderKind, SearchQuery, SearchResult,
};
use reqwest::Client;
use serde_json::Value;

pub(crate) async fn search(
    client: &Client,
    query: &SearchQuery,
    api_key: &str,
    endpoint: &str,
    policy: &SearchPolicy,
) -> Result<Vec<SearchResult>, SearchError> {
    let response = client
        .post(endpoint)
        .timeout(policy.request_timeout)
        .json(&serde_json::json!({
            "api_key": api_key,
            "query": query.text,
            "include_answer": false,
            "max_results": query.max_results,
        }))
        .send()
        .await
        .map_err(|error| map_reqwest_error(SearchProviderKind::Tavily, error))?;
    let payload = read_json_response(
        SearchProviderKind::Tavily,
        response,
        policy.max_response_bytes,
    )
    .await?;
    parse_payload(&payload, policy)
}

pub(super) fn parse_payload(
    payload: &Value,
    policy: &SearchPolicy,
) -> Result<Vec<SearchResult>, SearchError> {
    let items = payload
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            SearchError::new(
                SearchProviderKind::Tavily,
                SearchErrorKind::MalformedResponse,
                "missing results array",
            )
        })?;
    Ok(items
        .iter()
        .filter_map(|item| {
            SearchResult::normalized(
                item.get("title").and_then(Value::as_str).unwrap_or(""),
                item.get("url").and_then(Value::as_str).unwrap_or(""),
                item.get("content").and_then(Value::as_str).unwrap_or(""),
                policy.max_snippet_chars,
            )
        })
        .take(policy.max_results)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_and_bounds_results() {
        let payload = serde_json::json!({
            "results": [
                {"title":"Rust", "url":"https://example.com/rust", "content":"safe systems language"},
                {"title":"Bad", "url":"file:///etc/passwd", "content":"bad"}
            ]
        });
        let results = parse_payload(&payload, &SearchPolicy::default()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust");
    }

    #[test]
    fn rejects_missing_results_array() {
        assert_eq!(
            parse_payload(&serde_json::json!({}), &SearchPolicy::default())
                .unwrap_err()
                .kind,
            SearchErrorKind::MalformedResponse
        );
    }
}
