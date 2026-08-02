use bytes::BytesMut;
use memchr::{memchr, memchr_iter};
use std::hint::black_box;
use std::time::{Duration, Instant};

fn old_sse_extract(input: &[u8]) -> usize {
    let mut buffer = input.to_vec();
    let mut checksum = 0usize;
    while let Some(position) = buffer.iter().position(|value| *value == b'\n') {
        let line = buffer.drain(..position + 1).collect::<Vec<_>>();
        checksum = checksum.wrapping_add(line.len());
    }
    checksum.wrapping_add(buffer.len())
}

fn new_sse_extract(input: &[u8]) -> usize {
    let mut buffer = BytesMut::from(input);
    let mut checksum = 0usize;
    while let Some(position) = memchr(b'\n', buffer.as_ref()) {
        let line = buffer.split_to(position + 1);
        checksum = checksum.wrapping_add(line.len());
    }
    checksum.wrapping_add(buffer.len())
}

fn old_tool_lookup<'a>(name: &str, tools: &'a [String]) -> Option<&'a str> {
    let normalized = name.to_ascii_lowercase();
    tools
        .iter()
        .find(|tool| tool.to_ascii_lowercase() == normalized)
        .map(String::as_str)
}

fn new_tool_lookup<'a>(name: &str, tools: &'a [String]) -> Option<&'a str> {
    tools
        .iter()
        .find(|tool| tool.eq_ignore_ascii_case(name))
        .map(String::as_str)
}

fn old_bracket_scan(input: &str) -> usize {
    input.match_indices('[').map(|(index, _)| index).sum()
}

fn new_bracket_scan(input: &str) -> usize {
    memchr_iter(b'[', input.as_bytes()).sum()
}

fn measure(mut operation: impl FnMut() -> usize, iterations: usize) -> (Duration, usize) {
    let started = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..iterations {
        checksum = checksum.wrapping_add(black_box(operation()));
    }
    (started.elapsed(), checksum)
}

fn ratio(old: Duration, new: Duration) -> f64 {
    old.as_secs_f64() / new.as_secs_f64()
}

fn main() {
    let mut sse = Vec::with_capacity(512 * 1024);
    for index in 0..2_500 {
        sse.extend_from_slice(
            format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"line-{index}\"}},\"finish_reason\":null}}]}}\n\n"
            )
            .as_bytes(),
        );
    }
    let sse_iterations = 20;
    let (old_sse, old_checksum) = measure(|| old_sse_extract(black_box(&sse)), sse_iterations);
    let (new_sse, new_checksum) = measure(|| new_sse_extract(black_box(&sse)), sse_iterations);
    assert_eq!(old_checksum, new_checksum);

    let tools = (0..128)
        .map(|index| format!("ToolName{index:03}"))
        .collect::<Vec<_>>();
    let tool_iterations = 300_000;
    let (old_tool, old_tool_checksum) = measure(
        || {
            old_tool_lookup(black_box("toolname127"), black_box(&tools))
                .unwrap()
                .len()
        },
        tool_iterations,
    );
    let (new_tool, new_tool_checksum) = measure(
        || {
            new_tool_lookup(black_box("toolname127"), black_box(&tools))
                .unwrap()
                .len()
        },
        tool_iterations,
    );
    assert_eq!(old_tool_checksum, new_tool_checksum);

    let bracket_input = (0..20_000)
        .map(|index| format!("plain text {index} [candidate] more text\n"))
        .collect::<String>();
    let bracket_iterations = 300;
    let (old_bracket, old_bracket_checksum) = measure(
        || old_bracket_scan(black_box(&bracket_input)),
        bracket_iterations,
    );
    let (new_bracket, new_bracket_checksum) = measure(
        || new_bracket_scan(black_box(&bracket_input)),
        bracket_iterations,
    );
    assert_eq!(old_bracket_checksum, new_bracket_checksum);

    println!(
        "sse_buffer old={old_sse:?} new={new_sse:?} speedup={:.2}x",
        ratio(old_sse, new_sse)
    );
    println!(
        "tool_lookup old={old_tool:?} new={new_tool:?} speedup={:.2}x",
        ratio(old_tool, new_tool)
    );
    println!(
        "bracket_scan old={old_bracket:?} new={new_bracket:?} speedup={:.2}x",
        ratio(old_bracket, new_bracket)
    );
}
