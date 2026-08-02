# Claude Code CLI + DeepSeek V4 Flash Free Audit

**Ngày kiểm tra:** 2026-07-21
**Repo:** `/home/light/GitHub/opencode2claude`
**Claude Code:** `2.1.207`
**OpenCode:** `1.18.4`
**Model đích:** `opencode/deepseek-v4-flash-free`
**Bridge dùng thật:** `http://127.0.0.1:4000`

## 1. Kết luận

- Parser và mapper đã xử lý được các request mode quan trọng của Claude Code: adaptive/fixed/disabled thinking, effort, structured output, context management, tool history, reasoning history, metadata/service tier, beta header và các field mở rộng.
- Audit request shape tự động: **22/22 PASS**.
- Kiểm tra Rust: `cargo fmt --check`, clippy với `-D warnings`, và **415 test thực thi PASS**; một test WARP phụ thuộc proxy thật/Internet được ignore theo thiết kế.
- Manual test bằng model thật đã PASS cho adaptive reasoning, thinking disabled, fixed thinking 127K, effort low/max, built-in tool, skill, MCP tool, structured output, stream-json, session resume và autocompact.
- Bridge release hiện chạy dưới supervisor tại `127.0.0.1:4000`; các port test 4011/4012/4030 đã được dừng.

## 2. Capability model đã audit

Catalog OpenCode hiện tại của `opencode/deepseek-v4-flash-free` khai báo:

| Thuộc tính | Giá trị dùng để cấu hình |
|---|---:|
| Context window | `200000` tokens |
| Max output | `128000` tokens |
| Reasoning | Có |
| Tool call | Có |
| Reasoning field | `reasoning_content` |
| Effort cao nhất | `max` |
| Input/output | Text |

Bản free không được cấu hình theo thông số `1M context / 384K output` của biến thể trả phí. Catalog không công bố một trần reasoning-token riêng cho bản free. Vì vậy:

- Chế độ khuyến nghị: **adaptive thinking + effort=max**.
- Fixed thinking được đặt `127000`, nhỏ hơn `max_tokens=128000`, để tránh budget bằng hoặc vượt output cap.
- `MAX_THINKING_TOKENS=127000` là giới hạn an toàn phía Claude Code/bridge đã kiểm thử, không phải tuyên bố rằng provider luôn dùng đủ 127K reasoning tokens.

## 3. Cấu hình hiệu lực

### Bridge

File: `/home/light/opencode2api.toml`

```toml
schema_version = 1
port = 4000
host = "127.0.0.1"
model = "opencode/deepseek-v4-flash-free"
upstream_base_url = "https://opencode.ai/zen/v1"
enable_default_fallbacks = false
max_network_attempts = 5
max_provider_attempts = 1
retry_base_backoff_ms = 1000
retry_max_backoff_ms = 8000
egress_mode = "direct"
shell_policy = "disabled"
min_reasoning_stream_tokens = 1024
```

Repo `.env` đã được đồng bộ để không ghi đè sai TOML:

```text
BRIDGE_PORT=4000
BRIDGE_HOST=127.0.0.1
BRIDGE_CONFIG_PATH=/home/light/opencode2api.toml
OPENCODE_MODEL=opencode/deepseek-v4-flash-free
```

Bridge chỉ bind loopback nên bearer auth đã được tắt; không mở ra mạng ngoài.

### Claude Code

File: `/home/light/.claude/settings.json`

```json
{
  "model": "claude-sonnet-4-6",
  "alwaysThinkingEnabled": true,
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:4000",
    "ANTHROPIC_API_KEY": "opencode-bridge",
    "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "128000",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "200000",
    "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "90",
    "CLAUDE_CODE_DISABLE_1M_CONTEXT": "1",
    "CLAUDE_CODE_DISABLE_THINKING": "0",
    "CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING": "0",
    "CLAUDE_CODE_ALWAYS_ENABLE_EFFORT": "1",
    "CLAUDE_CODE_EFFORT_LEVEL": "max",
    "MAX_THINKING_TOKENS": "127000"
  }
}
```

