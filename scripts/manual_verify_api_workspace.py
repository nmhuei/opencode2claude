#!/usr/bin/env python3
"""Manual browser verification for the API workspace, icons, typography and live uptime."""

from __future__ import annotations

import json
import os
import re
import socket
import subprocess
import tempfile
import time
import tomllib
import urllib.request
from pathlib import Path

from playwright.sync_api import expect, sync_playwright

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target/release/opencode2api"
OUT = ROOT / "artifacts/redesign/api-workspace"
OUT.mkdir(parents=True, exist_ok=True)
SECRET_RE = re.compile(r"sk-oc2-[0-9a-f]+")


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


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


def parse_duration(value: str) -> int:
    total = 0
    for amount, unit in re.findall(r"(\d+)\s*([dhms])", value):
        multiplier = {"d": 86400, "h": 3600, "m": 60, "s": 1}[unit]
        total += int(amount) * multiplier
    return total


def redact(value: str) -> str:
    return SECRET_RE.sub("sk-oc2-[REDACTED]", value)


def main() -> None:
    port = free_port()
    base = f"http://127.0.0.1:{port}"
    dashboard_token = "dashboard-api-workspace-secret-20260721"
    first_key = "sk-oc2-11111111111111111111111111111111"  # EXAMPLE_SECRET_SCAN_ALLOW
    second_key = "sk-oc2-22222222222222222222222222222222"  # EXAMPLE_SECRET_SCAN_ALLOW
    summary: dict[str, object] = {
        "status": "PASS",
        "port": port,
        "actions": {},
        "console_errors": [],
        "page_errors": [],
        "request_failures": [],
        "screenshots": [],
    }

    with tempfile.TemporaryDirectory(prefix="opencode2api-api-workspace-") as raw:
        temp = Path(raw)
        runtime = temp / "runtime"
        config = temp / "config.toml"
        config.write_text(
            f'''# API workspace manual fixture
schema_version = 1
port = {port}
host = "127.0.0.1"
model = "opencode/deepseek-v4-flash-free"
auth_tokens = ["{first_key}", "{second_key}"]
dashboard_admin_token = "{dashboard_token}"
rest_api_token = "dashboard-api-rest-secret-20260721"
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
        start = subprocess.run(
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
        if start.returncode != 0:
            raise RuntimeError(start.stderr)
        wait_health(base, True)

        try:
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(headless=True)
                context = browser.new_context(
                    viewport={"width": 1440, "height": 900},
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

                # Inline icon system: no external icon/font network dependency.
                expect(page.locator(".nav-label .icon")).to_have_count(7)
                expect(page.locator(".metric-icon .icon")).to_have_count(4)
                external_resources = page.evaluate(
                    """() => performance.getEntriesByType('resource')
                      .map(entry => entry.name)
                      .filter(url => !url.startsWith(location.origin))"""
                )
                assert external_resources == [], external_resources
                summary["actions"]["inline_svg_icons"] = "PASS"

                # Uptime must advance locally each second while synchronized to backend.
                first_text = page.locator("#metricUptime").inner_text()
                first_seconds = parse_duration(first_text)
                page.wait_for_timeout(3200)
                second_text = page.locator("#metricUptime").inner_text()
                second_seconds = parse_duration(second_text)
                assert second_seconds - first_seconds >= 2, (first_text, second_text)
                summary["actions"]["live_uptime"] = {
                    "before": first_text,
                    "after": second_text,
                    "delta_seconds": second_seconds - first_seconds,
                }

                page.click(".nav-item[data-view='api']")
                page.wait_for_selector(
                    "[data-view-panel='api'].active", state="visible", timeout=15_000
                )
                expect(page.locator("#viewTitle")).to_have_text("API")
                expect(page.locator("#apiKeyInventory tbody tr")).to_have_count(
                    2, timeout=20_000
                )
                inventory_text = page.locator("#apiKeyInventory").inner_text()
                assert first_key not in inventory_text and second_key not in inventory_text
                assert "sk-oc2-" in inventory_text
                summary["actions"]["fingerprinted_inventory"] = "PASS"

                desktop_path = OUT / "desktop-api.png"
                page.screenshot(path=str(desktop_path), full_page=True)
                summary["screenshots"].append(str(desktop_path.relative_to(ROOT)))

                # Generate an ephemeral key and make it available to config generation.
                page.fill("#keyCount", "1")
                page.fill("#keyBytes", "16")
                page.uncheck("#keySave")
                page.click("#keyForm button[type='submit']")
                expect(page.locator("#keyOutput")).to_contain_text(
                    "sk-oc2-", timeout=20_000
                )
                generated_key = page.locator("#keyOutput").inner_text().strip()
                assert generated_key.startswith("sk-oc2-")
                assert not page.locator(
                    "#clientConfigKeySource option[value='latest']"
                ).is_disabled()
                summary["actions"]["ephemeral_key"] = "PASS"

                # Default Claude Code config must contain a placeholder, never a live key.
                page.select_option("#clientConfigFormat", "claude-code")
                page.select_option("#clientConfigKeySource", "placeholder")
                page.click("#clientConfigForm button[type='submit']")
                expect(page.locator("#clientConfigOutput")).to_contain_text(
                    "ANTHROPIC_BASE_URL", timeout=20_000
                )
                placeholder_text = page.locator("#clientConfigOutput").inner_text()
                settings = json.loads(placeholder_text)
                assert settings["env"]["ANTHROPIC_API_KEY"] == "sk-oc2-REPLACE_ME"
                assert settings["env"]["ANTHROPIC_BASE_URL"] == base
                assert first_key not in placeholder_text
                assert generated_key not in placeholder_text
                summary["actions"]["placeholder_claude_config"] = "PASS"

                with page.expect_download(timeout=20_000) as download_info:
                    page.click("#downloadClientConfigButton")
                download = download_info.value
                assert download.suggested_filename == "claude-code-settings.json"
                downloaded = OUT / "claude-code-settings.json"
                download.save_as(str(downloaded))
                json.loads(downloaded.read_text(encoding="utf-8"))
                summary["actions"]["config_download"] = "PASS"

                # Explicit Latest embeds the just-generated secret only after confirmation.
                page.select_option("#clientConfigFormat", "env")
                page.select_option("#clientConfigKeySource", "latest")
                page.click("#clientConfigForm button[type='submit']")
                expect(page.locator("#confirmDialog")).to_be_visible(timeout=10_000)
                page.click("#confirmAccept")
                expect(page.locator("#clientConfigOutput")).to_contain_text(
                    generated_key, timeout=20_000
                )
                expect(page.locator("#clientConfigSecretBadge")).to_have_text(
                    "Contains secret"
                )
                summary["actions"]["explicit_secret_export"] = "PASS"

                # Revoke the second saved key through the browser UI.
                revoke = page.locator("[data-revoke-key-index='1']")
                revoke.click()
                expect(page.locator("#confirmDialog")).to_be_visible(timeout=10_000)
                page.click("#confirmAccept")
                expect(page.locator("#apiKeyInventory tbody tr")).to_have_count(
                    1, timeout=20_000
                )
                parsed_config = tomllib.loads(config.read_text(encoding="utf-8"))
                assert parsed_config["auth_tokens"] == [first_key]
                assert "# API workspace manual fixture" in config.read_text(
                    encoding="utf-8"
                )
                summary["actions"]["revoke_key_ui"] = "PASS"

                # Geometry checks after larger typography.
                desktop_geometry = page.evaluate(
                    """() => ({
                      scrollWidth: document.documentElement.scrollWidth,
                      clientWidth: document.documentElement.clientWidth,
                      bodyWidth: document.body.getBoundingClientRect().width
                    })"""
                )
                assert desktop_geometry["scrollWidth"] <= desktop_geometry["clientWidth"] + 1
                summary["actions"]["desktop_no_overflow"] = desktop_geometry

                mobile = context.new_page()
                mobile.set_viewport_size({"width": 390, "height": 844})
                mobile.goto(base + "/dashboard/#api", wait_until="domcontentloaded")
                expect(mobile.locator("#sidebarStatusText")).to_contain_text(
                    "Connected", timeout=20_000
                )
                mobile.click("#menuButton")
                expect(mobile.locator("#sidebar")).to_be_visible()
                mobile.click(".nav-item[data-view='api']")
                mobile_geometry = mobile.evaluate(
                    """() => ({
                      scrollWidth: document.documentElement.scrollWidth,
                      clientWidth: document.documentElement.clientWidth
                    })"""
                )
                assert mobile_geometry["scrollWidth"] <= mobile_geometry["clientWidth"] + 1
                mobile_path = OUT / "mobile-api.png"
                mobile.screenshot(path=str(mobile_path), full_page=True)
                summary["screenshots"].append(str(mobile_path.relative_to(ROOT)))
                summary["actions"]["mobile_no_overflow"] = mobile_geometry
                mobile.close()
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

    if (
        summary["console_errors"]
        or summary["page_errors"]
        or summary["request_failures"]
    ):
        summary["status"] = "FAIL"
    rendered = redact(json.dumps(summary, indent=2, ensure_ascii=False)) + "\n"
    (OUT / "summary.json").write_text(rendered, encoding="utf-8")
    print(rendered)
    if summary["status"] != "PASS":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
