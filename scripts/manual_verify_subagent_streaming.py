#!/usr/bin/env python3
from __future__ import annotations

import argparse
import fcntl
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts" / "subagent-stream-timing"
PROMPT = "fan sub agent search toàn bộ các skill hữu ích cho claude code trong việc làm ctf, pentest, nhìn chung là liên quan đến security"
MODEL = "claude-sonnet-4-6"
BASE_URL = "http://127.0.0.1:4000"
RAW_MARKER = "[" + "Requesting Tool execution:"
URL_RE = re.compile(r"https?://[^\s\"<>\\]+")
ANSI_RE = re.compile(r"\x1b\[[0-9;]*[mK]")


def settings() -> dict[str, Any]:
    return {
        "model": MODEL,
        "alwaysThinkingEnabled": True,
        "env": {
            "ANTHROPIC_BASE_URL": BASE_URL,
            "ANTHROPIC_API_KEY": "opencode-bridge",
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000",
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "128000",
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "200000",
            "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "90",
            "CLAUDE_CODE_DISABLE_1M_CONTEXT": "1",
            "CLAUDE_CODE_ALWAYS_ENABLE_EFFORT": "1",
            "MAX_THINKING_TOKENS": "127000",
        },
    }


def parse_events(path: Path) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    for line in path.read_text(errors="replace").splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(item, dict):
            events.append(item)
    return events


