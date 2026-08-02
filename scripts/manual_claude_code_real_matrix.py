#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts" / "claude-cli-real-matrix"
CLAUDE = Path(shutil.which("claude") or "/home/light/.local/share/claude/versions/2.1.207")
MODEL_PROFILE = "claude-sonnet-4-6"
BASE_URL = "http://127.0.0.1:4000"

BASE_ENV = {
    "ANTHROPIC_BASE_URL": BASE_URL,
    "ANTHROPIC_API_KEY": "opencode-bridge",
    "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "128000",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "200000",
    "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "90",
    "CLAUDE_CODE_DISABLE_1M_CONTEXT": "1",
    "CLAUDE_CODE_ALWAYS_ENABLE_EFFORT": "1",
    "MAX_THINKING_TOKENS": "127000",
}


@dataclass
class Result:
    case: str
    passed: bool
    exit_code: int
    result: str | None = None
    details: dict[str, Any] | None = None
    stderr: str = ""
    elapsed_ms: int = 0

    def as_dict(self) -> dict[str, Any]:
        return {
            "case": self.case,
            "passed": self.passed,
            "exit_code": self.exit_code,
            "result": self.result,
            "details": self.details or {},
            "stderr": self.stderr,
            "elapsed_ms": self.elapsed_ms,
        }


def reset_output() -> None:
    if OUT.exists():
        shutil.rmtree(OUT)
    (OUT / "profiles").mkdir(parents=True)
    (OUT / "raw").mkdir(parents=True)


def write_settings(case: str, overrides: dict[str, str] | None = None) -> tuple[Path, Path]:
    profile = OUT / "profiles" / case
    profile.mkdir(parents=True, exist_ok=True)
    env = dict(BASE_ENV)
    env.update(overrides or {})
    settings = {
        "model": MODEL_PROFILE,
        "alwaysThinkingEnabled": env.get("CLAUDE_CODE_DISABLE_THINKING") != "1",
        "env": env,
    }
    path = profile / "settings.json"
    path.write_text(json.dumps(settings, indent=2) + "\n")
    return profile, path


def base_command(settings: Path, prompt: str, max_turns: int = 2) -> list[str]:
    return [
        str(CLAUDE),
        "-p",
        prompt,
        "--model",
        MODEL_PROFILE,
        "--settings",
        str(settings),
        "--setting-sources",
        "user",
        "--max-turns",
        str(max_turns),
    ]


def run_process(
    case: str,
    cmd: list[str],
    profile: Path,
    *,
    cwd: Path = ROOT,
    stdin: str | None = None,
    timeout: int = 45,
) -> tuple[subprocess.CompletedProcess[str], int]:
    env = os.environ.copy()
    env["CLAUDE_CONFIG_DIR"] = str(profile)
    started = time.monotonic()
    proc = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        input=stdin,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    elapsed_ms = int((time.monotonic() - started) * 1000)
    (OUT / "raw" / f"{case}.stdout").write_text(proc.stdout)
    (OUT / "raw" / f"{case}.stderr").write_text(proc.stderr)
    return proc, elapsed_ms


def parse_single_json(stdout: str) -> dict[str, Any]:
    text = stdout.strip()
    if not text:
        return {}
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        for line in reversed(text.splitlines()):
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                return value
    return {}


def parse_jsonl(stdout: str) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    for line in stdout.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            events.append(value)
    return events


def result_text(payload: dict[str, Any]) -> str | None:
    value = payload.get("result")
    return value if isinstance(value, str) else None


