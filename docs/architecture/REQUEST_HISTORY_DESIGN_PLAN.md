# Request, Prompt, Reasoning and Response History — Design Plan

Status: **IMPLEMENTED — local redacted deployment verified 2026-07-22**

Last updated: **2026-07-22**

Repository:

```text
/home/light/GitHub/opencode2claude
```

## Implementation result

Implemented in the current working tree and verified against the live service:

```text
Storage:          SQLite WAL
Database:         ~/.opencode2api/history/request-history.sqlite3
Directory mode:   0700
Database mode:    0600
Capture mode:     redacted
Local enabled:    true
Release default:  false
Retention:        30 days / 10,000 records / 1 GiB
Dashboard page:   History
Claude Code test: PASS with CLI 2.1.217
```

Completed implementation areas:

- Persistent settings and schema migration bootstrap.
- Anthropic sync/SSE capture.
- OpenAI sync/SSE transparent tee capture.
- Inbound request and effective upstream payload.
- Reasoning, response, raw provider response when available.
- Tool/search/attempt/event metadata.
- Admin list/detail/content/stats/settings/delete/purge/export APIs.
- Dashboard History page, detail drawer and settings modal.
- EN/VI desktop and mobile verification.
- Redaction, byte limits, retention and degraded-storage behavior.

Deferred optional items:

- SQLCipher encryption at rest.
- Full-text/semantic search.
- Full web-search result body capture by default.
- Cloud sync and multi-user RBAC.

## 1. Purpose

Thiết kế một hệ thống lịch sử cục bộ để quản trị viên có thể kiểm tra:

- Client đã gửi request gì vào bridge.
- Prompt/history nào đã được gửi vào.
- Policy API key đã thay đổi request ra sao.
- Effective request nào thực sự được gửi upstream.
- Model nào đã xử lý request.
- Reasoning/thinking model trả về.
- Final response model trả về.
- Tool calls, tool results và web-search loops liên quan.
- Retry, fallback, proxy và lỗi xảy ra trong quá trình xử lý.
- Token usage, latency, time-to-first-token và finish reason.

Hệ thống này phải hoạt động với cả:

```text
POST /v1/messages
POST /v1/chat/completions
```

và cả hai chế độ:

```text
sync
SSE streaming
```

## 2. Trạng thái hiện tại

Repo hiện chỉ có:

- Request metadata trong `src/observability.rs`:
  - request ID.
  - method.
  - path.
  - HTTP status.
  - elapsed time.
- In-memory management audit trong `src/audit.rs`.
- Operational metrics.
- API-key usage counters.
- Temporary `accumulated_thinking` và `accumulated_text` trong streaming context.

Repo hiện **không persist**:

- Request body.
- Prompt/messages.
- Effective upstream payload.
- Reasoning.
- Response.
- Tool calls/results.
- Search query/results.
- Per-request retry/fallback timeline.

## 3. Design principles

### 3.1 History must never break inference

History là observability subsystem, không phải routing dependency.

Nếu database lỗi, queue đầy hoặc disk full:

- Request LLM vẫn phải tiếp tục.
- History record được đánh dấu incomplete nếu có thể.
- Chỉ ghi warning có rate limit.
- Không trả lỗi 5xx cho client chỉ vì history subsystem lỗi.

### 3.2 Separate operational logs from conversation history

Không ghi full prompt/reasoning/response vào:

```text
~/.opencode2api/opencode2api.log
```

Conversation history phải nằm trong storage riêng với:

- Quyền truy cập riêng.
- Retention riêng.
- Delete/purge riêng.
- Export riêng.
- Redaction riêng.

### 3.3 Capture both inbound and effective prompts

Cần phân biệt:

1. **Inbound request**: dữ liệu client gửi vào bridge.
2. **Policy-applied request**: sau khi áp API-key policy.
3. **Effective upstream request**: payload thực sự gửi tới upstream sau mapping, model normalization, tool-history conversion và search injection.
4. **Attempt payloads**: payload của từng retry, fallback hoặc search loop nếu có thay đổi.

Nếu chỉ lưu inbound request, quản trị viên không biết model thực sự đã nhìn thấy gì.

### 3.4 Content is sensitive by default

Prompt, reasoning, response và tool result có thể chứa:

- API token.
- Password.
- Cookie.
- Personal data.
- Source code riêng tư.
- File content.
- Shell command.
- Internal hostname/path.

Do đó cần explicit capture mode, redaction, retention và access control.

### 3.5 Bounded storage and bounded memory

Không được có:

- Unbounded in-memory queue.
- Unbounded stream accumulation.
- Database tăng vô hạn.
- List API trả toàn bộ record trong một request.

## 4. Recommended user experience

## 4.1 Add a fifth dashboard page

Sidebar đề xuất:

```text
Dashboard
API Keys
Models
History
System
```

Không nên nhét History vào Logs vì:

- Logs là operational text output.
- History là structured sensitive data.
- History cần filter, detail view, delete, export và retention settings riêng.

## 4.2 History list view

Header:

```text
Request history
Inspect prompts, reasoning, responses and execution details.
```

Summary cards:

```text
Requests today
Success rate
Average latency
Stored size
```

Filters:

