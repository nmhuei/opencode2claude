# OpenCode2API Repository Worklog

> File theo dõi dài hạn cho repo `/home/light/GitHub/opencode2claude`.
>
> Mục đích: cung cấp một bản tóm tắt repo có thể đọc nhanh khi bắt đầu session mới, đồng thời lưu lại quá trình chỉnh sửa, quyết định kỹ thuật, kiểm thử và vấn đề còn tồn tại.
>
> `CHANGELOG.md` vẫn dùng cho release notes. File này dùng cho trạng thái làm việc thực tế và lịch sử triển khai trong working tree.

## Quy tắc bắt buộc khi cập nhật

1. Đọc file này trước khi bắt đầu thay đổi lớn.
2. Sau mỗi task hoàn tất, thêm một entry mới vào phần **Nhật ký công việc**; không xóa entry cũ.
3. Cập nhật **Snapshot hiện tại** nếu service, model, endpoint, PID, test result hoặc phạm vi working tree thay đổi.
4. Ghi rõ:
   - Mục tiêu.
   - File đã sửa.
   - Logic/thiết kế đã thay đổi.
   - Kiểm thử đã chạy và kết quả.
   - Artifacts liên quan.
   - Việc còn dở hoặc giới hạn đã biết.
5. Không ghi raw API secret, admin token, session cookie hoặc credential nhạy cảm vào file này.
6. Không reset hoặc checkout toàn bộ working tree chỉ để làm sạch thay đổi cũ.
7. Nếu quality gate fail vì thay đổi ngoài phạm vi task, phải ghi rõ file/lỗi thay vì sửa lan sang phần khác mà chưa đánh giá tác động.

---

## Snapshot hiện tại

Cập nhật gần nhất: **2026-08-03**

### Repo và service

```text
Repo:          /home/light/GitHub/opencode2claude
Service URL:   http://127.0.0.1:4000
Dashboard:     http://127.0.0.1:4000/dashboard
Binary:        /home/light/.local/bin/opencode2api-serve  (deployment thật — supervisor spawn sibling của controller)
Controller:    /home/light/.local/bin/opencode2api
Port:          4000
PID snapshot:  10427   (serve chạy từ target/release, binary build Aug 3 05:02 — hiện tại, không stale)
Status:        running
Managed:       true
Model runtime: opencode/deepseek-v4-flash-free
Proxy pool:    5/5 healthy — primaries 40001-40003 + standbys 40004-40005 online; verified unique exit IPs = 4
Dashboard auth configured: true
Client API auth snapshot:  false
```

PID và uptime là snapshot tại thời điểm ghi; phải kiểm tra lại bằng:

```bash
target/release/opencode2api server status --json
ss -ltnp | grep '127.0.0.1:4000'
```

### Working tree

- Chưa tạo commit cho chuỗi thay đổi dashboard/API/Claude Code hiện tại.
- Working tree có rất nhiều thay đổi cũ ở backend, frontend, docs, scripts và verification.
- Không được `git reset --hard`, checkout toàn repo hoặc xóa artifacts không rõ nguồn gốc.
- Chỉ sửa file liên quan trực tiếp đến task và tạo backup trước thay đổi lớn.

### Quality gate gần nhất

Kết quả gần nhất đã ghi nhận ngày 2026-07-22:

```text
git diff --check                          PASS
node --check src/webui/app.js             PASS
cargo fmt --check                         PASS
cargo check --locked                      PASS
cargo clippy --locked --all-targets -- -D warnings  PASS
cargo build --release --locked --bins     PASS
cargo test --locked                       PASS

478 passed
0 failed
1 ignored
```

Luôn chạy lại vì working tree có thể được thay đổi đồng thời bởi session khác.

---

## Mục tiêu và kiến trúc repo

OpenCode2API là bridge HTTP cục bộ chuyển đổi giữa Claude Code/Anthropic Messages API và upstream OpenAI-compatible API của OpenCode.

Luồng chính:

```text
Claude Code
  -> OpenCode2API :4000
  -> OpenCode/OpenAI-compatible upstream
  -> model được cấu hình
```

Các API chính:

```text
Anthropic-compatible
POST /v1/messages
POST /v1/messages/count_tokens
GET  /v1/models

OpenAI-compatible
POST /v1/chat/completions

Dashboard management
/api/dashboard/control/*
```

### File và module quan trọng

```text
src/serve_main.rs                 Server entrypoint
src/server/                       Routes và runtime HTTP
src/handlers/                     Anthropic/OpenAI handlers
src/opencode/                     Mapping, forwarding, retry, search, streaming
src/api_key.rs                    Managed API-key registry và policy
src/dashboard/control/            Dashboard management handlers
src/config/                       Config loader, types, security
src/webui/index.html              Dashboard markup
src/webui/style.css               Dashboard/login/modal styles
src/webui/app.js                  Dashboard client logic
src/webui/landing.html            Login portal
scripts/manual_verify_*.py        Manual Playwright/API verification
verification/                     Durable verification reports
artifacts/                        Screenshots và machine-readable test output
```

### Workflow mong muốn

```text
ASSESS -> PLAN -> IMPLEMENT -> VERIFY -> REVIEW -> FINAL VERIFY
```

Trước khi kết thúc task:

```bash
git diff --check
node --check src/webui/app.js              # nếu sửa frontend JS
cargo fmt --check
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked --bins
cargo test --locked
```

Sau khi build frontend embedded vào binary, phải restart service và manual test trên bản live, không chỉ test server tạm.

---

## Trạng thái chức năng đã hoàn thành

### Claude Code compatibility

Đã triển khai và kiểm tra:

- Adaptive, fixed và disabled thinking.
- Reasoning effort từ `low` đến `max`.
- Structured output.
- Tool calls, MCP và skills.
- Stream JSON, resume và autocompact.
- Tool-result history.
- Sync và SSE streaming.
- Model configuration cho DeepSeek V4 Flash Free.

Cấu hình mục tiêu đã dùng:

```text
model:            opencode/deepseek-v4-flash-free
context:          200000
output:           128000
fixed thinking:   127000
```

### API gateway

Đã có:

- Anthropic-compatible endpoints.
- OpenAI-compatible chat completions.
- Authentication.
- Streaming.
- Tool calls.
- Count tokens.
- Models endpoint.
- CLI/service lifecycle.

### Managed API key V2

API key là resource riêng với:

- Stable ID.
- Name, description, environment.
- Expiration.
- Active, disabled, revoked.
- Default model và allowed models.
- Output/reasoning limits.
- Requests per minute.
- Concurrent requests.
- Daily quota.
- Endpoint/tool permissions.
- Usage counters.

Managed key format:

```text
sk-oc2-<key-id>.<secret>
```

Registry:

```text
opencode2api.api-keys.json
```

Registry không lưu raw secret; chỉ lưu fingerprint, SHA-256 digest, metadata và policy.

Management API:

```text
GET    /api/dashboard/control/api-keys
POST   /api/dashboard/control/api-keys
POST   /api/dashboard/control/api-keys/verify
GET    /api/dashboard/control/api-keys/:id
PATCH  /api/dashboard/control/api-keys/:id
DELETE /api/dashboard/control/api-keys/:id
POST   /api/dashboard/control/api-keys/:id/rotate
```

### Dashboard redesign

Sidebar hiện có 4 trang:

```text
Dashboard
API Keys
Models
System
```

Dashboard:

- Service, Model, Requests, Uptime summary cards.
- System status.
- Recent activity.
- Quick actions.

API Keys:

- Search/filter.
- Create.
- Batch check.
- Edit drawer: General, Policy, Permissions, Usage.
- Enable/disable, rotate, revoke.
- Secret chỉ hiển thị một lần.
- Client config copy/download.
- Hot reload.

Models:

- Current model.
- Compact model list.
- Search/select model.
- Restart warning khi configured model khác runtime.
- Test model với model selector, Thinking toggle, streaming, response, reasoning và latency.

System:

- Server information.
- Restart/stop.
- Security status.
- Proxy pool.
- Logs.
- Diagnostics.
- Advanced TOML configuration.
- Update và shell completion.

Giao diện chung:

- Responsive desktop/mobile.
- VI/EN.
- Consistent typography, spacing, status badges và controls.
- Không dùng shortcut số 1-7.
- Body/document không tạo scrollbar lồng nhau.
- Main shell là vùng cuộn cấp trang duy nhất.

### Login portal

Đã sửa:

- Login card centered.
- Width khoảng 430px.
- Input và Continue thẳng hàng.
- Một nút hiện/ẩn admin token duy nhất.
- Một SVG/path thay đổi theo trạng thái, không còn hai icon chồng nhau.
- Desktop/mobile responsive.
- Wrong token bị từ chối, đúng token vào dashboard.

### `.env` loading

Đã thêm `config::load_dotenv()` với thứ tự:

```text
1. BRIDGE_ENV_PATH
2. .env trong current working directory
3. .env ở các thư mục cha của binary hiện tại
```

Mục tiêu là daemon chạy từ thư mục khác vẫn đọc được `.env` của repo.

Không ghi giá trị admin token vào tài liệu này.

### Test model streaming và recovery

Đã sửa lỗi frontend treo ở `Waiting for response…`:

- Dừng stream ngay khi nhận `data: [DONE]`.
- Hủy reader thay vì chờ socket upstream đóng.
- Flush buffer SSE cuối.
- Timeout nếu stream ngừng gửi dữ liệu.
- Nếu reasoning có nội dung nhưng final response thiếu hoặc output limit bị dùng hết:
  - Giữ reasoning.
  - Tự chạy request fallback ngắn với Thinking disabled.
  - Điền final response vào khung Response.

Prompt mặc định đã rút ngắn để vừa khung.

Response và Reasoning dùng chiều cao cố định và chỉ cuộn bên trong từng pane; không làm panel nở dài.

### Scroll/overflow architecture

Đã chuẩn hóa:

```text
html/body          overflow hidden trên dashboard
app shell          không cuộn
main shell         page scroller duy nhất
dialog outer       không cuộn
modal shell        không cuộn
modal body         cuộn khi thật sự cần
logs               chỉ log pane cuộn
config             editor/preview hoặc body mobile cuộn
response/reasoning chỉ pane tương ứng cuộn
```

Đã audit các cửa sổ:

- Create API key.
- API key drawer.
- Secret dialog.
- Check API keys.
- Logs.
- Diagnostics.
- Configuration.
- Confirm dialog.

### Proxy action alignment

Cột thao tác Proxy dùng grid cố định:

```text
Primary action: 112 x 32px
Logs action:     68 x 32px
```

`Restart`/`Protected` và `Logs` luôn cùng trục ở mọi hàng, cả EN và VI.

### Batch Check API keys

Nút Check API key không còn yêu cầu paste secret.

Khi mở modal, dashboard tự gọi endpoint verify với body rỗng để kiểm tra toàn bộ registry.

Kết quả gồm:

- Total.
- Healthy.
- Expiring soon.
- Unavailable.
- Danh sách từng key: name, fingerprint, status, expiration, last used.

Trạng thái authoritative:

- Healthy: active và chưa hết hạn.
- Expiring soon: active, hết hạn trong 7 ngày.
- Disabled: unavailable.
- Expired: dead/unavailable.

Giới hạn quan trọng:

- Raw secret không được lưu nên không thể gửi live request bằng từng managed key sau khi cửa sổ secret đã đóng.
- Batch check xác thực registry state và khả năng được chấp nhận về mặt trạng thái, không phải upstream model probe bằng secret thật.
- Endpoint verify secret cũ vẫn được giữ tương thích: gửi `{ "secret": "..." }` vẫn xác minh một secret cụ thể.

---

## Artifacts và script đáng chú ý

### Dashboard/UI

```text
artifacts/dashboard-simple-redesign/
artifacts/dashboard-login-fix/
artifacts/dashboard-status-badges/
artifacts/dashboard-ui-consistency/
artifacts/modal-fit-redesign/
artifacts/login-password-toggle/
artifacts/model-reasoning-test/
artifacts/model-stream-recovery/
artifacts/proxy-action-alignment/
artifacts/api-key-batch-check/
```

### Manual verification scripts

```text
scripts/manual_verify_simple_dashboard.py
scripts/manual_verify_simple_dashboard_lifecycle.py
scripts/manual_verify_api_key_management_redesign.py
scripts/manual_verify_dashboard_mutations.py
scripts/manual_verify_free_models.py
scripts/manual_claude_code_real_matrix.py
scripts/audit_claude_code_cli.py
scripts/audit_claude_code_surface.py
```

Không mặc định xóa artifacts; một số file là bằng chứng manual verification.

---

## Quyết định kỹ thuật cần giữ

1. Raw managed API-key secret chỉ hiển thị một lần và không được lưu lại.
2. Dashboard changes phải hot reload khi backend hỗ trợ; không yêu cầu restart không cần thiết.
3. CSS component phải dùng Grid/Flex và selector có scope, không căn thủ công từng hàng.
4. Desktop ưu tiên hiển thị đầy đủ trong modal; mobile được phép cuộn nội bộ có kiểm soát.
5. Không để dialog outer và modal body cùng cuộn.
6. Không giả lập reasoning/response trong UI production; dữ liệu trình diễn phải đến từ request thật hoặc fallback request thật.
7. Không báo PASS nếu chỉ nhìn screenshot; cần đo bounding box/overflow và kiểm tra console/page errors.
8. Không báo flag/secret hoặc kết quả chưa verify.
9. Không tự sửa file ngoài phạm vi chỉ để làm sạch working tree.

---

## Việc cần kiểm tra ở session kế tiếp