def run_reasoning_case(
    case: str,
    *,
    effort: str,
    settings_overrides: dict[str, str] | None = None,
    expect_thinking: bool,
) -> Result:
    profile, settings = write_settings(case, settings_overrides)
    cmd = base_command(settings, "Calculate 17*19 carefully, then reply with only 323.")
    cmd += [
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--tools",
        "",
        "--effort",
        effort,
    ]
    proc, elapsed = run_process(case, cmd, profile)
    events = parse_jsonl(proc.stdout)
    thinking_deltas = 0
    text_deltas = 0
    final = None
    for event in events:
        if event.get("type") == "stream_event":
            inner = event.get("event")
            if isinstance(inner, dict) and inner.get("type") == "content_block_delta":
                delta = inner.get("delta")
                if isinstance(delta, dict):
                    if delta.get("type") == "thinking_delta":
                        thinking_deltas += 1
                    if delta.get("type") == "text_delta":
                        text_deltas += 1
        if event.get("type") == "result":
            final = result_text(event)
    passed = (
        proc.returncode == 0
        and final is not None
        and "323" in final
        and text_deltas > 0
        and ((thinking_deltas > 0) if expect_thinking else (thinking_deltas == 0))
    )
    return Result(
        case,
        passed,
        proc.returncode,
        final,
        {
            "effort": effort,
            "thinking_deltas": thinking_deltas,
            "text_deltas": text_deltas,
            "event_count": len(events),
        },
        proc.stderr[-1000:],
        elapsed,
    )


def run_json_result_case(
    case: str,
    prompt: str,
    expected: str,
    extra_args: list[str] | None = None,
    max_turns: int = 2,
) -> Result:
    profile, settings = write_settings(case)
    cmd = base_command(settings, prompt, max_turns)
    cmd += ["--output-format", "json", "--tools", "", "--effort", "max"]
    cmd += extra_args or []
    proc, elapsed = run_process(case, cmd, profile)
    payload = parse_single_json(proc.stdout)
    final = result_text(payload)
    passed = proc.returncode == 0 and final is not None and expected in final and not payload.get("is_error", False)
    return Result(case, passed, proc.returncode, final, {"payload": payload}, proc.stderr[-1000:], elapsed)


def run_structured_output() -> Result:
    case = "structured_output"
    profile, settings = write_settings(case)
    schema = json.dumps(
        {
            "type": "object",
            "properties": {"ok": {"type": "boolean"}},
            "required": ["ok"],
            "additionalProperties": False,
        },
        separators=(",", ":"),
    )
    cmd = base_command(settings, "Return an object whose ok field is true.", 3)
    cmd += ["--output-format", "json", "--json-schema", schema, "--effort", "max"]
    proc, elapsed = run_process(case, cmd, profile)
    payload = parse_single_json(proc.stdout)
    structured = payload.get("structured_output")
    passed = proc.returncode == 0 and structured == {"ok": True} and not payload.get("is_error", False)
    return Result(case, passed, proc.returncode, result_text(payload), {"structured_output": structured}, proc.stderr[-1000:], elapsed)


def run_read_tool() -> Result:
    case = "built_in_read_tool"
    profile, settings = write_settings(case)
    work = OUT / "work" / case
    work.mkdir(parents=True, exist_ok=True)
    (work / "audit.txt").write_text("REAL_READ_TOOL_OK\n")
    audit_path = (work / "audit.txt").resolve()
    prompt = (
        f"Use the Read tool to read {audit_path}, then reply with exactly "
        "REAL_READ_TOOL_OK and nothing else."
    )
    cmd = base_command(settings, prompt, 4)
    cmd += [
        "--output-format",
        "json",
        "--tools",
        "Read",
        "--allowedTools",
        "Read",
        "--permission-mode",
        "bypassPermissions",
        "--effort",
        "max",
    ]
    proc, elapsed = run_process(case, cmd, profile, cwd=work)
    payload = parse_single_json(proc.stdout)
    final = result_text(payload)
    passed = proc.returncode == 0 and final is not None and "REAL_READ_TOOL_OK" in final and payload.get("num_turns", 0) >= 2
    return Result(
        case,
        passed,
        proc.returncode,
        final,
        {"num_turns": payload.get("num_turns"), "permission_denials": payload.get("permission_denials")},
        proc.stderr[-1000:],
        elapsed,
    )


def run_skill() -> Result:
    case = "custom_skill"
    profile, settings = write_settings(case)
    skill = profile / "skills" / "real-audit-skill"
    skill.mkdir(parents=True)
    (skill / "SKILL.md").write_text(
        "---\n"
        "name: real-audit-skill\n"
        "description: Deterministic Claude Code real-model audit skill.\n"
        "---\n\n"
        "Reply with exactly REAL_SKILL_OK.\n"
    )
    cmd = base_command(settings, "/real-audit-skill", 2)
    cmd += ["--output-format", "json", "--tools", "", "--effort", "max"]
    proc, elapsed = run_process(case, cmd, profile)
    payload = parse_single_json(proc.stdout)
    final = result_text(payload)
    passed = proc.returncode == 0 and final is not None and "REAL_SKILL_OK" in final
    return Result(case, passed, proc.returncode, final, {"num_turns": payload.get("num_turns")}, proc.stderr[-1000:], elapsed)