```text
Search text
Time range
Status
Protocol
Endpoint
Model
API key/client
Thinking on/off
Streaming on/off
Has tools
Has web search
Has error
```

Columns:

```text
Time
Client
Prompt preview
Model
Status
Latency
Tokens
```

List endpoint không trả full content; chỉ trả preview đã escaped/redacted.

## 4.3 Request detail drawer/page

Tabs đề xuất:

```text
Overview
Inbound request
Effective prompt
Reasoning
Response
Tools & Search
Attempts
Raw JSON
```

### Overview

Hiển thị:

- Request ID.
- Timestamp.
- Protocol/endpoint.
- API-key ID/name/environment; không hiển thị secret.
- Requested model.
- Effective model.
- Response model.
- Streaming.
- Thinking mode/effort/budget.
- HTTP status.
- Finish reason.
- Input/output/reasoning token counts.
- Total latency.
- Time to first chunk.
- Retry/fallback/search/tool counts.
- Capture completeness.
- Redacted/truncated indicators.

### Inbound request

Hiển thị messages client gửi vào theo role:

```text
system
user
assistant history
tool result
```

Có toggle:

```text
Rendered
JSON
```

### Effective prompt

Hiển thị payload cuối cùng gửi upstream sau:

- API key policy.
- Model mapping.
- Thinking normalization.
- Tool conversion.
- Search result injection.
- Compatibility repair.

Nên có diff summary giữa inbound và effective:

```text
Model overridden
Max tokens clamped
Thinking enabled/disabled
Tools removed/normalized
Search context injected
Messages transformed
```

### Reasoning

- Plain text pane có fixed height.
- Scroll nội bộ.
- Copy button.
- Redaction/truncation badge.
- Token/character count.

### Response

- Plain text/JSON view.
- Tool-call blocks tách riêng.
- Copy button.
- Finish reason.

### Tools & Search

Timeline:

```text
Tool call requested
Tool arguments
Tool result returned
Search query
Search provider
Search result size
Search loop number
```

Mặc định search result body có thể không lưu đầy đủ; xem phần quyết định cần duyệt.

### Attempts

Mỗi attempt:

```text
Attempt number
Loop number
Model
Proxy/node
Started/completed time
Status
HTTP status
Error class
Backoff
Fallback reason
Payload changed yes/no
```

## 4.4 Actions

Per request:

```text
Copy request ID
Export JSON
Delete record
```

Bulk:

```text
Export filtered
Delete filtered
Purge all history
```

Mọi destructive action phải:

- Dashboard admin authentication.
- CSRF protection.
- Confirmation modal.
- Management audit event.

## 5. Capture modes

Đề xuất enum:

```text
off
metadata
redacted
full
```

### off

Không tạo history record.

Operational counters/logs hiện tại vẫn hoạt động.

### metadata

Lưu:

- Request ID.
- Endpoint/protocol.
- Client key ID/name.
- Models.
- Status/latency/tokens.
- Retry/fallback/tool/search counts.
- Prompt/response character counts và hashes.

Không lưu content.

### redacted — recommended default when history is enabled

Lưu full structured content sau redaction.

Không lưu headers hoặc credentials.

### full

Lưu content gần nguyên bản nhưng vẫn phải loại bỏ credential fields bắt buộc.

`full` không có nghĩa là lưu Authorization, cookies hoặc raw API keys.

## 6. Recommended default configuration

Đề xuất cấu hình ban đầu:

```toml
history_enabled = false
history_capture_mode = "redacted"
history_capture_inbound = true
history_capture_effective = true
history_capture_reasoning = true
history_capture_response = true
history_capture_tools = true
history_capture_search_queries = true
history_capture_search_results = false
history_capture_shell_commands = false

history_retention_days = 30
history_max_records = 10000
history_max_database_bytes = 1073741824

history_max_request_bytes = 1048576
history_max_reasoning_bytes = 2097152
history_max_response_bytes = 2097152
history_max_tool_payload_bytes = 262144
history_max_record_bytes = 6291456

history_queue_capacity = 512
history_flush_interval_ms = 500
history_flush_chunk_bytes = 16384
```

Release default nên là `history_enabled = false` vì content logging nhạy cảm.

Deployment cục bộ của user có thể bật sau khi xác nhận consent/config.

## 7. Configuration model

Thêm `HistoryConfig` riêng thay vì nhét vào `ObservabilityConfig`:

```rust
pub struct HistoryConfig {
    pub enabled: bool,
    pub capture_mode: HistoryCaptureMode,
    pub capture_inbound: bool,
    pub capture_effective: bool,
    pub capture_reasoning: bool,
    pub capture_response: bool,
    pub capture_tools: bool,
    pub capture_search_queries: bool,
    pub capture_search_results: bool,
    pub capture_shell_commands: bool,
    pub retention_days: u32,
    pub max_records: usize,
    pub max_database_bytes: u64,
    pub max_request_bytes: usize,
    pub max_reasoning_bytes: usize,
    pub max_response_bytes: usize,
    pub max_tool_payload_bytes: usize,
    pub max_record_bytes: usize,
    pub queue_capacity: usize,
    pub flush_interval: Duration,
    pub flush_chunk_bytes: usize,
    pub path: Option<PathBuf>,
    pub encryption: HistoryEncryptionMode,
}
```

