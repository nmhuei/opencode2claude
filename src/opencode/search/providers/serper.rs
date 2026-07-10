use super::format_results;
use reqwest::Client;

pub(crate) async fn search(client: &Client, query: &str, api_key: &str) -> Result<String, String> {
    let response = client
        .post("https://google.serper.dev/search")
        .header("X-API-KEY", api_key)
        .json(&serde_json::json!({"q": query}))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Serper status {status}: {}",
            response.text().await.unwrap_or_default()
        ));
    }

    let payload: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    let mut results = Vec::new();
    if let Some(snippet) = payload
        .get("answerBox")
        .and_then(|value| value.get("snippet"))
        .and_then(|value| value.as_str())
    {
        results.push(format!("Answer Box (Direct Answer):\n{snippet}\n"));
    }
    if let Some(items) = payload.get("organic").and_then(|value| value.as_array()) {
        results.extend(items.iter().map(|item| {
            format!(
                "URL: {}\nTitle: {}\nSnippet: {}\n",
                item.get("link")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
                item.get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
                item.get("snippet")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
            )
        }));
    }
    Ok(format_results(results, "No results found on Serper.dev."))
}
