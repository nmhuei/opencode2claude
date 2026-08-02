#!/usr/bin/env python3
"""Probe every curated OpenCode Zen free model through the OpenAI endpoint."""

from __future__ import annotations

import json
import os
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target/release/opencode2api"
OUT = ROOT / "artifacts/redesign/free-model-probes.json"
MODELS = [
    "opencode/deepseek-v4-flash-free",
    "opencode/nemotron-3-ultra-free",
    "opencode/mimo-v2.5-free",
    "opencode/north-mini-code-free",
    "opencode/big-pickle",
]


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    port = free_port()
    key = "free-model-probe-key-20260721"
    with tempfile.TemporaryDirectory(prefix="opencode2api-model-probe-") as raw:
        temp = Path(raw)
        runtime = temp / "runtime"
        config = temp / "config.toml"
        config.write_text(
            f'''schema_version = 1
port = {port}
host = "127.0.0.1"
auth_tokens = ["{key}"]
egress_mode = "direct"
runtime_dir = "{runtime}"
upstream_base_url = "https://opencode.ai/zen/v1"
enable_default_fallbacks = false
max_network_attempts = 1
max_provider_attempts = 1
retry_base_backoff_ms = 100
retry_max_backoff_ms = 200
''',
            encoding="utf-8",
        )
        env = os.environ.copy()
        for name in ["BRIDGE_AUTH_TOKEN", "OPENCODE_MODEL", "BRIDGE_PORT", "BRIDGE_HOST", "BRIDGE_CONFIG_PATH", "RUNTIME_DIR"]:
            env.pop(name, None)
        env["BRIDGE_CONFIG_PATH"] = str(config)
        env["RUNTIME_DIR"] = str(runtime)
        start = subprocess.run(
            [str(BIN), "--quiet", "server", "start", "--no-proxy", "--config", str(config), "--port", str(port)],
            cwd=temp,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
        )
        if start.returncode != 0:
            raise RuntimeError(start.stderr)
        base = f"http://127.0.0.1:{port}"
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(base + "/health", timeout=1):
                    break
            except Exception:
                time.sleep(0.2)
        results = []
        try:
            for index, model in enumerate(MODELS, start=1):
                marker = f"FREE_MODEL_{index}_OK"
                payload = {
                    "model": model,
                    "messages": [{"role": "user", "content": f"Reply with only {marker}"}],
                    "max_tokens": 1024,
                    "stream": False,
                }
                request = urllib.request.Request(
                    base + "/v1/chat/completions",
                    data=json.dumps(payload).encode(),
                    headers={"content-type": "application/json", "authorization": f"Bearer {key}"},
                    method="POST",
                )
                started = time.monotonic()
                try:
                    with urllib.request.urlopen(request, timeout=55) as response:
                        status = response.status
                        body = json.load(response)
                except urllib.error.HTTPError as error:
                    status = error.code
                    try:
                        body = json.loads(error.read())
                    except Exception:
                        body = {"error": {"message": "non-json error"}}
                elapsed = round((time.monotonic() - started) * 1000)
                text = ""
                reasoning = ""
                if isinstance(body, dict):
                    message = ((body.get("choices") or [{}])[0].get("message") or {})
                    text = message.get("content") or ""
                    reasoning = message.get("reasoning_content") or message.get("reasoning") or message.get("thinking") or ""
                result = {
                    "model": model,
                    "status": status,
                    "latency_ms": elapsed,
                    "response_nonempty": bool(text.strip()),
                    "reasoning_nonempty": bool(reasoning.strip()),
                    "marker_returned": marker in text,
                    "error": body.get("error") if isinstance(body, dict) and status != 200 else None,
                }
                results.append(result)
                print(json.dumps(result))
        finally:
            subprocess.run(
                [str(BIN), "--quiet", "server", "stop"],
                cwd=temp,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=20,
            )
        summary = {
            "status": "PASS" if all(item["status"] == 200 and item["response_nonempty"] for item in results) else "FAIL",
            "models": results,
            "passed": sum(item["status"] == 200 and item["response_nonempty"] for item in results),
            "total": len(results),
        }
        OUT.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(summary, indent=2))
        if summary["status"] != "PASS":
            raise SystemExit(1)


if __name__ == "__main__":
    main()