Config sources:

```text
CLI override, nếu cần về sau
Environment
TOML
Default
```

Environment names đề xuất:

```text
BRIDGE_HISTORY_ENABLED
BRIDGE_HISTORY_CAPTURE_MODE
BRIDGE_HISTORY_RETENTION_DAYS
BRIDGE_HISTORY_MAX_RECORDS
BRIDGE_HISTORY_MAX_DATABASE_BYTES
BRIDGE_HISTORY_PATH
BRIDGE_HISTORY_ENCRYPTION
BRIDGE_HISTORY_KEY
```

Không ghi `BRIDGE_HISTORY_KEY` vào TOML hoặc dashboard response.

### Hot reload behavior

Có thể áp dụng cho request mới không restart:

```text
capture mode
capture fields
retention
size limits
```

Yêu cầu restart:

```text
database path
encryption mode/encryption key
```

## 8. Storage choice

## 8.1 Recommended: SQLite

Path mặc định:

```text
~/.opencode2api/history/request-history.sqlite3
```

Directory/file permissions:

```text
history directory: 0700
database file:     0600
```

Lý do chọn SQLite:

- Local-first.
- Không cần service phụ.
- Query/filter/pagination tốt hơn JSONL.
- Atomic migrations.
- Dễ export và backup.
- Phù hợp với một daemon local.

## 8.2 Rust access layer

Đề xuất:

```text
rusqlite with bundled SQLite
```

Không dùng async SQLite call trực tiếp trên Tokio worker threads.

Kiến trúc:

```text
request handlers
    -> bounded tokio mpsc
    -> single history writer task/thread
    -> SQLite connection
```

Read query từ dashboard:

```text
spawn_blocking + short-lived read connection
```

Lợi ích single writer:

- Thứ tự event ổn định.
- Giảm lock contention.
- Dễ batch transaction.
- Dễ xử lý shutdown/flush.

Queue phải bounded để có backpressure và không làm process hết RAM.

## 8.3 SQLite pragmas

Đề xuất khi mở database:

```sql
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;
PRAGMA secure_delete = FAST;
```

Lưu ý bắt buộc:

- Dùng bundled SQLite đã vá WAL reset bug, tối thiểu SQLite 3.51.3 hoặc một bản backport chính thức có fix.
- Database phải nằm trên local filesystem, không đặt trên network filesystem khi dùng WAL.
- Có periodic checkpoint và size monitoring.

## 8.4 Optional encryption at rest

Hai lựa chọn:

### Option A — filesystem permissions only

Ưu điểm:

- Ít dependency.
- Build đơn giản.
- Không cần quản lý key.

Nhược điểm:

- Người có quyền đọc filesystem có thể đọc database.

### Option B — SQLCipher

Ưu điểm:

- Database content mã hóa AES tại rest.

Yêu cầu:

- `BRIDGE_HISTORY_KEY` từ environment hoặc secure key source.
- Không lưu key vào `.env.example` dưới dạng giá trị thật.
- Fail closed nếu encryption được yêu cầu nhưng key thiếu.
- Cần kiểm tra release build Linux/macOS và migration/rekey.

Khuyến nghị triển khai theo hai phase:

```text
Phase 1: SQLite + 0600 + redaction
Phase 2: optional SQLCipher after compatibility verification
```

User cần duyệt lựa chọn trước implementation.

## 9. Database schema

Schema version quản lý bằng:

```sql
PRAGMA user_version;
```

## 9.1 `history_requests`

Một row cho một client request.

```sql
CREATE TABLE history_requests (
    id TEXT PRIMARY KEY,
    conversation_id TEXT,
    parent_request_id TEXT,

    started_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    duration_ms INTEGER,
    time_to_first_chunk_ms INTEGER,

    protocol TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    operation_kind TEXT NOT NULL,

    client_key_id TEXT,
    client_name TEXT,
    client_environment TEXT,

    requested_model TEXT,
    effective_model TEXT,
    response_model TEXT,

    stream INTEGER NOT NULL,
    thinking_requested INTEGER NOT NULL,
    reasoning_effort TEXT,
    reasoning_budget_tokens INTEGER,

    status TEXT NOT NULL,
    http_status INTEGER,
    finish_reason TEXT,
    error_type TEXT,
    error_message TEXT,

    input_tokens INTEGER,
    output_tokens INTEGER,
    reasoning_tokens INTEGER,

    retry_count INTEGER NOT NULL DEFAULT 0,
    fallback_count INTEGER NOT NULL DEFAULT 0,
    tool_call_count INTEGER NOT NULL DEFAULT 0,
    search_count INTEGER NOT NULL DEFAULT 0,

    prompt_preview TEXT,
    response_preview TEXT,

    capture_mode TEXT NOT NULL,
    capture_incomplete INTEGER NOT NULL DEFAULT 0,
    redacted INTEGER NOT NULL DEFAULT 0,
    truncated INTEGER NOT NULL DEFAULT 0,

    stored_bytes INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL
);
```

Indexes:

