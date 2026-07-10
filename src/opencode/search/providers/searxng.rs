use super::format_results;
use crate::opencode::search::util::urlencoding_simple;
use reqwest::Client;

pub(crate) async fn search(client: &Client, query: &str, base_url: &str) -> Result<String, String> {
    let encoded = urlencoding_simple(query);
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
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "SearXNG status {status}: {}",
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
            format!(
                "URL: {}\nTitle: {}\nSnippet: {}\n",
                item.get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
                item.get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
                item.get("content")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
            )
        })
        .collect();
    Ok(format_results(results, "No results found on SearXNG."))
}