1. Đọc `REPO_WORKLOG.md` và `git status --short`.
2. Kiểm tra service/PID hiện tại.
3. Kiểm tra thay đổi đồng thời trong working tree trước khi patch.
4. Xác nhận endpoint/dashboard live trước manual test.
5. Sau task, cập nhật entry mới ở cuối file này.
6. Cập nhật Snapshot nếu PID, model, test result hoặc service state thay đổi.

---

## Nhật ký công việc

### 2026-07-21 đến 2026-07-22 — Dashboard, API key, model tester và UI consistency

**Mục tiêu**

- Redesign dashboard thành UI đơn giản, dễ đọc và responsive.
- Xây API-key management V2.
- Sửa login và `.env` loading.
- Chuẩn hóa status badges, tables, controls, modal và scrollbar.
- Mở rộng Test model có model selector và Thinking toggle.
- Sửa stream reasoning treo và thiếu final response.
- Đổi Check API key thành batch registry check.

**File chính đã sửa**

```text
src/webui/index.html
src/webui/style.css
src/webui/app.js
src/webui/landing.html
src/dashboard/control/api_keys.rs
src/server/routes.rs
src/api_key.rs
src/config/mod.rs
src/app/mod.rs
src/app/dashboard.rs
src/serve_main.rs
```

**Kết quả nổi bật**

- Dashboard 4 trang hoàn chỉnh.
- Login ổn định desktop/mobile.
- Managed API keys có policy/usage/lifecycle.
- Test model hiển thị reasoning và response, xử lý `[DONE]`, timeout và fallback.
- Modal không còn scrollbar lồng nhau.
- Proxy action columns thẳng hàng.
- Batch key health check không cần paste secret.
- EN/VI và viewport 1440, 390, 320 được manual test.

**Verification gần nhất**

```text
478 passed
0 failed
1 ignored
Console errors: 0
Page errors: 0
```

**Service snapshot sau triển khai**

```text
PID: 1446255
Port: 4000
Model: opencode/deepseek-v4-flash-free
Status: running
```

**Chưa làm**

- Chưa tạo commit.
- Chưa dọn toàn bộ artifacts/working tree.
- Batch key check không thể live-probe từng managed secret do thiết kế không lưu raw secret.

---

### 2026-07-22 — Tạo persistent repository worklog

**Mục tiêu**

- Tạo một file tóm tắt repo và lưu lịch sử chỉnh sửa để session sau đọc và ghi tiếp.
- Phân biệt operational worklog với release `CHANGELOG.md`.
- Buộc agent làm việc trong repo cập nhật worklog sau task.

**File đã sửa**

```text
REPO_WORKLOG.md
CLAUDE.md
```

**Thay đổi kỹ thuật**

- Tạo snapshot service, kiến trúc, chức năng đã hoàn thành, quality gates, artifacts, quyết định kỹ thuật và giới hạn hiện tại.
- Thêm mẫu entry dùng chung cho các lần cập nhật sau.
- Thêm chỉ dẫn vào `CLAUDE.md` để đọc worklog trước task lớn và append sau khi hoàn tất.
- Cấm ghi secret/token/cookie vào worklog.

**Kiểm thử**

```text
File created and readable       PASS
CLAUDE.md reference present     PASS
Markdown fence structure        PASS
```

**Việc còn dở / giới hạn**

- Snapshot có thể cũ sau khi service restart; session sau phải kiểm tra và cập nhật lại.
- File chưa được commit vì toàn bộ working tree hiện vẫn chưa commit.

---

## Mẫu entry cho lần sau

Sao chép block dưới đây và thêm vào cuối phần **Nhật ký công việc**:

````markdown
### YYYY-MM-DD — Tên task ngắn

**Mục tiêu**

- ...

**Phạm vi và file đã sửa**

```text
path/to/file
```

**Thay đổi kỹ thuật**

- ...

**Kiểm thử**

```text
command                         PASS/FAIL
```

**Artifacts**

```text
artifacts/...
```

**Service snapshot**

```text
PID:
Port:
Model:
Status:
```

**Việc còn dở / giới hạn**

- ...
````


### 2026-07-22 — Lập kế hoạch Request History

**Mục tiêu**

- Thiết kế nơi lưu chi tiết inbound request, effective upstream prompt, reasoning, response, tools, search, retry và fallback.
- Chỉ lập kế hoạch để user duyệt; chưa implement runtime, database, API hoặc UI.
- Nghiên cứu thêm tài liệu chính thức về GenAI observability, logging security, SQLite/WAL, bounded queues và encryption at rest.

**File đã thêm**

```text
docs/architecture/REQUEST_HISTORY_DESIGN_PLAN.md
```

**Nội dung kế hoạch**

- Đề xuất trang sidebar thứ năm `History`.
- Thiết kế SQLite schema cho requests, content, attempts và events.
- Phân biệt inbound request với effective upstream payload.
- Capture lifecycle cho Anthropic/OpenAI, sync/SSE, reasoning, tools, search, retry, fallback và cancellation.
- Redaction, retention, storage caps, delete/export, audit và failure semantics.
- Cấu hình `HistoryConfig` và các phase triển khai.
- Checklist quyết định cần user duyệt trước implementation.

**Kiểm thử tài liệu**

```text
Plan file created                  PASS
Runtime implementation changed    NO
Service restart required          NO
```

**Việc còn dở / giới hạn**

- Chờ user duyệt các quyết định A-J trong plan.
- Chưa thêm dependency SQLite.
- Chưa tạo database hoặc History page.
- Chưa thay đổi request/response flow.


### 2026-07-22 — Bổ sung History Dashboard UI vào kế hoạch

**Mục tiêu**

- Xác nhận Request History không chỉ có backend/storage mà bắt buộc phải có dashboard UI để người dùng kiểm tra dữ liệu đã lưu.

**File đã sửa**

```text
docs/architecture/REQUEST_HISTORY_DESIGN_PLAN.md
```

**Thay đổi kế hoạch**

- Đánh dấu Phase 5 Dashboard History UI là deliverable bắt buộc.
- Quy định sidebar có trang thứ năm `History`.
- Chi tiết hóa list page, filters, cursor pagination, detail drawer, lazy content tabs, settings, storage status, delete/purge/export và responsive EN/VI.
- Thêm completion gate: backend không được xem là hoàn thành nếu dashboard chưa chạy trên dữ liệu thật.
- Bổ sung acceptance criteria về fixed-height content panes, internal scroll, không nested scrollbar và không mock data.

**Implementation status**

```text
Backend history implementation    NOT STARTED
Dashboard History UI              NOT STARTED
Planning document                 UPDATED
Service restart                   NOT REQUIRED
```


### 2026-07-22 — Implement Request History backend, dashboard and Claude Code verification

**Mục tiêu**

- Lưu chi tiết request, effective upstream prompt, reasoning, response và execution timeline.
- Thêm trang History bắt buộc vào Dashboard UI.
- Kiểm tra end-to-end bằng Claude Code CLI thật qua service live.

**Phạm vi và file đã sửa/thêm**

```text
Cargo.toml
Cargo.lock
.env.example
README.md
docs/architecture/REQUEST_HISTORY_DESIGN_PLAN.md
src/config/types.rs
src/config/file.rs
src/config/loader.rs
src/config/mod.rs
src/runtime.rs
src/state.rs
src/lib.rs
src/history/mod.rs
src/history/types.rs
src/history/redact.rs
src/history/store.rs
src/handlers/messages.rs
src/handlers/openai.rs
src/opencode/forward/sync.rs
src/opencode/forward/stream/execute.rs
src/dashboard/control/history.rs
src/dashboard/control/mod.rs
src/dashboard/mod.rs
src/server/routes.rs
src/webui/index.html
src/webui/style.css
src/webui/app.js
```

Local `.env` được bật:

```text
BRIDGE_HISTORY_ENABLED=true
BRIDGE_HISTORY_CAPTURE_MODE=redacted
BRIDGE_HISTORY_RETENTION_DAYS=30
BRIDGE_HISTORY_MAX_RECORDS=10000
BRIDGE_HISTORY_MAX_DATABASE_BYTES=1073741824
```

**Backend/storage**

- SQLite WAL database:

```text
~/.opencode2api/history/request-history.sqlite3
```

- Directory permission `0700`, database permission `0600`.
- Bounded single-writer queue; History failure không làm inference failure.
- Tables:
  - `history_requests`.
  - `history_content`.
  - `history_attempts`.
  - `history_events`.
  - `history_settings`.
- Request đang `running` được đổi thành `interrupted` sau process restart/crash.
- Settings từ dashboard được persist vào SQLite và override baseline config sau restart.
- Retention theo ngày, số record và logical storage size.
- Delete, filtered purge, export và settings có admin auth, CSRF và audit.

**Capture**

- Anthropic `/v1/messages` sync và SSE.
- OpenAI `/v1/chat/completions` sync và transparent SSE tee.
- Inbound request.
- Effective upstream request sau policy/mapping.
- Reasoning/thinking.
- Final response.
- Raw provider response khi có.
- Attempt, tool, search, retry/fallback metadata.
- Status completed/failed/cancelled/interrupted.
- Latency, first chunk, usage và finish reason.

**Bảo mật**

- Redaction chạy trước persistence.
- Không cố ý lưu Authorization, cookie, CSRF, admin token hoặc raw API-key secret.
- Recursive sensitive-key redaction và token/managed-key/JWT/PEM pattern redaction.
- Byte cap cho request, reasoning, response, tool payload và toàn record.
- Database scan live không tìm thấy client bridge key hoặc fake secret dùng trong test.

**Dashboard History UI**

- Sidebar có trang thứ năm `History`.
- Summary cards: requests today, success rate, average latency, stored size.
- Search/filter theo status, protocol, model, thinking và streaming.
- Pagination.
- Detail drawer với tabs:
  - Overview.
  - Inbound request.
  - Effective prompt.
  - Reasoning.
  - Response.
  - Tools & Search.
  - Attempts.
  - Raw JSON.
- Content tải lazy và dùng `textContent`, không render model HTML.
- Fixed-height internal panes; dialog outer không cuộn.
- Settings modal, export, delete và purge.
- EN/VI, desktop/mobile.

**Claude Code test thật**

```text
Claude Code:       2.1.217
Prompt marker:     CLAUDE_HISTORY_E2E_20260722
Exit code:         0
Thinking deltas:   89
Text deltas:       20
Result:
product: 943
marker: CLAUDE_HISTORY_E2E_20260722
```

History record trên PID cuối:

```text
Request ID:         req-3d58d9-10
Protocol:           anthropic
Status:             completed
Model:              opencode/deepseek-v4-flash-free
Inbound request:    31,528 bytes
Effective request:  30,809 bytes
Reasoning:          229 bytes
Response:           48 bytes
Attempts:           1
Events:             5
```

Assertions:

```text
Inbound contains marker       PASS
Effective contains marker     PASS
Reasoning captured            PASS
Response contains marker      PASS
Terminal event recorded       PASS
Bridge key absent in history  PASS
```

**UI verification**

```text
1440x1000 EN       PASS
1440x1000 VI       PASS
390x844 EN         PASS
320x800 VI         PASS
Horizontal overflow           0
Outer dialog scrollbar        0
Console errors                0
Page errors                   0
```

**Quality gates**

```text
git diff --check                          PASS
node --check src/webui/app.js             PASS
cargo fmt --check                         PASS
cargo check --locked                      PASS
cargo clippy --locked --all-targets -- -D warnings  PASS
cargo build --release --locked --bins     PASS
cargo test --locked                       PASS

478 passed
0 failed
1 ignored
```

**Service snapshot**

```text
URL:    http://127.0.0.1:4000
PID:    4020441
Model:  opencode/deepseek-v4-flash-free
Status: running
```

**Artifacts**

```text
artifacts/request-history-implementation/history-ui-verification.json
artifacts/request-history-implementation/claude-code-history-verification.json
artifacts/request-history-implementation/claude-history.stdout.jsonl
artifacts/request-history-implementation/claude-history.stderr.log
artifacts/request-history-implementation/history-ui-desktop.png
artifacts/request-history-implementation/history-ui-desktop-vi.png
artifacts/request-history-implementation/history-ui-mobile.png
artifacts/request-history-implementation/history-ui-mobile-narrow-vi.png
artifacts/request-history-implementation/history-export-claude.json
artifacts/request-history-implementation/history-settings-patch.json
```

**Giới hạn còn lại**

- Chưa có SQLCipher; phase hiện tại dùng filesystem permissions + redaction.
- Chưa có full-text/semantic search.
- Full web-search result body mặc định không lưu.
- Raw upstream SSE của Anthropic không lưu nguyên byte; reasoning/response đã parse và lưu.
- Chưa tạo commit vì working tree có nhiều thay đổi cũ chưa commit.


### 2026-07-22 — Dashboard single-viewport redesign và brand icon

**Mục tiêu**

- Redesign toàn bộ trang chức năng để desktop nhìn được tổng quan trong một viewport, không phải cuộn toàn trang.
- Chuyển History sang master-detail: danh sách bên trái, nội dung request bên phải.
- Dùng biểu tượng vòng tròn + hai chevron do user cung cấp làm icon chính của dashboard.

**File đã sửa/thêm**

```text
src/webui/index.html
src/webui/landing.html
src/webui/app.js
src/webui/style.css
src/webui/brand-mark.svg
src/webui/favicon.svg
artifacts/dashboard-single-viewport-redesign/
```

**Thay đổi kỹ thuật**

