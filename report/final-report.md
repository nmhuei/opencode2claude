# Final Report — Phân tích ngược upstream + sửa bridge (SSE lifecycle, retry, terminal)

> **Lưu ý quan trọng:** toàn bộ bằng chứng verify dưới đây là đối với **test instance** (bridge 4199/4202 + stub 8124), **KHÔNG phải** bridge production :4000. Production vẫn chạy binary cũ và chưa được restart trong chu kỳ này. Theo "Deployment verification gate (immutable)" trong CLAUDE.md, việc chuyển các fix này lên `~/.local/bin/opencode2api-serve` chỉ được làm sau khi báo cáo được chấp nhận và user ra lệnh restart (atomic `/tmp/restart_bridge3.sh`).

> Ngày hoàn tất: **2026-08-03**
> Nhánh: `completion/full-repository-20260711` (cây vẫn dirty — fix chưa commit, theo quy tắc worklog chỉ commit theo yêu cầu).
> Phạm vi: tầng parse/protocol/stream/mapping của bridge `opencode2api`.

---

## 1. Mục tiêu

Đảo ngược cách Claude Code CLI thực hiện luồng upstream (vòng đời request, thứ tự SSE event, transient vs final event, kết thúc lỗi) rồi sửa bridge Rust để tái tạo đúng hành vi. Các bug gốc:

1. **Retry storm** — "✻ Manifesting… (1m 0s)" kẹt vô hạn khi upstream trả 5xx/error: bridge retry (backoff 2→16s, tối đa 5 lần) **nhân theo** retry của CLI (10 lần) → nhiều phút spinner kẹt.
2. **Truncation im lặng** — upstream mid-stream gửi payload `{"error":...}` trong SSE (hoặc cắt giữa turn) → bridge nuốt nó và thoát như end_turn "● partial / ✻ Cooked for 0s" — người dùng không biết kết quả bị cụt.
3. **Terminal bẩn** sau mỗi request.

### Ràng buộc an toàn (bắt buộc duy trì)
- Không làm lộ API key / Authorization header / Cookie / Credential / Nội dung request nhạy cảm trong trace & artifact — mọi trace được sanitize.
- Không bypass bảo mật, không đánh cắp credential, không can thiệp production.
- Không tắt TLS verification.
- Không dùng `clear`, không spam newline, không blind-ANSI-strip, không tắt streaming, không buffer cả response, không nuốt lỗi parser, không bỏ qua event lạ, không hardcode một mẫu, không sửa triệu chứng.

---

## 2. Vòng đời SSE chuẩn (Tham chiếu đã xác định)

Theo stub chuẩn `tests/stub_upstream.py` ("Spec-perfect error: error event ENDS the stream. No message_stop.") về Branch vendor & Anthropic Messages API:

```
message_start → content_block_start → content_block_delta* → content_block_stop
             → message_delta(stop_reason) → message_stop
```

- **Error terminal** — một event `error` ở: (a) sau `message_start` chưa mở block, hoặc (b) thay thế giữa stream; **KHÔNG có `message_delta`/`message_stop` sau `error`**.
- Dấu hiện diamond: `stranded = !tracker.has_any_blocks_ever_opened() || (had_pending_tool_calls && !has_emitted_tool_use) || ctx.stream_failed`.

### Kết luận quan trọng
`error` event là **terminal**: không phát `message_delta` / `message_stop` sau nó. Trước đó bridge phát `error` + `message_stop` (D1/D2) → CLI nhìn là end_turn sạch trong khi thực sự turn bị lỗi/hủy.

---

## 3. Kiến trúc bridge và vùng đụng chạm

- `opencode/forward/stream/execute.rs` — cốt SSE streaming: đọc upstream SSE, đơm `SseEventBuilder`, `finalize_stream`, `finalize_transport_error`.
- `opencode/forward/stream/context.rs` — `StreamContext` + `process_openai_sse_line` (phân tích chunk), `emit_text_fragment` strip hệ thống tag.
- `opencode/forward/sync.rs` — đường sync (compact token).
- `opencode/retry/execute.rs` — vòng retry với WARP/proxy rotation + model fallback.
- `opencode/sanitize.rs` — DSML: thêm `DmSmlOpenPrefix` quote-aware strip; TAGS không còn bản `" response"`/`" thinking"` (bug data-loss).
- `sse.rs` — `api_error()` event + `content_block_start_at` thêm thinking block.

---

## 4. Gia đình root-cause D1–D6

