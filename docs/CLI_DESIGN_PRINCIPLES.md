# Universal CLI Design Principles

**Phiên bản:** 1.0
**Phạm vi:** CLI tool, developer tool, agent, gateway, daemon manager và utility chạy trong terminal
**Mục tiêu:** tạo giao diện dòng lệnh đơn giản, đẹp, dễ đọc, dễ tự động hóa và hoạt động ổn định trên nhiều terminal

---

## 1. Triết lý thiết kế

Một CLI tốt không cần giả lập ứng dụng desktop trong terminal. Nó cần giúp người dùng hiểu nhanh ba điều:

1. Công cụ đang làm gì.
2. Trạng thái hiện tại là gì.
3. Người dùng nên làm gì tiếp theo.

Thiết kế nên ưu tiên:

- Nội dung trước trang trí.
- Khoảng trắng trước đường viền.
- Cấu trúc tuyến tính trước bố cục phức tạp.
- Trạng thái rõ ràng trước hiệu ứng bắt mắt.
- Tính nhất quán trước sự sáng tạo của từng command.
- Khả năng pipe và automation ngang hàng với giao diện dành cho con người.

CLI đẹp không phải CLI có nhiều màu, box hoặc spinner. CLI đẹp là CLI có nhịp thị giác tốt, căn lề chính xác, thông tin có thứ bậc và không làm người dùng phải đoán.

---

## 2. Khi nào dùng static CLI và khi nào dùng TUI

Mặc định, hãy dùng **static CLI output**.

Static CLI phù hợp khi:

- Command chạy xong rồi thoát.
- Kết quả chủ yếu là status, config, logs, danh sách hoặc summary.
- Công cụ cần dùng trong shell script, CI hoặc pipe.
- Người dùng không cần điều hướng liên tục giữa nhiều màn hình.
- Thao tác chỉ cần một hoặc vài câu lệnh rõ ràng.

Chỉ dùng full-screen TUI khi:

- Người dùng phải theo dõi dữ liệu thay đổi liên tục.
- Có nhiều vùng focus, bảng scroll, filter hoặc tương tác lâu dài.
- Công cụ cần giữ session mở trong terminal.
- Giá trị của TUI lớn hơn chi phí event loop, raw mode, alternate screen và terminal recovery.

Nguyên tắc mặc định:

> Nếu một command có thể trình bày đẹp trong transcript tuyến tính, không xây TUI.

---

## 3. Cấu trúc chuẩn của một command

Một command human-readable nên theo thứ tự:

```text
Brand header
Subtitle hoặc context

Primary status hoặc primary result

Facts / table / details

Summary

Next action hoặc hint
```

Ví dụ:

```text
◆ ProductName
  Gateway status · Local model bridge

  ● Running

  Endpoint     http://127.0.0.1:4000
  Model        provider/model-name
  Uptime       18m 42s

  3 healthy   0 degraded

  › Run `product logs` to inspect activity.
```

Không bắt buộc command nào cũng có đủ mọi phần. Command đơn giản chỉ cần primary result.

---

## 4. Brand header

Header chỉ nên chiếm tối đa hai dòng:

```text
◆ ProductName
  Command context · Optional subtitle
```

Quy tắc:

- Chỉ dùng một brand symbol có chiều rộng terminal ổn định.
- Không dùng ASCII art lớn cho mọi command.
- Không lặp version, website, slogan và copyright ở mỗi lần chạy.
- Header chỉ xuất hiện trong human mode.
- Quiet và JSON không được có brand header.

Brand symbol đề xuất:

```text
◆  ◇  ●  ▸
```

Tránh phụ thuộc vào emoji nhiều cell hoặc emoji có variation selector vì có thể lệch lề giữa các terminal.

---

## 5. Hệ thống khoảng cách

Dùng một hệ thống spacing duy nhất cho toàn bộ CLI.

Giá trị tham khảo:

```text
Outer indent          2 cells
Continuation indent   4 cells
Section gap           1 blank line
Header height         2 lines
Label-value gap       3 cells
```

Ví dụ token:

```rust
pub const INDENT: usize = 2;
pub const CONTINUATION_INDENT: usize = 4;
pub const SECTION_GAP: usize = 1;
pub const LABEL_GAP: usize = 3;
pub const BRAND_SYMBOL: &str = "◆";
```

Không căn lề bằng chuỗi space hard-code theo từng command. Mọi facts, table, error và hint phải dùng chung renderer.