`claude-sonnet-4-6` chỉ là profile Anthropic-compatible mà Claude Code gửi vào bridge; bridge pin request sang `opencode/deepseek-v4-flash-free`.

## 4. Parser và mapper

Các điểm chính:

- `src/handlers/types.rs:69`: parse cấu hình thinking.
- `src/handlers/types.rs:91`: parse `output_config` và effort/format.
- `src/handlers/types.rs:101`: parse request Claude Code, giữ unknown fields bằng `serde(flatten)`.
- `src/handlers/types.rs:131`: đọc effort từ `output_config.effort`, có fallback tương thích.
- `src/opencode/mapper/policy.rs:55`: normalize thinking sang DeepSeek.
- `src/opencode/mapper/policy.rs:74`: normalize effort sang `high`/`max`.
- `src/opencode/mapper/policy.rs:150`: giữ/điều chỉnh output token cho reasoning stream.
- `src/handlers/tests.rs:123`: regression test request shape Claude Code 2.1.207.
- `src/opencode/mapper/tests.rs:381`: adaptive thinking + max effort.
- `src/opencode/mapper/tests.rs:436`: toàn bộ effort mapping.

## 5. Audit request shape tự động

Script: `scripts/audit_claude_code_cli.py`
Kết quả: `artifacts/claude-cli-audit/summary.json`
Báo cáo sinh tự động: `artifacts/claude-cli-audit/REPORT.md`

**Kết quả: 22/22 PASS**

| Nhóm | Case đã kiểm tra |
|---|---|
| Thinking | baseline, disabled, adaptive, fixed 127K |
| Effort | low, medium, high, xhigh, max, invalid auto, invalid ultracode |
| Request mở rộng | `CLAUDE_CODE_EXTRA_BODY`, metadata, service tier, context management |
| Output | JSON Schema / structured output |
| Prompt | system prompt, append system prompt |
| Tool surface | tools disabled, Read tool present |
| Protocol | beta header, stream-json, prompt suggestions |
| Context | forced autocompact |
| Skill | slash skill expansion |

Wire assertions quan trọng:

- Adaptive: `thinking.type=adaptive`.
- Fixed: `thinking.type=enabled`, `budget_tokens=127000`.
- Disabled: không gửi field `thinking`.
- Max output: `max_tokens=128000`.
- Effort được đặt trong `output_config.effort`.
- Unknown/extra body được giữ nguyên để parser không làm mất feature mới.

## 6. Manual test model thật

| Chức năng | Kết quả | Bằng chứng |
|---|---|---|
| Global config, không truyền settings riêng | PASS, `GLOBAL_CONFIG_OK` | `tmp/claude-global-result.json` |
| Adaptive thinking + effort max | PASS, có reasoning block | `tmp/claude-real-result.json` |
| Thinking disabled | PASS, 0 thinking delta, kết quả `323` | `artifacts/claude-cli-manual/real-thinking-disabled.jsonl` |
| Fixed thinking 127K | PASS, 23 thinking delta, kết quả `323` | `artifacts/claude-cli-manual/real-thinking-fixed-127k.jsonl` |
| Effort low | PASS, 48 thinking delta, kết quả `323` | `artifacts/claude-cli-manual/real-effort-low.jsonl` |
| Built-in Read tool | PASS, 2 turns, đọc `GLOBAL_TOOL_OK` | `artifacts/claude-cli-manual/global-tool.json` |
| Custom skill | PASS, `GLOBAL_SKILL_OK` | `artifacts/claude-cli-manual/global-skill.json` |
| MCP stdio tool | PASS, `GLOBAL_MCP_OK`, không permission denial | `artifacts/claude-cli-manual/global-mcp.json` |
| Structured output | PASS, `structured_output={"ok":true}` | `artifacts/claude-cli-manual/global-structured.json` |
| Stream JSON + partial messages | PASS, `STREAM_GLOBAL_OK`, 48 stream events | `artifacts/claude-cli-manual/global-stream.jsonl` |
| Session persistence / resume | PASS, nhớ đúng `RESUME_ALPHA`, cùng session ID | `artifacts/claude-cli-manual/resume-first.json`, `resume-second.json` |

