use super::{map_reqwest_error, read_json_response};
use crate::opencode::search::types::{
    SearchError, SearchErrorKind, SearchPolicy, SearchProviderKind, SearchQuery, SearchResult,
};
use reqwest::{header::HeaderValue, Client};
use serde_json::Value;

pub(crate) async fn search(
    client: &Client,
    query: &SearchQuery,
    api_key: &str,
    endpoint: &str,
    policy: &SearchPolicy,
) -> Result<Vec<SearchResult>, SearchError> {
    let mut request =
        client
            .post(endpoint)
            .timeout(policy.request_timeout)
            .json(&serde_json::json!({
                "query": query.text,
                "numResults": query.max_results,
                "useAutoprompt": true
            }));
    // Skip invalid header values (control characters in a misconfigured key)
    // instead of panicking.
    if let Ok(value) = HeaderValue::from_str(api_key) {
        request = request.header("x-api-key", value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| map_reqwest_error(SearchProviderKind::Exa, error))?;
    let payload =
        read_json_response(SearchProviderKind::Exa, response, policy.max_response_bytes).await?;
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
                SearchProviderKind::Exa,
                SearchErrorKind::MalformedResponse,
                "missing results array",
            )
        })?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let highlights = item
                .get("highlights")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ... ")
                })
                .unwrap_or_default();
            let snippet = if highlights.is_empty() {
                item.get("text").and_then(Value::as_str).unwrap_or("")
            } else {
                highlights.as_str()
            };
            SearchResult::normalized(
                item.get("title").and_then(Value::as_str).unwrap_or(""),
                item.get("url").and_then(Value::as_str).unwrap_or(""),
                snippet,
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
    fn highlights_take_precedence_and_are_utf8_bounded() {
        let payload = serde_json::json!({
            "results": [{
                "title":"Tiếng Việt",
                "url":"https://example.com/vi",
                "text":"fallback",
                "highlights":["một đoạn mô tả", "nhiều ký tự"]
            }]
        });
        let policy = SearchPolicy {
            max_snippet_chars: 12,
            ..Default::default()
        };
        let results = parse_payload(&payload, &policy).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet.chars().count(), 12);
        assert!(results[0].snippet.starts_with("một đoạn"));
    }

    #[test]
    fn rejects_malformed_payload() {
        assert_eq!(
            parse_payload(
                &serde_json::json!({"results":"bad"}),
                &SearchPolicy::default()
            )
            .unwrap_err()
            .kind,
            SearchErrorKind::MalformedResponse
        );
    }
}
