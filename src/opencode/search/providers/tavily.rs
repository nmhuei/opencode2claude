use super::format_results;
use reqwest::Client;

pub(crate) async fn search(client: &Client, query: &str, api_key: &str) -> Result<String, String> {
    let response = client
        .post("https://api.tavily.com/search")
        .json(&serde_json::json!({
            "api_key": api_key,
            "query": query,
            "include_answer": false,
            "max_results": 5
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Tavily status {status}: {}",
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
    Ok(format_results(results, "No results found on Tavily."))
}