- Dashboard desktop dùng layout cố định theo viewport; chỉ pane nội bộ cần thiết được cuộn.
- API Keys chuyển sang danh sách bên trái và overview policy/usage bên phải; double-click hoặc nút Edit mở drawer chỉnh sửa đầy đủ.
- Models chuyển thành model catalog bên trái và Test model bên phải.
- History chuyển thành request list bên trái và detail tabs bên phải; không còn drawer che toàn màn hình.
- System chuyển thành server/security/maintenance bên trái và proxy pool bên phải.
- Mỗi lần đổi view reset page scroll về đầu.
- Mobile 320–390px vẫn dùng cuộn dọc có kiểm soát và không horizontal overflow.
- Thay nhãn `O2` bằng SVG brand mark trên sidebar và login.
- Thêm SVG favicon nền tối cho browser tab.

**Kiểm thử**

```text
git diff --check                          PASS
node --check src/webui/app.js             PASS
cargo fmt --check                         PASS
cargo check --locked                      PASS
cargo clippy --locked --all-targets -- -D warnings  PASS
cargo build --release --locked --bins     PASS
cargo test --locked                       PASS

478 passed
0 failed
1 ignored
```

Viewport verification:

```text
1440 x 900     PASS
1024 x 768     PASS
1280 x 720     PASS
390 x 844      PASS
320 x 800      PASS
Console errors 0
Page errors    0
```

Brand verification:

```text
/dashboard/brand-mark.svg  HTTP 200
/dashboard/favicon.svg     HTTP 200
Login icon loaded          PASS
Sidebar icon loaded        PASS
Favicon configured         PASS
```

**Artifacts**

```text
artifacts/dashboard-single-viewport-redesign/single-viewport-verification.json
artifacts/dashboard-single-viewport-redesign/brand-icon-verification.json
artifacts/dashboard-single-viewport-redesign/brand-login.png
artifacts/dashboard-single-viewport-redesign/brand-dashboard.png
artifacts/dashboard-single-viewport-redesign/*-{dashboard,api,models,history,system}.png
```

**Service snapshot**

```text
PID:    163241
Port:   4000
Status: running
```

**Việc còn dở / giới hạn**

- Chưa tạo commit.
- Mobile vẫn cuộn dọc vì không thể hiển thị an toàn toàn bộ nội dung trong một viewport hẹp.
- SVG được dựng lại sạch theo biểu tượng user cung cấp để giữ nét ở kích thước nhỏ; không nhúng nguyên ảnh raster nền đen.


### 2026-07-22 — Debug chính xác History reasoning/response và merge recovery

**Mục tiêu**

- Kiểm tra bằng Claude Code CLI thật xem History có lưu đúng inbound, effective prompt, reasoning và response hay không.
- Xác định nguyên nhân một số dòng History chỉ có reasoning, trong khi dòng kế tiếp chỉ có response.
- Sửa Model Tester recovery để một logical interaction hiển thị reasoning và response trong cùng record chính.

**Nguyên nhân xác nhận**

- Claude Code request chính đã lưu reasoning và response thành hai content section riêng nhưng cùng một `request_id`; đây là thiết kế đúng để UI tải lazy theo tab.
- Claude Code còn gửi request phụ tạo session title không-thinking, nên có thể xuất hiện hai request gần nhau nhưng operation khác nhau.
- Model Tester có flow recovery thật sự gửi hai HTTP request: request Thinking bị client đóng sau reasoning, sau đó request không-thinking lấy final response. Trước bản sửa, hai HTTP request hiện thành hai dòng độc lập.

**File đã sửa**

```text
src/history/types.rs
src/history/store.rs
src/handlers/openai.rs
src/webui/app.js
REPO_WORKLOG.md
```

**Thay đổi kỹ thuật**

- Sử dụng hai cột SQLite đã có `conversation_id` và `parent_request_id`.
- Dashboard Model Tester gửi correlation headers:
  - `x-opencode-history-conversation-id`.
  - `x-opencode-history-parent-request-id`.
  - `x-opencode-history-operation`.
- Operation được phân biệt:
  - `model_test`.
  - `response_recovery`.
  - `chat_completions`.
- Khi recovery hoàn tất:
  - Response được gắn vào parent record.
  - Parent chuyển sang `completed`, finish reason `recovered`.
  - Thêm attempt `response_recovery`.
  - Thêm event `response_recovered` chứa child request ID.
  - Child recovery được giữ trong SQLite để audit nhưng ẩn khỏi list/stats mặc định.
- Delete parent đồng thời xóa child recovery.
- Thêm index cho `conversation_id` và `parent_request_id`.
- History Overview hiển thị Operation, Conversation và Parent request.

**Kiểm thử thực tế**

Claude Code CLI 2.1.217 trên binary cuối:

```text
request_id:             req-82778-6f
protocol:               anthropic
status:                 completed
thinking:               enabled
reasoning bytes:        463
response bytes:         48
CLI/API/SQLite hashes:  exact match
```

Real OpenAI recovery cố tình đóng stream sau reasoning:

```text
parent request:          req-82778-49
child recovery:          req-82778-4b
parent before recovery:  cancelled
parent after recovery:   completed
finish reason:           recovered
parent reasoning bytes:  250
parent response bytes:   12
visible list rows:        1 parent only
```

Dashboard UI:

```text
Reasoning tab loads parent content       PASS
Response tab loads parent content        PASS
response_recovered event visible         PASS
child recovery hidden from table         PASS
horizontal overflow                      0
console errors                            0
page errors                               0
```

Frontend correlation headers:

```text
initial model_test conversation ID       PASS
recovery reuses conversation ID          PASS
recovery parent request ID                PASS
recovery operation marker                 PASS
```

Quality gates:

```text
git diff --check                          PASS
node --check src/webui/app.js             PASS
cargo fmt --check                         PASS
cargo check --locked                      PASS
cargo clippy --locked --all-targets -- -D warnings  PASS
cargo build --release --locked --bins     PASS
cargo test --locked                       PASS

479 passed
0 failed
1 ignored
```

**Artifacts**

```text
artifacts/history-capture-debug/fresh-exact-verification.json
artifacts/history-capture-debug/real-recovery-merge-verification.json
artifacts/history-capture-debug/recovery-ui-verification.json
artifacts/history-capture-debug/tester-correlation-verification.json
artifacts/history-capture-debug/recovery-history-ui.png
```

**Service snapshot**

```text
PID:    534392
Port:   4000
Model:  opencode/deepseek-v4-flash-free
Status: running
```

**Giới hạn / lưu ý**

- Các record Model Tester cũ được tạo trước bản sửa vẫn giữ cấu trúc tách rời lịch sử; bản sửa áp dụng cho request mới.
- Child recovery mới vẫn tồn tại trong SQLite để audit, nhưng không xuất hiện thành dòng riêng trên dashboard và không bị tính hai lần trong summary stats.


## 2026-07-23 — Fix Proxy Pool restart và dashboard server restart

**Mục tiêu**

- Điều tra nút `Restart` từng node trong Proxy Pool.
- Điều tra nút restart toàn bộ bridge/server báo lỗi.
- Rebuild, chạy live và kiểm thử các đường restart thực tế.

**Nguyên nhân gốc**

1. Restart từng proxy đã gọi đúng API và Docker thực sự recreate container, nhưng management path không cập nhật `ProxyPool` trong bộ nhớ. Dashboard tải lại ngay vẫn thấy trạng thái `degraded/open`, restart queue cũ vẫn còn, nên thao tác trông như không chạy và có nguy cơ đụng automatic restart worker.
2. Dashboard server restart gọi supervisor CLI, nhưng service có thể được khởi động trực tiếp bằng `opencode2api-serve` nên `/health` vẫn chạy trong khi không có PID file supervisor. CLI từ chối dừng process không được chứng minh ownership và dashboard báo restart thất bại.

**File đã sửa**

```text
src/application/lifecycle.rs
src/management/service.rs
src/proxy_pool/maintenance.rs
```

**Thay đổi chính**

- Trước restart proxy, node được atomically chuyển sang `recovering/half_open`, gỡ khỏi normal routing và automatic restart queue, reset cooldown/failure/identity cũ.
- Docker restart success/failure cập nhật metrics và trạng thái pool; failure được đưa lại vào bounded recovery queue khi còn retry budget.
- Server restart/stop từ dashboard tự nhận diện chính process đang phục vụ request và ghi PID file có process identity trước khi giao cho supervisor CLI.
- Không ghi đè PID file đang thuộc một process supervisor khác; PID reuse vẫn được bảo vệ bằng executable/start marker.
- Thêm regression tests cho manual proxy restart state transition, failure requeue và self-adoption của lifecycle.

**Kiểm thử live**

```text
Individual proxy restart API   PASS
Docker StartedAt changed       PASS
Restart all primary proxies    PASS (40001, 40002, 40003)
All 3 container start times    changed

Unmanaged-service simulation   PASS
Dashboard server restart API   HTTP 202
Old PID                        2184245
New PID                        2199305
Managed after restart          true

Final proxy state:
40001 healthy / closed
40002 healthy / closed
40003 healthy / closed
40004 healthy / closed (protected)
40005 healthy / closed (protected)
```

**Quality gates**

```text
git diff --check                                      PASS
node --check src/webui/app.js                         PASS
node --check src/webui/mecha-components.js            PASS
cargo fmt --check                                     PASS
cargo check --locked                                  PASS
cargo clippy --locked --all-targets -- -D warnings    PASS
cargo build --release --locked --bins                 PASS
cargo test --locked                                   PASS

490 passed
0 failed
1 ignored (requires local WARP + Internet)

python3 artifacts/mecha-control-deck/verify_mecha_functionality.py  PASS
console errors                                         0
page errors                                            0
```

**Service snapshot cuối**

```text
PID:      2199305
Port:     4000
Managed:  true
Model:    opencode/deepseek-v4-flash-free
Status:   running
```

**Việc còn dở**

- Không có lỗi restart còn tái hiện trong phạm vi task này.
- Chưa tạo commit theo đúng yêu cầu hiện tại.


## 2026-07-23 — Manual Mecha Dashboard UI review and polish

### Mục tiêu

Hoàn thành phần còn dở của Mecha Control Deck: không chỉ chạy automation, mà chụp screenshot live, review trực quan theo hướng UI designer, chỉnh lại presentation và verify lại toàn bộ gate liên quan.

### Screenshot đã review

Tạo mới bộ screenshot live trong:

```text
artifacts/manual-ui-review/
```

Before:

```text
before-login.png
before-desktop-dashboard.png
before-desktop-api.png
before-desktop-models.png
before-desktop-history.png
before-desktop-system.png
before-tablet-dashboard.png
before-tablet-models.png
before-tablet-history.png
before-tablet-system.png
before-mobile-dashboard.png
before-mobile-api.png
before-mobile-models.png
before-mobile-history.png
before-mobile-system.png
before-mobile-narrow-dashboard.png
before-mobile-narrow-models.png
before-mobile-narrow-history.png
before-mobile-narrow-system.png
```

After:

```text
after-login.png
after-desktop-dashboard.png
after-desktop-api.png
after-desktop-models.png
after-desktop-history.png
after-desktop-system.png
after-tablet-dashboard.png
after-tablet-models.png
after-tablet-history.png
after-tablet-system.png
after-mobile-dashboard.png
after-mobile-api.png
after-mobile-models.png
after-mobile-history.png
after-mobile-system.png
after-mobile-narrow-dashboard.png
after-mobile-narrow-models.png
after-mobile-narrow-history.png
after-mobile-narrow-system.png
```

Comparison boards:

```text
artifacts/manual-ui-review/comparison-desktop.webp
artifacts/manual-ui-review/comparison-tablet.webp
artifacts/manual-ui-review/comparison-mobile.webp
artifacts/manual-ui-review/comparison-mobile-narrow.webp
```

Review notes:

```text
artifacts/manual-ui-review/manual-review-notes.md
```

### Các lỗi thiết kế phát hiện

- Mecha theme có xu hướng giống dashboard cũ được phủ skin game/pixel vì frame 9-slice và glow áp lên quá nhiều panel.
- Background texture/scanline và panel noise cạnh tranh với vùng nhiều chữ như History/System/API tables.
- Muted text hơi thiếu contrast cho dashboard vận hành.
- Metric icons và active nav hơi to/sáng hơn hierarchy cần thiết.
- Quick Actions mascot/decor có nguy cơ cạnh tranh với nội dung.
- Mobile avatar bị ẩn quá mạnh trong lần tuning đầu, làm hỏng visual/functionality gate.
- Mobile login button và modal icon buttons cần giữ đúng 40px theo regression, trong khi nav vẫn cần touch target thoải mái hơn.

### File đã sửa

```text
src/webui/mecha.css
```

### Quyết định visual

- Giảm opacity của background grid/scanline.
- Tăng contrast `text-secondary` và `text-muted`.
- Giảm shadow/glow mặc định của HUD panel.
- Giảm opacity/độ dày frame ở panel phụ; giữ chất pixel nhưng bớt cảm giác game menu.
- Thu gọn metric icons và giảm opacity decor strip.
- Làm active nav rõ nhưng ít chói hơn.
- Giảm topbar shadow và giảm mascot/drone opacity.
- Mobile: giảm decoration, giữ avatar ở kích thước nhỏ 26px thay vì ẩn hẳn.
- Login/mobile modal: giữ button/icon button đúng 40px để pass regression.

### Verification commands

PASS:

```bash
git diff --check
node --check src/webui/app.js
node --check src/webui/mecha-components.js
cargo fmt --check
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked --bins
cargo test --locked
bash artifacts/mecha-control-deck/run_final_ui_suite.sh
```

Final UI suite status:

```text
mecha-ui            0
functionality       0
scroll-invariants   0
single-viewport     0
consistency-en      0
consistency-vi      0
login               0
modals              0
api-key-batch       0
tester-controls     0
```

Suite logs also report PASS for live reasoning and live thinking-off checks.

### Service PID cuối

```text
PID: 2234533
URL: http://127.0.0.1:4000
Dashboard: http://127.0.0.1:4000/dashboard
Model: opencode/deepseek-v4-flash-free
Managed: true
```

Note: service ban đầu chạy unmanaged bằng PID `678162`, nên `server restart` không thay được embedded CSS. Đã SIGTERM đúng PID giữ port 4000 rồi start lại release binary để service chuyển sang managed PID `2234533`.

### Việc còn dở

Không còn blocker trong pass này. Chưa tạo commit.


## 2026-07-24 — Restart bridge and live Claude Code CLI smoke verification

### Mục tiêu

Khởi động lại service `opencode2claude` trên cổng 4000 bằng release binary hiện tại, chuyển từ process unmanaged sang supervisor-managed, rồi xác minh Claude Code CLI thật có thể dùng bridge cho cả text response và vòng lặp tool-use.

### Trạng thái service

- Process cũ: PID `2212531`, unmanaged, executable `target/release/opencode2api-serve`.
- Đã gửi SIGTERM đúng process đang giữ `127.0.0.1:4000`; không tác động service audit ở cổng 4001.
- Khởi động lại bằng controller release với model `opencode/deepseek-v4-flash-free` và egress direct.
- Process mới: PID `2349354`.
- Managed: `true`.
- Health: `{"status":"ok","version":"0.5.0"}`.
- Executable: `/home/light/GitHub/opencode2claude/target/release/opencode2api-serve`.

### Claude Code CLI live verification

Claude Code version:

```text
2.1.218
```

Basic response test:

- Exit code: `0`.
- `is_error=false`.
- `stop_reason=end_turn`.
- Result: `OK`.

Read tool round-trip test:

- Exit code: `0`.
- Events: `201`.
- Assistant tool-use events: `1`.
- User tool-result events: `1`.
- Turns: `2`.
- Final result: `TOOL_READ_OK`.
- Raw `[Requesting ...]` marker count: `0`.
- Permission denials: none.

### Artifacts

```text
artifacts/live-cli-smoke-20260724/basic.json
artifacts/live-cli-smoke-20260724/basic.stderr
artifacts/live-cli-smoke-20260724/read-tool.jsonl
artifacts/live-cli-smoke-20260724/read-tool.stderr
```

### Kết luận

Bridge hiện dùng được qua Claude Code CLI thật cho phản hồi streaming và tool-use/tool-result loop. Đây mới là smoke verification ban đầu; phần audit/refactor parser tổng thể trong bản bàn giao vẫn cần tiếp tục, đặc biệt batch sentinel bằng JSON array, retry/duplicate side effects, fenced-code safety, stream/sync parity, resynchronization, unknown tools và search batches.


## 2026-07-24 — Full parser audit plan

### Mục tiêu

Lập kế hoạch audit toàn bộ pipeline parse/protocol, không chỉ compatibility marker: inbound Anthropic JSON, mapper, sync JSON, SSE streaming, native tool calls, DSML, compatibility markers, JSON repair, search interception, retry và outbound Anthropic blocks.

### Kết quả

Đã tạo:

```text
docs/architecture/PARSER_FULL_AUDIT_PLAN_20260724.md
```

Kế hoạch xác nhận các rủi ro ưu tiên từ code hiện tại:

- Batch đang dùng JSON array làm sentinel, có thể tách nhầm legitimate array input.
- Stream có thể retry sau khi đã emit tool use, gây duplicate side effect.
- Marker parser chưa nhận biết fenced code/inline code/quoted context.
- Stream, sync và EOF recovery chưa dùng chung một parser contract.
- Search batch có thể silently drop invocation sau call đầu.
- Unknown tool handling không parity giữa stream và sync.
- Streaming resynchronization sau malformed marker chưa đầy đủ.
- Prefix buffering `[Requesting ...` quá rộng.
- JSON repair cần property test semantic preservation và fail-closed khi ambiguous.

Kế hoạch bắt buộc test-fail trước refactor, sau đó unified parser core, transactional emission, retry safety, parity tests, fuzz/property tests, fake-upstream E2E và Claude Code CLI thật.

### Trạng thái

Chưa sửa production parser trong bước lập kế hoạch này. Không commit hoặc reset thay đổi hiện có.


## 2026-07-24 — Full parser/protocol audit and hardening

### Mục tiêu

Audit và refactor toàn bộ chuỗi parse/protocol, không chỉ hai marker `TaskUpdate`/`Read`: inbound Anthropic request, mapper, sync JSON, SSE, native tool calls, DSML, compatibility marker, JSON repair, search interception, retry, Anthropic output và Claude Code tool-result continuation.

Kế hoạch và pipeline map:

```text
docs/architecture/PARSER_FULL_AUDIT_PLAN_20260724.md
docs/architecture/PARSER_PIPELINE_MAP_20260724.md
```

Báo cáo đầy đủ:

```text
artifacts/parser-full-audit-20260724/REPORT.md
```

### Các lỗi logic đã sửa

- Xóa JSON-array sentinel dùng để biểu diễn batch; legitimate array input không còn bị split thành nhiều tool call.
- Compatibility parser trả structured calls và parse batch atomically.
- Khóa invariant chống duplicate side effect: đã emit `tool_use` thì không retry/replay toàn upstream turn.
- Marker compatibility và DSML trong fenced code, inline code, JSON string, quote hoặc escaped text không được thực thi.
- Thêm malformed-marker resynchronization; JSON repair không được nuốt marker hợp lệ phía sau.
- Batch có một item lỗi emit zero calls.
- Search batch ở compatibility/native/DSML bị reject rõ ràng, không silently drop invocation.
- Unknown tool không còn biến thành visible final text hoặc bị sync bỏ qua âm thầm.
- Native streaming tool fragments được buffer và validate atomically trước khi emit.
- Strict JSON được ưu tiên; malformed recovery chỉ được chấp nhận khi còn đúng một semantic interpretation.
- Giới hạn parser: 64 KiB argument sequence, 32 calls/marker batch, 128 compatibility calls/response.
- Prefix buffering chỉ giữ shorthand khi partial tool name khớp tool thật trong payload.
- Thêm semantic validation cho inbound `/v1/messages`: roles, block fields, tool schemas/names, tool history, tool choice và sampling limits.

### File chính đã sửa

```text
src/handlers/messages.rs
src/opencode/forward/mod.rs
src/opencode/forward/common.rs
src/opencode/forward/sync.rs
src/opencode/forward/stream/context.rs
src/opencode/forward/stream/execute.rs
src/opencode/forward/stream/tests.rs
src/opencode/sanitize.rs
tests/protocol_conformance.rs
```

Fixture mới:

```text
tests/fixtures/compat_markers.json
```

### Verification

PASS:

```text
git diff --check
cargo fmt --check
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked --bins
```

Test cuối:

```text
63 parser/stream tests passed
3 parser invariant/property tests passed
4 inbound request validation tests passed
398 library unit tests passed
87 fast tests passed
18 integration tests passed
2 parser fuzz tests passed
16 protocol conformance tests passed
1 WARP identity system test ignored by design
0 failures
```

### E2E HTTP/SSE

Fake upstream + release bridge thật ở process riêng:

```text
TaskUpdate batch: 2 tool_use
Fenced marker example: 0 tool_use
Malformed -> valid resync: 1 tool_use
Malformed native retry: 2 upstream attempts, 1 emitted tool_use
Raw marker leak: 0
```

Artifacts:

```text
artifacts/parser-full-audit-20260724/e2e/http-summary.json
artifacts/parser-full-audit-20260724/e2e/*.sse
```

### E2E Claude Code CLI

Fake upstream controlled session:

```text
exit code: 0
model turns: 2
unique Read calls: 1
unique tool results: 1
final result: CLI_PARSER_E2E_OK
raw marker leak: 0
```

Real service smoke:

```text
Basic result: OK
Tool: Read Cargo.toml
Unique tool IDs: 1
Unique tool results: 1
Final result: TOOL_READ_OK
Raw marker leak: 0
```

Lưu ý: Claude Code `stream-json` có thể phát cùng một completed tool block ở nhiều event representation, nhưng cùng một tool ID; đây không phải duplicate execution.

### Service cuối

```text
Endpoint: http://127.0.0.1:4000
Dashboard: http://127.0.0.1:4000/dashboard
PID: 3110534
Managed: true
Model: opencode/deepseek-v4-flash-free
Health: {"status":"ok","version":"0.5.0"}
Executable: /home/light/GitHub/opencode2claude/target/release/opencode2api-serve
Egress: direct
```

Proxy containers:

```text
40001 primary healthy
40002 primary healthy
40003 primary healthy
40004 standby offline
40005 standby offline
```

Proxy mode không được giữ làm route production vì strict exit-identity verification hiện chưa tạo được eligible route dù ba primary container đang chạy. Service được trả về `direct` để Claude Code hoạt động ổn định. Đây là vấn đề vận hành proxy/identity riêng, không phải parser.

### An toàn working tree

- Không reset/checkout/discard thay đổi cũ.
- Không xóa artifact không rõ nguồn gốc.
- Không ghi secret vào worklog hoặc report.
- Chưa tạo commit.


## 2026-07-24 — Parser and streaming code optimization

### Plan

Created `docs/architecture/CODE_OPTIMIZATION_PLAN_20260724.md` after reviewing the parser hot paths and current guidance for `BytesMut`, `memchr`, Tokio tracing, and Rust release profiles.

### Implemented

- Replaced per-line `Vec::drain(...).collect()` SSE extraction with `BytesMut::split_to`.
- Replaced scalar newline search with `memchr`.
- Corrected `max_sse_line_bytes` enforcement from aggregate network-chunk size to logical SSE-line size.
- Replaced lowercase-allocation tool lookup with `eq_ignore_ascii_case`.
- Replaced compatibility bracket candidate scans with `memchr_iter`.
- Removed repeated trim scans and avoidable attribute allocation in the sanitizer.
- Added `examples/parser_hotpath_bench.rs` and regression tests.

### Benchmark

Five release runs:

```text
SSE line extraction: 102.21x–105.79x
Tool lookup:          2.52x–2.55x
Marker bracket scan:  1.36x–1.43x
```

Detailed output: `artifacts/code-optimization-20260724/benchmark.txt`.

### Verification

```text
401 unit tests passed
87 fast tests passed
18 integration tests passed
2 parser fuzz tests passed
17 protocol tests passed
0 failures
clippy -D warnings passed
release build passed
```

### Deployment

```text
PID: 3541225
Endpoint: http://127.0.0.1:4000
Health: ok
Executable: /home/light/GitHub/opencode2claude/target/release/opencode2api-serve
Egress: direct
HTTP smoke: OPTIMIZED_OK
Claude Code smoke: OPTIMIZED_CLI_OK
```

No commit was created and no pre-existing working-tree changes were discarded.


## 2026-07-24 — Real Claude Code SOC project and synthetic attack–defense arena

### Manual objective

Used Claude Code CLI through the real bridge on port 4000 to build a substantial defensive SOC project, then ran a local-only synthetic attacker/defender exercise while recording every stream/tool event.

### Critical native tool duplicate-execution bug

During the first large project attempt, one native tool ID produced two tool results: the first succeeded and the second reported the file already existed.

Raw SSE inspection proved the bridge emitted two `content_block_stop` events for the same native tool block. `finalize_pending_native_tool_calls` emitted a stop without removing the tool from `SseBlockTracker`; stream finalization then closed it again.

Fix:

- `src/opencode/forward/stream/context.rs` now uses `tracker.close_tool_use(source_index)` before emitting the stop.
- `tests/protocol_conformance.rs` now asserts that a fragmented native tool call has exactly one `content_block_stop`.

Real Claude Code exact-once smoke after the fix:

```text
unique tool IDs: 1
unique tool results: 1
tool block starts: 1
tool block stops for that index: 1
raw marker leak: 0
final result: EXACT_ONCE_OK
created file: alpha\n
```

### Bridge quality gate

```text
git diff --check: pass
cargo fmt --check: pass
cargo check --locked: pass
cargo clippy --locked --all-targets -- -D warnings: pass
63 parser/stream tests: pass
401 unit/library tests: pass
87 fast tests: pass
18 integration tests: pass
2 parser fuzz tests: pass
17 protocol tests: pass
release build: pass
```

### SOC Workflow Guard

Project:

```text
/home/light/Workspace/Project/soc-workflow-guard-20260724
```

Current size:

```text
62 meaningful files
24 Python source files
14 test files
106 test methods
```

Independent validation:

```text
compileall: pass
106/106 tests: pass
full tests with ResourceWarning promoted to error: pass
fresh baseline pipeline: pass
```

The first independent test run exposed SQLite connection ResourceWarnings. A dedicated Claude Code repair added explicit/idempotent close handling, context-manager support, CLI/API lifecycle cleanup and test cleanup.

### Playbook template bug

Round 1 exposed a second real logic bug: an approved playbook reported success while creating `isolations/{{hostname}}.json` and blocklisting literal `{{hostname}}`.

Current project fix:

- recursive deterministic template rendering;
- explicit target precedence;
- approval target recovery from SQLite;
- alert ID rendering;
- fail-closed live execution for unresolved placeholders;
- rollback rendering/fail-closed checks;
- focused regression tests.

Independent live verification:

```text
approval target: webapp.arena.local
isolation artifact: isolations/webapp.arena.local.json
blocklist: webapp.arena.local
ticket contains: ALERT-TEMPLATE-001
literal {{hostname}} artifacts: 0
execution status: success
```

### Synthetic attack–defense arena

Arena:

```text
/home/light/Workspace/soc-attack-defense-arena-20260724
```

Round 1 attacker:

```text
89 total events
50 malicious
39 benign/decoy
6 expected techniques
4 stages
validation: pass
20 model turns
19 tool IDs / 19 results
0 duplicate tool IDs/results
0 raw marker leaks
```

Round 1 defender:

```text
89 ingested events
3 alerts
2 critical alerts
1 case
1 approved approval
2 successful execution records (dry-run plus approved live execution)
12 audit entries
61 model turns
60 tool IDs / 60 results
0 duplicate tool IDs/results
0 raw marker leaks
```

Referee score:

```text
Attacker: 83
Defender: 60
Engine detection coverage: 33.33%
Detected: T1048, T1110.003
Missed: T1059, T1068, T1078, T1110
```

Important audit findings:

- Defender report manually claimed six detected techniques, but SQLite alert records contained only two; referee correctly used SQLite.
- One internal-source password-spray alert appears to be a false positive.
- The original round-1 platform preserved the literal-template bug for reproducibility.
- `platform_round2/` contains the fixed project and passes 106 tests with ResourceWarning as errors.
- An attacker Bash path typo `/attouter/...` was already present in raw `input_json_delta`; it was a model typo, not parser mutation.
- Unsupported Edit parameters and wrong Read paths returned explicit errors; no silent success was accepted.
- A host foreground timeout started multiple concurrent template-repair sessions. All were terminated, and only independently tested filesystem changes were retained. This was orchestration behavior, not bridge marker parsing.

Detailed report:

```text
artifacts/manual-claude-soc-20260724/MANUAL_CLAUDE_SOC_ARENA_REPORT.md
```

### Final service snapshot

```text
Endpoint: http://127.0.0.1:4000
Dashboard: http://127.0.0.1:4000/dashboard
PID: 4034475
Managed: true
Model: opencode/deepseek-v4-flash-free
Health: ok
Executable: /home/light/GitHub/opencode2claude/target/release/opencode2api-serve
Egress: direct
```

Proxy containers:

```text
40001 primary healthy
40002 primary healthy
40003 primary healthy
40004 standby offline
40005 standby offline
```

No commit was created. No pre-existing working-tree changes were reset or discarded. Fake local fixture servers were stopped and ports 4930/4931 were released.


## 2026-07-24 — Manual Claude Code proxy rate-limit and automatic recovery drill

### Test setup

- Real Claude Code CLI through `http://127.0.0.1:4000`.
- Temporary test mode:
  - `BRIDGE_EGRESS_MODE=proxy`
  - `BRIDGE_ACTIVE_PROXY_COUNT=1`
  - `BRIDGE_REQUIRE_VERIFIED_EXIT_IP=false`
  - health interval 2s
  - restart interval 1s
- Only `opencode-warp-1` / port 40001 was routing-enabled.

### Baseline

- 40001, 40002 and 40003 initially shared exit IP `104.28.211.55` (HKG).
- 40002/40003 were duplicate/spare from the identity layer.
- 40004/40005 containers were offline protected standbys.
- `proxy-health`, `proxy-identity` and `proxy-restart` workers were running.

### Real rate-limit result

One minimal Claude Code invocation generated two initial API requests and both received a real upstream rate limit through 40001.

Observed:

```text
egress circuit opened for rate limit
cooldown_secs=44727
using upstream Retry-After value
```

State:

```text
40001 health=degraded
circuit=open
restart_attempts=0
cooldown about 12.4 hours
```

Metrics:

```text
retry_rate_limit=2
proxy_restart_attempts=0
proxy_restart_successes=0
proxy_restart_failures=0
```

The cooldown decreased normally over time. Historical same-day values (about 77k, 76k and 72k seconds) show this is a real decreasing quota-reset window rather than a Retry-After parser defect.

The current test found an already-active quota limit rather than creating it from zero. A second bridge process on port 4001 had been using the same proxy pool, and historical rate-limit events already existed.

### Rate-limit path defects found

1. After 40001 entered cooldown, retry routing attempted protected offline standbys 40004 and 40005. They generated transport errors and opened protected circuits. Offline protected standbys must not be considered eligible routes.
2. Claude Code continued receiving 502 no-route failures and retried for about 88.9 seconds before `aborted_streaming`. The bridge should expose bounded 429/retry information instead of producing a long 502 retry loop.
3. One minimal Claude Code prompt produced two concurrent initial requests, amplifying pressure near the quota boundary.

### Controlled transport automatic-restart drill

Because a 429 correctly does not recreate a container, a separate managed-primary transport fault was injected by stopping `opencode-warp-1`.

Timeline:

```text
tick 0: container exited, pool still falsely healthy
 tick 1: two request failures -> unhealthy/open, failure_count=2, active_requests=2
 tick 4: leases drained
 tick 5: container recreated, new ID, recovering/half_open, restart_attempts=1
 tick 6: healthy/closed
 tick 8: new verified exit IP 104.28.243.56 (HKG)
 tick 10: success_count=2
 tick 11: active_requests=0
```

Claude Code completed successfully:

```text
AUTO_RESTART_OK
exit code 0
about 11.2 seconds
```

Metrics:

```text
retry_transport=4
proxy_restart_attempts=1
proxy_restart_successes=1
proxy_restart_failures=0
responses_5xx=0
```

This confirms the transport recovery path works end-to-end: failure threshold, circuit opening, lease drain, queue processing, recreate, SOCKS verification, identity refresh and request continuation.

### Remaining recovery issues

- An idle stopped container remains marked healthy until traffic fails; no proactive Docker/healthy-node reconciliation.
- `BRIDGE_MAX_PROXY_RESTART_ATTEMPTS` is loaded but worker logic still uses hard-coded 3.
- Initial primary exits were identical, so extra primary containers did not provide independent quota failover.

### Restoration

Production was restored to:

```text
BRIDGE_EGRESS_MODE=direct
PID=392797
health=ok
```

Final Claude Code smoke:

```text
DIRECT_RESTORED_OK
is_error=false
```

Full report:

```text
artifacts/proxy-rate-limit-manual-20260724/REPORT.md
```

No commit was created and no unrelated working-tree changes were reset or discarded.


## 2026-07-24 — Proxy rotation and proactive dead-WARP recovery completed

This entry supersedes the earlier same-day proxy notes that listed proactive dead-container detection, hard-coded restart attempts and independent exit handling as unresolved.

### Fixes completed

- Added explicit rate-limit recovery state, quota deadline preservation and exit-IP quarantine.
- Rate-limited managed nodes now use Docker restart first, with recreate only as a bounded fallback.
- A recovered rate-limit node must present an acceptable verified exit before returning to routing.
- Offline/unverified protected standby nodes are never normal application routes.
- Identity monitoring is observational during rate-limit and transport recovery; it cannot prematurely close the circuit.
- Removed identity-monitor restart enqueueing and stale queue races.
- Exhausted rate-limit recovery remains circuit-open until the original quota deadline.
- Restart attempt limits now come from runtime config.
- Added proactive TCP monitoring for every eligible managed primary, including currently healthy nodes.
- Two consecutive TCP failures move a dead primary through degraded -> unhealthy/open -> automatic restart.
- Protected standby, rate-limit recovery, in-flight restart and exhausted nodes are excluded from proactive probes.
- Successful TCP/identity recovery clears transport state without fabricating application request successes.

### Manual Claude Code rate-limit evidence

- Real Claude Code result: `STALE_QUEUE_FIXED_OK`.
- The rate-limited exit was quarantined and WARP was restarted.
- The recovered node remained healthy after success; container state and restart metrics did not change again.
- Artifact: `artifacts/proxy-rotation-fix-20260724/final-stale-fixed-transitions.json`.

### Manual dead-primary evidence

Claude warm-up proved routing through 40001:

```text
DEAD_FINAL_WARMUP_OK
40001 success_count: 0 -> 2
```

After manually stopping `opencode-warp-1`, the final timeline showed:

```text
~0.5s  degraded, transport failure 1
~1.6s  unhealthy/open, transport failure 2
~2.4s  recovering/half_open, restart attempt 1
~8.6s  healthy/closed with new verified exit
```

Container identity:

```text
ID before: 0f531835745fb09f2064c4c341e13ee835152f0fad83a2d883c2e3e9c3674b30
ID after:  0f531835745fb09f2064c4c341e13ee835152f0fad83a2d883c2e3e9c3674b30
StartedAt changed: yes
old exit: 104.28.243.56
new exit: 104.28.211.56
```

Recovery metrics:

```text
proxy_restart_attempts=1
proxy_restart_successes=1
proxy_restart_failures=0
```

Final Claude Code smoke after recovery:

```text
DEAD_PROXY_RECOVERY_FINAL_OK
exit=0
duration_ms=4615
```

### Final verification

```text
git diff --check                                      PASS
cargo fmt --check                                     PASS
cargo clippy --locked --all-targets -- -D warnings   PASS
cargo test --locked --lib                            418 passed
cargo test --locked --test fast                       87 passed
cargo test --locked --test integration                18 passed
cargo test --locked --test parser_fuzz_smoke           2 passed
cargo test --locked --test protocol_conformance       17 passed
cargo build --release --locked --bins                 PASS
```

Total listed tests: 542 passed, 0 failed.

Full report:

```text
artifacts/proxy-rotation-fix-20260724/REPORT.md
```

No commit was created. No unrelated working-tree changes were reset or discarded.


## 2026-07-24 — Control Deck V2 final screenshot review and density polish

### Mục tiêu

Hoàn thiện vòng đại trùng tu frontend Control Deck V2 bằng screenshot live và review trực quan thực tế; không kết luận chỉ từ code. Giữ nguyên DOM, route và API backend.

### Screenshot live đã chụp và review

Bộ before/working captures:

```text
artifacts/frontend-v2-review/desktop-*.png
artifacts/frontend-v2-review/tablet-*.png
artifacts/frontend-v2-review/mobile-*.png
artifacts/frontend-v2-review/narrow-*.png
```

Bao phủ:

```text
Dashboard
API Keys
Models
History
System
Create API Key
Verify API Keys
History Settings
Logs
Diagnostics
Configuration
Confirm dialog
Desktop 1440x900
Tablet 1024x768
Mobile 390x844
Mobile narrow 320x800
```

Bộ after/final:

```text
artifacts/frontend-v2-review/after-v3-*.png
artifacts/frontend-v2-review/final-v5-desktop-dashboard.png
artifacts/frontend-v2-review/final-v5-mobile-dashboard.png
artifacts/frontend-v2-review/final-v5-mobile-models.png
artifacts/frontend-v2-review/final-v5-tablet-configuration.png
artifacts/frontend-v2-review/final-v5-mobile-create-api-key.png
artifacts/frontend-v2-review/final-v5/final-review-board.webp
artifacts/frontend-v2-review/comparisons-v3/desktop-before-after.webp
artifacts/frontend-v2-review/comparisons-v3/mobile-before-after.webp
```

### Lỗi visual phát hiện từ ảnh thật và metrics

- Desktop còn khoảng trống không đều; topbar, metric card và Quick Actions cao hơn cần thiết.
- Glow, grid/scanline và viền năng lượng hơi mạnh ở màn hình nhiều dữ liệu.
- Mobile bị kéo dài bởi min-height 320-600px trên API overview, Model Tester, History detail và Proxy Pool.
- Logs/Configuration tablet gần chạm mép viewport; cần khoảng thở ổn định.
- Badge trạng thái cảnh báo EN/VI bị tràn chữ ở 390px và 320px.
- Modal entrance dùng scale(.975), làm touch target 40px bị Playwright đo thành 39px trong frame animation.

### File presentation đã sửa

```text
src/webui/control-deck-v2.css
```

### Thay đổi visual chính

- Thu topbar 76px xuống 70px desktop và 60px mobile.
- Giảm padding/content gutter, section gap, card shadow và ambient decoration.
- Thu metric cards, icons, table rows, model rows, maintenance actions và Quick Actions.
- Giảm mascot/operator opacity và kích thước để không cạnh tranh với dữ liệu.
- Giảm min-height workspace mobile nhưng vẫn giữ internal scroll và touch target.
- Modal desktop/tablet được right-size theo nội dung; tablet có tối thiểu 16px viewport margin.
- Giảm modal glow/inner border; giữ border-energy animation nhẹ.
- Giữ control/icon button modal tối thiểu 40px.
- Mở rộng status badge mobile lên 142px và giảm font nhẹ để EN/VI không overflow.
- Đổi modal entrance sang translate + opacity, không scale, để geometry ổn định từ frame đầu.

### Kết quả before/after đo được

```text
Mobile main scrollHeight:
Dashboard 1153 -> 1087 (-66px)
API       1937 -> 1887 (-50px)
Models    1848 -> 1800 (-48px)
History   1745 -> 1681 (-64px)
System    1790 -> 1659 (-131px)

Narrow 320px:
Dashboard 1157 -> 1087 (-70px)
Models    1925 -> 1877 (-48px)
History   1763 -> 1698 (-65px)
System    1840 -> 1722 (-118px)
```

Final screenshots:

```text
No horizontal overflow
No console errors
No page errors
Desktop dashboard stays in one viewport
Mobile modal stays inside viewport
Tablet Configuration: x=72, y=16, width=880, height=736
Mobile Create API Key: x=16, width=358, bottom=699
```

