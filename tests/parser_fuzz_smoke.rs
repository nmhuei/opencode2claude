//! Deterministic parser fuzz-smoke corpus for per-commit CI.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use opencode2api::config::{migration, BridgeConfig};
use opencode2api::opencode::sanitize::{
    extract_and_clean_dsml, parse_dsml_tool_calls, strip_system_tags,
};
use opencode2api::opencode::search::{format_search_context, SearchQuery, SearchResult};
use opencode2api::server::build_router;
use opencode2api::state::AppState;
use tower::ServiceExt;

fn corpus(seed: u64, cases: usize, max_len: usize) -> Vec<Vec<u8>> {
    let mut state = seed;
    (0..cases)
        .map(|index| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let len = ((state as usize) % max_len).max(index % 7);
            (0..len)
                .map(|offset| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1 + offset as u64);
                    (state >> 24) as u8
                })
                .collect()
        })
        .collect()
}

#[test]
fn text_config_dsml_and_search_parsers_survive_deterministic_corpus() {
    for bytes in corpus(0x5eed_cafe, 2_000, 4_096) {
        let input = String::from_utf8_lossy(&bytes);
        let _ = strip_system_tags(&input);
        let _ = parse_dsml_tool_calls(&input);
        let (clean, calls) = extract_and_clean_dsml(&input);
        assert!(clean.len() <= input.len().saturating_mul(3).saturating_add(16));
        assert!(calls.len() <= input.matches("<｜DSML｜invoke").count());

        let _ = migration::migrate_document(&input);
        let _ = SearchQuery::new(input.as_ref(), 5);
        let result = SearchResult::normalized(
            input.as_ref(),
            "https://example.com/fuzz",
            input.as_ref(),
            500,
        );
        if let Some(result) = result {
            let formatted = format_search_context(&[result], "empty");
            assert!(formatted.len() <= 2_000);
        }
    }
}

#[tokio::test]
async fn malformed_json_corpus_never_panics_or_reaches_upstream() {
    let state = AppState::new(BridgeConfig {
        primary_proxies: None,
        warm_standby_proxies: None,
        max_body_size: 8 * 1024,
        ..Default::default()
    });
    let app = build_router(state);

    for bytes in corpus(0xdec0_de01, 256, 2_048) {
        let mut malformed = b"{\"model\":\"fuzz\",\"messages\":\x00".to_vec();
        malformed.extend_from_slice(&bytes);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(malformed))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ));
    }
}
