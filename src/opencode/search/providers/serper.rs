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
        .header("X-API-KEY", api_key)
        .json(&serde_json::json!({"q": query.text, "num": query.max_results}))
        .send()
        .await
        .map_err(|error| map_reqwest_error(SearchProviderKind::Serper, error))?;
    let payload = read_json_response(
        SearchProviderKind::Serper,
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
    if !payload.is_object() {
        return Err(SearchError::new(
            SearchProviderKind::Serper,
            SearchErrorKind::MalformedResponse,
            "response root must be an object",
        ));
    }
    let mut results = Vec::new();
    if let Some(answer) = payload.get("answerBox") {
        let snippet = answer
            .get("snippet")
            .or_else(|| answer.get("answer"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let url = answer
            .get("link")
            .and_then(Value::as_str)
            .unwrap_or("https://google.com/search");
        if !snippet.is_empty() {
            if let Some(result) =
                SearchResult::normalized("Direct answer", url, snippet, policy.max_snippet_chars)
            {
                results.push(result);
            }
        }
    }
    if let Some(items) = payload.get("organic").and_then(Value::as_array) {
        results.extend(items.iter().filter_map(|item| {
            SearchResult::normalized(
                item.get("title").and_then(Value::as_str).unwrap_or(""),
                item.get("link").and_then(Value::as_str).unwrap_or(""),
                item.get("snippet").and_then(Value::as_str).unwrap_or(""),
                policy.max_snippet_chars,
            )
        }));
    }
    results.truncate(policy.max_results);
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_answer_box_and_organic_results() {
        let payload = serde_json::json!({
            "answerBox":{"answer":"42","link":"https://example.com/answer"},
            "organic":[{"title":"Result","link":"https://example.com/r","snippet":"body"}]
        });
        let results = parse_payload(&payload, &SearchPolicy::default()).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Direct answer");
    }

    #[test]
    fn rejects_non_object_payload() {
        assert_eq!(
            parse_payload(&serde_json::json!([]), &SearchPolicy::default())
                .unwrap_err()
                .kind,
            SearchErrorKind::MalformedResponse
        );
    }
}