### UI verification cuối

```text
mecha-ui          PASS
functionality     PASS
scroll-invariants PASS
single-viewport   PASS
consistency-en    PASS
consistency-vi    PASS
login             PASS
modals            PASS
api-key-batch     PASS
tester-controls   PASS
```

Manifest sạch:

```text
artifacts/frontend-v2-review/final-v5/ui-status.tsv
```

### Quality gates

```text
git diff --check                                   PASS
node --check src/webui/app.js                      PASS
node --check src/webui/mecha-components.js         PASS
node --check src/webui/control-deck-v2.js           PASS
cargo fmt --check                                  PASS
cargo check --locked                               PASS
cargo clippy --locked --all-targets -- -D warnings PASS
cargo build --release --locked --bins              PASS
cargo test --locked                                PASS

542 passed
0 failed
1 ignored (requires local WARP proxies and Internet)
```

### Service snapshot cuối

```text
PID:       455214
Port:      4000
Status:    running
Managed:   true
Model:     opencode/deepseek-v4-flash-free
Dashboard: http://127.0.0.1:4000/dashboard
```

### Lưu ý working tree

- Không reset hoặc checkout các thay đổi hiện có.
- Trong lúc build có thay đổi song song tại `src/proxy_pool/maintenance.rs`; không ghi đè. Build/test cuối được chạy trên trạng thái mới nhất và PASS.
- Chưa tạo commit vì user chưa yêu cầu.


### Final deployed proxy runtime

The final release binary was restarted in verified proxy mode:

```text
PID=505459
active primary count=2
minimum unique exits=2
40001 active 104.28.211.57 healthy/closed
40002 active 104.28.243.56 healthy/closed
40003 spare  104.28.211.55 healthy/closed
health={"status":"ok","version":"0.5.0"}
```

Final real Claude Code runtime smoke:

```text
FINAL_PROXY_RUNTIME_OK
exit=0
```

The request survived a new rate limit on 40001 by continuing through 40002. Automatic recovery exhausted its bounded budget when WARP kept returning the quarantined IP; the circuit remained open as designed. Manual WARP off/on obtained the fresh exit `104.28.211.57`, and a final gateway restart loaded a clean three-exit topology.


## 2026-07-25 — Claude Code tool protocol reverse and full marker-parser hardening

### Mục tiêu

Reverse toàn bộ đường đi của tool call giữa upstream model, OpenCode2API bridge, Anthropic Messages/SSE, Claude Code CLI, client-side/deferred tools, tool result và recap; sửa tổng quát các marker dạng `Requesting/Creating`, không hard-code riêng `CronCreate`.

### Claude Code reverse result

Executable thực:

```text
/home/light/.local/bin/claude
  -> /home/light/.local/share/claude/versions/2.1.219
```

Loại file:

```text
ELF 64-bit x86-64
approximately 263 MiB
not stripped
contains an embedded Bun JavaScript bundle in the .bun section
```

Static bundle inspection và dynamic tracing xác nhận:

- Claude Code chỉ thực thi structured `tool_use`; raw assistant marker text không tự chạy.
- `CronCreate`, `CronDelete`, `CronList`, tool-result validation, deferred-tool resume, permission/approval, tool-use summary và recap logic đều có trong embedded bundle.
- `CronCreate`, `CronDelete`, `CronList` và `TaskList` được xác nhận có deferred behavior.
- Away recap là model call riêng với tool use bị deny; recap không phải bằng chứng tool đã chạy.
- Không cần IDA Pro vì relevant JavaScript/control flow đọc được qua `.bun` strings/windows và dynamic request tracing.

Real Claude request capture tìm được 27 non-interactive built-in tools:

```text
Agent
Bash
CronCreate
CronDelete
CronList
DesignSync
Edit
EnterWorktree
ExitWorktree
Monitor
NotebookEdit
PushNotification
Read
ReportFindings
ScheduleWakeup
SendMessage
Skill
TaskCreate
TaskGet
TaskList
TaskOutput
TaskStop
TaskUpdate
WebFetch
WebSearch
Workflow
Write
```

`AskUserQuestion` được xác nhận bằng interactive TUI tool-use/result thật. `approval` thuộc permission/control-plane flow, không phải normal tool trong non-interactive registry. MCP tools là dynamic registry entries từ `tools[]`.

### Root causes

- Parser chưa nhận dạng generic direct-colon marker:
  `[Requesting ToolName: {...}]`.
- Parser chưa classify generic JSON Creating marker và non-JSON Creating formatter text.
- Sync path chỉ parse marker trong visible content, bỏ marker trong `reasoning_content`.
- Chưa dedupe semantic invocation giữa native, DSML, reasoning và visible marker.
- Raw marker có thể đi vào client output/history.
- Model có thể nói “đã tạo/đã chạy/scheduled successfully” trước khi có `tool_result`.
- Unknown/client-side tool intent có thể leak hoặc bị drop nếu không khớp shorthand grammar.

### Files sửa/tạo

```text
src/opencode/forward/common.rs
src/opencode/forward/stream/context.rs
src/opencode/forward/stream/tests.rs
src/opencode/forward/sync.rs
tests/protocol_conformance.rs
tests/fixtures/claude_tool_markers.json
scripts/reverse_claude_tool_protocol.py
scripts/manual_verify_claude_tool_protocol.py
docs/architecture/CLAUDE_CODE_TOOL_PROTOCOL_REVERSE.md
artifacts/claude-tool-protocol-reverse/REPORT.md
artifacts/claude-tool-protocol-reverse/
```

### Parser/protocol fixes

- Thêm generic grammar:
  - `[Requesting ToolName: complete JSON]`
  - `[Creating ToolName: complete JSON]`
- Tool name luôn được validate case-insensitively với tool registry thực của request.
- Non-JSON formatter marker fail closed, không đoán arguments, không leak, bounded correction retry.
- Parse và redact marker trong cả reasoning lẫn visible text ở stream và sync.
- Semantic fingerprint theo normalized tool name + normalized JSON arguments để dedupe native/DSML/reasoning/text.
- Unknown tool không emit, không leak, không silently drop; bounded correction hoặc explicit error.
- Suppress unverified success narration trên tool-use turn; model chỉ có thể báo success sau result turn.
- Giữ invariant native-tool fix trước đây: một tool block có đúng một start và một stop.

### Regression coverage

```text
CronCreate after prose
reasoning marker
EOF marker
all UTF-8 chunk boundaries
byte-fragmented SSE
Unicode prompt
recurring=true
multiple/duplicate markers
valid -> malformed
malformed -> valid
fenced/inline/quote/escaped inert examples
unknown tool no leak
client-side/deferred tool
non-JSON Creating fail-closed
false-success suppression
native/DSML/reasoning/text dedupe
```

### Fake-server dynamic reverse

```text
scripts/reverse_claude_tool_protocol.py
artifacts/claude-tool-protocol-reverse/dynamic/summary.json
```

Results:

```text
Fake Anthropic native tool_use -> Claude Code:
4 tool uses, 4 results, create/list/delete/list PASS

Fake Anthropic raw text marker -> Claude Code:
0 tool uses, 0 results, raw marker visible/inert

Fake OpenAI marker -> bridge -> Claude Code:
4 tool uses, 4 results, 0 raw marker, cleanup PASS
```

### Real Claude Code release verification through port 4000

```text
scripts/manual_verify_claude_tool_protocol.py
artifacts/claude-tool-protocol-reverse/live/summary.json
artifacts/claude-tool-protocol-reverse/live/tool-telemetry.json
```

Final matrix:

```text
Cron lifecycle:       4 tool uses, 4 results, 0 duplicate IDs, 0 raw markers
Task lifecycle:       4 tool uses, 4 results, 0 duplicate IDs, 0 raw markers
Read/Edit/Bash:       4 tool uses, 4 results, 0 duplicate IDs, 0 raw markers
MCP echo:             1 tool use,  1 result,  0 duplicate IDs, 0 raw markers
Approval/denial:      1 tool use,  1 error result, no side effect, 0 raw markers
AskUserQuestion:      1 tool use,  1 result, YES_VERIFY, 0 raw markers
```

Cron exact input:

```json
{
  "cron": "*/30 * * * *",
  "prompt": "write CRON_PARSE_VERIFY_OK to a local test record",
  "recurring": true
}
```

Cron runtime evidence:

```text
job ID: cdc79c70
before cleanup: cdc79c70 — Every 30 minutes (recurring) [session-only]: write CRON_PARSE_VERIFY_OK to a local test record
delete result: Cancelled job cdc79c70.
after cleanup: No scheduled jobs.
```

### Quality gates

```text
git diff --check                                   PASS
cargo fmt --check                                  PASS
cargo check --locked                               PASS
cargo clippy --locked --all-targets -- -D warnings PASS
cargo test --locked                                PASS
cargo build --release --locked --bins              PASS

550 passed
0 failed
1 ignored (requires local WARP proxies and Internet)
```

### Service snapshot

```text
PID:       518622
Port:      4000
Status:    running
Managed:   true
Model:     opencode/deepseek-v4-flash-free
Dashboard: http://127.0.0.1:4000/dashboard
```

### Working-tree safety

- Không reset, checkout, clean hoặc discard thay đổi cũ.
- Không commit vì user chưa yêu cầu.
- Chỉ sửa các file liên quan đến tool protocol/parser, tests, scripts, docs và artifacts.

## 2026-07-29 — Claude Code 2.1.220 model/parser boundary audit

### Objective

Determine objectively whether each raw tool marker or missing execution came from the model, bridge parser/mapper, Claude Code runtime, verification harness, or WARP egress.

### Fixed

- Preserved reasoning-before-text order when reasoning contains an unverified-success phrase.
- Replaced free-model legacy marker history with native OpenAI `tool_calls` and `role=tool` history.
- Normalized inherited `socks5://` URLs to `socks5h://` so WARP identity probes use remote DNS and reach consensus.
- Made Claude protocol verification scripts compatible with Claude Code 2.1.220 by detecting optional CLI flags.

### Objective classifications

- Old raw marker on 0.4.2: parser capability gap.
- Agent killed port 4021: model-agent orchestration error.
- Cron list in a new session: harness design error.
- Inline marker code example: model formatting deviation; parser correctly kept it inert.
- Empty top-level final after Bash: bridge reasoning-buffer ordering bug.
- Repeated legacy markers in later turns: mapper architecture bug.
- Post-deploy 502: local-DNS SOCKS configuration caused IPv6/IPv4 identity mismatch.

### Verification

```text
Deployed service: http://127.0.0.1:4000
PID: 819143
Version: 0.5.0
Production matrix: PASS
Quality gates: PASS
Tests: 554 passed, 0 failed, 1 ignored
```

Evidence:

```text
artifacts/claude-tool-protocol-reverse/model-vs-parser/REPORT.md
artifacts/claude-tool-protocol-reverse/model-vs-parser/deployed-4000-final-matrix/summary.json
artifacts/claude-tool-protocol-reverse/model-vs-parser/warp-identity-probes/results.json
```

No reset, checkout, clean, commit, or unrelated service/tunnel restart was performed.


## 2026-08-02 — Parser/tool-call status re-verification

### Objective

Re-check the deployed `opencode2claude` parser against the historical acceptance criteria: no raw `[Requesting ...]`/`[Creating ...]` leakage, one result per tool use, no duplicate IDs, correct deferred/client-side tool lifecycle, and real Claude Code CLI verification rather than report-only conclusions.

### Deployed service

```text
Version:   0.5.0
PID:       11175
Endpoint:  http://127.0.0.1:4000
Model:     opencode/deepseek-v4-flash-free
Managed:   true
```

The release binaries were built on 2026-07-30 after the latest parser source changes inspected in `common.rs`, `sync.rs`, `stream/context.rs`, and `protocol_conformance.rs`; no evidence of a newer parser source being served by an older binary was found.

### Verification executed

```text
Claude Code: 2.1.220
cargo test --locked --test protocol_conformance
  21 passed, 0 failed

cargo test --locked
  563 passed, 0 failed, 1 ignored
  ignored test requires local WARP SOCKS proxies and Internet

TOOL_PROTOCOL_OUT=artifacts/claude-tool-protocol-reverse/live-20260802 \
TOOL_PROTOCOL_BASE_URL=http://127.0.0.1:4000 \
python3 scripts/manual_verify_claude_tool_protocol.py
  PASS
```

### Live Claude Code matrix