def run_mcp() -> Result:
    case = "mcp_stdio_tool"
    profile, settings = write_settings(case)
    server = OUT / "mcp_server.py"
    server.write_text(
        "from mcp.server.fastmcp import FastMCP\n"
        "mcp = FastMCP('real-audit')\n"
        "@mcp.tool()\n"
        "def echo(value: str) -> str:\n"
        "    return value\n"
        "if __name__ == '__main__':\n"
        "    mcp.run(transport='stdio')\n"
    )
    config = OUT / "mcp-config.json"
    config.write_text(
        json.dumps(
            {
                "mcpServers": {
                    "real-audit": {
                        "command": sys.executable,
                        "args": [str(server)],
                    }
                }
            },
            indent=2,
        )
        + "\n"
    )
    prompt = "Call the real-audit MCP echo tool with value REAL_MCP_OK, then reply with exactly its returned value."
    cmd = base_command(settings, prompt, 4)
    cmd += [
        "--output-format",
        "json",
        "--mcp-config",
        str(config),
        "--strict-mcp-config",
        "--allowedTools",
        "mcp__real-audit__echo",
        "--effort",
        "max",
    ]
    proc, elapsed = run_process(case, cmd, profile, timeout=60)
    payload = parse_single_json(proc.stdout)
    final = result_text(payload)
    passed = (
        proc.returncode == 0
        and final is not None
        and "REAL_MCP_OK" in final
        and payload.get("num_turns", 0) >= 2
        and not payload.get("permission_denials")
    )
    return Result(
        case,
        passed,
        proc.returncode,
        final,
        {"num_turns": payload.get("num_turns"), "permission_denials": payload.get("permission_denials")},
        proc.stderr[-1000:],
        elapsed,
    )


def run_stream_json() -> Result:
    case = "stream_json"
    profile, settings = write_settings(case)
    message = {
        "type": "user",
        "message": {"role": "user", "content": [{"type": "text", "text": "Reply with exactly REAL_STREAM_OK"}]},
    }
    cmd = base_command(settings, "", 2)
    cmd += [
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--replay-user-messages",
        "--tools",
        "",
        "--effort",
        "max",
    ]
    proc, elapsed = run_process(case, cmd, profile, stdin=json.dumps(message) + "\n")
    events = parse_jsonl(proc.stdout)
    final = next((result_text(e) for e in reversed(events) if e.get("type") == "result"), None)
    replayed = any(
        e.get("type") == "user" and "REAL_STREAM_OK" in json.dumps(e.get("message"), ensure_ascii=False)
        for e in events
    )
    partials = sum(1 for e in events if e.get("type") == "stream_event")
    passed = proc.returncode == 0 and final is not None and "REAL_STREAM_OK" in final and replayed and partials > 0
    return Result(case, passed, proc.returncode, final, {"events": len(events), "partials": partials, "replayed": replayed}, proc.stderr[-1000:], elapsed)


def run_resume() -> Result:
    case = "session_resume"
    profile, settings = write_settings(case)
    session_id = str(uuid.uuid4())
    first_cmd = base_command(settings, "Remember the marker REAL_RESUME_ALPHA and reply with only SAVED.", 2)
    first_cmd += ["--output-format", "json", "--session-id", session_id, "--tools", "", "--effort", "max"]
    first, first_elapsed = run_process(case + "-first", first_cmd, profile)
    first_payload = parse_single_json(first.stdout)

    second_cmd = base_command(settings, "Reply with only the marker I asked you to remember.", 2)
    second_cmd += ["--output-format", "json", "--resume", session_id, "--tools", "", "--effort", "max"]
    second, second_elapsed = run_process(case + "-second", second_cmd, profile)
    second_payload = parse_single_json(second.stdout)
    first_result = result_text(first_payload)
    second_result = result_text(second_payload)
    passed = (
        first.returncode == 0
        and second.returncode == 0
        and first_result is not None
        and "SAVED" in first_result
        and second_result is not None
        and "REAL_RESUME_ALPHA" in second_result
        and first_payload.get("session_id") == second_payload.get("session_id") == session_id
    )
    return Result(
        case,
        passed,
        second.returncode,
        second_result,
        {
            "first_result": first_result,
            "second_result": second_result,
            "session_id": session_id,
            "same_session": first_payload.get("session_id") == second_payload.get("session_id") == session_id,
        },
        (first.stderr + "\n" + second.stderr)[-1000:],
        first_elapsed + second_elapsed,
    )


