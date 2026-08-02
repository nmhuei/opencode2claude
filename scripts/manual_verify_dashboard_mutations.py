#!/usr/bin/env python3
"""Exercise mutation wiring through the rebuilt dashboard UI on an isolated daemon."""

from __future__ import annotations

import json
import os
import re
import socket
import subprocess
import tempfile
import tomllib
import time
import urllib.error
import urllib.request
from pathlib import Path

from playwright.sync_api import expect, sync_playwright

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target/release/opencode2api"
OUT = ROOT / "artifacts/redesign/dashboard-mutations"
OUT.mkdir(parents=True, exist_ok=True)
SECRET = re.compile(r"sk-oc2-[0-9a-f]+")


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def clean_env(config: Path, runtime: Path):
    env = os.environ.copy()
    for name in [
        "BRIDGE_AUTH_TOKEN", "DASHBOARD_ADMIN_TOKEN", "REST_API_TOKEN", "OPENCODE_MODEL",
        "BRIDGE_PORT", "BRIDGE_HOST", "BRIDGE_CONFIG_PATH", "RUNTIME_DIR",
    ]:
        env.pop(name, None)
    env["BRIDGE_CONFIG_PATH"] = str(config)
    env["RUNTIME_DIR"] = str(runtime)
    env["NO_COLOR"] = "1"
    return env