Sai:

```rust
println!("║  Daemon: port {}                         ║", port);
```

Đúng:

```rust
facts.row("Daemon", format!("port {port}"));
```

---

## 6. Màu sắc

Màu chỉ mang ý nghĩa ngữ nghĩa.

| Vai trò | Màu đề xuất |
|---|---|
| Brand / focus | Cyan hoặc accent duy nhất |
| Thành công / healthy | Green |
| Cảnh báo / degraded | Yellow |
| Lỗi / failed | Red |
| Metadata / subtitle / timestamp | Dim hoặc dark gray |
| Nội dung chính | Foreground mặc định của terminal |

Quy tắc:

- Không tô cả đoạn văn bằng màu.
- Không dùng nhiều accent color trong một command.
- Không dùng màu để thay thế text.
- Trạng thái phải có symbol hoặc label đi kèm.
- Giao diện phải vẫn hiểu được khi tắt màu.

Ví dụ:

```text
● healthy
▲ degraded
× failed
○ offline
```

Không nên dùng:

```text
Màu xanh = trạng thái tốt
Màu đỏ = trạng thái xấu
```

mà không có text hoặc symbol.

---

## 7. Output contract

Mọi command phải xác định rõ ba mode.

### 7.1 Human

Dành cho người đọc trực tiếp.

Có thể chứa:

- Header.
- Màu.
- Khoảng trắng.
- Bảng.
- Spinner.
- Hint.

Không được chứa:

- Secret không cần thiết.
- Stack trace mặc định.
- Dữ liệu khó parse nếu command được quảng bá là machine-readable.

### 7.2 Quiet

Dành cho shell script và command substitution.

Quiet chỉ in dữ liệu chính:

```text
running
```

hoặc:

```text
127.0.0.1:4000
```

hoặc shell exports:

```bash
export API_BASE_URL='http://127.0.0.1:4000'
```

Quiet không được có:

- Header.
- Spinner.
- Progress bar.
- Hint.
- Icon trang trí.
- ANSI.

### 7.3 JSON

Dành cho automation và tích hợp.

JSON phải:

- Luôn là JSON hợp lệ.
- Dùng field ổn định.
- Không trộn human text vào stdout.
- Có structured error.
- Không lộ secret theo mặc định.
- Dùng pretty-print nếu output không quá lớn và đây là command trực tiếp.

Ví dụ:

```json
{
  "status": "running",
  "endpoint": "http://127.0.0.1:4000",
  "uptime_seconds": 1122,
  "healthy": true
}
```

Lỗi JSON:

```json
{
  "status": "error",
  "operation": "restart",
  "message": "container did not become healthy"
}
```

Không được in lỗi human vào stderr rồi để stdout rỗng nếu người dùng đã chọn JSON.

---

## 8. stdout và stderr

Quy ước:

- Primary data và kết quả thành công: `stdout`.
- Diagnostic, warning ngoài dữ liệu chính và lỗi human: `stderr`.
- JSON result, kể cả structured error: ưu tiên `stdout` để caller parse được.
- Exit code vẫn phải phản ánh thành công hoặc thất bại.

Không để progress hoặc warning làm hỏng pipe của dữ liệu chính.

Ví dụ:

```bash
value=$(tool --quiet status)
```

phải nhận đúng một giá trị có thể dùng ngay.

---

## 9. Facts và key-value output

Facts phù hợp với status, config, environment và summary.

Wide layout:

```text
Endpoint     http://127.0.0.1:4000
Model        provider/model
Uptime       18m
```

Compact layout:

```text
Endpoint
  http://127.0.0.1:4000
Model
  provider/model
```

Quy tắc:

- Label căn trái.
- Value căn theo label dài nhất.
- Không dùng dấu `:` nếu toàn bộ layout đã căn cột rõ ràng.
- Label dùng semantic style nhẹ hoặc dim.
- Giá trị quan trọng dùng foreground mặc định hoặc bold vừa phải.
- Value dài phải wrap hoặc chuyển compact mode, không ép box rộng cố định.

---

## 10. Bảng

Mặc định dùng bảng không viền.

Tốt:

```text
PORT   ROLE     STATE      CONTAINER
40001  primary  ● healthy  proxy-1
40002  primary  ● healthy  proxy-2
40003  standby  ○ offline  proxy-3
```

Tránh:

```text
┌───────┬─────────┬────────────┐
│ PORT  │ ROLE    │ STATE      │
├───────┼─────────┼────────────┤
│ ...   │ ...     │ ...        │
└───────┴─────────┴────────────┘
```

Đường viền chỉ nên dùng khi:

- Dữ liệu cần phân tách ô rất rõ.
- Bảng có nhiều dòng multiline.
- Không có cách trình bày tuyến tính dễ đọc hơn.

Quy tắc table:

- Header ngắn.
- Numeric column căn phải.
- Không có trailing spaces.
- Không overflow terminal width.
- Terminal hẹp phải bỏ cột phụ hoặc chuyển sang list view.
- Không truncate status quan trọng.
- ID dài có thể truncate bằng dấu `…`.

---

## 11. Responsive behavior

CLI không cần breakpoint phức tạp, nhưng phải có tối thiểu ba mode.

### Wide: từ khoảng 100 cột

- Hiển thị đầy đủ facts.
- Bảng đủ cột.
- Description và metadata có thể xuất hiện.

### Medium: khoảng 70–99 cột

- Ẩn cột phụ.
- Rút gọn subtitle.
- Wrap hint.

### Compact: dưới khoảng 70 cột

- Facts chuyển thành label/value xếp dọc.
- Table chuyển thành list hoặc bỏ cột ít quan trọng.
- Long command hint có thể xuống dòng với continuation indent.

Tất cả width tính theo **visible Unicode width**, không phải byte length.

---

## 12. Unicode và ANSI

Terminal output phải đo chiều rộng sau khi bỏ mã ANSI.

Renderer cần hỗ trợ:

- ANSI SGR color.
- Cursor control sequence.
- OSC hyperlink.
- Unicode ký tự rộng.
- Vietnamese và ký tự có dấu.
- Truncate theo visible width.

Các helper tối thiểu:

```rust
strip_ansi(input)
visible_width(input)
truncate_visible(input, max_width)
pad_to_width(input, target_width)
wrap_visible(input, max_width)
```

Không dùng `str.len()` để căn terminal.

---

## 13. Progress và spinner

Progress chỉ được dùng khi command mất đủ lâu để người dùng cảm nhận được độ trễ.

Spinner chỉ bật khi:

```text
output mode == human
stdout hoặc stderr tương ứng là TTY
TERM không phải dumb
CI không bật
```

Không bật spinner trong:

- JSON.
- Quiet.
- Pipe.
- Redirect vào file.
- CI.

Progress tốt:

```text
◐ Starting gateway
  Checking configuration
  Preparing proxy pool
```

Khi hoàn thành, xóa progress tạm và thay bằng kết quả cuối:

```text
✓ Gateway started
```

Không để lại hàng chục dòng spinner history.

---

## 14. Error design

Error human phải trả lời ba câu hỏi:

1. Việc gì thất bại?
2. Nguyên nhân trực tiếp là gì?
3. Người dùng nên làm gì tiếp theo?

Template:

```text
◆ ProductName
  Operation failed

  × Could not restart proxy 40001

  Container did not become healthy before timeout.

  Try:
    product proxy logs 40001
    product doctor
```

Quy tắc:

- Title ngắn và cụ thể.
- Không bắt đầu bằng tên exception nội bộ.
- Không in full stack trace mặc định.
- Suggestion phải là lệnh có thể copy.
- Wrap error theo terminal width.
- Không dùng từ ngữ mơ hồ như “Something went wrong”.

Structured error cần có field ổn định:

```json
{
  "status": "error",
  "operation": "proxy_restart",
  "target": "40001",
  "message": "health check timeout"
}
```

---

## 15. Warning và confirmation

Warning không phải error.

```text
▲ Authentication is disabled
```

Destructive action cần confirmation trong human mode:

```text
Purge 3 primary proxies?
This will reset active connections.
Continue? [y/N]
```

Quy tắc:

- Default là không thực hiện.
- Hỗ trợ `--yes` cho automation.
- Quiet và JSON không được treo chờ prompt vô thời hạn.
- Với JSON, destructive action nên yêu cầu flag xác nhận rõ ràng.

---

## 16. Logs

Log output là dữ liệu tuyến tính, không phải dashboard.

Format gợi ý:

```text
19:42:03  INFO   Request completed   POST /v1/messages   200   1.24s
19:42:11  WARN   Proxy retry         port=40002 attempt=2
19:42:14  ERROR  Upstream failed     timeout
```

Quy tắc:

- Timestamp dim.
- Level có semantic color.
- Message dùng foreground mặc định.
- Metadata dim.
- Không thêm border quanh logs.
- Không thêm header container trước mỗi dòng.
- Hỗ trợ `--tail`, `--follow` nếu phù hợp.
- `--color never` phải strip ANSI từ cả renderer và nguồn log bên ngoài.
- Quiet logs có thể giữ raw data nhưng không được có decoration.

---

## 17. Secret và dữ liệu nhạy cảm

Human output chỉ hiển thị:

```text
configured
not configured
sk-abc1…92fe
```

Không hiển thị secret đầy đủ trong:

- Status.
- Config.
- List.
- Logs.
- JSON mặc định.

Secret chỉ được hiển thị khi:

- Vừa được tạo.
- Người dùng yêu cầu rõ ràng.
- Có cảnh báo rằng giá trị chỉ xuất hiện một lần.

Ví dụ:

```text
API key
sk-product-xxxxxxxxxxxxxxxx

▲ Store this credential now; it cannot be recovered later.
```

JSON mặc định nên dùng:

```json
{
  "api_key": "<redacted>",
  "api_key_configured": true
}
```

---

## 18. Help design

Help phải giúp người dùng hoàn thành tác vụ, không chỉ liệt kê option.

Nên có:

- Một câu mô tả command.
- Usage ngắn.
- Option có tên nhất quán.
- Một đến ba ví dụ thực tế.
- Global flags không lặp gây nhiễu nếu framework cho phép kiểm soát.

Ví dụ:

```text
Examples:
  product server start
  product server status --json
  eval "$(product --quiet env)"
```

Canonical executable name phải nhất quán trong:

- Help.
- Error suggestion.
- Installer.
- README.
- Completion.

Không dùng lẫn nhiều tên alias nếu installer không thực sự tạo alias đó.

---

## 19. Kiến trúc presentation layer

Không để mỗi command tự `println!` theo phong cách riêng.

Cấu trúc đề xuất:

```text
src/
├── presentation/
│   ├── theme
│   ├── text
│   ├── layout
│   ├── facts
│   ├── table
│   ├── progress
│   ├── error
│   └── renderer
├── output/
│   ├── human
│   ├── quiet
│   └── json
└── commands/
```

Presentation primitives tối thiểu:

```text
BrandHeader
Section
Facts
StatusLine
MinimalTable
SummaryLine
Hint
CommandError
Progress
```

Command handler nên:

1. Thu thập dữ liệu.
2. Tạo typed result DTO.
3. Chọn renderer theo output mode.
4. Không nhúng logic nghiệp vụ vào renderer.

Mục tiêu:

```text
Business logic → typed result → human / quiet / JSON renderer
```

Human và JSON phải dùng cùng nguồn dữ liệu để tránh kết quả khác nhau.

---

## 20. Typed result model

Ví dụ:

```rust
#[derive(Serialize)]
pub struct ServerStatus {
    pub status: ServiceState,
    pub endpoint: String,
    pub pid: Option<u32>,
    pub uptime_seconds: Option<u64>,
    pub model: String,
    pub proxies: Vec<ProxyStatus>,
}
```

Renderer:

```rust
match format {
    OutputFormat::Human => render_human(&status),
    OutputFormat::Quiet => render_quiet(&status),
    OutputFormat::Json => render_json(&status),
}
```

Không xây JSON riêng từ một subset nhỏ trong khi human output có dữ liệu đầy đủ hơn.

---

## 21. Portability và accessibility

CLI phải hoạt động trong:

- Dark terminal.
- Light terminal.
- tmux.
- SSH.
- VS Code terminal.
- Terminal 256-color.
- `NO_COLOR=1`.
- `TERM=dumb`.
- Pipe và redirect.

Quy tắc:

- Không đặt foreground sáng cố định cho text chính.
- Không tô background toàn màn hình.
- Không phụ thuộc hoàn toàn vào màu.
- Symbol phải có text đi kèm.
- Có thể cung cấp ASCII mode nếu môi trường mục tiêu hạn chế Unicode.

---

## 22. Testing strategy

### Unit test

Kiểm tra:

```text
visible width
ANSI stripping
Unicode truncation
facts alignment
table alignment
wrap behavior
secret redaction
JSON serialization
quiet output
```

### Width matrix

Tối thiểu:

```text
50
60
70
80
100
120
160
```

Kiểm tra:

- Không overflow.
- Không trailing spaces.
- Không panic.
- Không cắt status quan trọng.
- Không phá Unicode.

