#!/usr/bin/env python3
"""Real Claude Code CLI end-to-end gate (hermetic).

Exercises the REAL `claude` CLI against the REAL bridge binary built from the
current working tree, with a loopback OpenAI SSE stub standing in for the
upstream provider. Never touches production :4000, Docker/WARP resources, or
external upstreams (authoritative approach per CLAUDE.md's deployment gate and
scripts/pty_matrix.py).

Covered surfaces:
  1. Plain streaming request/response through the real CLI (SSE lifecycle).
  2. Real tool execution loop (CLI runs Bash, tool result folds back in).
  3. Multi-key upstream rotation (2026-09-01): the stub rejects every request
     bearing key-one with HTTP 429; the bridge must transparently retry with
     key-two so the CLI never observes the rate limit. The stub records every
     Authorization header as evidence.

Run:  python3 tests/claude_code_e2e.py
Exit: 0 = all cases passed, 1 = failure (evidence under artifacts/claude-code-e2e/).
"""
from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts" / "claude-code-e2e"

KEY_ONE = "e2e-key-one"
KEY_TWO = "e2e-key-two"
CLIENT_TOKEN = "e2e-client-token"
MODEL_PROFILE = "claude-sonnet-4-6"
MARKER_TEXT = "E2E_REAL_OK"
TOOL_ACCEPTED = "TOOL_RESULT_ACCEPTED"

STUB_MODEL = "deepseek-v4-flash"

ENV_STRIP_EXACT = {
    "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL", "BRIDGE_PORT", "BRIDGE_HOST", "BRIDGE_AUTH_TOKEN",
    "BRIDGE_CONFIG_PATH", "BRIDGE_EGRESS_MODE", "BRIDGE_PRIMARY_PROXIES",
    "BRIDGE_WARM_STANDBY_PROXIES", "BRIDGE_PROXIES", "RUNTIME_DIR",
    "OPENCODE_PORT", "OPENCODE_MODEL", "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS", "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
}
ENV_STRIP_SUBSTR = ("UPSTREAM", "PROXY", "proxy")


def strip_bridge_env(env: dict[str, str]) -> dict[str, str]:
    """Hermetic child env: no inherited upstream/auth/port config, no proxies."""
    cleaned = {}
    for key, value in env.items():
        if key in ENV_STRIP_EXACT or any(part in key for part in ENV_STRIP_SUBSTR):
            continue
        cleaned[key] = value
    return cleaned


