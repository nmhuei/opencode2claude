import json
import subprocess
import sys
import urllib.request
from http.server import HTTPServer, BaseHTTPRequestHandler

class ClaudeOpenCodeBridge(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path == '/v1/messages':
            content_length = int(self.headers.get('Content-Length', 0))
            post_data = self.rfile.read(content_length)
            req = json.loads(post_data.decode('utf-8'))
            
            # 1. Trích xuất prompt từ yêu cầu của Claude Code
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
            
            # Kiểm tra xem server opencode có đang chạy ở port 4096 không
            opencode_server_url = "http://127.0.0.1:4096"
            use_attach = False
            try:
                # Kiểm tra nhanh kết nối tới server 4096
                with urllib.request.urlopen(f"{opencode_server_url}/doc", timeout=1) as response:
                    if response.status == 200:
                        use_attach = True
            except Exception:
                pass

            # Chuẩn bị lệnh gọi OpenCode CLI
            cmd = ["opencode", "run"]
            if use_attach:
                cmd += ["--attach", opencode_server_url]
            cmd += ["--dangerously-skip-permissions", prompt]
            
            # 2. Xử lý phản hồi (Streaming hoặc Non-streaming)
            stream = req.get('stream', False)
            
            if stream:
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Cache-Control', 'no-cache')
                self.send_header('Connection', 'keep-alive')
                self.end_headers()
                
                # Gửi event start
                self.send_event("message_start", {
                    "type": "message_start",
                    "message": {
                        "id": "msg_opencode",
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": req.get('model', 'claude-3-5-sonnet'),
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
                    process = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1)
                    # Đọc từng ký tự từ stdout của OpenCode
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
                except Exception as e:
                    self.send_event("content_block_delta", {
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "text_delta", "text": f"\n[Bridge Error]: {str(e)}"}
                    })
                
                # Gửi event stop
                self.send_event("content_block_stop", {"type": "content_block_stop", "index": 0})
                self.send_event("message_delta", {
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                    "usage": {"output_tokens": 0}
                })
                self.send_event("message_stop", {"type": "message_stop"})
                
            else:
                # Trả về toàn bộ phản hồi một lượt (Non-stream)
                try:
                    result = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
                    response_text = result.stdout if result.returncode == 0 else f"Error: {result.stderr}"
                except Exception as e:
                    response_text = f"Bridge Execution Error: {str(e)}"
                
                response_data = {
                    "id": "msg_opencode",
                    "type": "message",
                    "role": "assistant",
                    "model": req.get('model', 'claude-3-5-sonnet'),
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

def run(port=4000):
    server_address = ('127.0.0.1', port)
    httpd = HTTPServer(server_address, ClaudeOpenCodeBridge)
    print(f"Bridge đang chạy tại http://127.0.0.1:{port}...", flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nĐang tắt bridge...", flush=True)
        sys.exit(0)

if __name__ == '__main__':
    run()