### Real-model matrix tái kiểm tra sau khi MCP khởi động lại

Script: `scripts/manual_claude_code_real_matrix.py`
Kết quả: `artifacts/claude-cli-real-matrix/summary.json`
Báo cáo: `artifacts/claude-cli-real-matrix/REPORT.md`

**Kết quả: 16/16 PASS**

- Thinking: disabled, adaptive max, fixed 127K.
- Effort: low, medium, high, xhigh, max.
- Prompt: system prompt và append system prompt.
- Output: structured JSON Schema.
- Tooling: built-in Read, custom skill, MCP stdio tool có allowlist chính xác.
- Protocol/session: stream-json + partial/replayed user events, session persistence/resume.
- Mỗi case dùng `CLAUDE_CONFIG_DIR` cô lập và gọi model thật qua bridge `127.0.0.1:4000`; không phụ thuộc profile global hay capture server giả.

## 7. Autocompact

Autocompact không chỉ được kiểm tra bằng biến môi trường; harness đã gửi nhiều lượt stream-json, chờ từng assistant turn hoàn tất, báo usage theo kích thước request và buộc Claude Code vượt ngưỡng.

Kết quả:

```text
trigger: auto
compact_result: success
pre_tokens: 277513
post_tokens: 208
cumulative_dropped_tokens: 277305
```

Sau compact, request cuối chỉ còn synthetic summary và final marker; các long turn cũ không còn trong payload. Case `autocompact_forced` PASS cả hai assertion: compact thành công và history thực sự giảm.

## 8. Test repo

```text
cargo fmt --check                                      PASS
cargo clippy --all-targets --all-features -- -D warnings PASS
cargo test                                             PASS
cargo build --release --bins                           PASS
```

Số test thực thi:

| Suite | Pass |
|---|---:|
| Unit | 302 |
| Fast/API | 81 |
| Integration | 18 |
| Parser fuzz smoke | 2 |
| Protocol conformance | 12 |
| **Tổng** | **415** |

Một test `real_warp_identity_consensus_and_duplicate_suppression` bị ignore theo thiết kế vì yêu cầu WARP SOCKS proxies thật trên 40001–40003 và Internet.

## 9. CLI smoke test

Các command sau đều exit code 0:

- `claude --version`
- `claude --help`
- `claude doctor`
- `claude mcp list`
- `claude plugin list`
- `claude agents --help`
- `claude auto-mode --help`
- `claude project --help`

Log: `artifacts/claude-cli-manual/logs/`

Không chạy các action có side effect ngoài máy hoặc không thuộc protocol bridge: auth/login, install/update, Chrome integration, remote control, cloud ultrareview, tạo worktree/tmux, tải plugin từ URL. Các command này được parse/help smoke test khi phù hợp, nhưng không được tuyên bố là integration-tested.

## 10. Trạng thái cuối

```text
opencode2api server status: running
bind: 127.0.0.1:4000
health: {"status":"ok","version":"0.5.0"}
model: opencode/deepseek-v4-flash-free
```

Các bridge/capture test tạm ở 4011, 4012 và 4030 đã dừng. Chỉ dịch vụ ổn định 4000 còn chạy.

## 11. Ghi chú reverse engineering

Không cần mở IDA vì request capture, OpenCode catalog, CLI help, binary strings và end-to-end tests đã cung cấp đủ bằng chứng. Nếu một phiên bản Claude Code tương lai ẩn hoặc đổi wire format mà capture không giải thích được, đường dẫn dự phòng là `/home/light/GitHub/CTF/ida-pro-mcp`.

## 12. Lệnh tái kiểm tra

```bash
cd /home/light/GitHub/opencode2claude
python3 scripts/audit_claude_code_cli.py
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
opencode2api server status --json
curl -sS http://127.0.0.1:4000/health
claude -p 'Reply with only OK' --output-format json --max-turns 1 --tools '' --effort max
```
