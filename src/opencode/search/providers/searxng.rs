use super::{map_reqwest_error, read_json_response};
use crate::opencode::search::types::{
    SearchError, SearchErrorKind, SearchPolicy, SearchProviderKind, SearchQuery, SearchResult,
};
use crate::opencode::search::util::urlencoding_simple;
use reqwest::Client;
use serde_json::Value;

pub(crate) async fn search(
    client: &Client,
    query: &SearchQuery,
    base_url: &str,
    policy: &SearchPolicy,
) -> Result<Vec<SearchResult>, SearchError> {
    let encoded = urlencoding_simple(&query.text);
    let url = if base_url.contains('?') {
        format!("{base_url}&q={encoded}&format=json")
    } else {
        format!(
            "{}/search?q={encoded}&format=json",
            base_url.trim_end_matches('/')
        )
    };
    let response = client
        .get(url)
        .timeout(policy.request_timeout)
        .send()
        .await
        .map_err(|error| map_reqwest_error(SearchProviderKind::SearXng, error))?;
    let payload = read_json_response(
        SearchProviderKind::SearXng,
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
                SearchProviderKind::SearXng,
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
    fn parses_fixture() {
        let payload = serde_json::json!({
            "results":[{"title":"Local result","url":"https://example.com/local","content":"snippet"}]
        });
        let results = parse_payload(&payload, &SearchPolicy::default()).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn rejects_missing_results() {
        assert_eq!(
            parse_payload(&serde_json::json!({}), &SearchPolicy::default())
                .unwrap_err()
                .kind,
            SearchErrorKind::MalformedResponse
        );
    }
}
