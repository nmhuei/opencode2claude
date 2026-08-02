import http.cookiejar
import json
import os
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CLI = ROOT / 'target/debug/opencode2api'
ART = ROOT / 'artifacts/dashboard-simple-redesign'
ART.mkdir(parents=True, exist_ok=True)
TOKEN = 'simple-dashboard-lifecycle-admin-20260721'
REST = 'simple-dashboard-lifecycle-rest-20260721'
AUTH = 'sk-oc2-lifecycle-12345678901234567890'  # EXAMPLE_SECRET_SCAN_ALLOW

with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    PORT = sock.getsockname()[1]
BASE = f'http://127.0.0.1:{PORT}'
results = {'port': PORT}


def is_healthy():
    try:
        with urllib.request.urlopen(BASE + '/health', timeout=1) as response:
            return response.status == 200
    except Exception:
        return False


def wait_health(expect_up, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if is_healthy() == expect_up:
            return True
        time.sleep(0.2)
    return False


def login():
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    request = urllib.request.Request(
        BASE + '/api/dashboard/login',
        data=json.dumps({'token': TOKEN}).encode(),
        headers={'content-type': 'application/json'},
        method='POST',
    )
    with opener.open(request, timeout=5) as response:
        assert response.status == 200
    csrf = next((cookie.value for cookie in jar if cookie.name == 'bridge_csrf_token'), None)
    assert csrf
    return opener, csrf


def dashboard_status(opener):
    with opener.open(BASE + '/api/dashboard/status', timeout=5) as response:
        return json.loads(response.read())


def mutate(opener, csrf, path):
    request = urllib.request.Request(
        BASE + path,
        data=b'{}',
        headers={'content-type': 'application/json', 'X-CSRF-Token': csrf},
        method='POST',
    )
    with opener.open(request, timeout=8) as response:
        return response.status, json.loads(response.read())


with tempfile.TemporaryDirectory(prefix='oc2api-lifecycle-outside-repo-') as raw:
    temp = Path(raw)
    runtime = temp / 'runtime'
    config = temp / 'config.toml'
    config.write_text(f'''schema_version = 1
port = {PORT}
host = "127.0.0.1"
dashboard_admin_token = "{TOKEN}"
rest_api_token = "{REST}"
auth_tokens = ["{AUTH}"]
egress_mode = "direct"
runtime_dir = "{runtime}"
model = "opencode/deepseek-v4-flash-free"
shell_policy = "disabled"
''')
    env = os.environ.copy()
    for key in ['DASHBOARD_ADMIN_TOKEN', 'REST_API_TOKEN', 'BRIDGE_AUTH_TOKEN', 'BRIDGE_PORT', 'BRIDGE_HOST']:
        env.pop(key, None)
    env['BRIDGE_CONFIG_PATH'] = str(config)
    env['RUNTIME_DIR'] = str(runtime)

    try:
        start = subprocess.run(
            [str(CLI), '--quiet', 'server', 'start', '--no-proxy', '--port', str(PORT), '--host', '127.0.0.1'],
            cwd=temp,
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        results['start_exit'] = start.returncode
        results['start_stdout'] = start.stdout.strip()
        results['start_stderr'] = start.stderr.strip()
        assert start.returncode == 0, results
        assert wait_health(True), 'server did not start'

        opener, csrf = login()
        first_pid = dashboard_status(opener)['pid']
        code, payload = mutate(opener, csrf, '/api/dashboard/control/server/restart')
        results['restart_http'] = code
        results['restart_payload'] = payload
        assert code == 202

        assert wait_health(False, timeout=20), 'restart never took server down'
        results['restart_saw_down'] = True
        assert wait_health(True, timeout=40), 'restart did not bring server back'
        results['restart_returned'] = True

        # Restart creates a new session secret, so authenticate again before
        # reading protected dashboard state.
        opener, csrf = login()
        second_pid = dashboard_status(opener)['pid']
        results['first_pid'] = first_pid
        results['second_pid'] = second_pid
        assert second_pid != first_pid, results

        code, payload = mutate(opener, csrf, '/api/dashboard/control/server/stop')
        results['stop_http'] = code
        results['stop_payload'] = payload
        assert code == 202
        assert wait_health(False, timeout=30), 'server did not stop'
        results['stopped'] = True

        status = subprocess.run(
            [str(CLI), '--quiet', 'server', 'status', '--port', str(PORT), '--host', '127.0.0.1'],
            cwd=temp,
            env=env,
            capture_output=True,
            text=True,
            timeout=15,
        )
        results['final_status_stdout'] = status.stdout.strip()
        results['final_status_stderr'] = status.stderr.strip()
        results['status'] = 'PASS'
    finally:
        time.sleep(0.5)
        debug_dir = ART / 'lifecycle-runtime'
        if debug_dir.exists():
            shutil.rmtree(debug_dir)
        debug_dir.mkdir(parents=True)
        shutil.copy2(config, debug_dir / 'config.toml')
        for filename in ['opencode2api.log', 'opencode2api.pid.json']:
            source = runtime / filename
            if source.exists():
                shutil.copy2(source, debug_dir / filename)
        if is_healthy():
            subprocess.run(
                [str(CLI), '--quiet', 'server', 'stop', '--port', str(PORT), '--host', '127.0.0.1'],
                cwd=temp,
                env=env,
                capture_output=True,
                timeout=15,
            )

(ART / 'lifecycle-summary.json').write_text(json.dumps(results, indent=2, ensure_ascii=False))
print(json.dumps(results, indent=2, ensure_ascii=False))