### Color matrix

```text
--color auto
--color always
--color never
NO_COLOR=1
non-TTY
```

### Output matrix

Cho mỗi command quan trọng:

```text
human
quiet
JSON
success
warning
failure
```

### Manual verification

Chạy thật trong:

```text
GNOME Terminal hoặc terminal mặc định
VS Code terminal
tmux
SSH
dark theme
light theme
```

---

## 23. Anti-patterns

Không nên:

- Bao toàn bộ output bằng box.
- Box lồng box.
- Dùng border cho mọi table.
- Căn lề bằng space hard-code.
- Dùng emoji dày đặc.
- Spinner trong pipe hoặc JSON.
- Quiet nhưng vẫn in header.
- JSON nhưng stderr chứa kết quả chính.
- Human và JSON dùng hai nguồn dữ liệu khác nhau.
- Tô màu toàn bộ câu hoặc paragraph.
- In secret trong status hoặc JSON mặc định.
- Để ANSI từ Docker, Git hoặc process ngoài lọt qua `--color never`.
- Dùng `str.len()` để đo chiều rộng terminal.
- Hiển thị stack trace cho lỗi người dùng có thể tự xử lý.
- Dùng alias trong hint nhưng installer không tạo alias.
- Tạo TUI chỉ vì static CLI hiện tại chưa đẹp.

---

## 24. Definition of Done

Một CLI redesign chỉ hoàn thành khi:

```text
[ ] Header tối đa hai dòng
[ ] Không có box lớn hoặc box lồng nhau
[ ] Mọi command dùng chung visual tokens
[ ] Human, quiet và JSON có contract rõ
[ ] JSON luôn parse được
[ ] Quiet không có decoration hoặc ANSI
[ ] Spinner chỉ chạy trong TTY human mode
[ ] Không overflow ở width đã hỗ trợ
[ ] Không trailing spaces
[ ] Unicode và ANSI được đo đúng
[ ] Error có nguyên nhân và hướng xử lý
[ ] Secret được che mặc định
[ ] Logs tuân thủ color policy
[ ] Human và JSON dùng cùng typed result
[ ] Hoạt động trên dark/light terminal
[ ] Unit test và width matrix đều pass
```

---

## 25. Quick-start checklist cho dự án mới

### Foundation

```text
[ ] Chọn canonical executable name
[ ] Chọn một brand symbol
[ ] Chọn một accent color
[ ] Định nghĩa spacing tokens
[ ] Định nghĩa status symbols
[ ] Định nghĩa Human / Quiet / JSON contract
```

### Components

```text
[ ] BrandHeader
[ ] StatusLine
[ ] Facts
[ ] BorderlessTable
[ ] Hint
[ ] ErrorRenderer
[ ] ProgressRenderer
[ ] ANSI/Unicode helpers
```

### Commands

```text
[ ] status
[ ] start
[ ] stop
[ ] restart
[ ] config
[ ] doctor
[ ] logs
[ ] completion
[ ] update
```

### Verification

```text
[ ] Width 50–160
[ ] Color auto/always/never
[ ] Human/quiet/JSON
[ ] Pipe và redirect
[ ] Secret scan
[ ] Dark/light theme
[ ] tmux/SSH
```

---

## 26. Mẫu visual contract ngắn

Có thể copy phần này vào README hoặc design spec của dự án:

```text
Style          Minimal, linear, whitespace-first
Brand          One stable-width symbol + product name
Header         Maximum two lines
Accent         One primary accent color
Status         Text + symbol + semantic color
Tables         Borderless by default
Progress       TTY-only spinner; cleared on completion
Errors         What failed + why + next command
Human          Styled and readable
Quiet          Primary value only, no ANSI
JSON           Stable typed schema, secret-safe
Responsive     Wide, medium and compact layouts
Security       Secrets redacted by default
Testing        Width, color, mode and terminal matrix
```

---

## 27. Nguyên tắc chốt

> Một CLI chuyên nghiệp phải trông đơn giản vì hệ thống thiết kế phía sau nó chặt chẽ, không phải vì nó thiếu thông tin.

> Dùng màu để truyền đạt trạng thái, dùng khoảng trắng để tạo cấu trúc, dùng typed data để giữ mọi output mode nhất quán.

> Khi người dùng nhìn vào terminal, họ phải biết ngay công cụ đang làm gì, trạng thái ra sao và bước tiếp theo là gì.
