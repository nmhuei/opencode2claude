#!/usr/bin/env python3
"""Inventory the installed Claude Code CLI surface without executing side effects.

The script records `--help` output for the top-level CLI and every discovered
subcommand up to two levels deep. It is a discovery/help-smoke audit, not an
integration test for cloud- or UI-dependent actions.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts" / "claude-code-surface"
CLAUDE = Path(shutil.which("claude") or "/home/light/.local/share/claude/versions/2.1.217")
ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")


@dataclass
class HelpResult:
    path: tuple[str, ...]
    exit_code: int
    stdout: str
    stderr: str
    elapsed_ms: int
    options: list[str]
    commands: list[str]

    def as_dict(self) -> dict[str, Any]:
        return {
            "path": list(self.path),
            "command": " ".join([str(CLAUDE), *self.path, "--help"]),
            "exit_code": self.exit_code,
            "elapsed_ms": self.elapsed_ms,
            "options": self.options,
            "commands": self.commands,
            "stderr": self.stderr,
        }


def clean(text: str) -> str:
    return ANSI_RE.sub("", text).replace("\r\n", "\n")


def section_lines(text: str, heading: str) -> list[str]:
    lines = text.splitlines()
    try:
        start = next(i for i, line in enumerate(lines) if line.strip() == heading)
    except StopIteration:
        return []
    result: list[str] = []
    for line in lines[start + 1 :]:
        if line and not line.startswith(" "):
            break
        if line.strip():
            result.append(line)
    return result


def parse_options(text: str) -> list[str]:
    options: list[str] = []
    for line in section_lines(text, "Options:"):
        # Commander prints each real entry with exactly two leading spaces;
        # wrapped descriptions are indented much farther and must be ignored.
        if not re.match(r"^  \S", line):
            continue
        stripped = line.strip()
        if not stripped.startswith("-"):
            continue
        head = re.split(r"\s{2,}", stripped, maxsplit=1)[0]
        for token in re.split(r"[\s,]+", head):
            if re.fullmatch(r"--?[A-Za-z0-9][A-Za-z0-9-]*", token) and token not in options:
                options.append(token)
    return options


def parse_commands(text: str) -> list[str]:
    commands: list[str] = []
    for line in section_lines(text, "Commands:"):
        if not re.match(r"^  \S", line):
            continue
        stripped = line.strip()
        if not stripped or stripped.startswith("-"):
            continue
        token = stripped.split()[0].split("|")[0]
        if re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]*", token) and token != "help":
            if token not in commands:
                commands.append(token)
    return commands


def run_help(path: tuple[str, ...]) -> HelpResult:
    cmd = [str(CLAUDE), *path, "--help"]
    env = os.environ.copy()
    env.update({"NO_COLOR": "1", "TERM": "dumb", "CI": "1"})
    started = time.monotonic()
    proc = subprocess.run(
        cmd,
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=20,
        check=False,
    )
    elapsed_ms = int((time.monotonic() - started) * 1000)
    stdout = clean(proc.stdout)
    stderr = clean(proc.stderr)
    filename = "root" if not path else "__".join(path)
    (OUT / "raw" / f"{filename}.stdout.txt").write_text(stdout)
    (OUT / "raw" / f"{filename}.stderr.txt").write_text(stderr)
    return HelpResult(
        path,
        proc.returncode,
        stdout,
        stderr,
        elapsed_ms,
        parse_options(stdout),
        parse_commands(stdout),
    )


def main() -> int:
    shutil.rmtree(OUT, ignore_errors=True)
    (OUT / "raw").mkdir(parents=True)

    version = subprocess.check_output([str(CLAUDE), "--version"], text=True).strip()
    root = run_help(())
    results = [root]
    seen: set[tuple[str, ...]] = {()}
    queue: list[tuple[str, ...]] = [(command,) for command in root.commands]

    while queue:
        path = queue.pop(0)
        if path in seen or len(path) > 2:
            continue
        seen.add(path)
        result = run_help(path)
        results.append(result)
        if len(path) < 2:
            queue.extend(path + (command,) for command in result.commands)

    payload = {
        "generated_at_epoch": int(time.time()),
        "claude_binary": str(CLAUDE),
        "claude_version": version,
        "summary": {
            "help_surfaces": len(results),
            "passed": sum(result.exit_code == 0 for result in results),
            "failed": sum(result.exit_code != 0 for result in results),
            "top_level_options": len(root.options),
            "top_level_commands": len(root.commands),
        },
        "top_level_options": root.options,
        "top_level_commands": root.commands,
        "surfaces": [result.as_dict() for result in results],
    }
    (OUT / "summary.json").write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n")

    lines = [
        "# Claude Code Installed CLI Surface Audit",
        "",
        f"- Claude Code: `{version}`",
        f"- Binary: `{CLAUDE}`",
        f"- Help surfaces: **{payload['summary']['passed']}/{payload['summary']['help_surfaces']} PASS**",
        f"- Top-level options: **{len(root.options)}**",
        f"- Top-level commands: **{len(root.commands)}**",
        "",
        "> This verifies discovery and argument parsing only. It does not claim that cloud, browser, IDE, login, update, remote-control, or other side-effecting workflows were integration-tested.",
        "",
        "## Top-level options",
        "",
        "`" + "`, `".join(root.options) + "`",
        "",
        "## Command tree",
        "",
        "| Command path | Help | Options | Child commands |",
        "|---|---:|---:|---:|",
    ]
    for result in results:
        label = "claude" if not result.path else "claude " + " ".join(result.path)
        lines.append(
            f"| `{label}` | {'PASS' if result.exit_code == 0 else 'FAIL'} | "
            f"{len(result.options)} | {len(result.commands)} |"
        )
    lines += [
        "",
        "Raw stdout/stderr for every help surface is stored under `raw/`.",
    ]
    (OUT / "REPORT.md").write_text("\n".join(lines) + "\n")

    print(json.dumps({
        "version": version,
        "help_surfaces": payload["summary"]["help_surfaces"],
        "passed": payload["summary"]["passed"],
        "failed": [" ".join(result.path) or "root" for result in results if result.exit_code != 0],
        "report": str(OUT / "REPORT.md"),
        "summary": str(OUT / "summary.json"),
    }, ensure_ascii=False, indent=2))
    return 0 if payload["summary"]["failed"] == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
