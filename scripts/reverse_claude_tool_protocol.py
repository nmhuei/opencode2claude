#!/usr/bin/env python3
"""Dynamic Claude Code tool-protocol reverse harness.

Compares:
1. Fake Anthropic native tool_use -> Claude Code directly.
2. Fake Anthropic text marker -> Claude Code directly (expected inert text).
3. Fake OpenAI text markers -> opencode2api -> Claude Code.

The lifecycle creates, lists, deletes, then lists a harmless session-only cron.
All request bodies, Claude stream-json, bridge logs, and summary telemetry are
written below artifacts/claude-tool-protocol-reverse/dynamic/.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import threading
import time
import uuid
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts" / "claude-tool-protocol-reverse" / "dynamic"
CLAUDE = Path(shutil.which("claude") or "/home/light/.local/bin/claude")
BRIDGE = ROOT / "target" / "debug" / "opencode2api-serve"
ANTHROPIC_PORT = 4140
OPENAI_PORT = 4141
BRIDGE_PORT = 4142
RAW_REQUESTING = "[Requesting"
RAW_CREATING = "[Creating"


def flatten_strings(value: Any) -> list[str]:
    if isinstance(value, str):
        return [value]
    if isinstance(value, list):
        result: list[str] = []
        for item in value:
            result.extend(flatten_strings(item))
        return result
    if isinstance(value, dict):
        result = []
        for item in value.values():
            result.extend(flatten_strings(item))
        return result
    return []


def extract_job_id(value: Any) -> str | None:
    text = "\n".join(flatten_strings(value))
    patterns = [
        r"Scheduled recurring job\s+([A-Za-z0-9_-]+)",
        r"Scheduled one-shot task\s+([A-Za-z0-9_-]+)",
        r"(?:^|\n)([A-Za-z0-9_-]{4,})\s+—\s+",
        r'"id"\s*:\s*"([A-Za-z0-9_-]+)"',
    ]
    for pattern in patterns:
        match = re.search(pattern, text)
        if match:
            return match.group(1)
    return None


def has_tool_result(value: Any, token: str) -> bool:
    return token in "\n".join(flatten_strings(value))


def anthropic_sse(model: str, blocks: list[dict[str, Any]], stop_reason: str) -> bytes:
    events: list[tuple[str, dict[str, Any]]] = [
        (
            "message_start",
            {
                "type": "message_start",
                "message": {
                    "id": f"msg_fake_{uuid.uuid4().hex[:10]}",
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": None,
                    "stop_sequence": None,
                    "usage": {"input_tokens": 1, "output_tokens": 0},
                },
            },
        )
    ]
    for index, block in enumerate(blocks):
        if block["type"] == "text":
            events.extend(
                [
                    (
                        "content_block_start",
                        {
                            "type": "content_block_start",
                            "index": index,
                            "content_block": {"type": "text", "text": ""},
                        },
                    ),
                    (
                        "content_block_delta",
                        {
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {"type": "text_delta", "text": block["text"]},
                        },
                    ),
                    ("content_block_stop", {"type": "content_block_stop", "index": index}),
                ]
            )
        elif block["type"] == "tool_use":
            events.extend(
                [
                    (
                        "content_block_start",
                        {
                            "type": "content_block_start",
                            "index": index,
                            "content_block": {
                                "type": "tool_use",
                                "id": block["id"],
                                "name": block["name"],
                                "input": {},
                            },
                        },
                    ),
                    (
                        "content_block_delta",
                        {
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": json.dumps(
                                    block["input"], ensure_ascii=False, separators=(",", ":")
                                ),
                            },
                        },
                    ),
                    ("content_block_stop", {"type": "content_block_stop", "index": index}),
                ]
            )
    events.extend(
        [
            (
                "message_delta",
                {
                    "type": "message_delta",
                    "delta": {"stop_reason": stop_reason, "stop_sequence": None},
                    "usage": {"output_tokens": 1},
                },
            ),
            ("message_stop", {"type": "message_stop"}),
        ]
    )
    return "".join(
        f"event: {name}\ndata: {json.dumps(payload, ensure_ascii=False, separators=(',', ':'))}\n\n"
        for name, payload in events
    ).encode()


def openai_sse(delta: dict[str, Any], finish_reason: str) -> bytes:
    first = {"choices": [{"delta": delta, "finish_reason": finish_reason}]}
    return (
        f"data: {json.dumps(first, ensure_ascii=False, separators=(',', ':'))}\n\n"
        "data: [DONE]\n\n"
    ).encode()


@dataclass
class ScenarioState:
    name: str
    requests: list[dict[str, Any]] = field(default_factory=list)
    tool_turn: int = 0
    job_id: str | None = None

    def record(self, path: str, headers: Any, body: Any) -> None:
        self.requests.append(
            {
                "path": path,
                "headers": {key.lower(): value for key, value in headers.items()},
                "body": body,
                "received_at": time.time(),
            }
        )
        self.job_id = self.job_id or extract_job_id(body)


class QuietHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args: object) -> None:
        return

    def read_json(self) -> Any:
        length = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            return json.loads(raw)
        except Exception:
            return {"_raw": raw.decode(errors="replace")}

    def send_bytes(self, status: int, content_type: str, payload: bytes, chunk: int = 0) -> None:
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("cache-control", "no-cache")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        if chunk <= 0:
            self.wfile.write(payload)
            return
        for start in range(0, len(payload), chunk):
            self.wfile.write(payload[start : start + chunk])
            self.wfile.flush()


class FakeAnthropicHandler(QuietHandler):
    state: ScenarioState
    mode: str

    def do_HEAD(self) -> None:  # noqa: N802
        self.send_response(200)
        self.send_header("content-length", "0")
        self.end_headers()

    def do_POST(self) -> None:  # noqa: N802
        body = self.read_json()
        self.state.record(self.path, self.headers, body)
        if self.path.split("?", 1)[0].endswith("/count_tokens"):
            self.send_bytes(200, "application/json", b'{"input_tokens":1}')
            return
        model = body.get("model", "fake") if isinstance(body, dict) else "fake"
        tools = body.get("tools", []) if isinstance(body, dict) else []
        if not tools:
            self.send_bytes(200, "text/event-stream", anthropic_sse(model, [{"type": "text", "text": "Audit session"}], "end_turn"))
            return
        if self.mode == "text-marker":
            marker = '[Requesting CronCreate: {"cron":"*/30 * * * *","prompt":"write CRON_PARSE_VERIFY_OK","recurring":true}]'
            self.send_bytes(200, "text/event-stream", anthropic_sse(model, [{"type": "text", "text": marker}], "end_turn"), chunk=3)
            return
        turn = self.state.tool_turn
        self.state.tool_turn += 1
        if turn == 0:
            blocks = [{"type": "tool_use", "id": "toolu_direct_create", "name": "CronCreate", "input": {"cron": "*/30 * * * *", "prompt": "write CRON_PARSE_VERIFY_OK", "recurring": True}}]
            payload = anthropic_sse(model, blocks, "tool_use")
        elif turn == 1:
            payload = anthropic_sse(model, [{"type": "tool_use", "id": "toolu_direct_list_before", "name": "CronList", "input": {}}], "tool_use")
        elif turn == 2:
            job_id = self.state.job_id or extract_job_id(body) or "missing-job-id"
            payload = anthropic_sse(model, [{"type": "tool_use", "id": "toolu_direct_delete", "name": "CronDelete", "input": {"id": job_id}}], "tool_use")
        elif turn == 3:
            payload = anthropic_sse(model, [{"type": "tool_use", "id": "toolu_direct_list_after", "name": "CronList", "input": {}}], "tool_use")
        else:
            payload = anthropic_sse(model, [{"type": "text", "text": "DIRECT_NATIVE_LIFECYCLE_OK"}], "end_turn")
        self.send_bytes(200, "text/event-stream", payload, chunk=5)


class FakeOpenAiHandler(QuietHandler):
    state: ScenarioState

    def do_POST(self) -> None:  # noqa: N802
        body = self.read_json()
        self.state.record(self.path, self.headers, body)
        tools = body.get("tools", []) if isinstance(body, dict) else []
        if not tools:
            self.send_bytes(200, "text/event-stream", openai_sse({"content": "Audit session"}, "stop"))
            return
        turn = self.state.tool_turn
        self.state.tool_turn += 1
        if turn == 0:
            marker = '[Requesting CronCreate: {"cron":"*/30 * * * *","prompt":"write CRON_PARSE_VERIFY_OK","recurring":true}]'
            payload = openai_sse({"content": f"Cron đã được tạo thành công.\n{marker}"}, "stop")
        elif turn == 1:
            payload = openai_sse({"reasoning_content": "[Requesting CronList: {}]"}, "stop")
        elif turn == 2:
            job_id = self.state.job_id or extract_job_id(body) or "missing-job-id"
            marker = f'[Requesting CronDelete: {{"id":"{job_id}"}}]'
            payload = openai_sse({"content": marker}, "stop")
        elif turn == 3:
            payload = openai_sse({"content": "[Requesting CronList: {}]"}, "stop")
        else:
            payload = openai_sse({"content": "BRIDGE_MARKER_LIFECYCLE_OK"}, "stop")
        self.send_bytes(200, "text/event-stream", payload, chunk=1)


def make_server(port: int, handler: type[BaseHTTPRequestHandler]) -> tuple[ThreadingHTTPServer, threading.Thread]:
    server = ThreadingHTTPServer(("127.0.0.1", port), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, thread


def claude_settings(base_url: str, directory: Path) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    settings = {
        "model": "claude-sonnet-4-6",
        "alwaysThinkingEnabled": True,
        "env": {
            "ANTHROPIC_BASE_URL": base_url,
            "ANTHROPIC_API_KEY": "reverse-key",
            "CLAUDE_CODE_DISABLE_1M_CONTEXT": "1",
            "CLAUDE_CODE_DISABLE_THINKING": "0",
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000",
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "32000",
            "MAX_THINKING_TOKENS": "31000",
        },
    }
    path = directory / "settings.json"
    path.write_text(json.dumps(settings, ensure_ascii=False, indent=2) + "\n")
    return path


def claude_supports(option: str) -> bool:
    """Return whether the installed Claude Code exposes a CLI option."""
    proc = subprocess.run(
        [str(CLAUDE), "--help"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    return option in proc.stdout


def run_claude(name: str, base_url: str, max_turns: int) -> dict[str, Any]:
    profile = OUT / "profiles" / name
    work = OUT / "work" / name
    profile.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    settings = claude_settings(base_url, profile)
    cmd = [
        str(CLAUDE),
        "-p",
        "Create the harmless requested session cron, verify it exists, then delete it and verify cleanup. Do not use Bash.",
        "--settings",
        str(settings),
        "--setting-sources",
        "user",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]
    if claude_supports("--max-turns"):
        cmd += ["--max-turns", str(max_turns)]
    cmd += [
        "--tools",
        "CronCreate,CronList,CronDelete",
        "--allowedTools",
        "CronCreate,CronList,CronDelete",
        "--permission-mode",
        "bypassPermissions",
        "--session-id",
        str(uuid.uuid4()),
    ]
    env = os.environ.copy()
    env["CLAUDE_CONFIG_DIR"] = str(profile)
    started = time.monotonic()
    proc = subprocess.run(
        cmd,
        cwd=work,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=120,
        check=False,
    )
    elapsed_ms = int((time.monotonic() - started) * 1000)
    (OUT / f"{name}.stdout.jsonl").write_text(proc.stdout)
    (OUT / f"{name}.stderr.txt").write_text(proc.stderr)
    (OUT / f"{name}.command.json").write_text(json.dumps(cmd, indent=2) + "\n")
    return summarize_claude(name, proc.returncode, elapsed_ms, proc.stdout, proc.stderr)


def parse_jsonl(text: str) -> list[dict[str, Any]]:
    result = []
    for line in text.splitlines():
        try:
            value = json.loads(line)
        except Exception:
            continue
        if isinstance(value, dict):
            result.append(value)
    return result


def summarize_claude(name: str, exit_code: int, elapsed_ms: int, stdout: str, stderr: str) -> dict[str, Any]:
    events = parse_jsonl(stdout)
    tool_uses: list[dict[str, Any]] = []
    tool_results: list[dict[str, Any]] = []
    for event in events:
        message = event.get("message")
        if not isinstance(message, dict) or not isinstance(message.get("content"), list):
            continue
        for block in message["content"]:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_use":
                tool_uses.append({"id": block.get("id"), "name": block.get("name"), "input": block.get("input")})
            elif block.get("type") == "tool_result":
                tool_results.append({"tool_use_id": block.get("tool_use_id"), "content": block.get("content"), "is_error": block.get("is_error")})
    ids = [str(item.get("id") or "") for item in tool_uses]
    final = next((event.get("result", "") for event in reversed(events) if event.get("type") == "result"), "")
    return {
        "name": name,
        "exit_code": exit_code,
        "elapsed_ms": elapsed_ms,
        "tool_uses": tool_uses,
        "tool_results": tool_results,
        "tool_use_count": len(tool_uses),
        "tool_result_count": len(tool_results),
        "duplicate_tool_ids": len(ids) - len(set(ids)),
        "raw_requesting_count": stdout.count(RAW_REQUESTING) + stderr.count(RAW_REQUESTING),
        "raw_creating_count": stdout.count(RAW_CREATING) + stderr.count(RAW_CREATING),
        "false_success_count": stdout.lower().count("tạo thành công") + stdout.lower().count("scheduled successfully"),
        "final": final,
        "stderr_tail": stderr[-1000:],
    }


def wait_health(url: str, timeout: float = 15.0) -> None:
    deadline = time.monotonic() + timeout
    last: Exception | None = None
    while time.monotonic() < deadline:
        try:
            with urlopen(url, timeout=1) as response:
                if response.status == 200:
                    return
        except Exception as error:  # noqa: PERF203
            last = error
        time.sleep(0.1)
    raise RuntimeError(f"health timeout: {last}")


def run_direct(mode: str, name: str) -> tuple[dict[str, Any], ScenarioState]:
    state = ScenarioState(name)
    handler = type(f"Anthropic_{name}", (FakeAnthropicHandler,), {"state": state, "mode": mode})
    server, thread = make_server(ANTHROPIC_PORT, handler)
    try:
        summary = run_claude(name, f"http://127.0.0.1:{ANTHROPIC_PORT}", 8)
    finally:
        server.shutdown()
        thread.join(timeout=2)
        server.server_close()
    (OUT / f"{name}.requests.json").write_text(json.dumps(state.requests, ensure_ascii=False, indent=2) + "\n")
    return summary, state


def run_bridge() -> tuple[dict[str, Any], ScenarioState, dict[str, Any]]:
    state = ScenarioState("bridge-marker")
    handler = type("OpenAiBridgeScenario", (FakeOpenAiHandler,), {"state": state})
    server, thread = make_server(OPENAI_PORT, handler)
    config_path = OUT / "bridge-marker.toml"
    config_path.write_text(
        "\n".join(
            [
                "schema_version = 1",
                f"port = {BRIDGE_PORT}",
                'host = "127.0.0.1"',
                'model = "fixture-model"',
                "auth_tokens = []",
                f'upstream_base_url = "http://127.0.0.1:{OPENAI_PORT}/v1"',
                'egress_mode = "direct"',
                "primary_proxies = []",
                "warm_standby_proxies = []",
                "require_verified_exit_ip = false",
                "history_enabled = false",
                "max_network_attempts = 1",
                "max_provider_attempts = 1",
                "retry_base_backoff_ms = 0",
                "retry_max_backoff_ms = 0",
                "",
            ]
        )
    )
    env = os.environ.copy()
    for key in [
        "BRIDGE_AUTH_TOKEN",
        "BRIDGE_PRIMARY_PROXIES",
        "BRIDGE_WARM_STANDBY_PROXIES",
        "BRIDGE_PROXIES",
    ]:
        env.pop(key, None)
    env["RUST_LOG"] = "opencode2api=trace"
    log_path = OUT / "bridge-marker.bridge.log"
    with log_path.open("w") as log:
        bridge = subprocess.Popen(
            [
                str(BRIDGE),
                "--config",
                str(config_path),
                "--host",
                "127.0.0.1",
                "--port",
                str(BRIDGE_PORT),
                "--model",
                "fixture-model",
            ],
            cwd=ROOT,
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
            text=True,
        )
    try:
        wait_health(f"http://127.0.0.1:{BRIDGE_PORT}/health")
        summary = run_claude("bridge-marker", f"http://127.0.0.1:{BRIDGE_PORT}", 10)
    finally:
        bridge.send_signal(signal.SIGTERM)
        try:
            bridge.wait(timeout=8)
        except subprocess.TimeoutExpired:
            bridge.kill()
            bridge.wait(timeout=3)
        server.shutdown()
        thread.join(timeout=2)
        server.server_close()
    (OUT / "bridge-marker.requests.json").write_text(json.dumps(state.requests, ensure_ascii=False, indent=2) + "\n")
    bridge_meta = {"exit_code": bridge.returncode, "log": str(log_path)}
    return summary, state, bridge_meta


def evaluate(summary: dict[str, Any], expected_names: list[str], final_token: str, expect_raw: bool) -> dict[str, bool]:
    names = [item.get("name") for item in summary["tool_uses"]]
    return {
        "exit_zero": summary["exit_code"] == 0,
        "expected_tools": names == expected_names,
        "tool_results_match": summary["tool_result_count"] == len(expected_names),
        "no_duplicate_tool_ids": summary["duplicate_tool_ids"] == 0,
        "raw_marker_expectation": (summary["raw_requesting_count"] > 0) == expect_raw,
        "no_creating_marker": summary["raw_creating_count"] == 0,
        "no_false_success": summary["false_success_count"] == 0,
        "final_token": final_token in summary["final"],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()
    shutil.rmtree(OUT, ignore_errors=True)
    OUT.mkdir(parents=True)
    if not args.skip_build:
        subprocess.run(["cargo", "build", "--locked", "--bin", "opencode2api-serve"], cwd=ROOT, check=True)

    direct_native, _ = run_direct("native", "direct-native")
    direct_marker, _ = run_direct("text-marker", "direct-text-marker")
    bridge_marker, _, bridge_meta = run_bridge()

    expected_lifecycle = ["CronCreate", "CronList", "CronDelete", "CronList"]
    checks = {
        "direct_native": evaluate(direct_native, expected_lifecycle, "DIRECT_NATIVE_LIFECYCLE_OK", False),
        "direct_text_marker": {
            "exit_zero": direct_marker["exit_code"] == 0,
            "no_tool_execution": direct_marker["tool_use_count"] == 0 and direct_marker["tool_result_count"] == 0,
            "raw_marker_visible_proves_cli_inert": direct_marker["raw_requesting_count"] > 0,
        },
        "bridge_marker": evaluate(bridge_marker, expected_lifecycle, "BRIDGE_MARKER_LIFECYCLE_OK", False),
    }
    payload = {
        "claude": subprocess.check_output([str(CLAUDE), "--version"], text=True).strip(),
        "direct_native": direct_native,
        "direct_text_marker": direct_marker,
        "bridge_marker": bridge_marker,
        "bridge_process": bridge_meta,
        "checks": checks,
        "pass": all(all(group.values()) for group in checks.values()),
    }
    (OUT / "summary.json").write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if payload["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
