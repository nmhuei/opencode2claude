#!/usr/bin/env python3
"""Order-dependent CLI/dashboard verification against an isolated bridge daemon."""

from __future__ import annotations

import http.cookiejar
import json
import os
import re
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target/release/opencode2api"
OUT = ROOT / "artifacts/redesign"
OUT.mkdir(parents=True, exist_ok=True)
SECRET = re.compile(r"sk-oc2-[0-9a-f]+")


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def redact(value: str) -> str:
    return SECRET.sub("sk-oc2-[REDACTED]", value)


class Scenario:
    def __init__(self, root: Path):
        self.root = root
        self.port = free_port()
        self.config = root / "config.toml"
        self.runtime = root / "runtime"
        self.base = f"http://127.0.0.1:{self.port}"
        self.dashboard_token = "dashboard-control-secret-20260721"
        self.initial_key = "initial-client-key-20260721"
        self.steps: list[dict] = []
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        self.env = os.environ.copy()
        for key in [
            "BRIDGE_AUTH_TOKEN",
            "DASHBOARD_ADMIN_TOKEN",
            "REST_API_TOKEN",
            "OPENCODE_MODEL",
            "BRIDGE_PORT",
            "BRIDGE_HOST",
            "BRIDGE_CONFIG_PATH",
            "RUNTIME_DIR",
        ]:
            self.env.pop(key, None)
        self.env["BRIDGE_CONFIG_PATH"] = str(self.config)
        self.env["RUNTIME_DIR"] = str(self.runtime)
        self.env["NO_COLOR"] = "1"
        self.write_config()

    def write_config(self):
        self.config.write_text(
            f'''schema_version = 1
port = {self.port}
host = "127.0.0.1"
model = "opencode/deepseek-v4-flash-free"
auth_tokens = ["{self.initial_key}"]
dashboard_admin_token = "{self.dashboard_token}"
rest_api_token = "rest-control-secret-20260721"
csrf_enabled = true
egress_mode = "direct"
runtime_dir = "{self.runtime}"
upstream_base_url = "https://opencode.ai/zen/v1"
enable_default_fallbacks = false
max_network_attempts = 2
max_provider_attempts = 1
retry_base_backoff_ms = 200
retry_max_backoff_ms = 500
worker_shutdown_timeout_secs = 3
server_shutdown_timeout_secs = 5
''',
            encoding="utf-8",
        )

    def record(self, name: str, status: str, detail=None):
        self.steps.append({"name": name, "status": status, "detail": detail})

    def cli(self, name: str, args: list[str], expected=(0,), timeout=45):
        started = time.monotonic()
        process = subprocess.run(
            [str(BIN), "--color", "never", *args],
            cwd=self.root,
            env=self.env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
        entry = {
            "name": name,
            "status": "PASS" if process.returncode in expected else "FAIL",
            "exit_code": process.returncode,
            "duration_ms": round((time.monotonic() - started) * 1000),
            "stdout": redact(process.stdout),
            "stderr": redact(process.stderr),
        }
        self.steps.append(entry)
        if process.returncode not in expected:
            raise AssertionError(f"{name} exit={process.returncode}: {process.stderr}")
        return process

    def start_args(self):
        return [
            "server",
            "start",
            "--no-proxy",
            "--config",
            str(self.config),
            "--port",
            str(self.port),
            "--host",
            "127.0.0.1",
        ]

    def wait_health(self, desired: bool, timeout=30):
        deadline = time.monotonic() + timeout
        last = False
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(self.base + "/health", timeout=1.2) as response:
                    last = response.status == 200
            except Exception:
                last = False
            if last == desired:
                return
            time.sleep(0.2)
        raise AssertionError(f"health did not become {desired}; last={last}")

    def request(self, path: str, method="GET", body=None, headers=None, opener=None, timeout=30):
        data = None if body is None else json.dumps(body).encode()
        merged = dict(headers or {})
        if body is not None:
            merged["content-type"] = "application/json"
        request = urllib.request.Request(self.base + path, data=data, headers=merged, method=method)
        client = opener or urllib.request
        try:
            response = client.open(request, timeout=timeout) if hasattr(client, "open") else client.urlopen(request, timeout=timeout)
            raw = response.read()
            status = response.status
            response_headers = dict(response.headers.items())
        except urllib.error.HTTPError as error:
            raw = error.read()
            status = error.code
            response_headers = dict(error.headers.items())
        try:
            payload = json.loads(raw) if raw else None
        except json.JSONDecodeError:
            payload = raw.decode("utf-8", "replace")
        return status, payload, response_headers

    def login(self):
        status, payload, _ = self.request(
            "/api/dashboard/login",
            method="POST",
            body={"token": self.dashboard_token},
            opener=self.opener,
        )
        assert status == 200, payload
        csrf = next((cookie.value for cookie in self.jar if cookie.name == "bridge_csrf_token"), None)
        assert csrf
        self.record("dashboard-login", "PASS")
        return csrf

    def dashboard_mutation(self, path: str, body: dict, csrf: str, timeout=45):
        return self.request(
            path,
            method="POST",
            body=body,
            headers={"X-CSRF-Token": csrf},
            opener=self.opener,
            timeout=timeout,
        )

    def openai_probe(self, key: str, marker: str, timeout=45):
        status, payload, _ = self.request(
            "/v1/chat/completions",
            method="POST",
            body={
                "model": "client-selected",
                "messages": [{"role": "user", "content": f"Reply with only {marker}"}],
                "max_tokens": 64,
                "stream": False,
            },
            headers={"Authorization": f"Bearer {key}"},
            timeout=timeout,
        )
        text = ""
        if isinstance(payload, dict):
            text = (((payload.get("choices") or [{}])[0].get("message") or {}).get("content") or "")
        return status, text

    def restart_from_dashboard(self, csrf: str, name: str):
        status, payload, _ = self.dashboard_mutation(
            "/api/dashboard/control/server/restart", {}, csrf
        )
        assert status == 202, payload
        # The process can restart quickly enough that a polling client never observes the down edge.
        old_pid = json.loads((self.runtime / "opencode2api.pid.json").read_text())["pid"]
        deadline = time.monotonic() + 35
        new_pid = old_pid
        while time.monotonic() < deadline:
            try:
                new_pid = json.loads((self.runtime / "opencode2api.pid.json").read_text())["pid"]
            except Exception:
                pass
            if new_pid != old_pid:
                break
            time.sleep(0.25)
        assert new_pid != old_pid, (old_pid, new_pid)
        self.wait_health(True, 20)
        self.record(name, "PASS", {"old_pid": old_pid, "new_pid": new_pid})

    def run(self):
        # status -> start -> status -> start again -> CLI restart -> status
        stopped = self.cli("status-before-start", ["--json", "server", "status"])
        assert json.loads(stopped.stdout)["status"] == "stopped"
        self.cli("start-first", self.start_args())
        self.wait_health(True)
        running = self.cli("status-after-start", ["--json", "server", "status"])
        first_pid = json.loads(running.stdout)["pid"]
        self.cli("start-idempotent", self.start_args())
        repeated = self.cli("status-after-second-start", ["--json", "server", "status"])
        assert json.loads(repeated.stdout)["pid"] == first_pid
        self.cli("restart-cli", ["server", "restart"])
        self.wait_health(True)
        restarted = self.cli("status-after-cli-restart", ["--json", "server", "status"])
        assert json.loads(restarted.stdout)["pid"] != first_pid

        csrf = self.login()
        models_status, models_payload, _ = self.request(
            "/api/dashboard/control/models", opener=self.opener
        )
        assert models_status == 200 and len(models_payload["models"]) == 5
        self.record("free-model-catalog", "PASS", 5)

        # Model A -> B -> restart -> A -> restart: catches stale resolved-config state.
        for model in ["opencode/nemotron-3-ultra-free", "opencode/deepseek-v4-flash-free"]:
            status, payload, _ = self.dashboard_mutation(
                "/api/dashboard/control/models/select", {"model": model}, csrf
            )
            assert status == 200 and payload["restart_required"] is True
            assert f'model = "{model}"' in self.config.read_text(encoding="utf-8")
            self.restart_from_dashboard(csrf, f"dashboard-restart-after-model-{model.rsplit('/', 1)[-1]}")
            status, payload, _ = self.request(
                "/v1/models",
                headers={"Authorization": f"Bearer {self.initial_key}"},
            )
            assert status == 200 and payload["data"][0]["id"] == model
            self.record(f"model-active-{model.rsplit('/', 1)[-1]}", "PASS")

        # Invalid preview must leave bytes unchanged.
        before = self.config.read_bytes()
        status, payload, _ = self.request(
            "/api/dashboard/config/preview",
            method="POST",
            body={"content": "schema_version = [invalid"},
            opener=self.opener,
        )
        assert status == 400 and self.config.read_bytes() == before
        self.record("invalid-config-preview-no-write", "PASS", payload.get("code"))

        # Append -> restart: old and new keys both work.
        status, payload, _ = self.dashboard_mutation(
            "/api/dashboard/control/api-keys",
            {"count": 1, "bytes": 16, "prefix": "sk-oc2-", "save": True, "replace": False},
            csrf,
        )
        assert status == 200 and payload["saved"] is True
        appended_key = payload["keys"][0]
        self.restart_from_dashboard(csrf, "dashboard-restart-after-key-append")
        for label, key in [("old-key-after-append", self.initial_key), ("new-key-after-append", appended_key)]:
            status, text = self.openai_probe(key, "KEY_APPEND_OK")
            assert status == 200 and "KEY_APPEND_OK" in text
            self.record(label, "PASS")

        # Replace -> restart: previous keys rejected, replacement accepted.
        status, payload, _ = self.dashboard_mutation(
            "/api/dashboard/control/api-keys",
            {"count": 1, "bytes": 16, "prefix": "sk-oc2-", "save": True, "replace": True},
            csrf,
        )
        assert status == 200
        replacement_key = payload["keys"][0]
        self.restart_from_dashboard(csrf, "dashboard-restart-after-key-replace")
        for label, key in [("initial-key-rejected", self.initial_key), ("appended-key-rejected", appended_key)]:
            status, _, = self.openai_probe(key, "SHOULD_NOT_RUN")
            assert status == 401
            self.record(label, "PASS")
        status, text = self.openai_probe(replacement_key, "KEY_REPLACE_OK")
        assert status == 200 and "KEY_REPLACE_OK" in text
        self.record("replacement-key-accepted", "PASS")

        # Stop while an authenticated SSE request is active; verify bounded shutdown.
        stream_started = threading.Event()
        stream_finished = threading.Event()

        def hold_stream():
            request = urllib.request.Request(
                self.base + "/api/dashboard/test/stream?delay_ms=800&thinking=active-stream&text=" + ("x" * 300)
            )
            try:
                with self.opener.open(request, timeout=20) as response:
                    response.read(64)
                    stream_started.set()
                    while response.read(64):
                        pass
            except Exception:
                stream_started.set()
            finally:
                stream_finished.set()

        thread = threading.Thread(target=hold_stream, daemon=True)
        thread.start()
        assert stream_started.wait(5)
        status, payload, _ = self.dashboard_mutation(
            "/api/dashboard/control/server/stop", {}, csrf
        )
        assert status == 202, payload
        self.wait_health(False, 15)
        assert stream_finished.wait(15)
        self.record("stop-with-active-sse", "PASS")

        # Repeated stop is idempotent; start recovers.
        self.cli("stop-idempotent", ["server", "stop"])
        self.cli("start-after-dashboard-stop", self.start_args())
        self.wait_health(True)
        self.record("recovery-after-dashboard-stop", "PASS")
        self.cli("final-stop", ["server", "stop"])
        self.wait_health(False)

        # Stale PID metadata is cleaned by status and does not block start.
        self.runtime.mkdir(parents=True, exist_ok=True)
        stale_path = self.runtime / "opencode2api.pid.json"
        stale_path.write_text(
            json.dumps(
                {
                    "pid": 4294967294,
                    "port": self.port,
                    "host": "127.0.0.1",
                    "started_at": 1,
                    "executable": str(BIN.parent / "opencode2api-serve"),
                    "start_marker": "stale-marker",
                    "instance_id": "stale-instance",
                }
            ),
            encoding="utf-8",
        )
        status = self.cli("status-cleans-stale-pid", ["--json", "server", "status"])
        assert json.loads(status.stdout)["status"] == "stopped"
        assert not stale_path.exists()
        self.cli("start-after-stale-pid", self.start_args())
        self.wait_health(True)
        self.cli("cleanup-stop", ["server", "stop"])
        self.wait_health(False)


def main():
    if not BIN.exists():
        raise SystemExit("Build release binaries before running this script")
    with tempfile.TemporaryDirectory(prefix="opencode2api-state-order-") as raw:
        scenario = Scenario(Path(raw))
        try:
            scenario.run()
            status = "PASS"
        except Exception as error:
            status = "FAIL"
            scenario.record("exception", "FAIL", str(error))
            try:
                scenario.cli("emergency-stop", ["server", "stop"], expected=(0, 1), timeout=15)
            except Exception:
                pass
        summary = {
            "status": status,
            "port": scenario.port,
            "steps": scenario.steps,
            "passed": sum(step["status"] == "PASS" for step in scenario.steps),
            "failed": sum(step["status"] == "FAIL" for step in scenario.steps),
        }
        # Keep secrets out of permanent artifacts.
        rendered = redact(json.dumps(summary, indent=2)) + "\n"
        (OUT / "scenario-matrix.json").write_text(rendered, encoding="utf-8")
        print(rendered)
        if status != "PASS":
            raise SystemExit(1)


if __name__ == "__main__":
    main()
