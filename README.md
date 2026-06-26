# 🚀 OpenCode2Claude API Bridge (Rust Version)

**OpenCode2Claude** là một cầu nối API (API Bridge) cục bộ siêu nhanh, được viết bằng **Rust (Axum + Tokio)**, thiết kế để kết nối **Claude Code** (hoặc bất kỳ Agent nào sử dụng chuẩn API Anthropic) với bộ công cụ lập trình ngoại tuyến **OpenCode CLI**.

Nhờ hiệu năng và độ ổn định vượt trội của Rust, cầu nối này đem lại tốc độ xử lý đỉnh cao, đồng thời cho phép bạn dễ dàng mở rộng tính năng trong tương lai.

---

## 🌟 Tại sao nên dùng phiên bản Rust?

1. **Hiệu suất & Độ trễ tối thiểu**: Rust + Axum cho tốc độ định tuyến và xử lý các luồng dữ liệu (I/O) cận mức phần cứng, loại bỏ hoàn toàn độ trễ khởi động trình thông dịch của Python hay Node.js.
2. **Khởi chạy nền song song (Non-blocking I/O)**: Sử dụng mô hình async của Tokio để xử lý stream Server-Sent Events (SSE) theo thời gian thực mà không bao giờ bị nghẽn (non-blocking).
3. **Quản lý tiến trình an toàn**: Tích hợp các cơ chế tự động thu hồi tài nguyên và dọn dẹp tiến trình nền.
4. **Không phụ thuộc vào thư viện ngoài khi chạy**: Sau khi biên dịch ra file binary (`target/release/opencode2claude`), bạn có thể sao chép file này đi bất kỳ máy Linux nào cùng kiến trúc để chạy mà không cần cài đặt thêm Rust hay Python.

---

## 📦 Hướng dẫn cài đặt & Khởi chạy từng bước

### 1. Yêu cầu hệ thống
- **Rust & Cargo**: Phiên bản 1.70 trở lên (để biên dịch).
- **OpenCode CLI**: Đã được cài đặt và cấu hình sẵn trên hệ thống của bạn.

---

### 2. Tải mã nguồn về máy
Sao chép thư mục hoặc clone dự án này vào thư mục cá nhân của bạn:
```bash
cd ~/GitHub/opencode2claude
```

---

### 3. Cách chạy dự án

#### Cách 1: Sử dụng Script tự động (Khuyến nghị)
Thư mục dự án đã đi kèm file `start.sh` để tự động hóa toàn bộ quy trình biên dịch và chạy ngầm:
1. Cấp quyền thực thi cho script:
   ```bash
   chmod +x start.sh
   ```
2. Khởi chạy script:
   ```bash
   source start.sh
   ```
   *Script sẽ tự động chạy `cargo build --release` (ở lần chạy đầu tiên), khởi động daemon `opencode serve` dưới nền, chạy Bridge Rust trên cổng `4000`, và tự động export các biến môi trường cấu hình cho Claude.*

#### Cách 2: Khởi chạy thủ công từng dịch vụ
1. **Biên dịch dự án**:
   ```bash
   cargo build --release
   ```
2. **Khởi động OpenCode Daemon**:
   ```bash
   opencode serve --port 4096 --hostname 127.0.0.1
   ```
3. **Khởi động API Bridge**:
   ```bash
   ./target/release/opencode2claude
   ```
   *Cầu nối sẽ lắng nghe tại địa chỉ: `http://127.0.0.1:4000`*

---

## 🔌 Cấu hình Claude Code để kết nối qua Bridge

Khi bạn sử dụng lệnh `source start.sh`, các cấu hình đã được tự động thiết lập. Nếu khởi chạy thủ công, bạn hãy chạy các lệnh sau trước khi mở Claude Code:

```bash
export ANTHROPIC_API_KEY="opencode-bridge"
export ANTHROPIC_API_URL="http://127.0.0.1:4000/v1"
claude
```

---

## 📁 Cấu trúc dự án

```
opencode2claude/
├── Cargo.toml      # Cấu hình dependency của Rust (axum, tokio, serde, ...)
├── src/
│   └── main.rs     # Source code Rust xử lý luồng API HTTP & Gọi lệnh OpenCode
├── start.sh        # Script tự động biên dịch và khởi chạy ngầm toàn bộ dịch vụ
├── stop.sh         # Script dọn dẹp và kết thúc an toàn các tiến trình nền
└── README.md       # Tài liệu hướng dẫn sử dụng này
```

---

## 📝 Tính năng Đánh chặn lệnh Shell trực tiếp (Direct Shell Command Interception)

Nếu bạn gửi một prompt bắt đầu bằng dấu **`!`** (Ví dụ: `!pwd`, `!ls -la`, `!git diff`), Bridge Rust sẽ **đánh chặn lệnh đó và chạy trực tiếp trên Shell cục bộ của máy** bằng `tokio::process::Command`, sau đó stream kết quả terminal trực tiếp về Claude Code mà không qua bất kỳ mô hình AI nào, cho phản hồi ngay lập tức trong **0.01s**.

---

## 📄 Giấy phép / License
Dự án được phát triển dưới giấy phép **MIT License**.
