# 🚀 OpenCode2Claude API Bridge

**OpenCode2Claude** là một cầu nối API (API Bridge) cục bộ gọn nhẹ được thiết kế để kết nối **Claude Code** (hoặc bất kỳ Agent nào sử dụng chuẩn API Anthropic) với bộ công cụ lập trình ngoại tuyến **OpenCode CLI**.

Cầu nối này giúp tăng tốc độ thực thi đáng kể (lên đến **300%**) nhờ việc giao tiếp trực tiếp qua HTTP tới một tiến trình daemon OpenCode đang chạy, thay vì phải khởi động lại OpenCode CLI cho mỗi lần gửi prompt.

---

## 🌟 Tại sao nên dùng OpenCode2Claude?

1. **Tốc độ vượt trội**: Tránh được độ trễ khởi động tiến trình (~15-20 giây mỗi lượt gọi) của OpenCode CLI bằng cách đính kèm (`--attach`) trực tiếp vào daemon `opencode serve`. Thời gian phản hồi giảm xuống chỉ còn khoảng 3-5 giây.
2. **Hỗ trợ Streaming**: Hỗ trợ đầy đủ cơ chế Server-Sent Events (SSE) để truyền dữ liệu (stream) ký tự theo thời gian thực từ OpenCode hiển thị trực tiếp lên giao diện của Claude.
3. **Tiết kiệm tài nguyên & Băng thông**: Sử dụng các mô hình miễn phí mạnh mẽ (như `deepseek-v4-flash-free` hay `nemotron-3-ultra-free`) thông qua cổng dịch vụ cục bộ.
4. **Không phụ thuộc thư viện ngoài (Zero Dependencies)**: Được viết hoàn toàn bằng thư viện chuẩn của Python (`http.server`, `subprocess`, `urllib`). Chỉ cần cài đặt Python 3 là có thể chạy ngay.

---

## 📦 Hướng dẫn cài đặt & Khởi chạy từng bước

### 1. Yêu cầu hệ thống
- **Python**: Phiên bản 3.8 trở lên.
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
Thư mục dự án đã đi kèm file `start.sh` để tự động hóa toàn bộ quy trình:
1. Cấp quyền thực thi cho script:
   ```bash
   chmod +x start.sh
   ```
2. Khởi chạy script:
   ```bash
   ./start.sh
   ```
   *Script sẽ tự động kiểm tra xem daemon `opencode serve` đã chạy ở cổng `4096` chưa. Nếu chưa, nó sẽ khởi động daemon dưới nền trước, sau đó khởi chạy Bridge Python trên cổng `4000`.*

#### Cách 2: Khởi chạy thủ công từng dịch vụ
Nếu muốn kiểm soát chi tiết hơn, bạn có thể chạy bằng 2 terminal riêng biệt:

1. **Terminal 1 - Khởi động OpenCode Daemon**:
   ```bash
   opencode serve --port 4096 --hostname 127.0.0.1
   ```
2. **Terminal 2 - Khởi động API Bridge**:
   ```bash
   python3 bridge.py
   ```
   *Cầu nối sẽ lắng nghe tại địa chỉ: `http://127.0.0.1:4000`*

---

## 🔌 Cấu hình Claude Code để kết nối qua Bridge

Để hướng luồng gọi từ ứng dụng Claude CLI sang Bridge cục bộ này, hãy cấu hình các biến môi trường trước khi chạy lệnh `claude`:

```bash
# 1. Định nghĩa khóa API Anthropic giả (Bắt buộc để Claude bỏ qua bước đăng nhập trình duyệt)
export ANTHROPIC_API_KEY="opencode-bridge"

# 2. Định hướng Endpoint của Claude sang địa chỉ của Bridge
export ANTHROPIC_API_URL="http://127.0.0.1:4000/v1"

# 3. Khởi chạy Claude Code
claude
```

Giờ đây, bất kỳ câu lệnh nào bạn yêu cầu trong Claude Code sẽ được gửi qua Bridge và thực thi trực tiếp bởi OpenCode cục bộ một cách nhanh chóng dưới dạng stream thời gian thực!

---

## 📁 Cấu trúc dự án

```
opencode2claude/
├── bridge.py       # File xử lý chính chứa HTTP Server và luồng ánh xạ API
├── start.sh        # Script tự động khởi chạy daemon OpenCode và Bridge
├── README.md       # Tài liệu hướng dẫn sử dụng này
└── .gitignore      # Cấu hình bỏ qua các file log và cache Python
```

---

## 📝 Cơ chế hoạt động bên trong (Under the Hood)

1. Khi Claude gửi yêu cầu POST đến `/v1/messages`, Bridge sẽ trích xuất nội dung tin nhắn (`messages`) từ định dạng JSON của Anthropic.
2. Bridge kiểm tra xem cổng `4096` có đang mở hay không. 
   - Nếu **Có** (daemon đang hoạt động): Bridge sẽ thực thi lệnh:
     `opencode run --attach http://127.0.0.1:4096 --dangerously-skip-permissions "<prompt>"`
   - Nếu **Không**: Bridge sẽ thực thi lệnh thông thường:
     `opencode run --dangerously-skip-permissions "<prompt>"`
3. Đối với yêu cầu có tham số `stream: true`, Bridge sẽ đọc từng ký tự từ stdout của tiến trình OpenCode và chuyển đổi thành định dạng Server-Sent Events (SSE) tiêu chuẩn để Claude hiển thị trực tiếp ký tự lên màn hình.

---

## 📄 Giấy phép / License
Dự án được phát triển dưới giấy phép **MIT License**.
