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
            SearchProviderKind::Yahoo,
            crate::opencode::search::types::SearchErrorKind::UnsafeEndpoint,
            error.to_string(),
        )
    })?;
    url.query_pairs_mut().append_pair("p", &query.text);
    let response = client
        .get(url)
        .timeout(policy.request_timeout)
        .header(
            "User-Agent",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/150 Safari/537.36",
        )
        .header("Accept-Language", "en-US,en;q=0.8")
        .send()
        .await
        .map_err(|error| map_reqwest_error(SearchProviderKind::Yahoo, error))?;
    let html = read_text_response(
        SearchProviderKind::Yahoo,
        response,
        policy.max_response_bytes,
    )
    .await?;
    Ok(parse_html(&html, policy))
}

pub(super) fn parse_html(html: &str, policy: &SearchPolicy) -> Vec<SearchResult> {
    if html.contains("cf-turnstile") || html.contains("captcha_header") {
        return Vec::new();
    }
    let mut results = Vec::new();
    let mut remaining = html;
    while let Some(marker) = remaining.find("data-matarget=\"algo\"") {
        let before = &remaining[..marker];
        let Some(anchor_start) = before.rfind("<a") else {
            remaining = &remaining[marker + 1..];
            continue;
        };
        let anchor = &remaining[anchor_start..];
        let Some(tag_end) = anchor.find('>') else {
            break;
        };
        let tag = &anchor[..tag_end];
        let Some(href) = attribute(tag, "href") else {
            remaining = &remaining[marker + 1..];
            continue;
        };
        let Some(anchor_end) = anchor[tag_end + 1..].find("</a>") else {
            break;
        };
        let body_end = tag_end + 1 + anchor_end;
        let body = &anchor[tag_end + 1..body_end];
        let title = extract_tag_text(body, "h3").unwrap_or_else(|| strip_html_tags(body));
        let after_anchor = &anchor[body_end + 4..];
        let block_end = after_anchor.find("</li>").unwrap_or(after_anchor.len());
        let snippet = extract_tag_text(&after_anchor[..block_end], "p").unwrap_or_default();
        if let Some(result) = SearchResult::normalized(
            title,
            decode_result_url(href),
            snippet,
            policy.max_snippet_chars,
        ) {
            results.push(result);
            if results.len() >= policy.max_results {
                break;
            }
        }
        remaining = &after_anchor[block_end.min(after_anchor.len())..];
    }
    results
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("{name}=\"");
    tag.split_once(&marker)?
        .1
        .split_once('"')
        .map(|value| value.0)
}

fn extract_tag_text(fragment: &str, tag: &str) -> Option<String> {
    let start_marker = format!("<{tag}");
    let start = fragment.find(&start_marker)?;
    let after_start = &fragment[start..];
    let body_start = after_start.find('>')? + 1;
    let end_marker = format!("</{tag}>");
    let body_end = after_start[body_start..].find(&end_marker)? + body_start;
    Some(strip_html_tags(&after_start[body_start..body_end]))
}

fn decode_result_url(href: &str) -> String {
    if let Some(encoded) = href
        .split("/RU=")
        .nth(1)
        .and_then(|value| value.split("/RK=").next())
    {
        url_decode(encoded)
    } else {
        href.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_yahoo_result_and_decodes_target_url() {
        let html = r#"
        <ol><li><div class="dd algo algo-sr">
          <div class="compTitle"><a data-matarget="algo" href="https://r.search.yahoo.com/x/RU=https%3a%2f%2fcode.claude.com%2fdocs%2fen%2fsecurity/RK=2/RS=x"><h3><span>Security - Claude Code Docs</span></h3></a></div>
          <div class="compText aAbs"><p>Learn about <b>security</b> safeguards.</p></div>
        </div></li></ol>
        "#;
        let results = parse_html(html, &SearchPolicy::default());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://code.claude.com/docs/en/security");
        assert_eq!(results[0].title, "Security - Claude Code Docs");
        assert_eq!(results[0].snippet, "Learn about security safeguards.");
    }

    #[test]
    fn captcha_page_returns_no_results() {
        assert!(parse_html(
            "<div class=\"captcha_header\">Verify</div>",
            &SearchPolicy::default()
        )
        .is_empty());
    }
}
