#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts" / "tool-call-manual"
CLAUDE = Path(shutil.which("claude") or "/home/light/.local/share/claude/versions/2.1.216")
MODEL = "claude-sonnet-4-6"
BASE_URL = "http://127.0.0.1:4000"
RAW_MARKER = "[" + "Requesting Tool execution:"

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
class Case:
    name: str
    prompt: str
    tools: str
    allowed_tools: str
    expected_tools: list[str]
    expected_result_tokens: list[str]
    expected_tool_input_tokens: list[str] = field(default_factory=list)
    setup: Callable[[Path, Path], None] | None = None
    side_effect: Callable[[Path], bool] | None = None
    max_turns: int = 7
    timeout: int = 150
    extra_args: list[str] = field(default_factory=list)


@dataclass
class Result:
    name: str
    passed: bool
    exit_code: int
    elapsed_ms: int
    final: str
    tool_uses: list[dict[str, Any]]
    tool_result_count: int
    raw_marker_seen: bool
    checks: dict[str, bool]
    stderr_tail: str

    def as_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "passed": self.passed,
            "exit_code": self.exit_code,
            "elapsed_ms": self.elapsed_ms,
            "final": self.final,
            "tool_uses": self.tool_uses,
            "tool_result_count": self.tool_result_count,
            "raw_marker_seen": self.raw_marker_seen,
            "checks": self.checks,
            "stderr_tail": self.stderr_tail,
        }


def reset_output() -> None:
    shutil.rmtree(OUT, ignore_errors=True)
    (OUT / "raw").mkdir(parents=True)
    (OUT / "profiles").mkdir(parents=True)
    (OUT / "work").mkdir(parents=True)


def write_settings(profile: Path) -> Path:
    settings = {
        "model": MODEL,
        "alwaysThinkingEnabled": True,
        "env": BASE_ENV,
    }
    path = profile / "settings.json"
    path.write_text(json.dumps(settings, indent=2) + "\n")
    return path


def parse_events(stdout: str) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    for line in stdout.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            events.append(value)
    return events


def collect_tool_uses(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    uses: list[dict[str, Any]] = []
    seen: set[tuple[str, str]] = set()
    for event in events:
        if event.get("type") != "assistant":
            continue
        message = event.get("message")
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict) or block.get("type") != "tool_use":
                continue
            name = str(block.get("name") or "")
            tool_id = str(block.get("id") or "")
            key = (name, tool_id)
            if key in seen:
                continue
            seen.add(key)
            uses.append({
                "name": name,
                "id": tool_id,
                "input": block.get("input"),
                "parent_tool_use_id": event.get("parent_tool_use_id"),
            })
    return uses


def flatten_string_values(value: Any) -> list[str]:
    if isinstance(value, str):
        return [value]
    if isinstance(value, dict):
        flattened: list[str] = []
        for child in value.values():
            flattened.extend(flatten_string_values(child))
        return flattened
    if isinstance(value, list):
        flattened = []
        for child in value:
            flattened.extend(flatten_string_values(child))
        return flattened
    return []


def count_tool_results(events: list[dict[str, Any]]) -> int:
    count = 0
    for event in events:
        if event.get("type") != "user":
            continue
        message = event.get("message")
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if not isinstance(content, list):
            continue
        count += sum(
            1
            for block in content
            if isinstance(block, dict) and block.get("type") == "tool_result"
        )
    return count


def final_result(events: list[dict[str, Any]]) -> str:
    for event in reversed(events):
        if event.get("type") == "result" and isinstance(event.get("result"), str):
            return event["result"]
    return ""


def run_case(case: Case) -> Result:
    profile = OUT / "profiles" / case.name
    work = OUT / "work" / case.name
    profile.mkdir(parents=True)
    work.mkdir(parents=True)
    if case.setup:
        case.setup(profile, work)
    settings = write_settings(profile)

    cmd = [
        str(CLAUDE),
        "-p",
        case.prompt,
        "--model",
        MODEL,
        "--settings",
        str(settings),
        "--setting-sources",
        "user",
        "--max-turns",
        str(case.max_turns),
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--tools",
        case.tools,
        "--allowedTools",
        case.allowed_tools,
        "--permission-mode",
        "bypassPermissions",
        "--effort",
        "max",
    ] + case.extra_args

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
        timeout=case.timeout,
        check=False,
    )
    elapsed_ms = int((time.monotonic() - started) * 1000)
    (OUT / "raw" / f"{case.name}.stdout.jsonl").write_text(proc.stdout)
    (OUT / "raw" / f"{case.name}.stderr").write_text(proc.stderr)
    (OUT / "raw" / f"{case.name}.command.json").write_text(
        json.dumps(cmd, ensure_ascii=False, indent=2) + "\n"
    )

    events = parse_events(proc.stdout)
    uses = collect_tool_uses(events)
    use_names = [item["name"] for item in uses]
    result = final_result(events)
    result_count = count_tool_results(events)
    marker_seen = RAW_MARKER in proc.stdout or RAW_MARKER in proc.stderr
    tool_input_text = "\n".join(
        text for use in uses for text in flatten_string_values(use.get("input"))
    )
    checks = {
        "exit_zero": proc.returncode == 0,
        "result_success": any(
            event.get("type") == "result" and event.get("is_error") is False
            for event in events
        ),
        "expected_tools_seen": all(name in use_names for name in case.expected_tools),
        "tool_results_seen": result_count >= len(case.expected_tools),
        "expected_result_tokens": all(token in result for token in case.expected_result_tokens),
        "expected_tool_inputs": all(
            token in tool_input_text for token in case.expected_tool_input_tokens
        ),
        "raw_marker_hidden": not marker_seen,
        "side_effect_ok": case.side_effect(work) if case.side_effect else True,
    }
    return Result(
        case.name,
        all(checks.values()),
        proc.returncode,
        elapsed_ms,
        result,
        uses,
        result_count,
        marker_seen,
        checks,
        proc.stderr[-2000:],
    )


