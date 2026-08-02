#!/usr/bin/env python3
"""Capture and validate width-aware CLI output without mutating the stable bridge."""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target/release/opencode2api"
OUT = ROOT / "artifacts/redesign/cli-captures"
OUT.mkdir(parents=True, exist_ok=True)
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
SECRET = re.compile(r"sk-oc2-[0-9a-f]+")
URL_OR_TOKEN = re.compile(r"(?:https?://|sk-oc2-|export .*API_KEY=)")
WIDTHS = [60, 80, 100, 120, 160]
COLORS = ["never", "always"]


def visible_width(text: str) -> int:
    width = 0
    for char in ANSI.sub("", text):
        if unicodedata.combining(char):
            continue
        width += 2 if unicodedata.east_asian_width(char) in {"W", "F"} else 1
    return width


def redact(text: str) -> str:
    return SECRET.sub("sk-oc2-[REDACTED]", text)


def run(args: list[str], width: int, color: str, *, cwd: Path = ROOT, env_extra=None, stdin=None, timeout=60):
    env = os.environ.copy()
    env.update({"COLUMNS": str(width), "LINES": "50", "NO_COLOR": "1" if color == "never" else "0"})
    if env_extra:
        env.update({key: str(value) for key, value in env_extra.items()})
    started = time.monotonic()
    proc = subprocess.run(
        [str(BIN), "--color", color, *args],
        cwd=cwd,
        env=env,
        input=stdin,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
    return {
        "args": args,
        "width": width,
        "color": color,
        "exit_code": proc.returncode,
        "duration_ms": round((time.monotonic() - started) * 1000),
        "stdout": redact(proc.stdout),
        "stderr": redact(proc.stderr),
    }


def analyze_capture(capture):
    overflows = []
    for stream_name in ["stdout", "stderr"]:
        for number, line in enumerate(capture[stream_name].splitlines(), start=1):
            width = visible_width(line)
            # Long secrets, URLs, completion scripts and raw log records cannot be safely wrapped.
            exempt = bool(URL_OR_TOKEN.search(ANSI.sub("", line))) or capture["args"][:1] == ["completion"]
            if width > capture["width"] and not exempt:
                overflows.append({"stream": stream_name, "line": number, "width": width, "text": ANSI.sub("", line)})
    capture["max_stdout_width"] = max([visible_width(line) for line in capture["stdout"].splitlines()] or [0])
    capture["max_stderr_width"] = max([visible_width(line) for line in capture["stderr"].splitlines()] or [0])
    capture["overflows"] = overflows
    capture["ansi_expected"] = capture["color"] == "always"
    capture["ansi_found"] = bool(ANSI.search(capture["stdout"] + capture["stderr"]))


def write_capture(name: str, capture: dict):
    stem = f"{name}__w{capture['width']}__{capture['color']}"
    (OUT / f"{stem}.stdout.txt").write_text(capture.pop("stdout"), encoding="utf-8")
    (OUT / f"{stem}.stderr.txt").write_text(capture.pop("stderr"), encoding="utf-8")
    capture["name"] = name


def main():
    if not BIN.exists():
        raise SystemExit(f"release binary missing: {BIN}")

    with tempfile.TemporaryDirectory(prefix="opencode2api-cli-redesign-") as temp_raw:
        temp = Path(temp_raw)
        init_path = temp / "generated.toml"
        cases = {
            "root-help": ["--help"],
            "server-help": ["server", "--help"],
            "server-start-help": ["server", "start", "--help"],
            "server-status": ["server", "status"],
            "server-config": ["server", "config"],
            "proxy-help": ["proxy", "--help"],
            "proxy-ps": ["proxy", "ps"],
            "proxy-restart-plan": ["proxy", "restart", "--dry-run"],
            "proxy-purge-plan": ["proxy", "purge", "--dry-run"],
            "dashboard-help": ["dashboard", "--help"],
            "dashboard-status": ["dashboard", "status"],
            "env": ["env"],
            "doctor": ["doctor"],
            "api-key-help": ["api-key", "generate", "--help"],
            "api-key-generate": ["api-key", "generate", "--bytes", "16"],
            "init-help": ["init", "--help"],
            "update-help": ["update", "--help"],
            "completion-help": ["completion", "--help"],
            "legacy-status": ["status"],
        }
        captures = []
        for name, args in cases.items():
            for width in WIDTHS:
                for color in COLORS:
                    capture = run(args, width, color, cwd=ROOT)
                    analyze_capture(capture)
                    write_capture(name, capture)
                    captures.append(capture)

        # File-state command ordering: create -> conflict -> force.
        init_sequence = []
        for name, args in [
            ("init-create", ["init", "--output", str(init_path)]),
            ("init-conflict", ["init", "--output", str(init_path)]),
            ("init-force", ["init", "--output", str(init_path), "--force"]),
        ]:
            capture = run(args, 80, "never", cwd=temp)
            analyze_capture(capture)
            write_capture(name, capture)
            init_sequence.append(capture)
        assert init_sequence[0]["exit_code"] == 0
        assert init_sequence[1]["exit_code"] != 0
        assert init_sequence[2]["exit_code"] == 0
        assert "schema_version = 1" in init_path.read_text(encoding="utf-8")
        captures.extend(init_sequence)

        # JSON and quiet surfaces must remain machine-clean.
        machine_cases = {
            "status-json": ["--json", "server", "status"],
            "config-json": ["--json", "server", "config"],
            "dashboard-json": ["--json", "dashboard", "status"],
            "env-json": ["--json", "env"],
            "proxy-json": ["--json", "proxy", "ps"],
            "key-json": ["--json", "api-key", "generate", "--bytes", "16"],
            "env-quiet": ["--quiet", "env"],
            "status-quiet": ["--quiet", "server", "status"],
        }
        machine_results = []
        for name, args in machine_cases.items():
            result = run(args, 100, "never")
            stdout = result["stdout"]
            stderr = result["stderr"]
            if "json" in name:
                json.loads(stdout)
            assert not ANSI.search(stdout + stderr)
            analyze_capture(result)
            write_capture(name, result)
            machine_results.append(result)
        captures.extend(machine_results)

        # Completion for every supported shell.
        completion_results = []
        for shell in ["bash", "zsh", "fish", "powershell", "elvish"]:
            result = run(["completion", shell], 100, "never")
            assert result["exit_code"] == 0 and "opencode2api" in result["stdout"]
            analyze_capture(result)
            write_capture(f"completion-{shell}", result)
            completion_results.append(result)
        captures.extend(completion_results)

    failures = [capture for capture in captures if capture["overflows"]]
    unexpected_codes = [
        capture
        for capture in captures
        if capture["exit_code"] != 0 and capture["name"] not in {"init-conflict"}
    ]
    color_failures = [
        capture for capture in captures
        if capture.get("color") == "never" and capture.get("ansi_found")
    ]
    summary = {
        "status": "PASS" if not failures and not unexpected_codes and not color_failures else "FAIL",
        "captures": len(captures),
        "widths": WIDTHS,
        "overflow_failures": failures,
        "unexpected_exit_codes": unexpected_codes,
        "color_failures": color_failures,
        "init_sequence": [item["exit_code"] for item in init_sequence],
        "supported_completions": 5,
    }
    (OUT / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))
    if summary["status"] != "PASS":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
