#!/usr/bin/env python3
from __future__ import annotations
import json, os, socket, subprocess, tempfile, time, urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
BIN=ROOT/'target/release/opencode2api'
OUT=ROOT/'artifacts/search-loop-manual'
OUT.mkdir(parents=True,exist_ok=True)

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1',0)); return s.getsockname()[1]

def wait(base, desired=True):
    end=time.time()+30
    while time.time()<end:
        ok=False
        try:
            with urllib.request.urlopen(base+'/health',timeout=1) as r: ok=r.status==200
        except Exception: pass
        if ok==desired:return
        time.sleep(.2)
    raise RuntimeError('health timeout')

p=port(); base=f'http://127.0.0.1:{p}'
with tempfile.TemporaryDirectory(prefix='opencode2api-search-guard-') as raw:
    d=Path(raw); runtime=d/'runtime'; config=d/'config.toml'; settings=d/'settings.json'
    config.write_text(f'''schema_version = 1
port = {p}
host = "127.0.0.1"
model = "opencode/deepseek-v4-flash-free"
egress_mode = "direct"
runtime_dir = "{runtime}"
upstream_base_url = "https://opencode.ai/zen/v1"
max_search_loops = 1
enable_default_fallbacks = false
''')
    settings.write_text(json.dumps({'env':{'ANTHROPIC_BASE_URL':base,'ANTHROPIC_API_KEY':'search-guard'}}))
    env=os.environ.copy()
    for k in ['BRIDGE_CONFIG_PATH','BRIDGE_PORT','BRIDGE_HOST','RUNTIME_DIR','OPENCODE_MODEL','ANTHROPIC_BASE_URL','ANTHROPIC_API_KEY']:
        env.pop(k,None)
    env['BRIDGE_CONFIG_PATH']=str(config); env['RUNTIME_DIR']=str(runtime)
    start=subprocess.run([str(BIN),'--quiet','server','start','--no-proxy','--config',str(config),'--port',str(p)],cwd=d,env=env,text=True,capture_output=True,timeout=30)
    if start.returncode: raise RuntimeError(start.stderr)
    wait(base)
    try:
        cmd=['claude','-p','Use WebSearch for at least three distinct searches about Claude Code security, then provide a short sourced summary. Even if further search is unavailable, synthesize from existing results.','--settings',str(settings),'--tools','WebSearch','--allowedTools','WebSearch','--permission-mode','bypassPermissions','--max-turns','10','--effort','max','--output-format','json']
        run=subprocess.run(cmd,cwd=d,env=env,text=True,capture_output=True,timeout=180)
        (OUT/'loop-budget-one.json').write_text(run.stdout)
        (OUT/'loop-budget-one.stderr').write_text(run.stderr)
        data=json.loads(run.stdout); result=data.get('result') or ''
        summary={'status':'PASS' if run.returncode==0 and not data.get('is_error') and 'search_loop_protection' not in result and 'API Error' not in result and '[Requesting Tool execution:' not in result and len(result)>80 else 'FAIL','exit_code':run.returncode,'is_error':data.get('is_error'),'has_search_loop':'search_loop_protection' in result,'has_api_error':'API Error' in result,'has_marker':'[Requesting Tool execution:' in result,'has_url':'http' in result,'result':result,'session_id':data.get('session_id')}
        (OUT/'loop-budget-one-summary.json').write_text(json.dumps(summary,indent=2,ensure_ascii=False)+'\n')
        print(json.dumps(summary,indent=2,ensure_ascii=False))
        if summary['status']!='PASS': raise SystemExit(1)
    finally:
        subprocess.run([str(BIN),'--quiet','server','stop'],cwd=d,env=env,text=True,capture_output=True,timeout=20)
