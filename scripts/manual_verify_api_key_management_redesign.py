#!/usr/bin/env python3
"""Manual browser verification for the redesigned API-key management workspace."""

from __future__ import annotations

import json
import os
import re
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

from playwright.sync_api import expect, sync_playwright

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target/release/opencode2api"
OUT = ROOT / "artifacts/redesign/api-key-management-v2"
OUT.mkdir(parents=True, exist_ok=True)
SECRET_RE = re.compile(r"sk-oc2-key_[A-Za-z0-9_]+\.[0-9a-f]+")


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def clean_env(config: Path, runtime: Path) -> dict[str, str]:
    env = os.environ.copy()
    for name in [
        "BRIDGE_AUTH_TOKEN",
        "DASHBOARD_ADMIN_TOKEN",
        "REST_API_TOKEN",
        "OPENCODE_MODEL",
        "BRIDGE_PORT",
        "BRIDGE_HOST",
        "BRIDGE_CONFIG_PATH",
        "RUNTIME_DIR",
    ]:
        env.pop(name, None)
    env["BRIDGE_CONFIG_PATH"] = str(config)
    env["RUNTIME_DIR"] = str(runtime)
    env["NO_COLOR"] = "1"
    return env


def wait_health(base: str, desired: bool, timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        healthy = False
        try:
            with urllib.request.urlopen(base + "/health", timeout=1) as response:
                healthy = response.status == 200
        except Exception:
            healthy = False
        if healthy == desired:
            return
        time.sleep(0.2)
    raise AssertionError(f"health did not become {desired}")


def client_status(base: str, secret: str) -> tuple[int, str]:
    request = urllib.request.Request(
        base + "/v1/models",
        headers={"authorization": f"Bearer {secret}"},
        method="GET",
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            return response.status, response.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode("utf-8", errors="replace")


def save_summary(summary: dict[str, object]) -> None:
    rendered = json.dumps(summary, indent=2, ensure_ascii=False)
    rendered = SECRET_RE.sub("sk-oc2-key_[REDACTED]", rendered) + "\n"
    (OUT / "summary.json").write_text(rendered, encoding="utf-8")
    print(rendered)


def main() -> None:
    if not BIN.exists():
        raise SystemExit("release binary is missing; run cargo build --release --locked")

    port = free_port()
    base = f"http://127.0.0.1:{port}"
    dashboard_token = "dashboard-api-key-redesign-secret-20260721"
    summary: dict[str, object] = {
        "status": "PASS",
        "port": port,
        "actions": {},
        "console_errors": [],
        "page_errors": [],
        "request_failures": [],
        "screenshots": [],
    }

    with tempfile.TemporaryDirectory(prefix="opencode2api-api-key-v2-") as raw:
        temp = Path(raw)
        runtime = temp / "runtime"
        config = temp / "config.toml"
        config.write_text(
            f'''schema_version = 1
port = {port}
host = "127.0.0.1"
model = "opencode/deepseek-v4-flash-free"
shell_policy = "disabled"
dashboard_admin_token = "{dashboard_token}"
rest_api_token = "rest-api-key-redesign-secret-20260721"
csrf_enabled = true
egress_mode = "direct"
runtime_dir = "{runtime}"
upstream_base_url = "https://opencode.ai/zen/v1"
enable_default_fallbacks = false
max_network_attempts = 1
max_provider_attempts = 1
worker_shutdown_timeout_secs = 3
server_shutdown_timeout_secs = 5
''',
            encoding="utf-8",
        )
        env = clean_env(config, runtime)
        started = subprocess.run(
            [
                str(BIN),
                "--quiet",
                "server",
                "start",
                "--no-proxy",
                "--config",
                str(config),
                "--port",
                str(port),
            ],
            cwd=temp,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
        )
        if started.returncode != 0:
            raise RuntimeError(started.stderr or started.stdout)
        wait_health(base, True)

        first_secret = ""
        rotated_secret = ""
        try:
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(headless=True)
                context = browser.new_context(
                    viewport={"width": 1440, "height": 960},
                    accept_downloads=True,
                )
                context.grant_permissions(
                    ["clipboard-read", "clipboard-write"], origin=base
                )
                page = context.new_page()
                page.on(
                    "console",
                    lambda message: summary["console_errors"].append(message.text)
                    if message.type == "error"
                    else None,
                )
                page.on(
                    "pageerror", lambda error: summary["page_errors"].append(str(error))
                )
                page.on(
                    "requestfailed",
                    lambda request: summary["request_failures"].append(
                        {"url": request.url, "failure": request.failure}
                    )
                    if "/api/dashboard/events" not in request.url
                    else None,
                )

                page.goto(base + "/", wait_until="domcontentloaded", timeout=20_000)
                page.fill("#password", dashboard_token)
                page.click("#submitBtn")
                page.wait_for_url("**/dashboard/**", timeout=20_000)
                expect(page.locator("#sidebarStatusText")).to_contain_text(
                    "Connected", timeout=30_000
                )
                page.click(".nav-item[data-view='api']")
                page.wait_for_selector(
                    "[data-view-panel='api'].active", state="visible", timeout=15_000
                )
                expect(page.locator("#apiSummaryTotal")).to_have_text("0", timeout=20_000)
                summary["actions"]["empty_workspace"] = "PASS"

                # Create a managed key with model, reasoning, traffic, and scope policy.
                page.click("#createApiKeyButton")
                expect(page.locator("#apiKeyCreateDialog")).to_be_visible()
                page.fill("#createKeyName", "Mobile App Production")
                page.fill(
                    "#createKeyDescription",
                    "Manual verification credential for the redesigned workspace",
                )
                page.select_option("#createKeyEnvironment", "production")
                page.select_option("#createKeyExpiration", "30")
                page.select_option("#createKeyPreset", "mobile")
                page.select_option(
                    "#createKeyModel", "opencode/deepseek-v4-flash-free"
                )
                page.fill("#createKeyMaxOutput", "2048")
                page.select_option("#createKeyReasoning", "enabled")
                page.select_option("#createKeyReasoningEffort", "high")
                page.fill("#createKeyMaxReasoning", "1024")
                page.select_option("#createKeyLimitAction", "reject")
                page.fill("#createKeyRpm", "30")
                page.fill("#createKeyConcurrent", "1")
                page.click("#apiKeyCreateForm button[type='submit']")
                expect(page.locator("#apiKeySecretDialog")).to_be_visible(
                    timeout=20_000
                )
                first_secret = page.locator("#keyOutput").inner_text().strip()
                assert first_secret.startswith("sk-oc2-key_")
                summary["actions"]["managed_key_create"] = "PASS"

                registry = temp / "config.api-keys.json"
                deadline = time.monotonic() + 10
                while not registry.exists() and time.monotonic() < deadline:
                    time.sleep(0.1)
                assert registry.exists(), "managed registry sidecar was not created"
                registry_text = registry.read_text(encoding="utf-8")
                assert first_secret not in registry_text
                registry_payload = json.loads(registry_text)
                assert registry_payload["keys"][0]["secret_hash"]
                summary["actions"]["secret_hashed_at_rest"] = "PASS"

                page.click('[data-close-dialog="apiKeySecretDialog"]')
                expect(page.locator("#apiKeyInventory tbody tr")).to_have_count(
                    1, timeout=20_000
                )
                expect(page.locator("#apiSummaryActive")).to_have_text("1")
                status, body = client_status(base, first_secret)
                assert status == 200, (status, body)
                summary["actions"]["hot_auth_without_restart"] = "PASS"

                # Local key validation dialog.
                page.click("#verifyApiKeyButton")
                page.fill("#verifyApiKeySecret", first_secret)
                page.click("#verifyApiKeyForm button[type='submit']")
                expect(page.locator("#verifyApiKeyResult")).to_contain_text(
                    "Mobile App Production", timeout=20_000
                )
                expect(page.locator("#verifyApiKeyResult")).to_contain_text("Active")
                page.click('[data-close-dialog="apiKeyVerifyDialog"]')
                summary["actions"]["local_key_check"] = "PASS"

                # Open settings drawer and validate saved policy fields.
                page.click('[data-edit-api-key]')
                expect(page.locator("#apiKeyDrawer")).to_be_visible(timeout=20_000)
                expect(page.locator("#editKeyName")).to_have_value(
                    "Mobile App Production"
                )
                expect(page.locator("#editKeyMaxOutput")).to_have_value("2048")
                expect(page.locator("#editKeyMaxReasoning")).to_have_value("1024")
                expect(page.locator("#editKeyReasoning")).to_have_value("enabled")
                expect(
                    page.locator(
                        '#editKeyPermissions [data-permission="shell"]'
                    )
                ).not_to_be_checked()
                summary["actions"]["settings_drawer_policy"] = "PASS"

                desktop = OUT / "desktop-api-key-workspace.png"
                page.screenshot(path=str(desktop), full_page=True)
                summary["screenshots"].append(str(desktop.relative_to(ROOT)))

                # Endpoint permissions apply instantly.
                page.uncheck(
                    '#editKeyPermissions [data-permission="list_models"]'
                )
                with page.expect_response(
                    lambda response: response.request.method == "PATCH"
                    and "/api/dashboard/control/api-keys/" in response.url,
                    timeout=20_000,
                ) as update_response:
                    page.click("#apiKeyEditForm button[type='submit']")
                assert update_response.value.status == 200
                status, _ = client_status(base, first_secret)
                assert status == 403, status
                page.check('#editKeyPermissions [data-permission="list_models"]')
                with page.expect_response(
                    lambda response: response.request.method == "PATCH"
                    and "/api/dashboard/control/api-keys/" in response.url,
                    timeout=20_000,
                ) as update_response:
                    page.click("#apiKeyEditForm button[type='submit']")
                assert update_response.value.status == 200
                status, body = client_status(base, first_secret)
                assert status == 200, (status, body)
                summary["actions"]["permission_hot_reload"] = "PASS"

                # Disable and re-enable without restarting the server.
                page.select_option("#editKeyStatus", "disabled")
                with page.expect_response(
                    lambda response: response.request.method == "PATCH"
                    and "/api/dashboard/control/api-keys/" in response.url,
                    timeout=20_000,
                ) as update_response:
                    page.click("#apiKeyEditForm button[type='submit']")
                assert update_response.value.status == 200
                status, _ = client_status(base, first_secret)
                assert status == 403, status
                page.select_option("#editKeyStatus", "active")
                with page.expect_response(
                    lambda response: response.request.method == "PATCH"
                    and "/api/dashboard/control/api-keys/" in response.url,
                    timeout=20_000,
                ) as update_response:
                    page.click("#apiKeyEditForm button[type='submit']")
                assert update_response.value.status == 200
                status, body = client_status(base, first_secret)
                assert status == 200, (status, body)
                summary["actions"]["disable_enable_hot_reload"] = "PASS"

                # Placeholder client configuration never leaks the secret.
                page.select_option("#clientConfigFormat", "claude-code")
                page.select_option("#clientConfigKeySource", "placeholder")
                page.click("#generateClientConfigButton")
                expect(page.locator("#clientConfigOutput")).to_contain_text(
                    "ANTHROPIC_BASE_URL", timeout=20_000
                )
                placeholder = page.locator("#clientConfigOutput").inner_text()
                assert first_secret not in placeholder
                assert "sk-oc2-REPLACE_ME" in placeholder
                summary["actions"]["placeholder_client_config"] = "PASS"

                # Rotation invalidates the old secret and exposes a replacement once.
                page.click("#rotateApiKeyButton")
                expect(page.locator("#confirmDialog")).to_be_visible(timeout=10_000)
                page.click("#confirmAccept")
                expect(page.locator("#apiKeySecretDialog")).to_be_visible(
                    timeout=20_000
                )
                rotated_secret = page.locator("#keyOutput").inner_text().strip()
                assert rotated_secret.startswith("sk-oc2-key_")
                assert rotated_secret != first_secret
                old_status, _ = client_status(base, first_secret)
                new_status, new_body = client_status(base, rotated_secret)
                assert old_status == 401, old_status
                assert new_status == 200, (new_status, new_body)
                registry_text = registry.read_text(encoding="utf-8")
                assert first_secret not in registry_text
                assert rotated_secret not in registry_text
                summary["actions"]["rotate_immediate"] = "PASS"
                page.click('[data-close-dialog="apiKeySecretDialog"]')

                # Mobile layout must not create page-level horizontal overflow.
                page.set_viewport_size({"width": 390, "height": 844})
                page.wait_for_timeout(300)
                mobile_metrics = page.evaluate(
                    """() => ({
                      scrollWidth: document.documentElement.scrollWidth,
                      clientWidth: document.documentElement.clientWidth,
                      bodyWidth: document.body.scrollWidth
                    })"""
                )
                assert mobile_metrics["scrollWidth"] <= mobile_metrics["clientWidth"]
                mobile = OUT / "mobile-api-key-workspace.png"
                page.screenshot(path=str(mobile), full_page=True)
                summary["screenshots"].append(str(mobile.relative_to(ROOT)))
                summary["actions"]["mobile_no_overflow"] = mobile_metrics
                page.set_viewport_size({"width": 1440, "height": 960})

                # Revoke is permanent and takes effect immediately.
                page.click('[data-edit-api-key]')
                expect(page.locator("#apiKeyDrawer")).to_be_visible(timeout=20_000)
                page.click("#revokeApiKeyButton")
                expect(page.locator("#confirmDialog")).to_be_visible(timeout=10_000)
                page.click("#confirmAccept")
                expect(page.locator("#apiSummaryTotal")).to_have_text("0", timeout=20_000)
                revoked_status, _ = client_status(base, rotated_secret)
                assert revoked_status == 403, revoked_status
                summary["actions"]["revoke_immediate"] = "PASS"

                browser.close()
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
            wait_health(base, False, timeout=20)

    if (
        summary["console_errors"]
        or summary["page_errors"]
        or summary["request_failures"]
    ):
        summary["status"] = "FAIL"
    save_summary(summary)
    if summary["status"] != "PASS":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
