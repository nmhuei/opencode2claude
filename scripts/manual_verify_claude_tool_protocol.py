#!/usr/bin/env python3
"""Real Claude Code verification against a configurable release bridge."""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
import uuid
from collections import Counter
from pathlib import Path
from typing import Any

import pexpect

ROOT = Path(__file__).resolve().parents[1]
OUT = Path(
    os.environ.get(
        "TOOL_PROTOCOL_OUT",
        str(ROOT / "artifacts" / "claude-tool-protocol-reverse" / "live"),
    )
).resolve()
CLAUDE = Path(shutil.which("claude") or "/home/light/.local/bin/claude")
BASE_URL = os.environ.get("TOOL_PROTOCOL_BASE_URL", "http://127.0.0.1:4000")
MODEL = os.environ.get("TOOL_PROTOCOL_MODEL", "claude-sonnet-4-6")
BASE_ENV = {
    "ANTHROPIC_BASE_URL": BASE_URL,
    "ANTHROPIC_API_KEY": "opencode-bridge",
    "CLAUDE_CODE_DISABLE_1M_CONTEXT": "1",
    "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "128000",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "200000",
    "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "90",
    "CLAUDE_CODE_ALWAYS_ENABLE_EFFORT": "1",
    "MAX_THINKING_TOKENS": "127000",
}


