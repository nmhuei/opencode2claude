use super::{map_reqwest_error, read_text_response};
use crate::opencode::search::types::{
    SearchError, SearchPolicy, SearchProviderKind, SearchQuery, SearchResult,
};
use crate::opencode::search::util::{strip_html_tags, url_decode};
use reqwest::Client;

pub(crate) async fn search(
    client: &Client,
    query: &SearchQuery,
    endpoint: &str,
    policy: &SearchPolicy,
) -> Result<Vec<SearchResult>, SearchError> {
    let mut url = reqwest::Url::parse(endpoint).map_err(|error| {
        SearchError::new(
            SearchProviderKind::DuckDuckGo,
            crate::opencode::search::types::SearchErrorKind::UnsafeEndpoint,
            error.to_string(),
        )
    })?;
    url.query_pairs_mut().append_pair("q", &query.text);
    let response = client
        .get(url)
        .timeout(policy.request_timeout)
        .header(
            "User-Agent",
            "Mozilla/5.0 (compatible; OpenCode2API search adapter)",
        )
        .send()
        .await
        .map_err(|error| map_reqwest_error(SearchProviderKind::DuckDuckGo, error))?;
    let html = read_text_response(
        SearchProviderKind::DuckDuckGo,
        response,
        policy.max_response_bytes,
    )
    .await?;
    Ok(parse_html(&html, policy))
}

pub(super) fn parse_html(html: &str, policy: &SearchPolicy) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut remaining = html;
    while let Some(start) = remaining.find("result__body") {
        remaining = &remaining[start..];
        let end = remaining
            .find("result__body")
            .filter(|index| *index > 1)
            .unwrap_or(remaining.len());
        let block = &remaining[..end];
        if let Some(result) = parse_block(block, policy.max_snippet_chars) {
            results.push(result);
            if results.len() >= policy.max_results {
                break;
            }
        }
        if end == remaining.len() {
            break;
        }
        remaining = &remaining[end..];
    }

    // Some DuckDuckGo variants omit result__body. Preserve compatibility with
    // snippet-only fixture/HTML by parsing snippet anchors directly.
    if results.is_empty() {
        remaining = html;
        while let Some(start) = remaining.find("result__snippet") {
            remaining = &remaining[start..];
            if let Some(result) = parse_snippet_anchor(remaining, policy.max_snippet_chars) {
                results.push(result);
                if results.len() >= policy.max_results {
                    break;
                }
            }
            let Some(next) = remaining.find("</a>") else {
                break;
            };
            remaining = &remaining[next + 4..];
        }
    }
    results
}

fn parse_block(block: &str, max_snippet_chars: usize) -> Option<SearchResult> {
    let title_anchor = find_anchor(block, "result__a")?;
    let snippet_anchor = find_anchor(block, "result__snippet");
    let title = strip_html_tags(title_anchor.text);
    let snippet = snippet_anchor
        .map(|anchor| strip_html_tags(anchor.text))
        .unwrap_or_default();
    SearchResult::normalized(
        title,
        decode_result_url(title_anchor.href),
        snippet,
        max_snippet_chars,
    )
}

fn parse_snippet_anchor(fragment: &str, max_snippet_chars: usize) -> Option<SearchResult> {
    let anchor = find_anchor(fragment, "result__snippet")?;
    let snippet = strip_html_tags(anchor.text);
    SearchResult::normalized(
        "DuckDuckGo result",
        decode_result_url(anchor.href),
        snippet,
        max_snippet_chars,
    )
}

struct Anchor<'a> {
    href: &'a str,
    text: &'a str,
}

fn find_anchor<'a>(fragment: &'a str, class: &str) -> Option<Anchor<'a>> {
    let class_pos = fragment.find(class)?;
    let before = &fragment[..class_pos];
    let start = before.rfind("<a")?;
    let anchor = &fragment[start..];
    let tag_end = anchor.find('>')?;
    let tag = &anchor[..tag_end];
    let href = tag.split_once("href=\"")?.1.split_once('"')?.0;
    let text = anchor[tag_end + 1..].split_once("</a>")?.0;
    Some(Anchor { href, text })
}

fn decode_result_url(href: &str) -> String {
    if let Some(encoded) = href.split("uddg=").nth(1) {
        url_decode(encoded.split('&').next().unwrap_or(encoded))
    } else if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_fixture_without_network() {
        let html = r#"
        <div class="result__body">
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Frust">Rust &amp; Safety</a>
          <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Frust">Một <b>đoạn</b> mô tả</a>
        </div>
        "#;
        let results = parse_html(html, &SearchPolicy::default());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/rust");
        assert_eq!(results[0].title, "Rust & Safety");
        assert_eq!(results[0].snippet, "Một đoạn mô tả");
    }

    #[test]
    fn empty_fixture_returns_no_structured_results() {
        assert!(parse_html("<html>empty</html>", &SearchPolicy::default()).is_empty());
    }

    #[test]
    fn hostile_html_never_panics_or_loops() {
        let policy = SearchPolicy::default();
        let padded_multibyte = format!("ế{}ế", "result__body".repeat(50));
        let hostile = [
            "result__body",
            "result__body result__body",
            "<a class=\"result__a\"",       // unterminated anchor
            "<a class=\"result__a\" href=", // href without value
            "<div class=\"result__body\"><a class=\"result__a\" href=\"x\">tiếng",
            "result__snippet</a>",
            "<a class=\"result__snippet\">no close",
            padded_multibyte.as_str(),
        ];
        for input in hostile {
            let results = parse_html(input, &policy);
            assert!(results.len() <= policy.max_results);
        }
    }
}