def setup_read(_profile: Path, work: Path) -> None:
    (work / "alpha.txt").write_text("REAL_READ_OK\n")


def setup_parallel_read(_profile: Path, work: Path) -> None:
    (work / "app.py").write_text("REAL_APP_READ_OK\n")
    (work / "CLAUDE.md").write_text("REAL_CLAUDE_READ_OK\n")


def setup_glob(_profile: Path, work: Path) -> None:
    (work / "nested").mkdir()
    (work / "alpha.audit").write_text("alpha\n")
    (work / "nested" / "beta.audit").write_text("beta\n")
    (work / "nested" / "ignore.txt").write_text("ignore\n")


def setup_grep(_profile: Path, work: Path) -> None:
    (work / "src").mkdir()
    (work / "src" / "one.txt").write_text("first\nREAL_GREP_NEEDLE\nlast\n")
    (work / "src" / "two.txt").write_text("nothing here\n")


def setup_edit(_profile: Path, work: Path) -> None:
    (work / "edit.txt").write_text("BEFORE_EDIT\n")


def setup_webfetch(_profile: Path, work: Path) -> None:
    (work / "README.txt").write_text("network case\n")


def setup_mcp(profile: Path, _work: Path) -> None:
    server = profile / "mcp_server.py"
    server.write_text(
        "from mcp.server.fastmcp import FastMCP\n"
        "mcp = FastMCP('tool-audit')\n"
        "@mcp.tool()\n"
        "def echo(value: str) -> str:\n"
        "    return value\n"
        "if __name__ == '__main__':\n"
        "    mcp.run(transport='stdio')\n"
    )
    config = profile / "mcp.json"
    config.write_text(json.dumps({
        "mcpServers": {
            "tool-audit": {
                "command": sys.executable,
                "args": [str(server)],
            }
        }
    }, indent=2) + "\n")


def created_file_ok(work: Path) -> bool:
    path = work / "created.txt"
    return path.exists() and path.read_text() == "REAL_WRITE_OK\n"


def edited_file_ok(work: Path) -> bool:
    path = work / "edit.txt"
    return path.exists() and path.read_text() == "AFTER_EDIT\n"


