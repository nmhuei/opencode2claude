use super::format_results;
use crate::opencode::search::util::truncate_chars;
use reqwest::Client;

pub(crate) async fn search(client: &Client, query: &str, api_key: &str) -> Result<String, String> {
    let response = client
        .post("https://api.exa.ai/search")
        .header("x-api-key", api_key)
        .json(&serde_json::json!({
            "query": query,
            "numResults": 5,
            "useAutoprompt": true
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Exa status {status}: {}",
            response.text().await.unwrap_or_default()
        ));
    }

    let payload: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    let results = payload
        .get("results")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .map(|item| {
            let highlights = item
                .get("highlights")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str())
                        .collect::<Vec<_>>()
                        .join(" ... ")
                })
                .unwrap_or_default();
            let snippet = if highlights.is_empty() {
                item.get("text")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
            } else {
                &highlights
            };
            format!(
                "URL: {}\nTitle: {}\nSnippet: {}\n",
                item.get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
                item.get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
                truncate_chars(snippet, 300)
            )
        })
        .collect();
    Ok(format_results(results, "No results found on Exa."))
}