```sql
CREATE INDEX idx_history_started ON history_requests(started_at_ms DESC);
CREATE INDEX idx_history_status ON history_requests(status, started_at_ms DESC);
CREATE INDEX idx_history_model ON history_requests(effective_model, started_at_ms DESC);
CREATE INDEX idx_history_client ON history_requests(client_key_id, started_at_ms DESC);
CREATE INDEX idx_history_endpoint ON history_requests(endpoint, started_at_ms DESC);
```

## 9.2 `history_content`

Lưu các content section tách biệt.

```sql
CREATE TABLE history_content (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL,
    attempt_id INTEGER,
    sequence INTEGER NOT NULL,
    kind TEXT NOT NULL,
    content_type TEXT NOT NULL,
    body BLOB NOT NULL,
    encoding TEXT NOT NULL DEFAULT 'utf8',
    original_bytes INTEGER NOT NULL,
    stored_bytes INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    redacted INTEGER NOT NULL DEFAULT 0,
    truncated INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    FOREIGN KEY(request_id) REFERENCES history_requests(id) ON DELETE CASCADE
);
```

`kind` values:

```text
inbound_request
effective_request
system_instructions
user_messages
assistant_history
tool_definitions
tool_results
upstream_request
reasoning
response
provider_raw_response
shell_command
search_query
search_results
```

Initial implementation có thể dùng `utf8` JSON/text không nén.

Compression chỉ thêm khi benchmark chứng minh cần thiết.

## 9.3 `history_attempts`

Một row cho mỗi upstream attempt hoặc internal loop.

```sql
CREATE TABLE history_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL,
    attempt_number INTEGER NOT NULL,
    loop_number INTEGER NOT NULL DEFAULT 0,
    attempt_kind TEXT NOT NULL,
    model TEXT,
    proxy_node TEXT,
    proxy_exit_ip TEXT,
    started_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    duration_ms INTEGER,
    http_status INTEGER,
    status TEXT NOT NULL,
    finish_reason TEXT,
    error_type TEXT,
    error_message TEXT,
    payload_sha256 TEXT,
    payload_changed INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY(request_id) REFERENCES history_requests(id) ON DELETE CASCADE
);
```

`attempt_kind`:

```text
primary
transport_retry
provider_retry
model_fallback
search_loop
final_synthesis
response_recovery
```

## 9.4 `history_events`

Structured timeline.

```sql
CREATE TABLE history_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL,
    attempt_id INTEGER,
    sequence INTEGER NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    severity TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    content_id INTEGER,
    FOREIGN KEY(request_id) REFERENCES history_requests(id) ON DELETE CASCADE,
    FOREIGN KEY(attempt_id) REFERENCES history_attempts(id) ON DELETE CASCADE,
    FOREIGN KEY(content_id) REFERENCES history_content(id) ON DELETE SET NULL
);
```

`event_type` examples:

```text
request_received
policy_applied
model_resolved
upstream_attempt_started
first_chunk
reasoning_started
reasoning_completed
response_started
response_completed
tool_call
tool_result
search_requested
search_completed
retry_scheduled
model_fallback
client_cancelled
request_failed
request_completed
capture_truncated
capture_dropped
```

## 10. Capture lifecycle

```text
1. Observability middleware creates request_id.
2. Handler parses inbound JSON.
3. HistoryStore.begin() creates running record.
4. Store redacted inbound request.
5. Apply API-key policy.
6. Resolve/map model.
7. Store policy/effective summary.
8. Mapper creates upstream request.
9. Store effective upstream payload.
10. Record each retry/search/fallback attempt.
11. Accumulate reasoning and response in bounded buffers.
12. Periodically flush stream buffers.
13. Finalize record on success/error/cancel.
14. Retention worker prunes old/oversized history.
```

## 11. Proposed Rust architecture

New module:

```text
src/history/
├── mod.rs
├── types.rs
├── redact.rs
├── limits.rs
├── store.rs
├── sqlite.rs
├── writer.rs
├── capture.rs
├── query.rs
├── retention.rs
├── migration.rs
└── tests.rs
```

Core interface:

```rust
pub trait HistoryStore: Send + Sync {
    fn enabled(&self) -> bool;
    fn begin(&self, start: HistoryRequestStart) -> HistoryCapture;
    fn list(&self, query: HistoryQuery) -> Result<HistoryPage, HistoryError>;
    fn detail(&self, id: &str) -> Result<Option<HistoryDetail>, HistoryError>;
    fn delete(&self, id: &str) -> Result<bool, HistoryError>;
    fn purge(&self, filter: HistoryPurgeFilter) -> Result<usize, HistoryError>;
}
```

`HistoryCapture` API đề xuất:

```rust
capture.inbound(...)
capture.effective(...)
capture.attempt_started(...)
capture.attempt_finished(...)
capture.reasoning_chunk(...)
capture.response_chunk(...)
capture.tool_call(...)
capture.tool_result(...)
capture.search_event(...)
capture.first_chunk(...)
capture.finish(...)
capture.fail(...)
capture.cancel(...)
```

Methods không nên block request hot path.

## 12. Writer and streaming behavior

Không ghi SQLite cho mỗi token/chunk nhỏ.

Đề xuất:

```text
Accumulate per request in memory
Flush every 500 ms hoặc 16 KiB
Flush immediately on terminal event
```

Bounded queue:

```text
capacity: 512 commands
```

Khi queue đầy:

1. Không block stream lâu.
2. Ưu tiên terminal metadata.
3. Có thể drop intermediate content flush.
4. Mark `capture_incomplete=true`.
5. Increment `history_events_dropped` metric.

Per-request buffers phải áp size cap trước khi append.

## 13. Integration points in current repo

## 13.1 `src/state.rs`

Thêm:

```rust
pub history: Arc<dyn HistoryStore>
```

Khởi tạo store và history writer từ resolved config.

Nếu disabled:

```text
NoopHistoryStore
```

## 13.2 `src/runtime.rs`

Thêm path helpers:

```text
history_dir()
history_database()
history_export_dir()
```

## 13.3 `src/observability.rs`

Giữ request metadata log hiện tại.

Không đọc body trong middleware.

Request ID hiện tại được dùng làm primary history ID.

Có thể bổ sung metrics:

```text
history_records_started
history_records_completed
history_records_failed
history_capture_dropped
history_write_failures
history_database_bytes
history_cleanup_deleted
```

## 13.4 `src/handlers/messages.rs`

Hook points:

```text
After JSON parse: begin + inbound capture
After policy/model resolution: effective metadata
Before shell handling: classify operation
Before forward sync/stream: pass capture handle
On handler error: finalize failed
```

Cần nhận `Extension<RequestId>` từ middleware.

Không dùng authorization/routing secret làm stored client identifier.

Chỉ lưu:

```text
AuthenticatedClient.key_id
AuthenticatedClient.name
AuthenticatedClient.environment
```

Nếu anonymous/system client:

```text
client_key_id = null hoặc system_claude_code
```

## 13.5 `src/handlers/openai.rs`

Hiện OpenAI response được pass-through bằng `Body::from_stream`.

Cần tee stream:

```text
upstream bytes
  -> SSE parser/capture
  -> unchanged downstream bytes
```

Yêu cầu:

- Không sửa byte order.
- Không chờ full stream trước khi trả client.
- Parse capture errors không được làm stream client lỗi.
- `[DONE]`, EOF, client drop và upstream error đều finalize đúng.

Sync response:

- Bounded body capture theo existing max sync response limit.
- Parse reasoning/content/tool calls.
- Forward response semantics không thay đổi.

## 13.6 `src/opencode/mapper/`

Sau mapping, capture serialized `OpenAiRequest` để biết effective upstream prompt.

Redaction phải xảy ra trước khi enqueue database command.

## 13.7 `src/opencode/forward/sync.rs`

Capture:

- Upstream response model.
- Reasoning.
- Visible text.
- Tool calls.
- Search loops.
- Finish reason.
- Usage.

Final mapped Anthropic response cũng có thể được lưu ở `response` kind.

## 13.8 `src/opencode/forward/stream/context.rs`

Đang có:

```text
accumulated_thinking
accumulated_text
```

Không nên clone toàn bộ string sau mỗi chunk.

Capture hook append incremental cleaned fragments vào bounded history buffer.

Capture phải sử dụng content sau sanitize/compatibility cleanup để dashboard không hiển thị malformed internal markers như final content.

## 13.9 `src/opencode/forward/stream/execute.rs`

Capture:

- Attempt start/end.
- Search query/results metadata.
- Search loop count.
- Reasoning completion.
- Visible response completion.
- Stop reason.
- Client cancellation.

## 13.10 `src/opencode/retry/`

Emit structured event cho:

```text
retry class
attempt number
backoff
proxy node
provider status
fallback model
```

## 13.11 Shell commands

`!shell` request có thể chứa credential hoặc destructive command.

Recommended default:

```text
history_capture_shell_commands = false
```

Khi false chỉ lưu:

```text
operation_kind = shell
command_hash
command_length
exit status
latency
```

Khi true mới lưu command sau redaction.

## 14. Redaction design

Redaction phải chạy trước persistence, không chỉ lúc display.

## 14.1 Never store

- Authorization header.
- `x-api-key` header.
- Dashboard session cookie.
- CSRF token.
- Admin token.
- Raw managed API-key secret.
- Upstream provider API keys.
- Encryption key.

## 14.2 Recursive JSON key redaction

Case-insensitive key match examples:

```text
authorization
api_key
apikey
access_token
refresh_token
password
passwd
secret
cookie
set-cookie
client_secret
private_key
```

Replacement:

```json
"[REDACTED]"
```

## 14.3 String pattern redaction

Patterns đề xuất:

```text
Bearer <token>
sk-oc2-<id>.<secret>
sk-<provider-secret>
JWT-like three-part tokens
PEM private keys
known configured secret values
```

Known secret values nên được lấy từ resolved config dưới dạng in-memory matcher, không persist matcher contents.

## 14.4 Redaction metadata

Mỗi content section ghi:

```text
redacted=true/false
redaction_count
truncated=true/false
original_bytes
stored_bytes
sha256 of original or redacted content — cần duyệt
```

Khuyến nghị hash **redacted stored representation**, không hash raw sensitive content, để tránh tạo stable fingerprint của secret-bearing data.

## 14.5 Display safety

Dashboard luôn dùng:

```text
textContent
escaped HTML
pre/code text nodes
```