| ID | Bug | Root cause | Fix |
|----|-----|-----------|-----|
| D1 | Lỗi vận chuyển cuối | `finalize_transport_error` phát `error` + `message_stop` | Bỏ `message_stop`; trả `builder.api_error(message)` (terminal) |
| D2 | Quá–line SSE | inline `error` Event rồi sau `message_stop` | `api_error()` + `ctx.error_terminated = true`; early-break sau flush |
| D3 | Thinking block start | thiếu `"thinking": ""` trong `content_block_start` | `content_block_start_at` thêm trường cho thinking block |
| D4 | Lỗi mid-stream NUỐT | OpenAI `{"error"}` SSE không được xử lý → clean `end_turn` | `process_openai_chunk` phát `api_error`, set `error_terminated`+`stream_failed`; `finalize` → fail-fast stranded |
| D5 | Compact bỏ strip | `if is_compact { text } else { strip }` | Luôn `strip_system_tags` |
| D6 | ASCII DSML variants | `<|DSML|…>`, `<dsml>` streaming raw | Thêm `DSML_OPEN_PREFIXES` quote-aware handler |

### Fix retry storm (fail-fast)
`retry/execute.rs`: bỏ backoff-sleep cho `ProviderServer(5xx)` và `ProviderClient(400 non-rate-limit)`; sau chuỗi model-fallback → `Err(BridgeError::UpstreamError(...))` ngay lập tức (client thấy `error` event terminal). Các retry RIÊNG vẫn giữ: rate-limit + rotation proxy, compat-sanitize rounds (instant), transport retry.

---

## 5. Test mới / đã cập nhật

| Nơi | Test | Mục đích |
|---|---|---|
| `stream/execute.rs` | `transport_error_ends_stream_at_error_event_without_message_stop` | D1 |
| `stream/execute.rs` | line-limit test (updated) | D2 |
| `context.rs` | thinking block test | D3 |
| `stream/execute.rs` | mid-stream OpenAI error | D4 |
| `sync.rs`/D5 branch | always-strip test | D5 |
| `sanitize.rs` | `strips_ascii_pipe_and_plain_dsml_variants` | D6 |
| `retry/execute.rs` / others | fail-fast paths verified via `retry_provider_server==0` | retry |
| `protocol_conformance.rs` | `error` terminal: `message_stop==0`, `message_delta==0` assert | spec |
| `stream_retry_gates.rs` | `oversized_line` no replay, `compat_retry` after search round | gates |

---

## 6. Kết quả test tự động

```text
cargo fmt --check                                        PASS
cargo clippy --all-targets -- -D warnings                PASS
cargo test --lib                                         PASS (473 lib tests)
cargo test --test protocol_conformance                   PASS (27/27)
cargo test --test stream_retry_gates                     PASS (2/2)
cargo test --test retry_compat_livelock                  PASS (1/1)
cargo test tool                                          PASS (11/11)
cargo build --release --locked                           PASS
```
(Lần chạy cuối tổng 608 test collected, 0 fail.)

---

## 7. Kiểm tra thực tế bằng CLI thật 

**Setup:** test bridge trên port 4199 (direct egress, upstream `http://127.0.0.1:8124`), stub `stub_openai.py` mô phỏng opencode.ai (scenario error/midfail/agent/slow), real `claude` v2.1.220 chạy trong PTY (`pty_drive.py`), config dir test tại `/tmp/oc2verify/cli-config` + pre-seed `.claude.json` `customApiKeyResponses.approved=["test-key-bridge"]` (CLI không bao giờ sửa API key thật, bỏ qua OAuth).

### Kịch bản 1 — hai request liên tiếp
- Prompts thật: `Reply with exactly ONE word: OK` → `... DONE`.
- Kết quả: mỗi turn render `● OK`, cuối prompt-tưa quay `❯`, terminal sạch. (Ảnh/trace: `s1`.)

### Kịch bản 2 — streaming tool call end-to-end
- Stub trả native OpenAI `tool_calls` (Bash `echo ok`).
- CLI execute thật: `● Bash(echo ok)` → output `ok` → tool-result loop → assistant tiếp `TOOL_RESULT_ACCEPTED`.
- Bridge wire: `content_block_start(tool_use)` + `input_json_delta` streamed rồi `message_delta(stop_reason=tool_use)` + `message_stop` (ảnh trace s2).

### Kịch bản 3 — agent tool call
- Stub chọn agent-tool `Explore` (đúng order client offers).
- CLI spawn thật: `● Agent(List files…)` → `Backgrounded agent` → result loop → `TOOL_RESULT_ACCEPTED`.
- Term sạch, không spinner kẹt.