def main() -> int:
    reset_output()
    results: list[Result] = []

    results.append(run_reasoning_case("thinking_disabled", effort="max", settings_overrides={"CLAUDE_CODE_DISABLE_THINKING": "1"}, expect_thinking=False))
    results.append(run_reasoning_case("thinking_adaptive_max", effort="max", settings_overrides={"CLAUDE_CODE_DISABLE_THINKING": "0", "CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING": "0"}, expect_thinking=True))
    results.append(run_reasoning_case("thinking_fixed_127k", effort="max", settings_overrides={"CLAUDE_CODE_DISABLE_THINKING": "0", "CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING": "1"}, expect_thinking=True))
    for effort in ("low", "medium", "high", "xhigh", "max"):
        results.append(run_reasoning_case(f"effort_{effort}", effort=effort, settings_overrides={"CLAUDE_CODE_DISABLE_THINKING": "0", "CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING": "0"}, expect_thinking=True))

    results.append(run_json_result_case("system_prompt", "What should you reply?", "REAL_SYSTEM_OK", ["--system-prompt", "Reply with exactly REAL_SYSTEM_OK."]))
    results.append(run_json_result_case(
        "append_system_prompt",
        "What is the secret code? Reply with only the code.",
        "REAL_APPEND_OK",
        [
            "--append-system-prompt",
            "The secret code is REAL_APPEND_OK. When asked for the secret code, reply with exactly REAL_APPEND_OK and nothing else.",
        ],
    ))
    results.append(run_structured_output())
    results.append(run_read_tool())
    results.append(run_skill())
    results.append(run_mcp())
    results.append(run_stream_json())
    results.append(run_resume())

    payload = {
        "generated_at_epoch": int(time.time()),
        "claude_version": subprocess.check_output([str(CLAUDE), "--version"], text=True).strip(),
        "target_model": "opencode/deepseek-v4-flash-free",
        "bridge": BASE_URL,
        "summary": {
            "total": len(results),
            "passed": sum(r.passed for r in results),
            "failed": sum(not r.passed for r in results),
        },
        "cases": [r.as_dict() for r in results],
    }
    (OUT / "summary.json").write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n")
    report_lines = [
        "# Claude Code Real-Model Matrix",
        "",
        f"- Claude Code: `{payload['claude_version']}`",
        f"- Target model: `{payload['target_model']}`",
        f"- Bridge: `{payload['bridge']}`",
        f"- Result: **{payload['summary']['passed']}/{payload['summary']['total']} PASS**",
        "",
        "| Case | Status | Time (ms) | Result |",
        "|---|---:|---:|---|",
    ]
    for result in results:
        result_preview = (result.result or "").replace("|", "\\|").replace("\n", " ")[:100]
        report_lines.append(
            f"| `{result.case}` | {'PASS' if result.passed else 'FAIL'} | {result.elapsed_ms} | {result_preview} |"
        )
    report_lines += [
        "",
        "Raw stdout/stderr for every case is stored under `raw/`; isolated Claude profiles are stored under `profiles/`.",
    ]
    (OUT / "REPORT.md").write_text("\n".join(report_lines) + "\n")
    print(json.dumps({
        "total": payload["summary"]["total"],
        "passed": payload["summary"]["passed"],
        "failed": [r.case for r in results if not r.passed],
        "report": str(OUT / "REPORT.md"),
        "summary": str(OUT / "summary.json"),
    }, ensure_ascii=False, indent=2))
    return 0 if payload["summary"]["failed"] == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
