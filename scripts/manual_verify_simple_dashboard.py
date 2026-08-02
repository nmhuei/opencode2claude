import json
import os
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/debug/opencode2api-serve'
ART = ROOT / 'artifacts/dashboard-simple-redesign'
ART.mkdir(parents=True, exist_ok=True)
TOKEN = 'simple-dashboard-admin-20260721'
REST = 'simple-dashboard-rest-20260721'
LEGACY = 'sk-oc2-simple-legacy-12345678901234567890'  # EXAMPLE_SECRET_SCAN_ALLOW

with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    PORT = sock.getsockname()[1]
BASE = f'http://127.0.0.1:{PORT}'
results = {}


def wait_health(timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(BASE + '/health', timeout=1) as response:
                if response.status == 200:
                    return True
        except Exception:
            time.sleep(0.15)
    return False


def api_status(secret):
    request = urllib.request.Request(BASE + '/v1/models', headers={'Authorization': 'Bearer ' + secret})
    try:
        with urllib.request.urlopen(request, timeout=8) as response:
            return response.status
    except urllib.error.HTTPError as error:
        return error.code


with tempfile.TemporaryDirectory(prefix='oc2api-simple-dashboard-') as raw:
    temp = Path(raw)
    config = temp / 'config.toml'
    runtime = temp / 'runtime'
    config.write_text(f'''schema_version = 1
port = {PORT}
host = "127.0.0.1"
dashboard_admin_token = "{TOKEN}"
rest_api_token = "{REST}"
auth_tokens = ["{LEGACY}"]
egress_mode = "direct"
runtime_dir = "{runtime}"
model = "opencode/deepseek-v4-flash-free"
shell_policy = "disabled"
primary_proxies = ["socks5h://127.0.0.1:40001", "socks5h://127.0.0.1:40002", "socks5h://127.0.0.1:40003"]
warm_standby_proxies = ["socks5h://127.0.0.1:40004", "socks5h://127.0.0.1:40005"]
''')
    env = os.environ.copy()
    for key in ['DASHBOARD_ADMIN_TOKEN', 'REST_API_TOKEN', 'BRIDGE_AUTH_TOKEN', 'BRIDGE_PORT', 'BRIDGE_HOST', 'BRIDGE_CONFIG_PATH', 'RUNTIME_DIR']:
        env.pop(key, None)
    process = subprocess.Popen(
        [str(BIN), '--config', str(config), '--port', str(PORT), '--host', '127.0.0.1', '--egress-mode', 'direct'],
        cwd=temp,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert wait_health(), 'server not healthy'
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context(viewport={'width': 1440, 'height': 900}, permissions=['clipboard-read', 'clipboard-write'])
            page = context.new_page()
            console_errors = []
            page_errors = []
            request_failures = []
            page.on('console', lambda message: console_errors.append(message.text) if message.type == 'error' else None)
            page.on('pageerror', lambda error: page_errors.append(str(error)))
            page.on('requestfailed', lambda request: request_failures.append(request.url + ' ' + str(request.failure)))

            page.goto(BASE + '/', wait_until='domcontentloaded')
            page.fill('#password', TOKEN)
            page.click('#submitBtn')
            page.wait_for_url('**/dashboard/**')
            page.wait_for_selector('[data-view-panel="dashboard"].active')
            results['login'] = True
            results['nav_items'] = page.locator('#navList .nav-item').count()
            assert results['nav_items'] == 4
            results['desktop_overflow'] = page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth')
            assert not results['desktop_overflow']
            expect(page.locator('#metricService')).not_to_have_text('—')
            expect(page.locator('#metricModel')).not_to_have_text('—')
            results['dashboard_summary'] = True
            results['heartbeat_visible'] = 'heartbeat' in page.locator('#eventList').inner_text().lower()
            assert not results['heartbeat_visible']

            page.click('#languageToggle')
            expect(page.locator('[data-view="dashboard"] span')).to_have_text('Tổng quan')
            page.click('#languageToggle')
            expect(page.locator('[data-view="dashboard"] span')).to_have_text('Dashboard')
            results['language_toggle'] = True

            page.click('[data-view="api"]')
            page.wait_for_selector('[data-view-panel="api"].active')
            page.click('#createApiKeyButton')
            page.wait_for_selector('#apiKeyCreateDialog[open]')
            page.fill('#createKeyName', 'Manual Simple Key')
            page.select_option('#createKeyPreset', 'backend')
            page.locator('#apiKeyCreateForm button[type="submit"]').click()
            page.wait_for_timeout(1200)
            if not page.locator('#apiKeySecretDialog').evaluate('node => node.open'):
                print('CREATE_DEBUG', {'toasts': page.locator('.toast').all_inner_texts(), 'console_errors': console_errors, 'page_errors': page_errors})
            page.wait_for_selector('#apiKeySecretDialog[open]')
            secret = page.locator('#keyOutput').inner_text().strip()
            assert secret.startswith('sk-oc2-')
            results['create_key'] = True
            page.click('#copyGeneratedKeyButton')
            assert page.evaluate('navigator.clipboard.readText()') == secret
            with page.expect_download() as download_info:
                page.click('#downloadGeneratedConfigButton')
            generated_download = download_info.value
            generated_content = Path(generated_download.path()).read_text()
            assert secret in generated_content
            results['copy_and_download_secret_config'] = True
            page.click('#apiKeySecretDialog [data-close-dialog="apiKeySecretDialog"]')
            page.wait_for_selector('tr[data-api-key-id]')
            assert api_status(secret) == 200
            results['key_immediate_auth'] = True

            row = page.locator('tr[data-api-key-id]').filter(has_text='Manual Simple Key')
            row.click()
            page.wait_for_selector('#apiKeyDrawer[open]')
            assert page.locator('[data-drawer-tab]').count() == 4
            page.select_option('#editKeyStatus', 'disabled')
            page.locator('#apiKeyEditForm button[type="submit"]').click()
            page.wait_for_timeout(300)
            assert api_status(secret) in (401, 403)
            results['disable_hot_reload'] = True
            page.select_option('#editKeyStatus', 'active')
            page.locator('#apiKeyEditForm button[type="submit"]').click()
            page.wait_for_timeout(300)
            assert api_status(secret) == 200
            results['enable_hot_reload'] = True
            page.click('#apiKeyDrawer [data-close-dialog="apiKeyDrawer"]')

            page.click('#verifyApiKeyButton')
            page.fill('#verifyApiKeySecret', secret)
            page.locator('#verifyApiKeyForm button[type="submit"]').click()
            expect(page.locator('#verifyApiKeyResult')).to_contain_text('Manual Simple Key')
            results['check_key'] = True
            page.click('#apiKeyVerifyDialog [data-close-dialog="apiKeyVerifyDialog"]')

            page.locator('tr[data-api-key-id]').filter(has_text='Manual Simple Key').click()
            page.wait_for_selector('#apiKeyDrawer[open]')
            page.click('#rotateApiKeyButton')
            page.wait_for_selector('#confirmDialog[open]')
            page.click('#confirmAccept')
            page.wait_for_selector('#apiKeySecretDialog[open]')
            rotated = page.locator('#keyOutput').inner_text().strip()
            assert rotated != secret
            assert api_status(secret) in (401, 403)
            assert api_status(rotated) == 200
            results['rotate_key'] = True
            page.click('#apiKeySecretDialog [data-close-dialog="apiKeySecretDialog"]')
            page.locator('tr[data-api-key-id]').filter(has_text='Manual Simple Key').click()
            page.wait_for_selector('#apiKeyDrawer[open]')
            page.click('#revokeApiKeyButton')
            page.wait_for_selector('#confirmDialog[open]')
            page.click('#confirmAccept')
            page.wait_for_timeout(350)
            assert api_status(rotated) in (401, 403)
            results['revoke_key'] = True

            page.click('[data-view="models"]')
            page.wait_for_selector('[data-view-panel="models"].active')
            expect(page.locator('#currentModelName')).not_to_have_text('—')
            page.fill('#modelSearch', 'Nemotron')
            expect(page.locator('#modelGrid .model-row')).to_have_count(1)
            page.fill('#modelSearch', '')
            select_buttons = page.locator('#modelGrid [data-select-model]:not([disabled])')
            if select_buttons.count():
                select_buttons.first.click()
                page.wait_for_timeout(250)
                assert not page.locator('#modelRestartNotice').is_hidden()
            results['model_list_select'] = True
            original = page.locator('#modelGrid [data-select-model="opencode/deepseek-v4-flash-free"]')
            if original.count() and original.is_enabled():
                original.click()
                page.wait_for_timeout(250)
            page.fill('#testerPrompt', 'Reply with exactly: dashboard-ok')
            page.uncheck('#testerStream')
            page.click('#testerSubmit')
            expect(page.locator('#testerLatency')).not_to_have_text('Running', timeout=45000)
            tester_text = page.locator('#testerOutput').inner_text()
            results['model_test_output'] = tester_text.strip()
            results['model_test_completed'] = bool(tester_text.strip()) and not tester_text.lstrip().startswith('Error:')
            assert results['model_test_completed']

            page.click('[data-view="system"]')
            page.wait_for_selector('[data-view-panel="system"].active')
            expect(page.locator('#serverFacts')).not_to_be_empty()
            assert page.locator('#networkProxyTable tbody tr').count() == 5
            results['system_server_proxy'] = True
            page.click('#openLogsButton')
            page.wait_for_selector('#logsDialog[open]')
            page.wait_for_timeout(200)
            assert page.locator('#serverLogOutput').inner_text().strip()
            results['logs'] = True
            page.click('#logsDialog [data-close-dialog="logsDialog"]')
            page.click('#openDiagnosticsButton')
            page.wait_for_selector('#diagnosticsDialog[open]')
            page.wait_for_selector('#doctorResults .diagnostic-item', timeout=45000)
            results['diagnostics'] = page.locator('#doctorResults .diagnostic-item').count() > 0
            page.click('#diagnosticsDialog [data-close-dialog="diagnosticsDialog"]')
            page.click('#openConfigButton')
            page.wait_for_selector('#configDialog[open]')
            expect(page.locator('#configEditor')).not_to_have_value('', timeout=15000)
            page.click('#previewConfigButton')
            page.wait_for_timeout(300)
            assert page.locator('#configPreviewOutput').inner_text().strip()
            results['config_load_validate'] = True
            page.click('#applyConfigButton')
            page.wait_for_selector('#confirmDialog[open]')
            page.click('#confirmAccept')
            expect(page.locator('.toast.success').last).to_contain_text('Configuration applied', timeout=15000)
            results['config_save_unchanged'] = True
            page.click('#configDialog [data-close-dialog="configDialog"]')

            page.click('#restartServerButton')
            page.wait_for_selector('#confirmDialog[open]')
            page.locator('#confirmDialog button[value="cancel"]').click()
            page.click('#stopServerButton')
            page.wait_for_selector('#confirmDialog[open]')
            page.locator('#confirmDialog button[value="cancel"]').click()
            results['lifecycle_confirmations'] = True

            page.screenshot(path=str(ART / 'desktop-dashboard.png'), full_page=True)
            page.set_viewport_size({'width': 390, 'height': 844})
            page.goto(BASE + '/dashboard/#dashboard', wait_until='domcontentloaded')
            page.wait_for_timeout(300)
            results['mobile_overflow'] = page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth')
            assert not results['mobile_overflow']
            page.click('#menuButton')
            assert 'sidebar-open' in page.locator('body').get_attribute('class')
            page.screenshot(path=str(ART / 'mobile-dashboard.png'), full_page=True)
            results['console_errors'] = console_errors
            results['page_errors'] = page_errors
            results['request_failures'] = request_failures
            assert not page_errors, page_errors
            browser.close()
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)
        if process.stdout:
            (ART / 'server.log').write_text(process.stdout.read())

results['status'] = 'PASS'
(ART / 'summary.json').write_text(json.dumps(results, indent=2, ensure_ascii=False))
print(json.dumps(results, indent=2, ensure_ascii=False))
