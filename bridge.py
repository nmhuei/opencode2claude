import json
import os
import subprocess
import sys
import time
import urllib.request
import socket
from http.server import HTTPServer, BaseHTTPRequestHandler

# Color logs
GREEN = '\033[0;32m'
BLUE = '\033[0;34m'
YELLOW = '\033[1;33m'
RED = '\033[0;31m'
NC = '\033[0m'

def log_info(msg):
    print(f"{BLUE}[INFO]{NC} {msg}", flush=True)

def log_success(msg):
    print(f"{GREEN}[SUCCESS]{NC} {msg}", flush=True)

def log_warn(msg):
    print(f"{YELLOW}[WARNING]{NC} {msg}", flush=True)

def log_error(msg):
    print(f"{RED}[ERROR]{NC} {msg}", flush=True)


class ClaudeOpenCodeBridge(BaseHTTPRequestHandler):
    
    def log_message(self, format, *args):
        # Suppress default HTTP server logs to keep console clean
        pass

    def do_POST(self):
        if self.path == '/v1/messages':
            start_time = time.time()
            content_length = int(self.headers.get('Content-Length', 0))
            post_data = self.rfile.read(content_length)
            
            try:
                req = json.loads(post_data.decode('utf-8'))
            except Exception as e:
                log_error(f"Failed to parse request JSON: {str(e)}")
                self.send_response(400)
                self.end_headers()
                self.wfile.write(b"Invalid JSON")
                return
            
            # 1. Extract prompt from user messages
            prompt = ""
            messages = req.get('messages', [])
            for msg in messages:
                if msg.get('role') == 'user':
                    content = msg.get('content', '')
                    if isinstance(content, list):
                        prompt += "\n".join([item.get('text', '') for item in content if item.get('type') == 'text'])
                    else:
                        prompt += f"\n{content}"
            prompt = prompt.strip()
            
            # Determine target model
            req_model = req.get('model', 'claude-3-5-sonnet')
            env_model = os.getenv('OPENCODE_MODEL')
            
            # Model mapping: If user requests a specific model or we have an env override
            target_model = env_model if env_model else None
            
            log_info(f"Incoming prompt: '{prompt[:60]}...' [Model: {req_model}]")
            
            # 2. Check if OpenCode Daemon is active on the configured port
            opencode_port = os.getenv('OPENCODE_PORT', '4096')
            opencode_server_url = f"http://127.0.0.1:{opencode_port}"
            use_attach = False
            try:
                with urllib.request.urlopen(f"{opencode_server_url}/doc", timeout=1) as response:
                    if response.status == 200:
                        use_attach = True
            except Exception:
                pass

            # 3. Assemble command parameters
            cmd = ["opencode", "run"]
            if use_attach:
                cmd += ["--attach", opencode_server_url]
                log_info(f"Attached to active OpenCode Daemon on port {opencode_port}")
            else:
                log_warn("No active OpenCode Daemon found. Running in standalone mode (slower).")
            
            if target_model:
                cmd += ["-m", target_model]
                log_info(f"Using model: {target_model}")
            
            cmd += ["--dangerously-skip-permissions", prompt]
            
            # 4. Handle streaming vs non-streaming responses
            stream = req.get('stream', False)
            
            if stream:
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Cache-Control', 'no-cache')
                self.send_header('Connection', 'keep-alive')
                self.end_headers()
                
                # Send start events
                self.send_event("message_start", {
                    "type": "message_start",
                    "message": {
                        "id": "msg_opencode",
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": req_model,
                        "stop_reason": None,
                        "stop_sequence": None,
                        "usage": {"input_tokens": 0, "output_tokens": 0}
                    }
                })
                self.send_event("content_block_start", {
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "text", "text": ""}
                })
                
                try:
                    # Spawn process with error replacement to handle decoding issues
                    process = subprocess.Popen(
                        cmd, 
                        stdout=subprocess.PIPE, 
                        stderr=subprocess.PIPE, 
                        text=True, 
                        bufsize=1,
                        errors='replace'
                    )
                    
                    # Read character by character for instant stream feedback
                    while True:
                        char = process.stdout.read(1)
                        if not char and process.poll() is not None:
                            break
                        if char:
                            self.send_event("content_block_delta", {
                                "type": "content_block_delta",
                                "index": 0,
                                "delta": {"type": "text_delta", "text": char}
                            })
                            
                    # Check for execution errors in stderr
                    stderr_content = process.stderr.read()
                    if stderr_content.strip():
                        log_error(f"Subprocess stderr output: {stderr_content}")
                        self.send_event("content_block_delta", {
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "text_delta", "text": f"\n\n[OpenCode Error]: {stderr_content}"}
                        })
                except Exception as e:
                    log_error(f"Process execution failed: {str(e)}")
                    self.send_event("content_block_delta", {
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "text_delta", "text": f"\n\n[Bridge Error]: {str(e)}"}
                    })
                
                # Send completion events
                self.send_event("content_block_stop", {"type": "content_block_stop", "index": 0})
                self.send_event("message_delta", {
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                    "usage": {"output_tokens": 0}
                })
                self.send_event("message_stop", {"type": "message_stop"})
                
                duration = time.time() - start_time
                log_success(f"Stream request completed in {duration:.2f}s")
                
            else:
                # Non-streaming response
                try:
                    result = subprocess.run(cmd, capture_output=True, text=True, timeout=120, errors='replace')
                    if result.returncode == 0:
                        response_text = result.stdout
                        log_success(f"Execution completed in {time.time() - start_time:.2f}s")
                    else:
                        response_text = f"Error execution returned non-zero code {result.returncode}:\n{result.stderr}"
                        log_error(f"OpenCode error: {result.stderr}")
                except Exception as e:
                    response_text = f"Bridge Execution Error: {str(e)}"
                    log_error(f"Bridge execution error: {str(e)}")
                
                response_data = {
                    "id": "msg_opencode",
                    "type": "message",
                    "role": "assistant",
                    "model": req_model,
                    "content": [{"type": "text", "text": response_text}],
                    "stop_reason": "end_turn",
                    "stop_sequence": None,
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                }
                
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                self.wfile.write(json.dumps(response_data).encode('utf-8'))
        else:
            self.send_response(404)
            self.end_headers()

    def send_event(self, event, data):
        try:
            self.wfile.write(f"event: {event}\n".encode('utf-8'))
            self.wfile.write(f"data: {json.dumps(data)}\n\n".encode('utf-8'))
            self.wfile.flush()
        except Exception:
            pass


def is_port_in_use(port):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        return s.connect_ex(('127.0.0.1', port)) == 0


def run():
    bridge_port = int(os.getenv('BRIDGE_PORT', '4000'))
    
    if is_port_in_use(bridge_port):
        log_error(f"Port {bridge_port} is already in use. Please terminate the process or change BRIDGE_PORT.")
        sys.exit(1)
        
    server_address = ('127.0.0.1', bridge_port)
    httpd = HTTPServer(server_address, ClaudeOpenCodeBridge)
    
    log_success(f"OpenCode2Claude Bridge listening on http://127.0.0.1:{bridge_port}...")
    log_info(f"To redirect Claude Code, run: export ANTHROPIC_API_URL=\"http://127.0.0.1:{bridge_port}/v1\"")
    
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("")
        log_info("Stopping API Bridge...")
        sys.exit(0)


if __name__ == '__main__':
    run()