Không render prompt/response bằng `innerHTML` trừ khi đã escape chặt.

Không render model-produced HTML/SVG trực tiếp.

## 15. Retention and cleanup

Cleanup chạy:

```text
at startup
every 1 hour
after database crosses 90% max size
after settings change
```

Delete order:

1. Records older than retention days.
2. Records above max count, oldest first.
3. Records until DB logical content falls below max size target.

Database physical file size không giảm ngay sau DELETE.

Đề xuất:

- `secure_delete=FAST` cho routine deletes.
- Periodic checkpoint.
- Optional scheduled `VACUUM` only during idle/explicit maintenance because it can be expensive.
- Dashboard hiển thị logical size và physical file size riêng.

On startup:

```text
status=running older than process start
-> status=interrupted
-> capture_incomplete=true
```

## 16. History management APIs

Tất cả endpoint dưới đây yêu cầu dashboard admin auth.

## 16.1 List

```http
GET /api/dashboard/control/history
```

Query:

```text
cursor
limit <= 100
from
to
status
protocol
endpoint
model
client_id
thinking
stream
has_tools
has_search
has_error
q
```

Response:

```json
{
  "items": [],
  "next_cursor": null,
  "summary": {},
  "storage": {}
}
```

Không trả full request/reasoning/response.

## 16.2 Detail

```http
GET /api/dashboard/control/history/:id
```

Trả metadata, attempts, events và section descriptors.

## 16.3 Content section

```http
GET /api/dashboard/control/history/:id/content/:kind
```

Lý do tách endpoint:

- Detail metadata tải nhanh.
- Reasoning/response lớn chỉ tải khi mở tab.
- Dễ áp response-size limits.

## 16.4 Stats

```http
GET /api/dashboard/control/history/stats
```

## 16.5 Delete one

```http
DELETE /api/dashboard/control/history/:id
```

## 16.6 Purge filtered/all

```http
POST /api/dashboard/control/history/purge
```

Body:

```json
{
  "confirm": true,
  "before": null,
  "status": null,
  "all": false
}
```

## 16.7 Export

```http
POST /api/dashboard/control/history/export
```

Formats:

```text
json
jsonl
```

Default export phải giữ redaction hiện có.

Không có option export raw pre-redaction data vì raw data không được persist.

## 16.8 Settings/status

```http
GET   /api/dashboard/control/history/settings
PATCH /api/dashboard/control/history/settings
```

Sensitive key không được trả.

## 17. Audit requirements

Ghi management audit cho:

```text
history_view_detail
history_view_content
history_export
history_delete
history_purge
history_settings_update
```

Không ghi content vào audit details.

Audit details chỉ chứa:

```text
request_id
content_kind
record_count
filter summary
export format
```

## 18. Search behavior

Initial search đề xuất:

- Metadata filters bằng indexed columns.
- `q` tìm trong prompt preview, response preview, request ID, client name và model.

Không triển khai FTS5 trong phase đầu vì:

- Prompt content nhạy cảm.
- FTS shadow tables làm secure deletion phức tạp hơn.
- Có thể thêm sau khi retention/encryption được chứng minh.

Full-content search có thể là phase sau với explicit opt-in.

## 19. Performance targets

History enabled, redacted mode:

```text
Sync request median overhead:       <= 2%
Streaming time-to-first-byte delta: <= 5 ms
No per-token SQLite write
No unbounded memory growth
No request failure caused by history DB
```

Dashboard list:

```text
50 rows under 200 ms on 10,000 records
Detail metadata under 200 ms
Large content loaded lazily
```

## 20. Failure semantics

### Database cannot open

- Service starts.
- History state = degraded/disabled.
- Dashboard warning.
- LLM endpoints continue.

### Queue full

- Drop intermediate capture commands first.
- Preserve terminal status through reserved/high-priority path if feasible.
- Mark incomplete.

### Disk full

- Attempt retention cleanup once.
- If still full, suspend writes.
- Do not retry every token/request without backoff.

### Migration fails

- Do not destroy existing DB.
- Rename/copy only after explicit backup strategy.
- History disabled with actionable error.
- LLM service continues.

### Client disconnects

- Final status `cancelled`.
- Preserve partial reasoning/response already flushed.

### Process crash

- Running records become `interrupted` on next startup.

## 21. Security requirements

- Admin-only endpoints.
- CSRF on mutations.
- No content in query-string parameters.
- No raw content in regular tracing logs.
- File permissions verified at startup.
- Symlink-safe database path handling where practical.
- Pagination and response-size limits.
- Constant bounded preview size.
- Export requires confirmation.
- Delete/purge audited.
- Stored HTML treated as text.
- No secret values in error messages.

## 22. Implementation phases

## Phase 0 — Approval

No runtime code changes.

Approve:

- Capture mode.
- Retention/size limits.
- Encryption option.
- Search/tool/shell capture policy.
- Dashboard page design.
- Export/delete behavior.

## Phase 1 — Storage foundation

Files likely added/changed:

```text
Cargo.toml
Cargo.lock
src/history/*
src/config/types.rs
src/config/file.rs
src/config/loader.rs
src/config/security.rs
src/config/tests.rs
src/runtime.rs
src/state.rs
src/lib.rs
```