def free_port() -> int:
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def resolve_serve_bin() -> Path:
    override = os.environ.get("E2E_SERVE_BIN")
    if override:
        return Path(override)
    target = Path(os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target")))
    for profile in ("debug", "release"):
        candidate = target / profile / "opencode2api-serve"
        if candidate.exists():
            return candidate
    raise SystemExit("opencode2api-serve not found; build first (cargo build)")


def chunk(delta: dict, finish_reason: str | None = None) -> str:
    payload = {
        "id": "chatcmpl_e2e", "object": "chat.completion.chunk",
        "created": 1750000000, "model": STUB_MODEL,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    }
    return f"data: {json.dumps(payload, ensure_ascii=False)}\n\n"


def sse_ok(text: str) -> list[str]:
    return [
        chunk({"role": "assistant", "content": None}),
        chunk({"content": text}),
        chunk({"content": None}, finish_reason="stop"),
        "data: [DONE]\n\n",
    ]


def sse_tool_call() -> list[str]:
    return [
        chunk({"role": "assistant", "content": None}),
        chunk({"tool_calls": [{"index": 0, "id": "call_e2e", "type": "function",
                               "function": {"name": "Bash", "arguments": ""}}]}),
        chunk({"tool_calls": [{"index": 0, "function": {"arguments": '{"command"'}}]}),
        chunk({"tool_calls": [{"index": 0, "function": {"arguments": ': "echo ok"}'}}]}),
        chunk({}, finish_reason="tool_calls"),
        "data: [DONE]\n\n",
    ]


class StubLog:
    def __init__(self, path: Path):
        self._lock = threading.Lock()
        self.entries: list[dict[str, Any]] = []
        self._file = path.open("w")

    def append(self, entry: dict[str, Any]) -> None:
        with self._lock:
            self.entries.append(entry)
            self._file.write(json.dumps(entry, ensure_ascii=False) + "\n")
            self._file.flush()

    def close(self) -> None:
        self._file.close()


class StubHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    log = None  # injected StubLog

    def log_message(self, *args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802 (BaseHTTPRequestHandler API)
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length else b""
        try:
            req = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            req = {}
        auth = self.headers.get("Authorization", "") or ""
        last_user = ""
        for message in req.get("messages", []):
            if message.get("role") != "user":
                continue
            content = message.get("content", "")
            if isinstance(content, str):
                last_user = content
            elif isinstance(content, list):
                last_user = " ".join(
                    str(block.get("text", "")) for block in content
                    if isinstance(block, dict)
                )

        if auth == f"Bearer {KEY_ONE}":
            # Credential-quota simulation: every key-one attempt is rejected so
            # the bridge must rotate to key-two for the client to make progress.
            self.log.append({"auth": auth, "status": 429, "stream": req.get("stream"),
                             "model": req.get("model"), "path": self.path})
            body = json.dumps({"error": {"message": "rate limited",
                                         "type": "rate_limit_error"}}).encode()
            self.send_response(429)
            self.send_header("Content-Type", "application/json")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
            return

        has_tool_result = any(m.get("role") == "tool" for m in req.get("messages", []))
        if has_tool_result:
            lines = sse_ok(TOOL_ACCEPTED)
        elif "E2E_TOOL_PROMPT" in last_user:
            lines = sse_tool_call()
        else:
            lines = sse_ok(MARKER_TEXT)

        self.log.append({"auth": auth, "status": 200, "stream": req.get("stream"),
                         "model": req.get("model"), "path": self.path,
                         "tool_call": "E2E_TOOL_PROMPT" in last_user,
                         "tool_result": has_tool_result})
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        for event in lines:
            self.wfile.write(event.encode())
            self.wfile.flush()
            time.sleep(0.03)


def wait_health(port: int, timeout: float = 20.0) -> None:
    import urllib.request
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=2) as resp:
                if resp.status == 200:
                    return
        except Exception:
            time.sleep(0.2)
    raise SystemExit(f"bridge /health never became ready on port {port}")


def write_claude_settings(profile: Path, bridge_port: int) -> Path:
    profile.mkdir(parents=True, exist_ok=True)
    env_block = {
        "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{bridge_port}",
        "ANTHROPIC_API_KEY": CLIENT_TOKEN,
        "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000",
        "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "128000",
        "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "200000",
        "CLAUDE_CODE_DISABLE_1M_CONTEXT": "1",
        "MAX_THINKING_TOKENS": "8000",
    }
    settings = {"model": MODEL_PROFILE, "alwaysThinkingEnabled": False, "env": env_block}
    path = profile / "settings.json"
    path.write_text(json.dumps(settings, indent=2) + "\n")
    return path


def run_claude(case: str, profile: Path, prompt: str, *, max_turns: int,
               tools: list[str], work_dir: Path | None) -> tuple[subprocess.CompletedProcess[str], int]:
    cmd = [
        shutil.which("claude") or "claude",
        "-p", prompt,
        "--model", MODEL_PROFILE,
        "--settings", str(profile / "settings.json"),
        "--setting-sources", "user",
        "--max-turns", str(max_turns),
        "--output-format", "json",
    ]
    if tools:
        cmd += ["--tools", *tools, "--allowedTools", *tools,
                "--permission-mode", "bypassPermissions"]
    else:
        cmd += ["--tools", ""]

    env = dict(os.environ)
    env["CLAUDE_CONFIG_DIR"] = str(profile)
    for key in list(env):
        if key.startswith("ANTHROPIC_"):
            env.pop(key)
    no_proxy = env.get("NO_PROXY", "")
    env["NO_PROXY"] = f"{no_proxy},127.0.0.1,localhost" if no_proxy else "127.0.0.1,localhost"

    started = time.monotonic()
    proc = subprocess.run(cmd, cwd=work_dir or ROOT, env=env, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=180, check=False)
    elapsed = int((time.monotonic() - started) * 1000)
    (OUT / "raw" / f"{case}.stdout").write_text(proc.stdout)
    (OUT / "raw" / f"{case}.stderr").write_text(proc.stderr)
    return proc, elapsed


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


def main() -> int:
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    (OUT / "raw").mkdir()

    serve_bin = resolve_serve_bin()
    claude_bin = shutil.which("claude")
    if not claude_bin:
        raise SystemExit("claude CLI not found on PATH")

    stub_port = free_port()
    bridge_port = free_port()
    tmp = Path(f"/tmp/opencode2api-claude-e2e-{os.getpid()}")
    tmp.mkdir(parents=True)
    work_dir = tmp / "work"
    work_dir.mkdir()

    stub_log = StubLog(OUT / "stub-requests.jsonl")
    StubHandler.log = stub_log
    stub_server = ThreadingHTTPServer(("127.0.0.1", stub_port), StubHandler)
    stub_thread = threading.Thread(target=stub_server.serve_forever, daemon=True)
    stub_thread.start()

    config_path = tmp / "opencode2api.toml"
    config_path.write_text(f"""schema_version = 1
port = {bridge_port}
host = "127.0.0.1"
auth_tokens = ["{CLIENT_TOKEN}"]
egress_mode = "direct"
require_verified_exit_ip = false
runtime_dir = "{tmp / "runtime"}"
history_enabled = false
model = "{STUB_MODEL}"
upstream_base_url = "http://127.0.0.1:{stub_port}"
upstream_api_keys = ["{KEY_ONE}", "{KEY_TWO}"]
""")

    bridge_log = (OUT / "bridge.log").open("w")
    bridge_env = strip_bridge_env(dict(os.environ))
    bridge_env["NO_PROXY"] = "*"
    bridge = subprocess.Popen(
        [str(serve_bin), "--config", str(config_path), "--port", str(bridge_port)],
        cwd=tmp, env=bridge_env, stdout=bridge_log, stderr=subprocess.STDOUT,
    )

    results: list[dict[str, Any]] = []
    try:
        wait_health(bridge_port)

        cases = [
            ("plain_stream", "Reply with exactly E2E_REAL_OK and nothing else.",
             {"max_turns": 2, "tools": [], "expect": MARKER_TEXT, "min_turns": 0}),
            ("tool_loop", "E2E_TOOL_PROMPT. Use the Bash tool to run exactly `echo ok`, then report the tool result.",
             {"max_turns": 4, "tools": ["Bash"], "expect": TOOL_ACCEPTED, "min_turns": 2}),
        ]
        for case, prompt, spec in cases:
            profile = tmp / "profiles" / case
            write_claude_settings(profile, bridge_port)
            try:
                proc, elapsed = run_claude(case, profile, prompt, max_turns=spec["max_turns"],
                                           tools=spec["tools"], work_dir=work_dir)
                payload = parse_single_json(proc.stdout)
                final = payload.get("result") if isinstance(payload.get("result"), str) else None
                passed = (
                    proc.returncode == 0
                    and final is not None
                    and spec["expect"] in final
                    and not payload.get("is_error", False)
                    and payload.get("num_turns", 0) >= spec["min_turns"]
                )
                results.append({
                    "case": case, "passed": passed, "exit_code": proc.returncode,
                    "result": final, "num_turns": payload.get("num_turns"),
                    "elapsed_ms": elapsed, "stderr_tail": proc.stderr[-800:],
                })
                print(f"  {'✓' if passed else '✗'} {case}: exit={proc.returncode} "
                      f"turns={payload.get('num_turns')} elapsed={elapsed}ms")
            except subprocess.TimeoutExpired as error:
                results.append({"case": case, "passed": False, "error": f"timeout: {error}"})
                print(f"  ✗ {case}: TIMEOUT")

        # Rotation evidence: every 429 (key-one) must be immediately followed
        # by a successful key-two attempt, and key-one must never see a 200.
        entries = stub_log.entries
        auth_seq = [e["auth"] for e in entries]
        rotations = sum(
            1 for i in range(len(auth_seq) - 1)
            if auth_seq[i] == f"Bearer {KEY_ONE}" and auth_seq[i + 1] == f"Bearer {KEY_TWO}"
        )
        key_one_ever_200 = any(
            e["auth"] == f"Bearer {KEY_ONE}" and e["status"] == 200 for e in entries
        )
        unbalanced = any(
            auth_seq[i] == f"Bearer {KEY_ONE}"
            and (i + 1 >= len(auth_seq) or auth_seq[i + 1] != f"Bearer {KEY_TWO}")
            for i in range(len(auth_seq))
        )
        rotation_passed = rotations >= 1 and not key_one_ever_200 and not unbalanced
        results.append({
            "case": "multi_key_rotation", "passed": rotation_passed,
            "requests": len(entries), "rotations": rotations,
            "key_one_ever_200": key_one_ever_200, "unbalanced": unbalanced,
            "auth_sequence": auth_seq,
        })
        print(f"  {'✓' if rotation_passed else '✗'} multi_key_rotation: "
              f"requests={len(entries)} rotations={rotations} "
              f"key_one_ever_200={key_one_ever_200}")

        summary = {
            "generated_at_epoch": int(time.time()),
            "claude_version": subprocess.run([claude_bin, "--version"], capture_output=True,
                                             text=True).stdout.strip(),
            "serve_bin": str(serve_bin),
            "bridge_port": bridge_port, "stub_port": stub_port,
            "cases": results,
            "passed": all(r.get("passed", False) for r in results),
        }
        (OUT / "summary.json").write_text(json.dumps(summary, indent=2, ensure_ascii=False) + "\n")
        print(f"\n{'E2E PASS' if summary['passed'] else 'E2E FAIL'} — evidence: {OUT}")
        return 0 if summary["passed"] else 1
    finally:
        stub_server.shutdown()
        bridge.terminate()
        try:
            bridge.wait(timeout=10)
        except subprocess.TimeoutExpired:
            bridge.kill()
        bridge_log.close()
        stub_log.close()
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
