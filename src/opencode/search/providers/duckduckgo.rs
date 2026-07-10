use crate::opencode::search::util::{strip_html_tags, url_decode, urlencoding_simple};
use reqwest::Client;

pub(crate) async fn search(client: &Client, query: &str) -> Result<String, String> {
    let response = client
        .get(format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding_simple(query)
        ))
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36",
        )
        .send()
        .await
        .map_err(|error| format!("Search failed: {error}"))?;
    let html = response
        .text()
        .await
        .map_err(|error| format!("Failed to read search body: {error}"))?;

    let mut results = Vec::new();
    let mut remaining = html.as_str();
    while let Some(start) = remaining.find("<a class=\"result__snippet\"") {
        remaining = &remaining[start..];
        if let Some(result) = parse_result(remaining) {
            results.push(result);
        }
        let Some(next) = remaining.find("</a>") else {
            break;
        };
        remaining = &remaining[next + 4..];
        if results.len() >= 5 {
            break;
        }
    }

    Ok(if results.is_empty() {
        "No results found.".to_string()
    } else {
        results.join("\n")
    })
}

fn parse_result(fragment: &str) -> Option<String> {
    let href = fragment.split_once("href=\"")?.1.split_once('"')?.0;
    let url = if let Some(encoded) = href.split("uddg=").nth(1) {
        url_decode(encoded.split('&').next().unwrap_or(encoded))
    } else {
        format!("https:{href}")
    };
    let text = fragment.split_once('>')?.1.split_once("</a>")?.0;
    Some(format!("URL: {url}\nSnippet: {}\n", strip_html_tags(text)))
}