def cases() -> list[Case]:
    mcp_config = OUT / "profiles" / "mcp_echo" / "mcp.json"
    return [
        Case(
            "read_single",
            "Use Read to read alpha.txt, then reply with exactly REAL_READ_OK.",
            "Read",
            "Read",
            ["Read"],
            ["REAL_READ_OK"],
            setup=setup_read,
        ),
        Case(
            "read_parallel",
            "Use Read on app.py and CLAUDE.md. Read both files before replying with exactly REAL_APP_READ_OK REAL_CLAUDE_READ_OK.",
            "Read",
            "Read",
            ["Read", "Read"],
            ["REAL_APP_READ_OK", "REAL_CLAUDE_READ_OK"],
            setup=setup_parallel_read,
        ),
        Case(
            "bash",
            "Use Bash to run printf REAL_BASH_OK. Then reply with exactly REAL_BASH_OK.",
            "Bash",
            "Bash",
            ["Bash"],
            ["REAL_BASH_OK"],
        ),
        Case(
            "bash_quoted_multiline",
            "Use Bash to run this shell command exactly: printf \"%s\\n\" \"REAL_QUOTED_BASH_OK\". Then reply with exactly REAL_QUOTED_BASH_OK.",
            "Bash",
            "Bash",
            ["Bash"],
            ["REAL_QUOTED_BASH_OK"],
        ),
        Case(
            "bash_regex_then_read_anser",
            "First use Bash to run exactly: grep -rn 'rq\\|RQ\\|Queue\\|enqueue\\|redis' /tmp/ANSER/core/ --include='*.py' 2>/dev/null | grep -v '.pyc' | head -40. Then use Read on /tmp/ANSER/core/automation_engine.py. Finally reply with exactly RQ_REGEX_READ_OK.",
            "Bash,Read",
            "Bash,Read",
            ["Bash", "Read"],
            ["RQ_REGEX_READ_OK"],
            expected_tool_input_tokens=[
                "rq\\|RQ\\|Queue\\|enqueue\\|redis",
                "/tmp/ANSER/core/automation_engine.py",
            ],
            max_turns=8,
        ),
        Case(
            "glob",
            "Use Glob with pattern **/*.audit. Then reply with both matching paths alpha.audit and nested/beta.audit.",
            "Glob",
            "Glob",
            ["Glob"],
            ["alpha.audit", "beta.audit"],
            setup=setup_glob,
        ),
        Case(
            "grep",
            "Use Grep to search recursively for REAL_GREP_NEEDLE. Then reply with exactly REAL_GREP_NEEDLE src/one.txt.",
            "Grep",
            "Grep",
            ["Grep"],
            ["REAL_GREP_NEEDLE", "one.txt"],
            setup=setup_grep,
        ),
        Case(
            "write",
            "Use Write to create created.txt containing exactly REAL_WRITE_OK followed by one newline. Then reply with exactly REAL_WRITE_OK.",
            "Write",
            "Write",
            ["Write"],
            ["REAL_WRITE_OK"],
            side_effect=created_file_ok,
        ),
        Case(
            "edit",
            "Use Read to inspect edit.txt, then use Edit to replace BEFORE_EDIT with AFTER_EDIT. Then reply with exactly AFTER_EDIT.",
            "Read,Edit",
            "Read,Edit",
            ["Read", "Edit"],
            ["AFTER_EDIT"],
            setup=setup_edit,
            side_effect=edited_file_ok,
        ),
        Case(
            "webfetch",
            "Use WebFetch on https://example.com and reply with exactly EXAMPLE_DOMAIN_OK if the page identifies itself as Example Domain.",
            "WebFetch",
            "WebFetch",
            ["WebFetch"],
            ["EXAMPLE_DOMAIN_OK"],
            setup=setup_webfetch,
            max_turns=6,
            timeout=180,
        ),
        Case(
            "mcp_echo",
            "Call the tool-audit MCP echo tool with value REAL_MCP_OK, then reply with exactly REAL_MCP_OK.",
            "default",
            "mcp__tool-audit__echo",
            ["mcp__tool-audit__echo"],
            ["REAL_MCP_OK"],
            setup=setup_mcp,
            max_turns=6,
            timeout=180,
            extra_args=[
                "--mcp-config",
                str(mcp_config),
                "--strict-mcp-config",
            ],
        ),
    ]


def write_report(results: list[Result]) -> None:
    payload = {
        "generated_at_epoch": int(time.time()),
        "claude_version": subprocess.check_output([str(CLAUDE), "--version"], text=True).strip(),
        "bridge": BASE_URL,
        "target_model": "opencode/deepseek-v4-flash-free",
        "summary": {
            "total": len(results),
            "passed": sum(item.passed for item in results),
            "failed": sum(not item.passed for item in results),
        },
        "cases": [item.as_dict() for item in results],
    }
    (OUT / "summary.json").write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n")
    lines = [
        "# Claude Code Tool-Call Manual Verification",
        "",
        f"- Claude Code: `{payload['claude_version']}`",
        f"- Bridge: `{BASE_URL}`",
        f"- Target: `opencode/deepseek-v4-flash-free`",
        f"- Result: **{payload['summary']['passed']}/{payload['summary']['total']} PASS**",
        "",
        "| Case | Status | Tool uses | Tool results | Raw marker | Final |",
        "|---|---:|---:|---:|---:|---|",
    ]
    for item in results:
        preview = item.final.replace("|", "\\|").replace("\n", " ")[:100]
        lines.append(
            f"| `{item.name}` | {'PASS' if item.passed else 'FAIL'} | "
            f"{len(item.tool_uses)} | {item.tool_result_count} | "
            f"{'YES' if item.raw_marker_seen else 'NO'} | {preview} |"
        )
    lines += [
        "",
        "Each case stores raw Claude Code `stream-json`, stderr, and command arguments under `raw/`.",
    ]
    (OUT / "REPORT.md").write_text("\n".join(lines) + "\n")


def main() -> int:
    reset_output()
    results: list[Result] = []
    for case in cases():
        try:
            result = run_case(case)
        except subprocess.TimeoutExpired as error:
            result = Result(
                case.name,
                False,
                124,
                int(case.timeout * 1000),
                "",
                [],
                0,
                False,
                {"timeout": False},
                str(error),
            )
        results.append(result)
        print(json.dumps({
            "case": result.name,
            "status": "PASS" if result.passed else "FAIL",
            "tool_uses": [item["name"] for item in result.tool_uses],
            "tool_results": result.tool_result_count,
            "marker": result.raw_marker_seen,
            "final": result.final[:160],
            "checks": result.checks,
        }, ensure_ascii=False))
    write_report(results)
    failed = [item.name for item in results if not item.passed]
    print(json.dumps({
        "passed": len(results) - len(failed),
        "total": len(results),
        "failed": failed,
        "report": str(OUT / "REPORT.md"),
        "summary": str(OUT / "summary.json"),
    }, ensure_ascii=False, indent=2))
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