def settings(profile: Path) -> Path:
    profile.mkdir(parents=True, exist_ok=True)
    path = profile / "settings.json"
    path.write_text(
        json.dumps(
            {
                "model": MODEL,
                "alwaysThinkingEnabled": True,
                "theme": "auto",
                "env": BASE_ENV,
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n"
    )
    return path


def parse_jsonl(text: str) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    for line in text.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            events.append(value)
    return events


def flatten(value: Any) -> list[str]:
    if isinstance(value, str):
        return [value]
    if isinstance(value, list):
        out: list[str] = []
        for item in value:
            out.extend(flatten(item))
        return out
    if isinstance(value, dict):
        out = []
        for item in value.values():
            out.extend(flatten(item))
        return out
    return []


def collect(events: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    uses: list[dict[str, Any]] = []
    results: list[dict[str, Any]] = []
    seen_uses: set[str] = set()
    seen_results: set[str] = set()
    for event in events:
        message = event.get("message")
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_use":
                tool_id = str(block.get("id") or "")
                if tool_id and tool_id not in seen_uses:
                    seen_uses.add(tool_id)
                    uses.append(
                        {
                            "id": tool_id,
                            "name": block.get("name"),
                            "input": block.get("input"),
                        }
                    )
            elif block.get("type") == "tool_result":
                tool_id = str(block.get("tool_use_id") or "")
                if tool_id and tool_id not in seen_results:
                    seen_results.add(tool_id)
                    results.append(
                        {
                            "tool_use_id": tool_id,
                            "content": block.get("content"),
                            "is_error": block.get("is_error"),
                        }
                    )
    return uses, results


def final_event(events: list[dict[str, Any]]) -> dict[str, Any]:
    return next((event for event in reversed(events) if event.get("type") == "result"), {})


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


def run_print(
    name: str,
    prompt: str,
    tools: str,
    *,
    allowed_tools: str | None,
    permission_mode: str,
    max_turns: int,
    work_setup: callable | None = None,
    extra_args: list[str] | None = None,
    timeout: int = 240,
) -> dict[str, Any]:
    profile = OUT / "profiles" / name
    work = OUT / "work" / name
    profile.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    if work_setup:
        work_setup(profile, work)
    settings_path = settings(profile)
    command = [
        str(CLAUDE),
        "-p",
        prompt,
        "--model",
        MODEL,
        "--settings",
        str(settings_path),
        "--setting-sources",
        "user",
    ]
    if claude_supports("--max-turns"):
        command += ["--max-turns", str(max_turns)]
    command += [
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--tools",
        tools,
        "--permission-mode",
        permission_mode,
        "--effort",
        "max",
        "--session-id",
        str(uuid.uuid4()),
    ]
    if allowed_tools is not None:
        command += ["--allowedTools", allowed_tools]
    command += extra_args or []
    env = os.environ.copy()
    env["CLAUDE_CONFIG_DIR"] = str(profile)
    started = time.monotonic()
    proc = subprocess.run(
        command,
        cwd=work,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    elapsed_ms = int((time.monotonic() - started) * 1000)
    (OUT / "raw" / f"{name}.stdout.jsonl").write_text(proc.stdout)
    (OUT / "raw" / f"{name}.stderr.txt").write_text(proc.stderr)
    (OUT / "raw" / f"{name}.command.json").write_text(
        json.dumps(command, ensure_ascii=False, indent=2) + "\n"
    )
    events = parse_jsonl(proc.stdout)
    uses, results = collect(events)
    terminal = final_event(events)
    ids = [str(use["id"]) for use in uses]
    text = proc.stdout + "\n" + proc.stderr
    return {
        "name": name,
        "exit_code": proc.returncode,
        "elapsed_ms": elapsed_ms,
        "tool_uses": uses,
        "tool_results": results,
        "tool_use_count": len(uses),
        "tool_result_count": len(results),
        "tool_names": [use.get("name") for use in uses],
        "tool_name_counts": dict(Counter(use.get("name") for use in uses)),
        "duplicate_tool_ids": len(ids) - len(set(ids)),
        "unmatched_tool_results": [
            result["tool_use_id"]
            for result in results
            if result["tool_use_id"] not in set(ids)
        ],
        "raw_requesting_count": text.count("[Requesting"),
        "raw_creating_count": text.count("[Creating"),
        "final": terminal.get("result", ""),
        "is_error": terminal.get("is_error"),
        "permission_denials": terminal.get("permission_denials", []),
        "stderr_tail": proc.stderr[-2000:],
        "work": str(work),
    }


def setup_file_tools(_profile: Path, work: Path) -> None:
    (work / "edit.txt").write_text("BEFORE_EDIT\n")


def setup_mcp(profile: Path, _work: Path) -> None:
    server = profile / "mcp_server.py"
    server.write_text(
        "from mcp.server.fastmcp import FastMCP\n"
        "mcp = FastMCP('tool-protocol-live')\n"
        "@mcp.tool()\n"
        "def echo(value: str) -> str:\n"
        "    return value\n"
        "if __name__ == '__main__':\n"
        "    mcp.run(transport='stdio')\n"
    )
    config = profile / "mcp.json"
    config.write_text(
        json.dumps(
            {
                "mcpServers": {
                    "tool-protocol-live": {
                        "command": sys.executable,
                        "args": [str(server)],
                    }
                }
            },
            indent=2,
        )
        + "\n"
    )


def transcript_messages(profile: Path) -> list[dict[str, Any]]:
    candidates = sorted(
        profile.glob("projects/**/*.jsonl"),
        key=lambda path: path.stat().st_mtime,
        reverse=True,
    )
    if not candidates:
        return []
    (OUT / "raw" / "ask_user_question.transcript.path.txt").write_text(
        str(candidates[0]) + "\n"
    )
    text = candidates[0].read_text(errors="replace")
    (OUT / "raw" / "ask_user_question.transcript.jsonl").write_text(text)
    return parse_jsonl(text)


def run_ask_user_question() -> dict[str, Any]:
    name = "ask_user_question"
    profile = OUT / "profiles" / name
    work = OUT / "work" / name
    profile.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    claude_state = profile / ".claude.json"
    if not claude_state.exists():
        claude_state.write_text(
            json.dumps(
                {
                    "hasCompletedOnboarding": True,
                    "lastOnboardingVersion": "2.1.219",
                    "firstStartTime": "2026-07-25T00:00:00.000Z",
                    "installMethod": "native",
                    "numStartups": 1,
                    "migrationVersion": 13,
                },
                indent=2,
            )
            + "\n"
        )
    settings_path = settings(profile)
    command = [
        str(CLAUDE),
        "Use AskUserQuestion to ask exactly 'Choose verification answer'. Offer YES_VERIFY first and NO second. After I answer, reply exactly ASK_USER_QUESTION_OK.",
        "--model",
        MODEL,
        "--settings",
        str(settings_path),
        "--setting-sources",
        "user",
        "--tools",
        "AskUserQuestion",
        "--permission-mode",
        "manual",
        "--effort",
        "max",
        "--session-id",
        str(uuid.uuid4()),
    ]
    (OUT / "raw" / f"{name}.command.json").write_text(
        json.dumps(command, ensure_ascii=False, indent=2) + "\n"
    )
    env = os.environ.copy()
    env["CLAUDE_CONFIG_DIR"] = str(profile)
    terminal_log = OUT / "raw" / f"{name}.terminal.log"
    started = time.monotonic()
    child = pexpect.spawn(
        command[0],
        command[1:],
        cwd=str(work),
        env=env,
        encoding="utf-8",
        timeout=180,
        dimensions=(40, 140),
    )
    trusted = False
    api_key_confirmed = False
    answered = False
    final_seen = False
    with terminal_log.open("w", encoding="utf-8") as log:
        child.logfile = log
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            index = child.expect(
                [
                    "Security guide",
                    "ANTHROPIC_API_KEY",
                    "YES_VERIFY",
                    "Choose verification answer",
                    "ASK_USER_QUESTION_OK",
                    pexpect.EOF,
                    pexpect.TIMEOUT,
                ],
                timeout=min(10, max(1, int(deadline - time.monotonic()))),
            )
            if index == 0 and not trusted:
                time.sleep(0.3)
                child.send("\r")
                trusted = True
            elif index == 1 and not api_key_confirmed:
                time.sleep(0.3)
                child.send("\x1b[A\r")
                api_key_confirmed = True
            elif index in (2, 3) and not answered:
                time.sleep(0.4)
                child.send("\r")
                answered = True
            elif index == 4:
                final_seen = True
                child.send("/exit\r")
            elif index == 5:
                break
            elif index == 6 and final_seen:
                child.sendcontrol("d")
        if child.isalive():
            child.sendcontrol("c")
            time.sleep(0.2)
            child.sendcontrol("d")
            try:
                child.expect(pexpect.EOF, timeout=5)
            except pexpect.ExceptionPexpect:
                child.close(force=True)
    elapsed_ms = int((time.monotonic() - started) * 1000)
    messages = transcript_messages(profile)
    uses, results = collect(messages)
    ids = [str(use["id"]) for use in uses]
    raw = terminal_log.read_text(errors="replace")
    return {
        "name": name,
        "exit_code": child.exitstatus if child.exitstatus is not None else 0,
        "elapsed_ms": elapsed_ms,
        "answered": answered,
        "final_seen": final_seen,
        "tool_uses": uses,
        "tool_results": results,
        "tool_use_count": len(uses),
        "tool_result_count": len(results),
        "tool_names": [use.get("name") for use in uses],
        "duplicate_tool_ids": len(ids) - len(set(ids)),
        "raw_requesting_count": raw.count("[Requesting"),
        "raw_creating_count": raw.count("[Creating"),
    }


def assistant_text(events: list[dict[str, Any]]) -> str:
    chunks: list[str] = []
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
            if isinstance(block, dict) and block.get("type") == "text":
                text = block.get("text")
                if isinstance(text, str):
                    chunks.append(text)
    return "\n".join(chunks)


def run_ask_user_question_transcript() -> dict[str, Any]:
    """Drive the interactive question only after transcript tool_use evidence."""
    name = "ask_user_question"
    profile = OUT / "profiles" / name
    profile.mkdir(parents=True, exist_ok=True)
    settings_path = settings(profile)
    session_id = str(uuid.uuid4())
    command = [
        str(CLAUDE),
        "Use AskUserQuestion exactly once. Ask 'Choose verification answer' with YES_VERIFY first and NO second. After my answer, reply exactly ASK_USER_QUESTION_OK.",
        "--model",
        MODEL,
        "--settings",
        str(settings_path),
        "--tools",
        "AskUserQuestion",
        "--permission-mode",
        "manual",
        "--effort",
        "max",
        "--session-id",
        session_id,
    ]
    (OUT / "raw" / f"{name}.command.json").write_text(
        json.dumps(command, ensure_ascii=False, indent=2) + "\n"
    )
    env = os.environ.copy()
    env.update(BASE_ENV)
    terminal_log = OUT / "raw" / f"{name}.terminal.log"
    started = time.monotonic()
    child = pexpect.spawn(
        command[0],
        command[1:],
        cwd=str(ROOT),
        env=env,
        encoding="utf-8",
        timeout=1,
        dimensions=(44, 150),
    )
    trusted = False
    api_key_confirmed = False
    answer_sent = False
    final_seen = False
    transcript_path: Path | None = None
    terminal_seen = ""
    deadline = time.monotonic() + 180
    with terminal_log.open("w", encoding="utf-8") as log:
        child.logfile = log
        while time.monotonic() < deadline and child.isalive():
            try:
                chunk = child.read_nonblocking(size=8192, timeout=0.25)
                terminal_seen = (terminal_seen + chunk)[-30000:]
            except pexpect.TIMEOUT:
                pass
            except pexpect.EOF:
                break

            if not trusted and "Security guide" in terminal_seen:
                child.send("\r")
                trusted = True
                terminal_seen = ""
                continue
            if not api_key_confirmed and "ANTHROPIC_API_KEY" in terminal_seen:
                child.send("\x1b[A\r")
                api_key_confirmed = True
                terminal_seen = ""
                continue

            if transcript_path is None:
                matches = list((Path.home() / ".claude" / "projects").rglob(f"{session_id}.jsonl"))
                if matches:
                    transcript_path = matches[0]
            events: list[dict[str, Any]] = []
            if transcript_path is not None and transcript_path.exists():
                events = parse_jsonl(transcript_path.read_text(errors="replace"))
                uses, results = collect(events)
                ask_uses = [use for use in uses if use.get("name") == "AskUserQuestion"]
                ask_result_ids = {result["tool_use_id"] for result in results}
                if ask_uses and ask_uses[0]["id"] not in ask_result_ids and not answer_sent:
                    time.sleep(0.6)
                    child.send("\r")
                    answer_sent = True
                if ask_uses and ask_uses[0]["id"] in ask_result_ids:
                    if "ASK_USER_QUESTION_OK" in assistant_text(events):
                        final_seen = True
                        child.send("/exit\r")
                        try:
                            child.expect(pexpect.EOF, timeout=8)
                        except pexpect.ExceptionPexpect:
                            pass
                        break

        if child.isalive():
            child.sendcontrol("c")
            time.sleep(0.2)
            child.sendcontrol("d")
            try:
                child.expect(pexpect.EOF, timeout=5)
            except pexpect.ExceptionPexpect:
                child.close(force=True)

    elapsed_ms = int((time.monotonic() - started) * 1000)
    events = []
    if transcript_path is not None and transcript_path.exists():
        raw_transcript = transcript_path.read_text(errors="replace")
        (OUT / "raw" / "ask_user_question.transcript.path.txt").write_text(
            str(transcript_path) + "\n"
        )
        (OUT / "raw" / "ask_user_question.transcript.jsonl").write_text(raw_transcript)
        events = parse_jsonl(raw_transcript)
    uses, results = collect(events)
    ask_uses = [use for use in uses if use.get("name") == "AskUserQuestion"]
    ask_ids = {use["id"] for use in ask_uses}
    ask_results = [result for result in results if result["tool_use_id"] in ask_ids]
    raw_terminal = terminal_log.read_text(errors="replace")
    return {
        "name": name,
        "exit_code": child.exitstatus if child.exitstatus is not None else 0,
        "elapsed_ms": elapsed_ms,
        "answered": answer_sent,
        "final_seen": final_seen,
        "tool_uses": ask_uses,
        "tool_results": ask_results,
        "tool_use_count": len(ask_uses),
        "tool_result_count": len(ask_results),
        "tool_names": [use.get("name") for use in ask_uses],
        "duplicate_tool_ids": len(ask_uses) - len(ask_ids),
        "raw_requesting_count": raw_terminal.count("[Requesting"),
        "raw_creating_count": raw_terminal.count("[Creating"),
        "assistant_text": assistant_text(events),
        "transcript_path": str(transcript_path) if transcript_path else None,
    }


def base_invariants(result: dict[str, Any]) -> dict[str, bool]:
    use_ids = {use["id"] for use in result["tool_uses"]}
    result_ids = {item["tool_use_id"] for item in result["tool_results"]}
    return {
        "exit_zero": result["exit_code"] == 0,
        "one_result_per_use": len(use_ids) == len(result_ids) and use_ids == result_ids,
        "no_duplicate_tool_ids": result["duplicate_tool_ids"] == 0,
        "no_unmatched_result": not result.get("unmatched_tool_results"),
        "no_raw_requesting": result["raw_requesting_count"] == 0,
        "no_raw_creating": result["raw_creating_count"] == 0,
    }


def main() -> int:
    shutil.rmtree(OUT, ignore_errors=True)
    (OUT / "raw").mkdir(parents=True)
    (OUT / "profiles").mkdir()
    (OUT / "work").mkdir()

    results: dict[str, dict[str, Any]] = {}

    results["cron_lifecycle"] = run_print(
        "cron_lifecycle",
        "Use only CronCreate, CronList, and CronDelete. Call CronCreate exactly once with cron exactly '*/30 * * * *', prompt exactly 'write CRON_PARSE_VERIFY_OK to a local test record', and explicitly include recurring=true. Then list jobs and verify its schedule and prompt. Delete that exact job. List again and verify no jobs remain. Only after the final empty list reply exactly CRON_LIFECYCLE_OK.",
        "CronCreate,CronList,CronDelete",
        allowed_tools="CronCreate,CronList,CronDelete",
        permission_mode="bypassPermissions",
        max_turns=10,
    )

    results["task_lifecycle"] = run_print(
        "task_lifecycle",
        "Use TaskCreate to create exactly one task named 'Verify parser lifecycle' with a short harmless description. Use TaskUpdate to mark it in_progress, then completed. Use TaskList to verify the completed status. Reply exactly TASK_LIFECYCLE_OK only after tool results confirm it.",
        "TaskCreate,TaskUpdate,TaskList,TaskGet",
        allowed_tools="TaskCreate,TaskUpdate,TaskList,TaskGet",
        permission_mode="bypassPermissions",
        max_turns=10,
    )

    results["file_tools"] = run_print(
        "file_tools",
        "Use Read to inspect edit.txt. Use Edit to replace BEFORE_EDIT with AFTER_EDIT. Use Bash to run exactly: printf FILE_TOOL_BASH_OK. Use Read to verify edit.txt. Reply exactly FILE_TOOL_LIFECYCLE_OK after all successful tool results.",
        "Read,Edit,Bash",
        allowed_tools="Read,Edit,Bash",
        permission_mode="bypassPermissions",
        max_turns=10,
        work_setup=setup_file_tools,
    )

    mcp_config = OUT / "profiles" / "mcp_echo" / "mcp.json"
    results["mcp_echo"] = run_print(
        "mcp_echo",
        "Call the tool-protocol-live MCP echo tool once with value REAL_MCP_PROTOCOL_OK. Reply exactly REAL_MCP_PROTOCOL_OK after the result.",
        "default",
        allowed_tools="mcp__tool-protocol-live__echo",
        permission_mode="bypassPermissions",
        max_turns=8,
        work_setup=setup_mcp,
        extra_args=["--mcp-config", str(mcp_config), "--strict-mcp-config"],
    )

    approval_work = OUT / "work" / "approval_flow"
    approval_target = approval_work / "APPROVAL_SHOULD_NOT_RUN"
    results["approval_flow"] = run_print(
        "approval_flow",
        f"Attempt Bash exactly once to run: touch {approval_target}. If permission is denied, do not retry and stop.",
        "Bash",
        allowed_tools=None,
        permission_mode="manual",
        max_turns=2,
    )
    approval_exists = approval_target.exists()
    if approval_exists:
        approval_target.unlink()

    try:
        results["ask_user_question"] = run_ask_user_question_transcript()
    except Exception as error:
        results["ask_user_question"] = {
            "name": "ask_user_question",
            "exit_code": 1,
            "elapsed_ms": 0,
            "answered": False,
            "final_seen": False,
            "tool_uses": [],
            "tool_results": [],
            "tool_use_count": 0,
            "tool_result_count": 0,
            "tool_names": [],
            "duplicate_tool_ids": 0,
            "raw_requesting_count": 0,
            "raw_creating_count": 0,
            "error": repr(error),
        }

    checks: dict[str, dict[str, bool]] = {}

    cron = results["cron_lifecycle"]
    cron_text = "\n".join(flatten(cron["tool_results"]))
    checks["cron_lifecycle"] = {
        **base_invariants(cron),
        "exact_sequence": cron["tool_names"] == [
            "CronCreate",
            "CronList",
            "CronDelete",
            "CronList",
        ],
        "exact_four_uses_results": cron["tool_use_count"] == 4
        and cron["tool_result_count"] == 4,
        "exact_cron_input": cron["tool_uses"][0]["input"].get("cron")
        == "*/30 * * * *",
        "recurring_true_input": cron["tool_uses"][0]["input"].get("recurring") is True,
        "schedule_verified": "Every 30 minutes" in cron_text,
        "prompt_verified": "CRON_PARSE_VERIFY_OK" in cron_text,
        "created_verified": "Scheduled recurring job" in cron_text,
        "deleted_verified": "Cancelled job" in cron_text,
        "cleanup_verified": "No scheduled jobs" in cron_text,
        "terminal_not_error": cron["is_error"] is False,
    }

    task = results["task_lifecycle"]
    task_counts = Counter(task["tool_names"])
    checks["task_lifecycle"] = {
        **base_invariants(task),
        "task_create_seen": task_counts["TaskCreate"] == 1,
        "task_update_seen": task_counts["TaskUpdate"] >= 2,
        "task_list_seen": task_counts["TaskList"] >= 1,
        "final_token": "TASK_LIFECYCLE_OK" in task["final"],
    }

    file_tools = results["file_tools"]
    file_counts = Counter(file_tools["tool_names"])
    edit_path = Path(file_tools["work"]) / "edit.txt"
    checks["file_tools"] = {
        **base_invariants(file_tools),
        "read_seen": file_counts["Read"] >= 2,
        "edit_seen_once": file_counts["Edit"] == 1,
        "bash_seen_once": file_counts["Bash"] == 1,
        "edit_side_effect": edit_path.read_text() == "AFTER_EDIT\n",
        "final_token": "FILE_TOOL_LIFECYCLE_OK" in file_tools["final"],
    }

    mcp = results["mcp_echo"]
    checks["mcp_echo"] = {
        **base_invariants(mcp),
        "one_mcp_call": mcp["tool_names"] == ["mcp__tool-protocol-live__echo"],
        "final_token": "REAL_MCP_PROTOCOL_OK" in mcp["final"],
    }

    approval = results["approval_flow"]
    checks["approval_flow"] = {
        "expected_terminal_status": approval["exit_code"] in (0, 1),
        "bash_requested": "Bash" in approval["tool_names"],
        "all_attempts_have_results": approval["tool_use_count"]
        == approval["tool_result_count"],
        "permission_denial_recorded": bool(approval["permission_denials"])
        and all(item.get("is_error") is True for item in approval["tool_results"]),
        "side_effect_blocked": not approval_exists,
        "no_duplicate_tool_ids": approval["duplicate_tool_ids"] == 0,
        "no_raw_requesting": approval["raw_requesting_count"] == 0,
        "no_raw_creating": approval["raw_creating_count"] == 0,
    }

    ask = results["ask_user_question"]
    ask_names = ask.get("tool_names", [])
    checks["ask_user_question"] = {
        "exit_zero": ask.get("exit_code") == 0,
        "question_answered": ask.get("answered") is True,
        "final_seen": ask.get("final_seen") is True,
        "one_ask_call": ask_names.count("AskUserQuestion") == 1,
        "one_result": ask.get("tool_result_count") == 1,
        "no_duplicate_tool_ids": ask.get("duplicate_tool_ids") == 0,
        "no_raw_requesting": ask.get("raw_requesting_count") == 0,
        "no_raw_creating": ask.get("raw_creating_count") == 0,
    }

    passed = all(all(group.values()) for group in checks.values())
    payload = {
        "generated_at_epoch": int(time.time()),
        "claude_version": subprocess.check_output([str(CLAUDE), "--version"], text=True).strip(),
        "bridge": BASE_URL,
        "model": "opencode/deepseek-v4-flash-free",
        "results": results,
        "checks": checks,
        "pass": passed,
    }
    (OUT / "summary.json").write_text(
        json.dumps(payload, ensure_ascii=False, indent=2) + "\n"
    )
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