```text
CronCreate/CronList/CronDelete/CronList: 4 uses, 4 results, exact recurring input, cleanup verified
TaskCreate/TaskUpdate/TaskUpdate/TaskList: 4 uses, 4 results
Read/Edit/Bash/Read: 4 uses, 4 results, side effect verified
MCP echo: 1 use, 1 result
Approval denial: 1 use, 1 error result, side effect blocked
AskUserQuestion: 1 use, 1 result, interactive answer verified

All groups:
- zero duplicate tool IDs
- zero unmatched tool results
- zero raw `[Requesting` markers
- zero raw `[Creating` markers
```

Evidence:

```text
artifacts/claude-tool-protocol-reverse/live-20260802/summary.json
artifacts/claude-tool-protocol-reverse/live-20260802/raw/
artifacts/claude-tool-protocol-reverse/live-20260802/profiles/
```

### History/database check since current service startup

```text
70 requests inspected
34 tool calls recorded
47 stored response/reasoning rows inspected
raw `[Requesting` rows: 0
raw `[Creating` rows: 0
```

### Conclusion

The parser/tool-call boundary is currently stable for the historical failure matrix and the real Claude Code 2.1.220 lifecycle matrix. No parser regression was reproduced.

This does not mean the whole bridge is error-free. Separate current runtime issues were observed:

```text
7 failed requests: empty_upstream_stream
5 failed requests: upstream HTTP 400 after provider retry
1 failed request: no eligible proxy egress route
```

Runtime logs attribute the HTTP 400/empty-stream cases to upstream DeepSeek/DFLASH grammar-constrained speculative decoding incompatibility, and there are repeated WARP identity-probe failures for standby nodes. These are upstream/egress reliability issues, not raw-marker/tool parser failures.

### Working-tree safety

- No reset, checkout, clean, commit, or source edit was performed.
- Only verification artifacts and this worklog entry were added.

## 2026-08-02 — DFLASH HTTP 400/502 compatibility fix and restart

### Objective

Eliminate repeated Claude Code failures reported as `502 Upstream API error: Upstream returned HTTP 400 after 1 provider retry attempt(s)` and restart the deployed bridge only after reproducing and verifying the exact failing payloads.

### Root causes reproduced from History

1. `deepseek-v4-flash-free` rejected Claude structured-output requests because any forwarded `response_format` enabled grammar-constrained decoding. Upstream message: `DFLASH speculative decoding does not support grammar-constrained decoding`.
2. DFLASH synchronous requests with thinking enabled rejected long tool histories when an historical assistant message contained `tool_calls` but no non-empty `reasoning_content`.

Direct payload experiments established the second invariant:

```text
original long sync thinking request                  -> HTTP 502
same request with missing tool reasoning synthesized -> HTTP 200
same request with thinking disabled                  -> HTTP 200
strip reasoning only while leaving thinking enabled  -> HTTP 502
```

### Files changed

```text
src/opencode/mapper/policy.rs
src/opencode/mapper/request.rs
src/opencode/mapper/tests.rs
src/opencode/retry/execute.rs
```

### Implementation

- Do not forward `response_format` to `deepseek-v4-flash-free`.
- For DFLASH sync + thinking requests, synthesize `reasoning_content: "Tool call continuation."` only on historical assistant messages that contain tool calls but lack reasoning.
- Preserve genuine historical reasoning.
- On provider HTTP 400, detect reasoning/grammar compatibility errors and perform a bounded semantic repair before the ordinary provider retry:
  - repair missing tool-call reasoning;
  - if that cannot repair the payload, disable incompatible reasoning controls;
  - remove unsupported response format.
- The same defensive retry policy covers Anthropic-mapped and OpenAI passthrough requests.

### Verification

```text
git diff --check                                      PASS
cargo fmt --check                                     PASS
cargo clippy --locked --all-targets -- -D warnings    PASS
cargo build --release --locked --bins                 PASS
cargo test --locked                                   PASS

440 unit tests passed
87 fast tests passed
18 integration tests passed
2 parser-fuzz tests passed
21 protocol-conformance tests passed
0 failed
1 ignored (requires real WARP identity environment)
```

Manual deployed replay after restart:

```text
structured-output payload formerly returning 502 -> HTTP 200, valid JSON text
exact 497812-byte historical failing request       -> HTTP 200, thinking/text/tool call
```

Claude Code 2.1.220 lifecycle matrix after the fix preserved one result per tool use, zero duplicate IDs, zero unmatched results, and zero raw `[Requesting`/`[Creating` markers. The aggregate script reported false only because Claude issued an additional harmless `ls` Bash before the prescribed Bash while the harness expected exactly one Bash; this was behavioral variance, not a parser or lifecycle failure.

### Deployed service

```text
Endpoint: http://127.0.0.1:4000
PID:      422998
Model:    opencode/deepseek-v4-flash-free
Managed:  true
Primary WARP nodes 40001-40003: healthy
```

No new HTTP 400/502, grammar, malformed-marker, or raw-marker errors were observed after the final restart and replay.

### Repository state

The four source/test files above remain uncommitted. No reset, clean, checkout, or commit was performed.


## 2026-08-02 — Host WARP isolation, managed proxy rotation, Claude Code 2.1.220 compatibility, and real ANSER workflow verification

### Objective

Fix the production runtime so `opencode2api` can never invoke the machine-wide `warp-cli`, repair managed-proxy recovery so a rate-limited WARP registration is actually rotated, restore the `:4000` connection, and then verify the deployed bridge with a real Claude Code 2.1.220 multi-agent workflow against the previously authorized local ANSER pentest workspace.

### Root causes

1. Direct-mode network/rate-limit retry paths called `reconnect_warp()`. The production `CliWarpController` implemented that operation by executing host `warp-cli disconnect` followed by `warp-cli connect`, interrupting unrelated host traffic.
2. Managed rate-limit recovery used `docker restart`, which retained the named WARP registration volume and commonly returned the same rate-limited exit identity.
3. After exhausting three rotation attempts, a rate-limited node stayed dormant until the original provider quota deadline instead of scheduling another bounded rotation attempt.
4. Claude Code 2.1.220 sends `SessionStart hook additional context` as a `messages[].role == "system"` conversation entry. The bridge validator previously allowed only `user`/`assistant`, producing an immediate HTTP 400 before mapping.
5. The protocol-conformance fixture for a post-content stream read error could deliver a visible chunk and terminal error in one poll, making the intended lifecycle assertion nondeterministic.

### Implementation

#### Host WARP safety

- Added a production-safe `DisabledWarpController`.
- `AppState` now uses `DisabledWarpController`; no production path constructs `CliWarpController`.
- Direct network and rate-limit retry paths log the condition but never mutate host WARP.
- Removed the retry module's automatic host-WARP reconnect path; `src/opencode/retry/warp.rs` is intentionally inert documentation.
- Added regression coverage proving production state rejects host WARP reconnect and direct rate limits never call the controller.

#### Managed Docker WARP recovery

- Added `ContainerRuntime::rotate_managed`.
- Docker rotation now removes only the managed primary container, removes its named registration volume, and recreates the canonical container.
- Rate-limit recovery and `proxy purge -y` use true registration rotation instead of `docker restart`/volume reuse.
- Protected standby ports `40004-40005` remain outside destructive lifecycle operations.
- Exhausted rate-limit rotation schedules a short deferred retry while preserving fail-closed identity and duplicate-exit checks.

#### Claude Code 2.1.220 request compatibility

- Validator accepts a `system` message only when every block is text.
- Mapper folds text from role-`system` messages into the top-level system prompt and excludes those messages from OpenAI conversation history.
- Non-text tool/content blocks in a system message remain fail-closed.

#### Stream lifecycle fixture

- The post-content stream-reset fixture now yields across an async boundary between the visible SSE chunk and terminal read error so the test deterministically distinguishes pre-content retry from post-content no-replay behavior.

### Automated verification

Final quality gate on the exact deployed working tree:

```text
cargo fmt --all -- --check                         PASS
cargo test --locked                                PASS
cargo clippy --locked --all-targets -- -D warnings PASS
cargo build --release --locked --bins              PASS

448 unit tests passed
87 fast tests passed
18 integration tests passed
2 parser-fuzz tests passed
23 protocol-conformance tests passed
1 WARP identity system test ignored by design
0 failed
```

New/updated regression tests include:

```text
rotate_managed_replaces_container_and_registration_volume
exhausted_rate_limit_rotation_is_requeued_after_short_delay
direct_rate_limit_never_reconnects_host_warp
production_state_forbids_host_warp_reconnect
accepts_claude_code_system_message_with_text_only
rejects_non_text_blocks_in_system_messages
folds_claude_code_2_1_220_system_message_into_system_prompt
pre_content_stream_read_error_retries_without_duplicate_lifecycle
post_content_stream_read_error_is_not_replayed
```

### Practical host-WARP verification

A real `strace -f -e execve` capture of managed pool rotation showed only:

```text
docker rm -f <managed-primary>
docker volume rm -f <managed-registration-volume>
docker run ...
```

No `warp-cli` exec occurred.

The deployed service was then launched with a sentinel executable named `warp-cli` placed first in its `PATH`. Any attempted host invocation would append a timestamped record and return failure instead of touching host networking. The sentinel log remained absent through:

- managed pool rotation;
- raw Claude Code request replay;
- the complete real Claude Code ANSER workflow;
- provider retries and a rate-limit recovery cycle.

Evidence:

```text
artifacts/manual-host-warp-verify-20260802/proxy-rotate.execve.log
artifacts/manual-host-warp-verify-20260802/bin/warp-cli
artifacts/manual-host-warp-verify-20260802/host-warp-cli-invocations.log  # absent
```

A host process check also found no active `warp-cli` process. Existing unrelated host services and Claude sessions were not stopped.

### Raw Claude Code 2.1.220 replay

A local capture proxy recorded the exact Claude Code request shape with roles `[user, system]`. Replaying that request against the patched release returned HTTP 200 with a complete Anthropic SSE lifecycle and final text `OK`; the former `messages[n].role must be user or assistant` error did not recur.

Evidence:

```text
artifacts/manual-host-warp-verify-20260802/captured-request-latest.json
artifacts/manual-host-warp-verify-20260802/replay.headers.txt
artifacts/manual-host-warp-verify-20260802/replay.body.txt
```

### Real Claude Code multi-agent verification

Claude Code 2.1.220 was run against `http://127.0.0.1:4000` in the authorized workspace `/home/light/Documents/anser_pentest`. It used two background subagents before final conclusions:

1. independent static source/evidence reviewer;
2. dynamic local regression verifier.

The parent did not trust the dynamic verifier's initial session-invalid conclusion. It independently reproduced the cookie behavior, established that `curl -b <raw-value-file>` was the error, confirmed all five sessions authenticate when sent as `session=$(cat file)`, verified the local SQLite canary read-only, and wrote a correction artifact rather than silently rewriting the subagent transcript.

Observed bridge/tool lifecycle for the complete workflow:

```text
134 tool_use blocks
134 tool_result blocks
0 duplicate tool IDs
0 unmatched tool uses
0 orphan tool results
0 raw [Requesting ...] / [Creating ...] markers
```

One local Claude `TaskOutput` call occurred after Claude Code had already emitted `task_notification: completed` and removed that agent ID from its local task registry. It returned `No task found`; all surrounding bridge requests completed HTTP 200, and the parent consumed the already-delivered completion notification. This was classified as a Claude Code local post-completion race, not an `opencode2api` parser/mapping failure.

One provider stream ended with an upstream read error. The bridge recorded it as `upstream_read_error`, emitted no partial tool use, did not replay or duplicate any lifecycle block, and subsequent requests completed normally. This matches the post-content fail-closed/no-replay contract.

Evidence:

```text
artifacts/manual-host-warp-verify-20260802/claude-anser-after-fix.stream.jsonl
artifacts/manual-host-warp-verify-20260802/claude-anser-after-fix.debug.log
artifacts/manual-host-warp-verify-20260802/claude-anser-after-fix.stderr.log
```

### ANSER task outcome independently checked

Claude Code completed successfully and produced/updated only pentest-workspace deliverables, including:

```text
09_retest/20260802_review/dynamic_recheck_20260802.json
09_retest/20260802_review/dynamic_recheck_correction_20260802.json
09_retest/results/pre_fix_recheck_20260802.json
06_reports/final/ANSER_PENTEST_REPORT.md
06_reports/final/ANSER_PENTEST_REPORT.sha256
DELIVERABLES.md
DELIVERABLES.sha256
```

Independent post-run checks confirmed:

- `pre_fix_recheck_20260802.json` contains exactly 9 schema-complete records, F-001 through F-009, all verdict `OPEN`;
- correction file contains five authenticated identities and explicitly supersedes the false session-invalidation inference;
- F-002/F-005/F-006/F-001/Gunicorn/cleanup report passages now match source/evidence;
- Markdown code fences are balanced;
- report, Vietnamese report, and deliverables checksums all verify;
- `/home/light/GitHub/ANSER` source remains unchanged; only the pre-existing untracked `dump.rdb` is present;
- unrelated services and concurrent work under `10_impl_fanout` were not touched.

### Deployed service

```text
Endpoint: http://127.0.0.1:4000
PID:      1698227
Model:    opencode/deepseek-v4-flash-free
Managed:  true
Primary Docker WARP nodes 40001-40003: running
Protected standby nodes 40004-40005: offline/untouched
```

Cloudflare may still assign the same public exit IP after a true registration reset. The runtime therefore retains uniqueness/quarantine checks and deferred retries; it does not falsely claim that volume rotation guarantees a new IP.

### Repository safety

- No reset, checkout, clean, or commit was performed.
- No unrelated Claude session/process was stopped.
- No protected standby was mutated.
- Current source/test changes remain uncommitted pending an explicit user request.

### 2026-08-03 — Fix "incomplete tool request" terminal error: split native/compat search batches

**Mục tiêu**

- Chấm dứt lỗi terminal `The upstream model repeatedly emitted an incomplete tool request` khi DeepSeek free (opencode/deepseek-v4-flash-free) gửi batch nhiều tool call song song trong 1 response (2-4 calls, gồm cả bridge-search tools).
- Đảm bảo bridge chịu được prompt nghiên cứu dài + Subagent fan-out (trước đó fail 16/18 lần).

**Root cause (đã truy vết bằng evidence)**

1. Upstream free-tier **bỏ qua `parallel_tool_calls: false`** — đã chứng minh bằng serialization test (param có trong request) + repro live (vẫn batch calls=4).
2. Stream executor reject toàn bộ batch chứa search tool (`Rejecting native tool batch...`) → `compat_retry_requested` → hết MAX_COMPAT_TOOL_RETRIES=2 → terminal error, kèm stop_reason sai.
3. **Phát hiện quan trọng:** `oc2api server restart` spawn serve là **sibling của controller** (`current_exe().parent()/opencode2api-serve`, supervisor.rs:157-161). Controller chạy từ `~/.local/bin` nên các lần "restart + verify" trước đây (bao gồm cả Fix A) đều chạy binary **stale Jul 29** — kết luận "Fix A ineffective" trước đây dựa trên binary cũ, không phải code mới.

**File đã sửa**

```text
src/opencode/forward/stream/context.rs   # finalize_pending_native_tool_calls (native path)
src/opencode/forward/stream/context.rs   # emit_compat_tool_calls (compat/DSML path)
src/opencode/forward/stream/tests.rs     # 3 test cập nhật theo hành vi mới
src/opencode/types.rs                    # (từ phiên trước) parallel_tool_calls field
src/opencode/mapper/request.rs           # (từ phiên trước) map parallel_tool_calls:false
src/opencode/mapper/tests.rs             # (từ phiên trước) serialization test
```

**Thay đổi kỹ thuật**

- Bỏ reject batch; thay bằng split:
  - **Pure-search batch** (2+ search): giữ nguyên → interception branch bắt call đầu tiên, các call còn lại không vào history (model tự re-issue ở turn sau, mỗi lần 1 call → interception bình thường). Log: `Collapsing native batch of search calls; intercepting the first`.
  - **Mixed batch** (search + non-search): `retain` chỉ giữ non-search → emit thành tool_use bình thường cho client; search calls bị drop. Log: `Dropping search calls from mixed native batch; emitting non-search calls`.
- Không còn đường nào set `compat_retry_requested` từ batch → terminal error không thể xảy ra từ lỗi này.

**Kiểm thử**

```text
cargo test --lib            449 passed, 0 failed
cargo clippy --lib          clean
cargo fmt --check           PASS
```

**Deploy & verify**

- `~/.local/bin` là deployment thật (controller spawn sibling serve). Script atomic `/tmp/restart_bridge3.sh`: stop → backup Jul 29 (`/tmp/oc2api-serve.inst.bak`, `/tmp/oc2api.inst.bak`) → cp binary mới → start → health poll 45s → verify `strings /proc/PID/exe` có marker fix → rollback + direct spawn fallback nếu fail.
- PID hiện tại: 313216, running-marker=1 (fix đang chạy).
- Repro `/tmp/repro_long.json`: `tool_use events: 2`, `terminal error: 0`, `stop_reason: tool_use`; log mới chỉ có `Collapsing native batch` (không còn `Rejecting`).
- Live E2E: subagent research dài (2 WebSearch + WebFetch + synthesis) hoàn thành trong ~24s — lần đầu thành công sau 16/18 lần fail trước đó.

**Giới hạn đã biết**

- Search calls bị drop trong mixed batch có thể không được model re-issue (model-dependent); chưa có cơ chế nudge xuyên turn. Không ảnh hưởng tính đúng đắn protocol — chỉ có thể tốn thêm round-trip.
- Trước đây kết luận "Fix A ineffective" là do binary stale; `parallel_tool_calls:false` chưa bao giờ được live-test riêng. Hiện Fix A + Fix B cùng chạy — cả hai đều an toàn.
- `/tmp/restart_bridge.sh`, `/tmp/restart_bridge2.sh` (v1, v2) lỗi thời — chỉ dùng v3.

### 2026-08-03 — Fix block index reuse + thinking segmentation (lỗi "cắt rất ngu" khi hiển thị reasoning)

**Mục tiêu**

- Người dùng báo: reasoning (thinking) của model hiện ra trong chat bị cắt vụn, mất chữ, lộn xộn ("cắt rất ngu", "ảnh hưởng xấu đến người xem") — nhìn thấy cả fragment reasoning nội bộ lẫn trong stream.

**Root cause (2 bug, đã chứng minh bằng event dump)**

1. **Index reuse (protocol violation — nguyên nhân chính):** `tracker.reset()` ở các vòng lặp search interception (execute.rs:536), compat retry (execute.rs:559), và `finalize_stream_with_text` (context.rs:109) reset `next_idx` về 0 trong khi client ĐÃ nhận block 0, 1 → block mới lại bắt đầu từ index 0. Bằng chứng: repro cũ emit index sequence `[0,1,0,0]`. Anthropic spec yêu cầu index tăng dần trong 1 message → Claude Code build block theo index → block bị overwrite/merge → thinking hiển thị vỡ, mất nội dung.
2. **Segmentation 384 byte:** `THINKING_RENDER_CHUNK_BYTES = 384` (context.rs:26) đóng thinking block mỗi ~300-400 byte — cắt giữa câu/list, tạo hàng trăm block nhỏ khi thinking dài (127k tokens). Test cũ `long_reasoning_is_segmented_for_interactive_rendering` assert đúng hành vi này nhưng ngưỡng bệnh lý.

**File đã sửa**

```text
src/opencode/forward/stream/execute.rs   # bỏ tracker.reset() ở interception loop + compat retry
src/opencode/forward/stream/context.rs   # bỏ reset() ở finalize_stream_with_text; THINKING_RENDER_CHUNK_BYTES 384 → 16384
src/stream_tracker.rs                    # test close_all_keeps_indices_monotonic_without_reset
src/opencode/forward/stream/tests.rs     # update 2 test segmentation theo ngưỡng mới
```

**Thay đổi kỹ thuật**

- Giữ `reset()` DUY NHẤT ở pre-content retry (execute.rs:405, chưa emit block nào — an toàn).
- Segmentation giờ 16KB (≈4000 từ) → block hiếm khi bị cắt giữa câu; với thinking ngắn/trung bình chỉ 1 block.

**Kiểm thử**

```text
cargo test --lib            451 passed, 0 failed
cargo clippy --lib          clean
cargo fmt --check           PASS
```

**Deploy & verify**

- Atomic restart v3 (PID 465074, running-marker=1).
- Repro long: index sequence giờ `[0,1,2,3,4,5]` **strictly monotonic** qua 3 vòng search loop (trước: `[0,1,0,0]`); 4 thinking block đều là segment liền mạch hết câu tự nhiên, không còn cắt 300B giữa list; tool_use 4,5 nhận đúng.

**Giới hạn đã biết**

- Vẫn còn cắt giữa câu khi 1 fragment upstream >16KB (hiếm); chưa làm boundary-alignment theo câu.
- Ngưỡng 16KB là đánh đổi: nếu Claude Code giới hạn hiển thị thinking theo block thì block 16KB có thể bị truncate ở UI (nhưng history vẫn đủ) — cần xác nhận trực tiếp trên terminal.

### 2026-08-03 — Start 2 protected standby WARP proxies (40004-40005)

**Mục tiêu**

- Người dùng yêu cầu khởi động 2 proxy standby (40004-40005) đang offline để đưa pool về full 5/5.

**Thao tác**

- Kiểm tra trạng thái: `/health` chỉ trả minimal `{"status":"ok","version":"0.5.0"}` (thiết kế cố ý — proxy telemetry nằm ở `/health/ready` và `/api/v1/proxies`, không phải lỗi).
- `/health/ready`: ready, egress mode=proxy, verified_unique_exit_ips=2 (minimum 1).
- `opencode2api proxy status`: 3 primary healthy, 2 standby offline — containers `opencode-warp-4/5` exited 4 ngày, image `ghcr.io/mon-ius/docker-warp-socks:latest`, restart policy always.
- Không có CLI nào start standby (restart/purge chỉ tác động primary; standby được bảo vệ) → `docker start opencode-warp-4 opencode-warp-5`.

**Kết quả**

- 5/5 healthy, 0 offline; verified unique exit IPs tăng 2 → 4 (mỗi standby có exit IP riêng); `/health/ready` vẫn ready.
- Không sửa code, không sửa container config, không chạm primary.

**Ghi chú**

- Daemon hiện tại (PID 10427) chạy từ `/home/light/GitHub/opencode2claude/target/release` — controller `target/release/opencode2api` spawn serve sibling. Binary build 2026-08-03 05:02, mới hơn source change gần nhất (04:58) nên không stale; deployment `~/.local/bin` cũng cùng mtime.
- `/api/v1/proxies` yêu cầu Bearer token (middleware) dù env không có `BRIDGE_AUTH_TOKEN` — auth token đến từ nguồn config khác; chưa truy vết.

### 2026-08-03 — Fix 8 confirmed bugs từ audit 4 bug-class (stream SSE, tool-call/DSML parsing, retry boundaries, request validation)

**Mục tiêu**

- Người dùng yêu cầu fan-out agent tiếp tục debug tìm lỗi cùng lớp với thinking segmentation (index reuse + 384B chunk đã fix trước). 4 audit agent quét: stream SSE lifecycle, tool-call/DSML marker parsing, retry boundaries, request validation/mapper. ~13 finding → gộp trùng → verify bằng đọc code trực tiếp → 8 CONFIRMED, người dùng chọn "Tất cả CONFIRMED" (fix hết, TDD, quality gate cuối).

**Các fix (theo thứ tự ưu tiên HIGH → LOW)**

1. **HIGH — sync path reject batch search calls** (`src/opencode/forward/sync.rs`): block cũ reject mọi batch `(search > 0 && total > 1)` → terminal "invalid tool protocol". Thay bằng `classify_sync_tool_batch` → 3 outcome: `Normal` / `Collapse` (pure-search batch: intercept call đầu, drop phần còn lại) / `DropSearches` (mixed batch: giữ non-search, drop search calls). Err chỉ cho unavailable/malformed args. 5 test.
2. **MED — compat retry nhân đôi visible text** (`src/opencode/forward/stream/execute.rs`): retry path thiếu guard `has_any_blocks_ever_opened()` như path stream_failed → emit response cũ + response mới gộp vào nhau. Thêm guard `!tracker.has_any_blocks_ever_opened() && compat_tool_retries < MAX_COMPAT_TOOL_RETRIES`.
3. **MED — mid-stream failure drop tool calls im lặng** (`src/opencode/forward/stream/context.rs`): upstream kết thúc khi còn pending tool call → trước đây emit `message_delta end_turn` giả (client tưởng hoàn tất). Giờ emit error event thay vì end_turn giả.
4. **MED — image blocks bị drop im lặng** (`src/opencode/mapper/request.rs`): user-message match `_ => {}` nuốt image/source blocks → thay bằng placeholder `[attached {other} block not forwarded]` (không bỏ chữ im lặng, không leak base64).
5. **MED — fan-out heuristic bắn nhầm khi có negation** (`src/opencode/mapper/request.rs`): 19 pattern negation ("do not fan", "không fan", "đừng dùng subagent"...) → return None, không inject fan-out instruction.
6. **MED — malformed body trả plain-text 4xx** (`src/handlers/messages.rs` + `metadata.rs`): `Result<Json<T>, JsonRejection>` extractor → map thành `BridgeError::InvalidRequest` → Anthropic error shape JSON (400) thay vì axum plain-text 422.
7. **LOW — double error event + message_start sau error** (`src/opencode/forward/stream/execute.rs`): 2 block error cũ gửi error + finalize_stream với fresh tracker → 2 error + message_start sau error. Helper `finalize_transport_error` chuẩn hoá: message_start (nếu chưa) → 1 error → message_stop.
8. **LOW — DSML literal tag trong value misparse** (`src/opencode/sanitize.rs`): đếm raw tag bằng `text.matches()` misfire khi value chứa literal `</｜DSML｜parameter>` → parse một lần, `truncated` flag tại mỗi `break` thiếu close tag, `structurally_broken = truncated || invokes_processed != calls.len()`.

**Test mới (TDD — mỗi test fail đúng lý do trước khi fix)**

```text
src/opencode/forward/sync.rs           5 test: mixed/pure/single/unavailable/malformed batch
src/opencode/forward/stream/tests.rs   4 test: compat retry không merge response 2,
                                       pending tool call → error không end_turn giả,
                                       non-2xx → message_start rồi 1 error rồi message_stop,
                                       (fixture SseReadError/Sse/Raw)
src/opencode/mapper/tests.rs           2 test: image block placeholder, fan-out negation
tests/protocol_conformance.rs          5 test (black-box harness): 4 ở trên + malformed body 400
tests/fast.rs                          test_tc034: 422 → 400 + error shape (behavior mới có chủ đích)
```

**Kiểm thử (quality gate đầy đủ)**

```text
cargo fmt --all -- --check        PASS
cargo clippy --locked --all-targets -- -D warnings   PASS
cargo test --locked               593 passed, 0 failed
   (lib 459 + fast 87 + protocol_conformance 18 + integration 2 + others 27)
```

**Deploy & verify**

- CHƯA deploy — binary đang chạy (PID 10427) là bản trước 8 fix; cần build release + atomic restart (v3) khi người dùng xác nhận.
- Working tree: thay đổi chưa commit, chờ yêu cầu user.

**Giới hạn đã biết (SUSPECT — ngoài scope đã chọn, chưa fix)**

- `context.rs:824/857`: native tool_use emit khi intercepting_search active thiếu guard (SUSPECT).
- finish_reason sớm → finalize partial args (SUSPECT).
- orphan tool_result không cross-check (SUSPECT); tool_call thiếu index bị drop (SUSPECT).
- tool_result image base64 vào prompt (SUSPECT); temperature >1 cứng 400 (SUSPECT).
- Search calls bị drop trong mixed batch (sync path) có thể không được model re-issue (model-dependent).
- Snapshot: PID 10427 vẫn live, proxy pool 5/5, model opencode/deepseek-v4-flash-free — không đổi so với snapshot trên.