def tool_uses(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    found: list[dict[str, Any]] = []
    seen: set[tuple[str, str]] = set()
    for event in events:
        if event.get("type") != "assistant":
            continue
        message = event.get("message")
        if not isinstance(message, dict):
            continue
        for block in message.get("content") or []:
            if not isinstance(block, dict) or block.get("type") != "tool_use":
                continue
            key = (str(block.get("name") or ""), str(block.get("id") or ""))
            if key in seen:
                continue
            seen.add(key)
            found.append(
                {
                    "name": key[0],
                    "id": key[1],
                    "input": block.get("input"),
                    "parent_tool_use_id": event.get("parent_tool_use_id"),
                    "timestamp": event.get("timestamp"),
                }
            )
    return found


def tool_results(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    found: list[dict[str, Any]] = []
    for event in events:
        if event.get("type") != "user":
            continue
        message = event.get("message")
        if not isinstance(message, dict):
            continue
        for block in message.get("content") or []:
            if isinstance(block, dict) and block.get("type") == "tool_result":
                found.append(
                    {
                        "tool_use_id": block.get("tool_use_id"),
                        "is_error": block.get("is_error") is True,
                        "content_preview": str(block.get("content"))[:500],
                        "parent_tool_use_id": event.get("parent_tool_use_id"),
                        "timestamp": event.get("timestamp"),
                    }
                )
    return found


def stream_stats(events: list[dict[str, Any]], timed_path: Path) -> dict[str, Any]:
    deltas = []
    text_deltas = []
    for event in events:
        if event.get("type") != "stream_event" or not isinstance(event.get("event"), dict):
            continue
        wire = event["event"]
        if wire.get("type") == "content_block_delta":
            deltas.append(wire)
            delta = wire.get("delta")
            if isinstance(delta, dict) and delta.get("type") == "text_delta":
                text_deltas.append(delta)

    elapsed: list[float] = []
    for line in timed_path.read_text(errors="replace").splitlines():
        try:
            record = json.loads(line)
            event = json.loads(record.get("line", "{}"))
        except (json.JSONDecodeError, TypeError):
            continue
        wire = event.get("event") if isinstance(event, dict) else None
        if (
            event.get("type") == "stream_event"
            and isinstance(wire, dict)
            and wire.get("type") == "content_block_delta"
            and isinstance(wire.get("delta"), dict)
            and wire["delta"].get("type") == "text_delta"
        ):
            elapsed.append(float(record.get("elapsed_ms", 0)))
    return {
        "content_block_delta_count": len(deltas),
        "text_delta_count": len(text_deltas),
        "first_text_delta_ms": min(elapsed) if elapsed else None,
        "last_text_delta_ms": max(elapsed) if elapsed else None,
        "text_delta_span_ms": (max(elapsed) - min(elapsed)) if len(elapsed) > 1 else 0,
    }


def bridge_stats(log_path: Path, start_offset: int = 0) -> dict[str, Any]:
    if log_path.exists():
        with log_path.open("r", errors="replace") as handle:
            handle.seek(start_offset)
            text = ANSI_RE.sub("", handle.read())
    else:
        text = ""
    query_pattern = re.compile(r"Intercepted stream search tool call query=(.*?) used_fallback=")
    queries = query_pattern.findall(text)
    upstream_times = re.findall(
        r"^(\S+) .*stream_timing upstream_sse_bytes_received", text, re.MULTILINE
    )
    downstream_times = re.findall(
        r"^(\S+) .*stream_timing anthropic_(?:text|thinking)_delta_enqueued",
        text,
        re.MULTILINE,
    )
    return {
        "search_queries": queries,
        "distinct_search_queries": sorted(set(queries)),
        "upstream_sse_chunk_count": len(upstream_times),
        "downstream_delta_enqueue_count": len(downstream_times),
        "first_upstream_timestamp": upstream_times[0] if upstream_times else None,
        "first_downstream_timestamp": downstream_times[0] if downstream_times else None,
        "search_loop_protection_seen": "search_loop_protection" in text,
        "bridge_api_error_seen": bool(re.search(r"event=.?error|api_error", text, re.I)),
    }


def run(label: str, bridge_log: Path) -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    bridge_log_start = bridge_log.stat().st_size if bridge_log.exists() else 0
    profile = OUT / f"profile-{label}"
    workspace = Path(f"/tmp/opencode2api-subagent-{label}")
    shutil.rmtree(profile, ignore_errors=True)
    shutil.rmtree(workspace, ignore_errors=True)
    profile.mkdir(parents=True)
    workspace.mkdir(parents=True)
    settings_path = profile / "settings.json"
    settings_path.write_text(json.dumps(settings(), ensure_ascii=False, indent=2) + "\n")

    stdout_path = OUT / f"claude-{label}.stdout.jsonl"
    timed_path = OUT / f"claude-{label}.timed.jsonl"
    stderr_path = OUT / f"claude-{label}.stderr"
    command_path = OUT / f"command-{label}.json"
    summary_path = OUT / f"summary-{label}.json"
    report_path = OUT / f"REPORT-{label}.md"

    cmd = [
        shutil.which("claude") or "/home/light/.local/share/claude/versions/2.1.216",
        "-p",
        PROMPT,
        "--model",
        MODEL,
        "--settings",
        str(settings_path),
        "--setting-sources",
        "user",
        "--max-turns",
        "20",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--tools",
        "Agent,WebSearch",
        "--allowedTools",
        "Agent,WebSearch",
        "--permission-mode",
        "bypassPermissions",
        "--effort",
        "max",
    ]
    command_path.write_text(json.dumps(cmd, ensure_ascii=False, indent=2) + "\n")
    (OUT / f"prompt-{label}.txt").write_text(PROMPT + "\n")

    env = os.environ.copy()
    env["CLAUDE_CONFIG_DIR"] = str(profile)
    started = time.monotonic()
    process = subprocess.Popen(
        cmd,
        cwd=workspace,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=1,
    )
    assert process.stdout is not None
    with stdout_path.open("w") as raw, timed_path.open("w") as timed:
        for line in process.stdout:
            raw.write(line)
            raw.flush()
            timed.write(
                json.dumps(
                    {
                        "received_epoch_ms": round(time.time() * 1000, 3),
                        "elapsed_ms": round((time.monotonic() - started) * 1000, 3),
                        "line": line.rstrip("\n"),
                    },
                    ensure_ascii=False,
                )
                + "\n"
            )
            timed.flush()
    assert process.stderr is not None
    stderr = process.stderr.read()
    exit_code = process.wait(timeout=30)
    stderr_path.write_text(stderr)

    events = parse_events(stdout_path)
    uses = tool_uses(events)
    results = tool_results(events)
    agent_uses = [item for item in uses if item["name"] == "Agent"]
    result_events = [event for event in events if event.get("type") == "result"]
    top_level_results = [event for event in result_events if not event.get("origin")]
    successful_results = [event for event in top_level_results if event.get("is_error") is False]
    actual_errors = [
        event
        for event in events
        if event.get("type") == "stream_event"
        and isinstance(event.get("event"), dict)
        and event["event"].get("type") == "error"
    ]
    entire_output = stdout_path.read_text(errors="replace") + "\n" + stderr
    urls = sorted(set(URL_RE.findall(entire_output)))
    stream = stream_stats(events, timed_path)
    bridge = bridge_stats(bridge_log, bridge_log_start)
    agent_result_ids = {str(item["tool_use_id"]) for item in results if not item["is_error"]}
    successful_agent_results = [item for item in agent_uses if item["id"] in agent_result_ids]

    checks = {
        "process_exit_zero": exit_code == 0,
        "successful_final_result": bool(successful_results)
        and bool(top_level_results)
        and not top_level_results[-1].get("is_error", True),
        "multiple_agents": len(agent_uses) >= 2,
        "agents_received_results": len(successful_agent_results) >= 2,
        "multiple_distinct_web_queries": len(bridge["distinct_search_queries"]) >= 2,
        "real_source_urls": len(urls) >= 2,
        "no_raw_tool_marker": RAW_MARKER not in entire_output,
        "no_search_loop_protection": not bridge["search_loop_protection_seen"],
        "no_api_error_event": not actual_errors,
        "many_content_deltas": stream["content_block_delta_count"] >= 10,
        "text_streamed_over_time": stream["text_delta_count"] >= 2 and stream["text_delta_span_ms"] > 100,
    }
    summary = {
        "label": label,
        "prompt": PROMPT,
        "workspace": str(workspace),
        "exit_code": exit_code,
        "elapsed_ms": round((time.monotonic() - started) * 1000, 3),
        "claude_version": subprocess.check_output([cmd[0], "--version"], text=True).strip(),
        "tool_uses": uses,
        "tool_results": results,
        "agent_use_count": len(agent_uses),
        "successful_agent_result_count": len(successful_agent_results),
        "urls": urls,
        "stream": stream,
        "bridge": bridge,
        "actual_error_events": actual_errors,
        "result_events": result_events,
        "top_level_result_events": top_level_results,
        "checks": checks,
        "passed": all(checks.values()),
    }
    summary_path.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")

    lines = [
        "# Claude Code Subagent Streaming Verification",
        "",
        f"- Prompt: `{PROMPT}`",
        f"- Claude Code: `{summary['claude_version']}`",
        f"- Workspace: `{workspace}`",
        f"- Result: **{'PASS' if summary['passed'] else 'FAIL'}**",
        "",
        "## Checks",
        "",
    ]
    lines.extend(f"- {'PASS' if value else 'FAIL'} — `{name}`" for name, value in checks.items())
    lines += [
        "",
        "## Evidence",
        "",
        f"- Agent calls: {len(agent_uses)}",
        f"- Successful agent results: {len(successful_agent_results)}",
        f"- Distinct WebSearch queries: {len(bridge['distinct_search_queries'])}",
        f"- URLs: {len(urls)}",
        f"- `content_block_delta`: {stream['content_block_delta_count']}",
        f"- Text deltas: {stream['text_delta_count']}",
        f"- Text delta span: {stream['text_delta_span_ms']:.1f} ms",
        f"- Upstream SSE chunks traced: {bridge['upstream_sse_chunk_count']}",
        f"- Downstream deltas traced: {bridge['downstream_delta_enqueue_count']}",
        "",
        "Raw `stream-json`, timestamped lines, bridge trace, command, and summary are stored beside this report.",
    ]
    report_path.write_text("\n".join(lines) + "\n")
    print(json.dumps({
        "passed": summary["passed"],
        "checks": checks,
        "agent_calls": len(agent_uses),
        "agent_results": len(successful_agent_results),
        "queries": bridge["distinct_search_queries"],
        "url_count": len(urls),
        "stream": stream,
        "report": str(report_path),
        "summary": str(summary_path),
    }, ensure_ascii=False, indent=2))
    return 0 if summary["passed"] else 1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--label", default="run3")
    parser.add_argument("--bridge-log", type=Path, required=True)
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    lock_path = OUT / f".{args.label}.lock"
    with lock_path.open("w") as lock_file:
        try:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(f"verification label {args.label!r} is already running")
            return 75
        return run(args.label, args.bridge_log)


if __name__ == "__main__":
    raise SystemExit(main())
