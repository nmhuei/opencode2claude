#!/usr/bin/env python3
"""OpenAI Chat Completions SSE stub — models what opencode.ai/zen returns.

Used with a SEPARATE test bridge instance (OPENCODE_UPSTREAM_BASE_URL points
here) for deterministic scenarios: text, thinking, tool calls, upstream
errors, mid-stream failures. Port 8124.
"""
from __future__ import annotations

import argparse
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODEL = "deepseek-v4-flash-free"
REQ_LOG = None
WIRE_LOG = None
DONE_MARKER = None
CTR = [0]
SERVED_ERRORS = set()


def chunk(delta: dict, finish_reason=None, cid: str = "chatcmpl_test"):
    c = {"id": cid, "object": "chat.completion.chunk", "created": 1750000000,
         "model": MODEL, "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]}
    return f"data: {json.dumps(c, ensure_ascii=False)}\n\n"


def build_lines_for(prompt: str, req: dict) -> list[str]:
    p = prompt.lower()
    lines = []
    has_tool_result = any(m.get("role") == "tool" for m in req.get("messages", []))
    if has_tool_result:
        # Continuation after the CLI ran the previous tool call: confirm the
        # execution result was folded back in, closing the tool-result loop.
        lines.append(chunk({"role": "assistant", "content": None}))
        lines.append(chunk({"content": "TOOL_RESULT_ACCEPTED"}))
        lines.append(chunk({"content": None}, finish_reason="stop"))
        lines.append("data: [DONE]\n\n")
        return lines
    if "scenario agent" in p:
        tool_name, tool_args = client_tool_call(req)
        lines.append(chunk({"role": "assistant", "content": None}))
        lines.append(chunk({"tool_calls": [{"index": 0, "id": "call_agent",
                                            "type": "function",
                                            "function": {"name": tool_name,
                                                        "arguments": ""}}]}))
        args_json = json.dumps(tool_args, ensure_ascii=False)
        mid = max(1, len(args_json) // 2)
        lines.append(chunk({"tool_calls": [{"index": 0, "function": {"arguments": args_json[:mid]}}]}))
        lines.append(chunk({"tool_calls": [{"index": 0, "function": {"arguments": args_json[mid:]}}]}))
        lines.append(chunk({}, finish_reason="tool_calls"))
        lines.append("data: [DONE]\n\n")
        return lines
    if "scenario tool" in p:
        lines.append(chunk({"role": "assistant", "content": None}))
        lines.append(chunk({"tool_calls": [{"index": 0, "id": "call_1",
                                            "type": "function",
                                            "function": {"name": "Bash", "arguments": ""}}]}))
        lines.append(chunk({"tool_calls": [{"index": 0, "function": {"arguments": '{"command"'}}]}))
        lines.append(chunk({"tool_calls": [{"index": 0, "function": {"arguments": ': "echo ok"}'}}]}))
        lines.append(chunk({}, finish_reason="tool_calls"))
        lines.append("data: [DONE]\n\n")
        return lines
    if "scenario blob" in p:
        # Provider pathology reproduced from the live screenshot: one complete
        # SSE event contains a large expert/subagent report instead of token
        # deltas. The bridge must translate this into bounded Anthropic deltas.
        reasoning = "Lập luận chuyên gia đang kiểm tra từng gate — " * 650
        report = "• Gate đã đối chiếu; bằng chứng và sai lệch được ghi nhận rõ ràng.\n" * 180
        lines.append(chunk({"role": "assistant", "content": None}))
        lines.append(chunk({"reasoning_content": reasoning}))
        lines.append(chunk({"content": report}))
        lines.append(chunk({"content": None}, finish_reason="stop"))
        lines.append("data: [DONE]\n\n")
        return lines
    if "scenario long" in p:
        for i in range(8):
            lines.append(chunk({"reasoning_content": f"deep thinking segment {i} " * 30}))
        for i in range(3):
            lines.append(chunk({"content": f"final line {i}\n"}))
        lines.append(chunk({"content": None}, finish_reason="stop"))
        lines.append("data: [DONE]\n\n")
        return lines
    if "scenario huge" in p:
        # ~60KB of reasoning: forces bridge's 16KB thinking segmentation
        for i in range(60):
            lines.append(chunk({"reasoning_content": f"H{i:03d} " + ("x" * 1000)}))
        for i in range(4):
            lines.append(chunk({"content": f"tail line {i}\n"}))
        lines.append(chunk({"content": None}, finish_reason="stop"))
        lines.append("data: [DONE]\n\n")
        return lines
    if "scenario slow" in p:
        # long stream with an idle gap in the middle so Ctrl+C lands mid-stream
        for i in range(3):
            lines.append(chunk({"reasoning_content": f"slow thinking {i} " * 40}))
        lines.append(chunk({"content": "middle"}))
        for i in range(3):
            lines.append(chunk({"content": f"slow tail {i} "}))
        lines.append(chunk({"content": None}, finish_reason="stop"))
        lines.append("data: [DONE]\n\n")
        return lines
    if "scenario midfail" in p:
        lines.append(chunk({"role": "assistant", "content": "partial"}))
        lines.append("data: {\"error\": {\"message\": \"mid-stream failure\", "
                     "\"type\": \"server_error\", \"code\": 500}}\n\n")
        return lines
    # default
    lines.append(chunk({"role": "assistant", "content": None}))
    lines.append(chunk({"content": "OK"}))
    lines.append(chunk({"content": None}, finish_reason="stop"))
    lines.append("data: [DONE]\n\n")
    return lines


def client_tool_call(req: dict) -> tuple[str, dict]:
    """Pick a tool the CLIENT offered so the CLI executes a real (agent) tool.

    Prefer an agent-spawning tool (Task/Agent/Explore/Note); otherwise fall
    back to the first offered tool so the tool-result loop still runs end to
    end against the real CLI.
    """
    tools = req.get("tools") or []
    names = [t.get("function", {}).get("name", "") for t in tools]
    preferred = ["Task", "Agent", "Explore", "Note", "WebFetch", "Bash", "Read"]
    picked = next((n for n in preferred if n in names), names[0] if names else "Bash")
    args = {
        "Task": {"description": "List files in the current directory", "prompt": "List files in the current directory"},
        "Agent": {"description": "List files in the current directory", "prompt": "List files in the current directory"},
        "Explore": {"description": "List files in the current directory"},
        "Note": {"text": "ping"},
        "Bash": {"command": "echo ok"},
        "Read": {"file_path": "README.md"},
        "WebFetch": {"url": "https://example.com", "prompt": "title"},
    }.get(picked, {"description": "list the directory"})
    return picked, args


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length) if length else b""
        try:
            req = json.loads(body) if body else {}
        except Exception:
            req = {}
        last_user = ""
        for m in req.get("messages", []):
            if m.get("role") == "user":
                c = m.get("content", "")
                if isinstance(c, str):
                    last_user = c
                elif isinstance(c, list):
                    last_user = " ".join(str(b.get("text", "")) for b in c if isinstance(b, dict))
        CTR[0] += 1
        tool_names = [t.get("function", {}).get("name", "") for t in (req.get("tools") or [])]
        if REQ_LOG:
            print(f"REQ #{CTR[0]} path={self.path} stream={req.get('stream')} "
                  f"model={req.get('model')} last_user={last_user[:60]!r} "
                  f"tools={tool_names}",
                  file=REQ_LOG, flush=True)
        time.sleep(0.4)
        if "scenario error" in last_user.lower() and last_user not in SERVED_ERRORS:
            # First request for a given error-prompt: 500 so the CLI's own
            # retry loop is exercised (reference behavior: stream error →
            # client retries). Subsequent requests recover with a normal turn.
            SERVED_ERRORS.add(last_user)
            payload = {"error": {"message": "upstream exploded", "type": "server_error",
                                 "code": 500}}
            self.send_response(500)
            self.send_header("Content-Type", "application/json")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(json.dumps(payload).encode())
            return
        # "scenario error" requests after the first are handled by the normal
        # path below (JSON for stream=False, SSE OK for stream=True).
        if not req.get("stream"):
            payload = {"id": "chatcmpl_test", "object": "chat.completion", "created": 1750000000,
                       "model": MODEL, "choices": [{"index": 0, "message": {
                           "role": "assistant", "content": "OK"}, "finish_reason": "stop"}],
                       "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}}
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(json.dumps(payload).encode())
            return
        lines = build_lines_for(last_user, req)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        for i, ev in enumerate(lines):
            time.sleep(0.03)
            self.wfile.write(ev.encode())
            self.wfile.flush()
            if "scenario slow" in last_user and i == 5:
                # idle gap mid-stream: stream pauses so Ctrl+C lands during it
                time.sleep(6)
            if WIRE_LOG:
                print(f"WIRE sent {time.strftime('%H:%M:%S.%f')[:-3]} {ev.rstrip()!r}",
                      file=WIRE_LOG, flush=True)
        if DONE_MARKER:
            print(f"DONE {CTR[0]}", file=DONE_MARKER, flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8124)
    ap.add_argument("--req-log", default=None)
    ap.add_argument("--wire-log", default=None)
    ap.add_argument("--done-marker", default=None)
    args = ap.parse_args()
    global REQ_LOG, WIRE_LOG, DONE_MARKER
    if args.req_log:
        REQ_LOG = open(args.req_log, "a")
    if args.wire_log:
        WIRE_LOG = open(args.wire_log, "a")
    if args.done_marker:
        DONE_MARKER = open(args.done_marker, "a")
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