Deliverables:

- History config.
- Noop store.
- SQLite store.
- Schema/migrations.
- Writer queue.
- Redaction.
- Limits.
- Retention.
- Unit tests.

No dashboard yet.

## Phase 2 — Anthropic capture

Files likely changed:

```text
src/handlers/messages.rs
src/opencode/mapper/*
src/opencode/forward/sync.rs
src/opencode/forward/stream/context.rs
src/opencode/forward/stream/execute.rs
src/opencode/retry/*
```

Deliverables:

- Sync request/response capture.
- Streaming reasoning/response capture.
- Tool/search/retry timeline.
- Cancellation/error finalization.

## Phase 3 — OpenAI capture

Files likely changed:

```text
src/handlers/openai.rs
src/opencode/types.rs
src/opencode/retry/*
```

Deliverables:

- Sync capture.
- Transparent SSE tee capture.
- Reasoning/content/tool parsing.
- No downstream byte changes.

## Phase 4 — Management APIs

Files likely changed:

```text
src/dashboard/control/history.rs
src/dashboard/control/mod.rs
src/dashboard/mod.rs
src/server/routes.rs
src/audit.rs
```

Deliverables:

- List/detail/content/stats.
- Delete/purge/export/settings.
- Auth, CSRF and audit.

## Phase 5 — Dashboard History UI — mandatory deliverable

Backend storage/API không được xem là hoàn thành nếu chưa có phần History trong dashboard để người dùng kiểm tra dữ liệu đã lưu.

Files likely changed:

```text
src/webui/index.html
src/webui/style.css
src/webui/app.js
```

Sidebar sau implementation:

```text
Dashboard
API Keys
Models
History
System
```

### History list page

Bắt buộc có:

- Summary cards: requests today, success rate, average latency và stored size.
- Search theo request ID, prompt preview, model và client/API-key name.
- Filters theo time range, status, endpoint/protocol, model, API key, thinking, streaming, tools, search và error.
- Cursor pagination hoặc Load more; không tải toàn bộ history một lần.
- Table/list columns:
  - Time.
  - Client.
  - Prompt preview.
  - Effective model.
  - Status.
  - Latency.
  - Token usage.
- Loading, empty, degraded và database-disabled states rõ ràng.
- Auto-refresh có kiểm soát hoặc nút Refresh; không polling dày gây tải database.

### Request detail drawer

Bắt buộc có các tab:

```text
Overview
Inbound request
Effective prompt
Reasoning
Response
Tools & Search
Attempts
Raw JSON
```

Yêu cầu UI:

- Content lớn tải lazy khi mở tab.
- Reasoning, response và raw JSON có chiều cao cố định và chỉ cuộn trong pane.
- Không làm drawer/modal mở rộng vô hạn theo content.
- Có copy button cho request ID, prompt, reasoning và response.
- Hiển thị badge redacted, truncated, incomplete và interrupted.
- Hiển thị diff summary giữa inbound và effective request.
- Tool/search/retry/fallback hiển thị theo timeline có thứ tự.
- Không render model output bằng `innerHTML`; luôn escape và hiển thị như text.

### History settings and storage status

Bắt buộc có:

- Enabled/disabled state.
- Capture mode.
- Retention days.
- Max records.
- Logical stored size và physical database size.
- Last cleanup/checkpoint.
- Degraded/error warning.
- Purge history action với confirmation.
- Export filtered history dạng redacted JSON/JSONL.

### Responsive and consistency

- Dùng cùng design system hiện tại của Dashboard/API Keys/Models/System.
- EN/VI đầy đủ.
- Desktop 1440/1024 và mobile 390/320.
- Không horizontal overflow.
- Không nested outer scrollbar.
- Trên mobile, list chuyển sang card rows hoặc bảng cuộn ngang có chủ đích; ưu tiên card rows.
- Drawer mobile dùng full viewport nhưng header/footer vẫn cố định, body là vùng cuộn duy nhất.

### Completion gate

Phase 5 không phải phần tùy chọn. Toàn bộ feature Request History chỉ được bàn giao khi:

- Backend capture/storage hoạt động.
- Management API hoạt động.
- Dashboard History page hoạt động trên dữ liệu thật.
- Có manual screenshots và machine-readable verification cho EN/VI, desktop/mobile.

Deliverables:

- Fifth History page.
- Filters and cursor pagination.
- Request detail drawer with lazy tabs.
- Settings and storage status.
- Delete, purge and export UI.
- EN/VI desktop/mobile verification.

## Phase 6 — Hardening and verification

- Load/concurrency tests.
- Crash recovery.
- Disk-full simulation.
- Redaction corpus.
- XSS payload tests.
- Retention tests.
- Export/delete tests.
- Manual desktop/mobile verification.
- Optional SQLCipher proof.

## 23. Test plan

## 23.1 Unit tests

- Config defaults and env/TOML precedence.
- Schema migration.
- Redaction keys and string patterns.
- No known secret reaches stored output.
- Truncation and byte caps.
- Preview generation.
- Retention by age/count/size.
- Interrupted record recovery.
- Queue overflow behavior.

## 23.2 Integration tests

Anthropic:

```text
sync text
sync reasoning
stream text
stream reasoning
native tools
tool-result history
web search loops
retry
model fallback
client cancellation
provider error
```

