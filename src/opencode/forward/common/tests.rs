use super::*;

#[cfg(test)]
mod dsml_argument_tests {
    use super::*;
    use crate::handlers::AnthropicTool;

    #[test]
    fn coerces_dsml_values_from_the_matching_tool_schema() {
        let payload = MessagesRequest {
            tools: Some(vec![AnthropicTool {
                name: "Edit".to_string(),
                description: "edit a file".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "replace_all": {"type": "boolean"},
                        "timeout": {"type": "integer"},
                        "items": {"type": "array", "items": {"type": "integer"}}
                    }
                }),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let arguments = serde_json::json!({
            "command": {"key": "value"},
            "replace_all": "true",
            "timeout": "1500",
            "items": "[1,\"2\"]"
        });

        let normalized = normalize_dsml_arguments("edit", arguments, &payload);
        assert_eq!(normalized["command"], "{\"key\":\"value\"}");
        assert_eq!(normalized["replace_all"], true);
        assert_eq!(normalized["timeout"], 1500);
        assert_eq!(normalized["items"], serde_json::json!([1, 2]));
    }

    #[test]
    fn leaves_arguments_unchanged_without_a_matching_schema() {
        let payload = MessagesRequest::default();
        let arguments = serde_json::json!({"flag": "true"});
        assert_eq!(
            normalize_dsml_arguments("Unknown", arguments.clone(), &payload),
            arguments
        );
    }
}

#[cfg(test)]
mod compat_parser_invariant_tests {
    use super::*;

    #[test]
    fn strict_valid_json_is_semantically_preserved() {
        let values = [
            serde_json::json!({
                "command": "printf 'a\\|b'",
                "nested": {"x": [1, true, null]}
            }),
            serde_json::json!({"items": [{"path": "a"}, {"path": "b"}]}),
            serde_json::json!({
                "unicode": "Tiếng Việt 日本語 🦀",
                "quote": "a \" b"
            }),
        ];

        for value in values {
            let marker = format!(
                "[Requesting Tool execution: 'TestTool' with arguments: {}]",
                serde_json::to_string(&value).unwrap()
            );
            let parsed = parse_compat_tool_requests_with_consumed(&marker)
                .expect("strict JSON marker should parse");
            assert_eq!(parsed.calls.len(), 1);
            assert_eq!(parsed.calls[0].arguments, value);
            assert_eq!(parsed.consumed, marker.len());
            assert!(marker.is_char_boundary(parsed.consumed));
        }
    }

    #[test]
    fn compat_single_call_arguments_must_be_objects() {
        for raw in [r#""ls""#, "123", r#"["ls"]"#] {
            let marker = format!("[Requesting Tool execution: 'Bash' with arguments: {raw}]");
            assert!(
                parse_compat_tool_requests_with_consumed(&marker).is_none(),
                "non-object compatibility arguments must be rejected: {raw}"
            );
        }

        let marker = r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"ls"}]"#;
        let parsed = parse_compat_tool_requests_with_consumed(marker)
            .expect("object compatibility arguments should remain supported");
        assert_eq!(parsed.calls.len(), 1);
        assert!(parsed.calls[0].arguments.is_object());
    }

    #[test]
    fn deterministic_marker_mutations_never_panic_or_violate_offsets() {
        let seeds = [
            r#"[Requesting Read with arguments: {"file_path":"/tmp/a"}]"#,
            r#"[Requesting TaskUpdate with arguments: {"taskId":"1"},{"taskId":"2"}]"#,
            r#"prefix ```text
[Requesting Read with arguments: {"file_path":"secret"}]
``` suffix"#,
            r#"[Requesting Bash with arguments: {"command":"grep 'a\|b' file"}]"#,
        ];
        let insertions = ['[', ']', '{', '}', '"', '\\', ',', '\n', '🦀'];

        for seed in seeds {
            let boundaries = seed
                .char_indices()
                .map(|(index, _)| index)
                .chain(std::iter::once(seed.len()))
                .collect::<Vec<_>>();
            let mut cases = vec![seed.to_string()];
            for window in boundaries.windows(2) {
                let start = window[0];
                let end = window[1];
                let mut deleted = seed.to_string();
                deleted.replace_range(start..end, "");
                cases.push(deleted);
                for insertion in insertions {
                    let mut inserted = seed.to_string();
                    inserted.insert(start, insertion);
                    cases.push(inserted);
                }
            }

            for case in cases {
                let outcome = std::panic::catch_unwind(|| {
                    let parsed = parse_compat_tool_requests_with_consumed(&case);
                    let extraction = extract_compat_tool_requests_detailed(&case);
                    (parsed, extraction)
                });
                let (parsed, extraction) = outcome.expect("parser must never panic");
                if let Some(parsed) = parsed {
                    assert!(parsed.consumed <= case.len());
                    assert!(case.is_char_boundary(parsed.consumed));
                    assert!(!parsed.calls.is_empty());
                    assert!(parsed.calls.len() <= MAX_COMPAT_BATCH_ITEMS);
                }
                assert!(extraction.calls.len() <= MAX_COMPAT_CALLS_PER_RESPONSE);
                assert!(
                    extraction.cleaned_text.len()
                        <= case.len().saturating_mul(3).saturating_add(32)
                );
            }
        }
    }

    #[test]
    fn compatibility_parser_enforces_argument_and_batch_limits() {
        let oversized = format!(
            "[Requesting Write with arguments: {{\"content\":\"{}\"}}]",
            "x".repeat(MAX_COMPAT_ARGUMENT_BYTES + 1)
        );
        assert!(parse_compat_tool_requests_with_consumed(&oversized).is_none());

        let batch = (0..=MAX_COMPAT_BATCH_ITEMS)
            .map(|index| format!("{{\"index\":{index}}}"))
            .collect::<Vec<_>>()
            .join(",");
        let marker = format!("[Requesting TaskUpdate with arguments: {batch}]");
        assert!(parse_compat_tool_requests_with_consumed(&marker).is_none());
        let extraction = extract_compat_tool_requests_detailed(&marker);
        assert!(extraction.calls.is_empty());
        assert!(extraction.malformed_intent);
    }
}