def wait_health(base: str, desired: bool, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        healthy = False
        try:
            with urllib.request.urlopen(base + "/health", timeout=1) as response:
                healthy = response.status == 200
        except Exception:
            pass
        if healthy == desired:
            return
        time.sleep(0.2)
    raise AssertionError(f"health did not become {desired}")



def current_pid(runtime: Path) -> int:
    return int(json.loads((runtime / "opencode2api.pid.json").read_text(encoding="utf-8"))["pid"])


def wait_pid_change(runtime: Path, previous_pid: int, timeout=35) -> int:
    deadline = time.monotonic() + timeout
    last_pid = previous_pid
    while time.monotonic() < deadline:
        try:
            last_pid = current_pid(runtime)
        except (FileNotFoundError, json.JSONDecodeError, KeyError, ValueError):
            time.sleep(0.15)
            continue
        if last_pid != previous_pid:
            return last_pid
        time.sleep(0.15)
    raise AssertionError(f"PID did not change from {previous_pid}; last={last_pid}")


def restart_via_ui(page, runtime: Path, base: str) -> int:
    previous_pid = current_pid(runtime)
    page.click("#restartServerButton")
    expect(page.locator("#confirmDialog")).to_be_visible(timeout=10_000)
    page.click("#confirmAccept")
    new_pid = wait_pid_change(runtime, previous_pid)
    wait_health(base, True, 25)
    expect(page.locator("#serverFacts")).to_contain_text(str(new_pid), timeout=45_000)
    expect(page.locator("#sidebarStatusText")).to_contain_text("Connected", timeout=45_000)
    return new_pid

def completion_probe(base: str, key: str, marker: str):
    request = urllib.request.Request(
        base + "/v1/chat/completions",
        data=json.dumps({
            "model": "ignored",
            "messages": [{"role": "user", "content": f"Reply with only {marker}"}],
            "max_tokens": 1024,
            "stream": False,
        }).encode(),
        headers={"content-type": "application/json", "authorization": f"Bearer {key}"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            payload = json.load(response)
            text = (((payload.get("choices") or [{}])[0].get("message") or {}).get("content") or "")
            return response.status, text
    except urllib.error.HTTPError as error:
        return error.code, ""


def main():
    port = free_port()
    base = f"http://127.0.0.1:{port}"
    dashboard_token = "dashboard-ui-mutation-secret-20260721"
    initial_key = "dashboard-ui-initial-key-20260721"
    summary = {"status": "PASS", "actions": {}, "console_errors": [], "page_errors": [], "screenshots": []}

    with tempfile.TemporaryDirectory(prefix="opencode2api-dashboard-ui-") as raw:
        temp = Path(raw)
        runtime = temp / "runtime"
        config = temp / "config.toml"
        config.write_text(
            f'''schema_version = 1
port = {port}
host = "127.0.0.1"
model = "opencode/deepseek-v4-flash-free"
auth_tokens = ["{initial_key}"]
dashboard_admin_token = "{dashboard_token}"
rest_api_token = "dashboard-ui-rest-secret-20260721"
csrf_enabled = true
egress_mode = "direct"
runtime_dir = "{runtime}"
upstream_base_url = "https://opencode.ai/zen/v1"
enable_default_fallbacks = false
max_network_attempts = 2
max_provider_attempts = 1
retry_base_backoff_ms = 200
retry_max_backoff_ms = 500
worker_shutdown_timeout_secs = 3
server_shutdown_timeout_secs = 5
''', encoding="utf-8")
        env = clean_env(config, runtime)
        start = subprocess.run(
            [str(BIN), "--quiet", "server", "start", "--no-proxy", "--config", str(config), "--port", str(port)],
            cwd=temp, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
        )
        if start.returncode != 0:
            raise RuntimeError(start.stderr)
        wait_health(base, True)

        try:
            with sync_playwright() as p:
                browser = p.chromium.launch(headless=True)
                context = browser.new_context(viewport={"width": 1365, "height": 820})
                page = context.new_page()
                page.on("console", lambda msg: summary["console_errors"].append(msg.text) if msg.type == "error" else None)
                page.on("pageerror", lambda exc: summary["page_errors"].append(str(exc)))

                page.goto(base + "/", wait_until="domcontentloaded")
                page.fill("#password", dashboard_token)
                page.click("#submitBtn")
                page.wait_for_url("**/dashboard/**", timeout=15_000)
                expect(page.locator("#sidebarStatusText")).to_contain_text("Connected", timeout=20_000)

                # Model selection through the card button.
                page.click(".nav-item[data-view='models']")
                selector = "[data-select-model='opencode/nemotron-3-ultra-free']"
                page.wait_for_selector(selector, timeout=20_000)
                page.click(selector)
                expect(page.locator("#modelRestartNotice")).to_be_visible(timeout=20_000)
                assert 'model = "opencode/nemotron-3-ultra-free"' in config.read_text(encoding="utf-8")
                summary["actions"]["model_select_ui"] = "PASS"
                path = OUT / "model-selected.png"
                page.screenshot(path=str(path), full_page=True)
                summary["screenshots"].append(str(path.relative_to(ROOT)))

                # Restart through UI and verify the new model becomes active.
                page.click(".nav-item[data-view='server']")
                restart_via_ui(page, runtime, base)
                page.click(".nav-item[data-view='models']")
                expect(page.locator(".model-card.selected .model-id")).to_contain_text("nemotron-3-ultra-free", timeout=20_000)
                summary["actions"]["restart_after_model_ui"] = "PASS"

                # Structured config preview + apply via editor.
                page.click(".nav-item[data-view='configuration']")
                expect(page.locator("#configEditor")).to_have_value(re.compile(r"schema_version", re.S), timeout=20_000)
                content = page.input_value("#configEditor")
                if "max_body_size" in content:
                    content = re.sub(r"(?m)^max_body_size\s*=.*$", "max_body_size = 3145728", content)
                else:
                    content += "\nmax_body_size = 3145728\n"
                page.fill("#configEditor", content)
                page.click("#previewConfigButton")
                expect(page.locator("#configPreviewOutput")).to_contain_text("max_body_size", timeout=20_000)
                page.click("#applyConfigButton")
                expect(page.locator("#confirmDialog")).to_be_visible(timeout=20_000)
                page.click("#confirmAccept")
                expect(page.locator("#configPreviewOutput")).to_contain_text('"rollback_performed": false', timeout=20_000)
                parsed_config = tomllib.loads(config.read_text(encoding="utf-8"))
                assert parsed_config.get("max_body_size") == 3145728
                summary["actions"]["config_apply_ui"] = "PASS"

                # Load template into editor only, then reload active file to prove no write.
                before_template = config.read_bytes()
                page.click("#loadTemplateButton")
                expect(page.locator("#configEditor")).to_have_value(re.compile(r"OpenCode2API configuration", re.S), timeout=20_000)
                assert config.read_bytes() == before_template
                page.click("#reloadConfigButton")
                summary["actions"]["template_load_no_write_ui"] = "PASS"

                # Append a client key through Access UI.
                page.click(".nav-item[data-view='access']")
                page.check("#keySave")
                page.uncheck("#keyReplace")
                page.fill("#keyBytes", "16")
                page.click("#keyForm button[type='submit']")
                expect(page.locator("#confirmDialog")).to_be_visible(timeout=10_000)
                page.click("#confirmAccept")
                expect(page.locator("#keyOutput")).to_contain_text("sk-oc2-", timeout=20_000)
                appended_key = page.locator("#keyOutput").inner_text().strip()
                assert appended_key in config.read_text(encoding="utf-8")
                page.locator("#keyOutput").evaluate("node => node.textContent = 'sk-oc2-[REDACTED]'")
                summary["actions"]["key_append_ui"] = "PASS"

                # Restart applies both config and appended key.
                page.click(".nav-item[data-view='server']")
                restart_via_ui(page, runtime, base)
                assert completion_probe(base, initial_key, "UI_APPEND_OLD_OK")[0] == 200
                status, text = completion_probe(base, appended_key, "UI_APPEND_NEW_OK")
                assert status == 200 and "UI_APPEND_NEW_OK" in text
                summary["actions"]["key_append_auth_after_restart"] = "PASS"

                # Replace keys through UI, then restart and verify revocation.
                page.click(".nav-item[data-view='access']")
                page.check("#keySave")
                page.check("#keyReplace")
                page.click("#keyForm button[type='submit']")
                expect(page.locator("#confirmDialog")).to_be_visible(timeout=10_000)
                page.click("#confirmAccept")
                expect(page.locator("#keyOutput")).to_contain_text("sk-oc2-", timeout=20_000)
                replacement_key = page.locator("#keyOutput").inner_text().strip()
                page.locator("#keyOutput").evaluate("node => node.textContent = 'sk-oc2-[REDACTED]'")
                page.click(".nav-item[data-view='server']")
                restart_via_ui(page, runtime, base)
                assert completion_probe(base, initial_key, "REVOKED")[0] == 401
                assert completion_probe(base, appended_key, "REVOKED")[0] == 401
                status, text = completion_probe(base, replacement_key, "UI_REPLACE_OK")
                assert status == 200 and "UI_REPLACE_OK" in text
                summary["actions"]["key_replace_ui"] = "PASS"

                # Update check button is non-destructive and must render a result.
                page.click(".nav-item[data-view='server']")
                page.click("#checkUpdateButton")
                expect(page.locator("#updateStatus")).not_to_contain_text("Not checked", timeout=40_000)
                summary["actions"]["update_check_ui"] = "PASS"

                # Stop through UI; the page should settle into offline state.
                page.click("#stopServerButton")
                expect(page.locator("#confirmDialog")).to_be_visible(timeout=10_000)
                page.click("#confirmAccept")
                wait_health(base, False, 20)
                summary["actions"]["server_stop_ui"] = "PASS"
                browser.close()
        finally:
            subprocess.run(
                [str(BIN), "--quiet", "server", "stop"], cwd=temp, env=env,
                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20,
            )

    if summary["console_errors"] or summary["page_errors"]:
        summary["status"] = "FAIL"
    rendered = SECRET.sub("sk-oc2-[REDACTED]", json.dumps(summary, indent=2)) + "\n"
    (OUT / "summary.json").write_text(rendered, encoding="utf-8")
    print(rendered)
    if summary["status"] != "PASS":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
