#!/usr/bin/env python3
"""Audit Claude Code request modes against the Anthropic-compatible bridge.

The audit is intentionally self-contained: it starts a local capture server,
runs isolated Claude Code profiles, records the complete request body/header
shape, and validates the fields that opencode2api must parse and map.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
import json
import os
import select
import shutil
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CLI = Path(shutil.which("claude") or "/home/light/.local/share/claude/versions/2.1.217")
DEFAULT_OUT = ROOT / "artifacts" / "claude-cli-audit"
HOST = "127.0.0.1"
PORT = 4023

# Current OpenCode catalog values for opencode/deepseek-v4-flash-free.
MODEL_CONTEXT_TOKENS = 200_000
MODEL_OUTPUT_TOKENS = 128_000
# Fixed thinking must stay below max_tokens; adaptive + effort=max is preferred.
FIXED_THINKING_TOKENS = 127_000

CAPTURES: list[dict[str, Any]] = []
CURRENT_CASE = "unknown"
LOCK = threading.Lock()
OUT = DEFAULT_OUT


class CaptureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args: object) -> None:
        return

    def do_HEAD(self) -> None:  # noqa: N802
        self.send_response(200)
        self.send_header("content-length", "0")
        self.end_headers()

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(length) if length else b""
        try:
            body: Any = json.loads(raw or b"{}")
        except Exception:
            body = {"_raw": raw.decode("utf-8", errors="replace")}

        with LOCK:
            record = {
                "case": CURRENT_CASE,
                "method": "POST",
                "path": self.path,
                "headers": {k.lower(): v for k, v in self.headers.items()},
                "body": body,
            }
            CAPTURES.append(record)
            index = len(CAPTURES)
            (OUT / "requests" / f"{index:03d}-{CURRENT_CASE}.json").write_text(
                json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8"
            )

        if self.path.split("?", 1)[0].endswith("/count_tokens"):
            self._send_json(200, {"input_tokens": 1})
            return

        model = body.get("model", "capture") if isinstance(body, dict) else "capture"
        input_tokens = max(1, len(json.dumps(body, ensure_ascii=False)) // 4)
        output_config = body.get("output_config") if isinstance(body, dict) else None
        fmt = output_config.get("format") if isinstance(output_config, dict) else None
        schema = fmt.get("schema") if isinstance(fmt, dict) else None
        properties = schema.get("properties", {}) if isinstance(schema, dict) else {}
        if "title" in properties:
            text = '{"title":"Audit session"}'
        elif fmt:
            text = '{"ok":true}'
        else:
            text = "OK"

        if isinstance(body, dict) and body.get("stream"):
            events = [
                ("message_start", {"type": "message_start", "message": {
                    "id": "msg_audit", "type": "message", "role": "assistant",
                    "model": model, "content": [], "stop_reason": None,
                    "stop_sequence": None,
                    "usage": {"input_tokens": input_tokens, "output_tokens": 0},
                }}),
                ("content_block_start", {"type": "content_block_start", "index": 0,
                    "content_block": {"type": "text", "text": ""}}),
                ("content_block_delta", {"type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": text}}),
                ("content_block_stop", {"type": "content_block_stop", "index": 0}),
                ("message_delta", {"type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                    "usage": {"output_tokens": 1}}),
                ("message_stop", {"type": "message_stop"}),
            ]
            encoded = "".join(
                f"event: {name}\ndata: {json.dumps(event, separators=(',', ':'))}\n\n"
                for name, event in events
            ).encode()
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("cache-control", "no-cache")
            self.send_header("content-length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)
            return

        self._send_json(200, {
            "id": "msg_audit", "type": "message", "role": "assistant",
            "model": model, "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn", "stop_sequence": None,
            "usage": {"input_tokens": input_tokens, "output_tokens": 1},
        })

    def _send_json(self, status: int, payload: Any) -> None:
        encoded = json.dumps(payload, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


@dataclass
class Case:
    name: str
    prompt: str = "Respond with only OK"
    cli_args: list[str] = field(default_factory=list)
    env: dict[str, str] = field(default_factory=dict)
    settings: dict[str, Any] = field(default_factory=dict)
    stdin_jsonl: list[dict[str, Any]] | None = None
    skill_text: str | None = None
    max_turns: int = 2


def base_settings() -> dict[str, Any]:
    return {
        "model": "claude-sonnet-4-6",
        "alwaysThinkingEnabled": True,
        "env": {
            "ANTHROPIC_BASE_URL": f"http://{HOST}:{PORT}",
            "ANTHROPIC_API_KEY": "audit-key",
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS": str(MODEL_CONTEXT_TOKENS),
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS": str(MODEL_OUTPUT_TOKENS),
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW": str(MODEL_CONTEXT_TOKENS),
            "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "90",
            "CLAUDE_CODE_DISABLE_1M_CONTEXT": "1",
            "CLAUDE_CODE_DISABLE_THINKING": "0",
            "CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING": "0",
            "CLAUDE_CODE_ALWAYS_ENABLE_EFFORT": "1",
            "MAX_THINKING_TOKENS": str(FIXED_THINKING_TOKENS),
        },
    }


def deep_merge(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    result = json.loads(json.dumps(base))
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(result.get(key), dict):
            result[key] = deep_merge(result[key], value)
        else:
            result[key] = value
    return result


def cases() -> list[Case]:
    schema = json.dumps({
        "type": "object",
        "properties": {"ok": {"type": "boolean"}},
        "required": ["ok"],
        "additionalProperties": False,
    }, separators=(",", ":"))
    stream_message = {
        "type": "user",
        "message": {"role": "user", "content": [{"type": "text", "text": "Respond with only OK"}]},
    }
    return [
        Case("baseline"),
        Case("thinking_disabled", settings={"env": {"CLAUDE_CODE_DISABLE_THINKING": "1"}}),
        Case("thinking_adaptive"),
        Case("thinking_fixed_max", settings={"env": {"CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING": "1"}}),
        Case("effort_low", cli_args=["--effort", "low"]),
        Case("effort_medium", cli_args=["--effort", "medium"]),
        Case("effort_high", cli_args=["--effort", "high"]),
        Case("effort_xhigh", cli_args=["--effort", "xhigh"]),
        Case("effort_max", cli_args=["--effort", "max"]),
        Case("effort_invalid_auto", cli_args=["--effort", "auto"]),
        Case("effort_invalid_ultracode", cli_args=["--effort", "ultracode"]),
        Case("extra_body", settings={"env": {"CLAUDE_CODE_EXTRA_BODY": json.dumps({
            "reasoning_effort": "max", "custom_probe": {"enabled": True},
        }, separators=(",", ":"))}}),
        Case("structured_json_schema", cli_args=["--json-schema", schema]),
        Case("system_prompt", cli_args=["--system-prompt", "SYSTEM_AUDIT_MARKER"]),
        Case("append_system_prompt", cli_args=["--append-system-prompt", "APPEND_AUDIT_MARKER"]),
        Case("tools_none", cli_args=["--tools", ""]),
        Case("tools_read", prompt="Read-only request-shape audit", cli_args=["--tools", "Read"]),
        Case("allowed_beta", cli_args=["--betas", "context-1m-2025-08-07"]),
        Case("stream_json", cli_args=[
            "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
        ], stdin_jsonl=[stream_message]),
        Case("prompt_suggestions", cli_args=[
            "--prompt-suggestions", "true", "--output-format", "stream-json", "--verbose",
        ]),
        Case(
            "autocompact_forced",
            prompt="",
            cli_args=[
                "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
                "--include-partial-messages",
            ],
            stdin_jsonl=[
                {"type": "user", "message": {"role": "user", "content": [
                    {"type": "text", "text": f"LONG_TURN_{index} " + ("token " * 30000)},
                ]}}
                for index in range(1, 5)
            ] + [
                {"type": "user", "message": {"role": "user", "content": [
                    {"type": "text", "text": "FINAL_TURN_MARKER reply with only OK"},
                ]}},
            ],
            max_turns=8,
            settings={"env": {
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "20000",
                "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "4096",
                "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "20000",
                "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "50",
                "CLAUDE_CODE_DISABLE_THINKING": "1",
                "MAX_THINKING_TOKENS": "1024",
            }},
        ),
        Case("skill_invocation", prompt="/audit-skill", skill_text=(
            "---\nname: audit-skill\ndescription: Request-shape audit skill.\n---\n\n"
            "Reply with exactly SKILL_AUDIT_OK.\n"
        )),
    ]


def content_text(value: Any) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return "\n".join(
            item.get("text", "") for item in value
            if isinstance(item, dict) and isinstance(item.get("text"), str)
        )
    return ""


def system_text(body: dict[str, Any]) -> str:
    return content_text(body.get("system"))


def run_case(cli: Path, case: Case) -> dict[str, Any]:
    global CURRENT_CASE
    CURRENT_CASE = case.name
    profile = OUT / "profiles" / case.name
    if profile.exists():
        shutil.rmtree(profile)
    profile.mkdir(parents=True)
    if case.skill_text:
        skill_dir = profile / "skills" / "audit-skill"
        skill_dir.mkdir(parents=True)
        (skill_dir / "SKILL.md").write_text(case.skill_text, encoding="utf-8")

    settings = deep_merge(base_settings(), case.settings)
    settings_path = OUT / "settings" / f"{case.name}.json"
    settings_path.write_text(json.dumps(settings, indent=2), encoding="utf-8")

    common = [str(cli), "-p"]
    if case.prompt:
        common.append(case.prompt)
    common += [
        "--model", "claude-sonnet-4-6",
        "--settings", str(settings_path),
        "--setting-sources", "user",
        "--max-turns", str(case.max_turns),
    ]
    if "--output-format" not in case.cli_args:
        common += ["--output-format", "json"]
    if "--tools" not in case.cli_args:
        common += ["--tools", ""]
    if "--effort" not in case.cli_args:
        common += ["--effort", "max"]
    cmd = common + case.cli_args

    env = os.environ.copy()
    env.update(case.env)
    env["CLAUDE_CONFIG_DIR"] = str(profile)
    stdin = None
    if case.stdin_jsonl is not None:
        stdin = "".join(json.dumps(item, separators=(",", ":")) + "\n" for item in case.stdin_jsonl)

    before = len(CAPTURES)
    started = time.monotonic()
    if case.name == "autocompact_forced" and case.stdin_jsonl is not None:
        child = subprocess.Popen(
            cmd, cwd=ROOT, env=env, text=True, bufsize=1,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        assert child.stdin is not None and child.stdout is not None and child.stderr is not None
        output_lines: list[str] = []
        for item in case.stdin_jsonl:
            child.stdin.write(json.dumps(item, separators=(",", ":")) + "\n")
            child.stdin.flush()
            deadline = time.monotonic() + 30
            assistant_seen = False
            while time.monotonic() < deadline:
                ready, _, _ = select.select([child.stdout], [], [], 0.25)
                if not ready:
                    if child.poll() is not None:
                        break
                    continue
                line = child.stdout.readline()
                if not line:
                    break
                output_lines.append(line)
                try:
                    event = json.loads(line)
                except Exception:
                    continue
                if event.get("type") == "stream_event":
                    stream_event = event.get("event") or {}
                    if stream_event.get("type") == "message_stop":
                        assistant_seen = True
                        break
                if event.get("type") == "assistant":
                    message = event.get("message") or {}
                    if message.get("stop_reason") is not None:
                        assistant_seen = True
                        break
                if event.get("type") == "result" and event.get("is_error"):
                    break
            if not assistant_seen:
                break
        child.stdin.close()
        output_lines.extend(child.stdout.readlines())
        stderr_text = child.stderr.read()
        returncode = child.wait(timeout=40)
        stdout_text = "".join(output_lines)
    else:
        proc = subprocess.run(
            cmd, cwd=ROOT, env=env, input=stdin, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            timeout=40, check=False,
        )
        returncode = proc.returncode
        stdout_text = proc.stdout
        stderr_text = proc.stderr
    elapsed_ms = int((time.monotonic() - started) * 1000)
    time.sleep(0.05)

    records = CAPTURES[before:]
    messages = [
        record for record in records
        if record["path"].split("?", 1)[0].endswith("/v1/messages")
    ]
    bodies = [record["body"] for record in messages if isinstance(record["body"], dict)]
    body = bodies[-1] if bodies else {}
    headers = messages[-1]["headers"] if messages else {}
    structured_format_seen = any(
        isinstance(candidate.get("output_config"), dict)
        and isinstance(candidate["output_config"].get("format"), dict)
        and candidate["output_config"]["format"].get("type") == "json_schema"
        for candidate in bodies
    )
    summary = {
        "case": case.name,
        "exit_code": returncode,
        "elapsed_ms": elapsed_ms,
        "stderr": stderr_text[-2000:],
        "stdout_tail": stdout_text[-12000:],
        "request_count": len(records),
        "message_request_count": len(messages),
        "request_keys": sorted(body.keys()),
        "model": body.get("model"),
        "max_tokens": body.get("max_tokens"),
        "thinking": body.get("thinking"),
        "output_config": body.get("output_config"),
        "reasoning_effort": body.get("reasoning_effort"),
        "context_management": body.get("context_management"),
        "service_tier": body.get("service_tier"),
        "metadata": body.get("metadata"),
        "tool_names": [tool.get("name") for tool in body.get("tools", []) if isinstance(tool, dict)],
        "tool_choice": body.get("tool_choice"),
        "anthropic_beta": headers.get("anthropic-beta"),
        "custom_probe": body.get("custom_probe"),
        "system_contains_system_marker": "SYSTEM_AUDIT_MARKER" in system_text(body),
        "system_contains_append_marker": "APPEND_AUDIT_MARKER" in system_text(body),
        "structured_format_seen": structured_format_seen,
        "autocompact_success": (
            '"compact_result":"success"' in stdout_text
            and '"trigger":"auto"' in stdout_text
        ),
        "all_request_bodies": bodies,
        "raw_body": body,
    }
    summary["checks"] = checks_for(case.name, summary)
    summary["passed"] = all(summary["checks"].values())
    return summary


def checks_for(name: str, item: dict[str, Any]) -> dict[str, bool]:
    body = item["raw_body"]
    output_config = body.get("output_config") if isinstance(body.get("output_config"), dict) else {}
    thinking = body.get("thinking") if isinstance(body.get("thinking"), dict) else {}
    expected_max_tokens = 4096 if name == "autocompact_forced" else MODEL_OUTPUT_TOKENS
    checks: dict[str, bool] = {
        "process_exit_zero": item["exit_code"] == 0,
        "captured_messages_request": item["message_request_count"] >= 1,
        "model_shape": body.get("model") == "claude-sonnet-4-6",
        "max_output_cap": body.get("max_tokens") == expected_max_tokens,
    }
    if name in {"thinking_disabled", "autocompact_forced"}:
        checks["thinking_absent"] = body.get("thinking") is None
    elif name == "thinking_fixed_max":
        checks["fixed_thinking"] = (
            thinking.get("type") == "enabled"
            and thinking.get("budget_tokens") == FIXED_THINKING_TOKENS
        )
    else:
        checks["adaptive_thinking"] = thinking.get("type") == "adaptive"

    expected_effort = {
        "effort_low": "low", "effort_medium": "medium", "effort_high": "high",
        "effort_xhigh": "high", "effort_max": "max",
        "effort_invalid_auto": "high", "effort_invalid_ultracode": "high",
    }.get(name)
    if expected_effort:
        checks["effort_wire_value"] = output_config.get("effort") == expected_effort
    if name == "extra_body":
        checks["extra_body_preserved"] = (
            body.get("reasoning_effort") == "max"
            and body.get("custom_probe") == {"enabled": True}
        )
    if name == "structured_json_schema":
        checks["json_schema_present"] = item["structured_format_seen"]
    if name == "system_prompt":
        checks["system_prompt_present"] = item["system_contains_system_marker"]
    if name == "append_system_prompt":
        checks["append_system_present"] = item["system_contains_append_marker"]
    if name == "tools_none":
        checks["tools_disabled"] = not item["tool_names"]
    if name == "tools_read":
        checks["read_tool_present"] = "Read" in item["tool_names"]
    if name == "allowed_beta":
        checks["beta_header_present"] = "context-1m-2025-08-07" in (item["anthropic_beta"] or "")
    if name == "stream_json":
        checks["stream_enabled"] = body.get("stream") is True
    if name == "autocompact_forced":
        checks["autocompact_succeeded"] = item["autocompact_success"]
        checks["history_reduced"] = (
            item["message_request_count"] >= 6
            and "FINAL_TURN_MARKER" in json.dumps(body, ensure_ascii=False)
            and not any(f"LONG_TURN_{index}" in json.dumps(body, ensure_ascii=False) for index in range(1, 5))
        )
    if name == "skill_invocation":
        serialized = json.dumps(item["all_request_bodies"], ensure_ascii=False)
        checks["skill_expanded"] = "audit-skill" in serialized or "SKILL_AUDIT_OK" in serialized
    return checks


def write_report(results: list[dict[str, Any]], cli: Path) -> None:
    report = {
        "generated_at_epoch": int(time.time()),
        "claude_version": subprocess.check_output([str(cli), "--version"], text=True).strip(),
        "target_model": "opencode/deepseek-v4-flash-free",
        "capabilities": {
            "context_tokens": MODEL_CONTEXT_TOKENS,
            "output_tokens": MODEL_OUTPUT_TOKENS,
            "fixed_thinking_tokens_tested": FIXED_THINKING_TOKENS,
            "preferred_reasoning_mode": "adaptive + effort=max",
        },
        "summary": {
            "total": len(results),
            "passed": sum(1 for result in results if result["passed"]),
            "failed": sum(1 for result in results if not result["passed"]),
        },
        "cases": results,
    }
    (OUT / "summary.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")

    lines = [
        "# Claude Code CLI Request-Mode Audit",
        "",
        f"- Claude Code: `{report['claude_version']}`",
        "- Target: `opencode/deepseek-v4-flash-free`",
        f"- Model caps used: context `{MODEL_CONTEXT_TOKENS}`, output `{MODEL_OUTPUT_TOKENS}`",
        f"- Result: **{report['summary']['passed']}/{report['summary']['total']} passed**",
        "",
        "| Case | Result | Key wire fields |",
        "|---|---:|---|",
    ]
    for result in results:
        status = "PASS" if result["passed"] else "FAIL"
        fields = (
            f"thinking={json.dumps(result['thinking'], separators=(',', ':'))}; "
            f"effort={json.dumps(result['output_config'], separators=(',', ':'))}; "
            f"max_tokens={result['max_tokens']}"
        )
        lines.append(f"| `{result['case']}` | **{status}** | `{fields}` |")
    lines += ["", "Detailed request captures are stored in `requests/`; isolated settings are in `settings/`."]
    (OUT / "REPORT.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    global OUT, PORT
    parser = argparse.ArgumentParser()
    parser.add_argument("--claude", type=Path, default=DEFAULT_CLI)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--port", type=int, default=PORT)
    parser.add_argument("--case", action="append", dest="selected_cases")
    args = parser.parse_args()

    OUT = args.out.resolve()
    PORT = args.port
    cli = args.claude.resolve()
    if not cli.is_file():
        parser.error(f"Claude Code binary not found: {cli}")

    if OUT.exists():
        shutil.rmtree(OUT)
    for directory in (OUT, OUT / "requests", OUT / "settings", OUT / "profiles"):
        directory.mkdir(parents=True, exist_ok=True)

    selected = cases()
    if args.selected_cases:
        wanted = set(args.selected_cases)
        unknown = sorted(wanted - {case.name for case in selected})
        if unknown:
            parser.error(f"unknown case(s): {', '.join(unknown)}")
        selected = [case for case in selected if case.name in wanted]

    server = ThreadingHTTPServer((HOST, PORT), CaptureHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        results = [run_case(cli, case) for case in selected]
    finally:
        server.shutdown()
        server.server_close()

    write_report(results, cli)
    failed = [result["case"] for result in results if not result["passed"]]
    print(json.dumps({
        "total": len(results), "passed": len(results) - len(failed),
        "failed": failed, "report": str(OUT / "summary.json"),
    }, indent=2))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