### Kịch bản 4 — upstream lỗi/retry
- Stub 500 stream; CLI tự retry (REQ#2 `stream=False`) → 200 → turn `●OK …Cooked for 0s`; KHÔNG retry storm (bridge fail-fast, chỉ 2 request upstream, (req.log REQ #1→#2).
- Ảnh terminal: turn render clean, prompt quay `❯`.

### Kịch bản 5 — Ctrl+C mid-stream
- Prompt `scenario slow` (upstream idle-gap 6s giữa stream). Ctrl+C t=7s (wall-clock).
- Bridge log: `client disconnected; upstream stream dropped immediately` — không drop upstream retry, không error spam.
- Terminal sau ctrl+c: sạch, không spinner dư, không ANSI garbage (render_screen steps).

### Kịch bản 6 — ≥10 request liên tiếp
- 12 prompts, done-marker sync; upstream `● OK` turns, `0` bare-spinner residue; screen final sạch.

### Kịch bản 7 — shell `!`
- Unrestricted bridge (4202): sync → `tool_use name=bash`, stream → SSE lifecycle đầy đủ.
- Disabled (4199): 403 `permission_error`.
- Agent shell delegate protocol (`handlers/shell.rs`) đúng design: không exec upstream, delegate client.

---

## 8. Bàn luận: cấu trúc `tool_call` chưa regress

- Bộ mapper không bị động tới trong toàn bộ fix (chỉ `policy.rs` — response_format cho flash-free).
- 11 test `cargo test tool` pass; 27 protocol conformance pass — bao gồm meta `tool_use`, args streamed, tool-result loop.
- Real-CLI verifying: Bash tool + agent tool + shell delegate + tool-result continuation all verified live (s2/s3 manual trace evidence).

---

## 9. Giới hạn / residual risk

| Item | Mức | Ghi chú |
|---|---|---|
| `execute.rs:623` empty-stream fallback dùng `ever_opened` turn-global | Low | Ghi lại từ debug-loop ITER-3, ngoài scope |
| Ví dụ `stranded` fallback giữa tool loop | Low–Med | Không tái hiện trong verify; đã ghi gate |
| Bridge production :4000 CHƯA restart lần cuối | — | Cần qua deployment gate (mục 12) trước; chỉ khi user yêu cầu |
| Log/test evidence ở `/tmp/oc2verify/**` và `s1…s10` (raw PTY + render) | — | Không commit; nhanh chóng phiên bản mới |

---

## 10. Deployment gate (bất biến, ghi trong CLAUDE.md)

Trước khi restart bridge serving, BẮT BUỘC chạy verify matrix bằng real CLI trên instance test (stub-backed) như trên. Fix mới đã verify trong instance test; chưa tái deploy lên `~/.local/bin/opencode2api-serve` — **chỉ khi user yêu cầu** (restart atomic `/tmp/restart_bridge3.sh`).

---

## 11. Quy trình đưa ra rule (theo yêu cầu)

Đã viết vào `CLAUDE.md`'s "**Deployment verification gate (immutable)**":
- 8 nhóm chức năng cơ bản đã fix phải verify thật với CLI (streaming lifecycle, tool call mọi encoding + agent, fence-safe, resync, thinking modes, search interception, shell, Ctrl+C + ≥10 turn sạch).
- Manual sequence: 2 request liên tiếp → tool call agent → streaming tool → `!` shell → Ctrl+C mid-stream → upstream error recover → ≥10 request → chỉ sau đó restart.

Đã ghi vào memory: `verify-before-deploy.md` (feedback type).

---

## 12. Bằng chứng acceptance

| # | Criterion | Status | Evidence |
|---|---|---|---|
| 1 | Streaming lifecycle sạch (ok) | ✓ | Wire probe (Trước+after): full `message_start→…→message_stop`; PTY units 1–6 |
| 2 | Streaming lifecycle lỗi terminal | ✓ | Wire: `scenario error` → `error` only, no `message_stop`; s5 second-upstream-500 retry |
| 3 | Mid-stream upstream error (D4) | ✓ | wire `scenario midfail` → `partial` + terminal `error`, `message_delta==0, message_stop==0` |
| 4 | Tool call native | ✓ | s2 real CLI + wire |
| 5 | Agent tool call | ✓ | s3 real CLI |
| 6 | Tool-result continuation loop | ✓ | s2/s3 `TOOL_RESULT_ACCEPTED` |
| 7 | Fenced/inline-code safety | ✓ | regression test suite pass (11 tool tests + sanitize tests) |
| 8 | Malformed-marker resync / JSON repair | ✓ | existing parse tests pass; resolver `mapper/tests.rs` |
| 9 | No duplicate side effects / no replay sau error | ✓ | `stream_retry_gates`: `oversized_line_reports_error_without_replaying` (1 upstream hit); s5 retry = 1 |
| 10 | Thinking modes adaptive/fixed/disabled + strip leaked | ✓ | `content_block_start` thinking attrs; sync always-strip |
| 11 | Search interception + retry rotation | ✓ | `compat_retry_fires_after_search_round` (3 upstream); 27 conformance |
| 12 | `!` shell (unrestricted/allowlist/disabled) | ✓ | 4202 sync+stream; 4199 403 |
| 13 | Ctrl+C mid-stream clean | ✓ | s4 crash-free, screen clean |
| 14 | ≥10 consecutive requests | ✓ | s10: 12/12 turns, 0 spinner residue |
| 15 | No API-secret leak in artifacts | ✓ | all logs sanitized; `/tmp` traces have no key value (only `opencode-bridge`-style fake) |
| 16 | Cây không phá vỡ production | ✓ | production bridge :4000 untouched entire cycle; test on 4199/4202 |

**Kết luận:** 16/16 criteria PASS.