OpenAI:

```text
sync text/reasoning
stream text/reasoning
[DONE]
premature EOF
fragmented UTF-8
fragmented SSE JSON
tool-call fragments
client cancellation
```

Security:

```text
Authorization not stored
API keys not stored
Cookies not stored
Admin token not stored
XSS content displayed as text
Unauthorized history access rejected
CSRF mutation rejected
```

## 23.3 Performance/load tests

- 100 concurrent streaming requests.
- Queue saturation.
- 10,000 history rows.
- Large 128k-token-compatible response capture.
- Retention during active reads/writes.
- WAL checkpoint behavior.

## 23.4 Manual UI verification

Viewports:

```text
1440 x 1000
1024 x 768
390 x 844
320 x 800
```

Languages:

```text
English
Tiếng Việt
```

Checks:

- No horizontal overflow.
- No nested outer modal scrollbar.
- Fixed-height content panes with internal scroll.
- Search/filter/pagination.
- Copy/export/delete.
- Long reasoning.
- Huge JSON.
- Redaction/truncation badges.
- Console errors = 0.
- Page errors = 0.

## 24. Acceptance criteria

Feature chỉ được xem là hoàn tất khi:

1. Có thể mở dashboard History và thấy request mới.
2. Detail hiển thị inbound request và effective upstream request riêng biệt.
3. Sync/stream đều lưu reasoning và response đúng.
4. Client cancellation tạo partial record, không treo.
5. Retry/fallback/search/tool timeline đúng thứ tự.
6. Không có API secret/cookie/admin token trong DB qua automated secret scan.
7. History DB lỗi không làm LLM request lỗi.
8. Retention giới hạn được disk usage.
9. Delete/purge/export hoạt động và được audit.
10. Dashboard History dùng dữ liệu thật từ management API, không dùng mock/static data.
11. Dashboard EN/VI desktop/mobile PASS, không horizontal overflow hoặc nested outer scrollbar.
12. Reasoning/response/raw JSON chỉ cuộn trong pane cố định và không làm cửa sổ nở dài.
13. Full Rust tests và quality gates PASS.
14. Manual request replay comparison xác nhận capture không làm đổi response bytes/semantics.

## 25. Decisions requiring user approval

### Decision A — Capture mode

Recommended:

```text
redacted
```

Options:

```text
metadata
redacted
full
```

### Decision B — Enable by default

Recommended for public/release default:

```text
off
```

Recommended for this local deployment after implementation:

```text
enabled with redacted mode
```

### Decision C — Encryption at rest

Choose one:

```text
A. SQLite + filesystem permissions first
B. SQLCipher from initial implementation
```

Recommendation:

```text
A first, B as hardening phase
```

### Decision D — Retention

Recommended:

```text
30 days
10,000 records
1 GiB database cap
```

### Decision E — Search result content

Recommended:

```text
Store search query and metadata
Do not store full search result body by default
```

### Decision F — Tool payloads

Recommended:

```text
Store redacted tool calls/results
Cap each payload at 256 KiB
```

### Decision G — Shell commands

Recommended:

```text
Do not store command text by default
Store hash, length, exit status and latency
```

### Decision H — Dashboard placement

Recommended:

```text
Add fifth sidebar page: History
```

### Decision I — Delete and export

Recommended:

```text
Allow admin delete, purge and redacted JSON/JSONL export
Audit every operation
```

### Decision J — Effective upstream payload

Recommended:

```text
Store it
```

Đây là phần quan trọng nhất để kiểm soát model thực sự đã được prompt những gì.

## 26. Recommended approval package

Nếu user đồng ý toàn bộ recommendation, implementation target sẽ là:

```text
History page:              yes
Capture mode:              redacted
Enabled release default:   false
Enabled local deployment:  true
Storage:                   SQLite
Encryption phase 1:        filesystem permissions
Retention:                 30 days / 10,000 / 1 GiB
Inbound request:           yes
Effective upstream prompt: yes
Reasoning:                 yes
Response:                  yes
Tool calls/results:        yes, redacted and capped
Search queries:            yes
Full search results:       no
Shell command text:        no
Export:                    redacted JSON/JSONL
Delete/purge:              yes, audited
```

## 27. External references consulted

- OWASP Logging Cheat Sheet: sensitive data exclusions, configurable logging and log protection.
- OpenTelemetry semantic conventions: structured GenAI operation/model/token fields and event modeling.
- SQLite Write-Ahead Logging documentation: concurrency, checkpointing and local-filesystem constraints.
- SQLite PRAGMA documentation: `busy_timeout`, `secure_delete`, `synchronous` and WAL behavior.
- Tokio bounded MPSC documentation: bounded queue and backpressure.
- rusqlite documentation: bundled SQLite support.
- SQLCipher official project: optional encrypted SQLite storage.

## 28. Non-goals for initial implementation

- Cloud sync.
- Multi-user RBAC beyond existing dashboard admin auth.
- Vector search/semantic search over prompts.
- Replay request button.
- Editing historical records.
- Capturing raw authorization headers.
- Storing raw pre-redaction secrets.
- Using history data for model training.
- Sending history to third-party telemetry by default.
