#!/usr/bin/env python3
"""Minimal Anthropic Messages SSE stub for baseline captures.

Serves /v1/messages with several deterministic, spec-perfect streaming
scenarios so the Claude Code CLI TUI can be compared against a clean
reference upstream. No secrets are involved; it accepts any auth.
"""
from __future__ import annotations

import argparse
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODEL = "claude-sonnet-4-6"
REQ_LOG = None
WIRE_LOG = None
DONE_MARKER = None


def sse(event_type: str, payload: dict) -> str:
    return f"event: {event_type}\ndata: {json.dumps(payload, ensure_ascii=False)}\n\n"


def lifecycle(msg_id: str, blocks: list[dict], stop_reason: str, output_tokens: int):
    """Yield the standard Anthropic message lifecycle around given blocks.

    blocks: list of dicts, each with "kind" (thinking|text|tool_use) plus
    "segments" (list of str chunks for text/thinking) or full tool payload.
    """
    lines = [
        sse("message_start", {
            "type": "message_start",
            "message": {
                "id": msg_id, "type": "message", "role": "assistant",
                "content": [], "model": MODEL, "stop_reason": None,
                "stop_sequence": None,
                "usage": {"input_tokens": 42, "output_tokens": 0},
            },
        }),
    ]
    for idx, blk in enumerate(blocks):
        kind = blk["kind"]
        start_payload = {"type": "content_block_start", "index": idx}
        if kind == "text":
            start_payload["content_block"] = {"type": "text", "text": ""}
        elif kind == "thinking":
            start_payload["content_block"] = {
                "type": "thinking", "thinking": "",
                "signature": f"sig_{blk.get('sig', 'baseline')}_{idx}",
            }
        elif kind == "tool_use":
            start_payload["content_block"] = {
                "type": "tool_use", "id": blk["id"], "name": blk["name"], "input": {},
            }
        lines.append(sse("content_block_start", start_payload))

        if kind == "tool_use":
            lines.append(sse("content_block_delta", {
                "type": "content_block_delta", "index": idx,
                "delta": {"type": "input_json_delta", "partial_json": blk["input_json"]},
            }))
        elif kind == "thinking":
            for chunk in blk["chunks"]:
                lines.append(sse("content_block_delta", {
                    "type": "content_block_delta", "index": idx,
                    "delta": {"type": "thinking_delta", "thinking": chunk},
                }))
        else:
            for chunk in blk["chunks"]:
                lines.append(sse("content_block_delta", {
                    "type": "content_block_delta", "index": idx,
                    "delta": {"type": "text_delta", "text": chunk},
                }))
        lines.append(sse("content_block_stop", {"type": "content_block_stop", "index": idx}))

    lines.append(sse("message_delta", {
        "type": "message_delta",
        "delta": {"stop_reason": stop_reason, "stop_sequence": None},
        "usage": {"output_tokens": output_tokens},
    }))
    lines.append(sse("message_stop", {"type": "message_stop"}))
    return lines


def build_lines_for(prompt: str, msg_id: str) -> list[str]:
    prompt_l = prompt.lower()
    if "scenario error" in prompt_l:
        # Spec-perfect error: error event ENDS the stream. No message_stop.
        return [
            sse("message_start", {
                "type": "message_start",
                "message": {
                    "id": msg_id, "type": "message", "role": "assistant",
                    "content": [], "model": MODEL, "stop_reason": None,
                    "stop_sequence": None,
                    "usage": {"input_tokens": 42, "output_tokens": 0},
                },
            }),
            sse("error", {
                "type": "error",
                "error": {"type": "api_error", "message": "upstream exploded"},
            }),
        ]
    if "scenario miderror" in prompt_l:
        # Spec-perfect mid-stream error: partial text block, then error ends
        # the stream. No content_block_stop, no message_delta, no message_stop.
        return [
            sse("message_start", {
                "type": "message_start",
                "message": {
                    "id": msg_id, "type": "message", "role": "assistant",
                    "content": [], "model": MODEL, "stop_reason": None,
                    "stop_sequence": None,
                    "usage": {"input_tokens": 42, "output_tokens": 0},
                },
            }),
            sse("content_block_start", {
                "type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""},
            }),
            sse("content_block_delta", {
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "partial answer"},
            }),
            sse("error", {
                "type": "error",
                "error": {"type": "api_error", "message": "mid-stream exploded"},
            }),
        ]
    if "scenario tool" in prompt_l:
        blocks = [{
            "kind": "tool_use", "id": "toolu_test_1", "name": "Bash",
            "input_json": '{"command": "echo ok"}',
        }]
        return lifecycle(msg_id, blocks, "tool_use", 16)
    if "scenario long" in prompt_l:
        blocks = [
            {"kind": "thinking", "chunks": [f"deep thought paragraph {i} " * 40 for i in range(3)]},
            {"kind": "text", "chunks": [f"final line {i}\n" for i in range(3)]},
        ]
        return lifecycle(msg_id, blocks, "end_turn", 64)
    # default: short text only
    blocks = [{"kind": "text", "chunks": ["OK"]}]
    return lifecycle(msg_id, blocks, "end_turn", 8)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    msg_count = 0

    def log_message(self, *args):
        pass

    def _auth_envelope(self):
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
                    last_user = " ".join(
                        str(b.get("text", "")) for b in c if isinstance(b, dict)
                    )
        if REQ_LOG:
            print(
                f"POST path={self.path} stream={req.get('stream')} model={req.get('model')} "
                f"last_user={last_user[:80]!r} tools={len(req.get('tools') or [])}",
                file=REQ_LOG, flush=True,
            )
        if self.path.startswith("/v1/messages"):
            Handler.msg_count += 1
            msg_id = f"msg_stub_{Handler.msg_count}"
            # sleep so the CLI's TUI has time to show a spinner/status frame
            time.sleep(0.6)
            streams = req.get("stream", False)
            resp_body = build_lines_for(last_user, msg_id)
            if streams:
                out = ("".join(resp_body)).encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-cache")
                self.send_header("Connection", "close")
                self.end_headers()
                for ev in resp_body:
                    time.sleep(0.05)
                    self.wfile.write(ev.encode())
                    self.wfile.flush()
                    if WIRE_LOG:
                        print(f"WIRE sent {time.strftime('%H:%M:%S.%f')[:-3]} {ev.rstrip()!r}",
                              file=WIRE_LOG, flush=True)
                if DONE_MARKER:
                    print(f"DONE {Handler.msg_count}", file=DONE_MARKER, flush=True)
            else:
                payload = {
                    "id": msg_id, "type": "message", "role": "assistant",
                    "model": MODEL, "content": [{"type": "text", "text": "OK"}],
                    "stop_reason": "end_turn", "stop_sequence": None,
                    "usage": {"input_tokens": 42, "output_tokens": 8},
                }
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(json.dumps(payload).encode())
            return
        self.send_response(404)
        self.end_headers()

    def do_GET(self):
        if self.path.startswith("/v1/messages") or self.path.startswith("/count_tokens"):
            self.send_response(404)
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"ok": True}).encode())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8123)
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