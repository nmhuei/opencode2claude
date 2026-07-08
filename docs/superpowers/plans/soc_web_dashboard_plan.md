# Kế hoạch Thiết kế & Triển khai IDE-Style Web Workspace cho opencode2api

Kế hoạch này phác thảo cấu trúc, luồng dữ liệu, giao diện và các bước triển khai để xây dựng một **Giao diện Web Dashboard (Operations Console)** cho `opencode2api` mô phỏng chính xác giao diện làm việc của các **IDE hiện đại như VS Code và Cursor**.

---

## 1. Kiến trúc Tổng thể (Architecture)

Để giữ cho ứng dụng nhẹ, không cần build-step phức tạp và dễ dàng phân phối toàn cầu, chúng ta chọn giải pháp **Single-Page Application (SPA) nhúng trực tiếp** (Embedded Static Assets) vào binary `opencode2api-serve`:

```
                       ┌──────────────────────────────┐
                       │   Trình duyệt (Giao diện)    │
                       └──────────────┬───────────────┘
                                      │ HTTP / SSE (Event Stream)
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  opencode2api-serve (API Server Engine)                                  │
│                                                                             │
│  ┌──────────────────┐  ┌──────────────────┐  ┌───────────────────────────┐  │
│  │ Dashboard Routes │  │   Upstream API   │  │   Proxy Pool Controller   │  │
│  │ (Embedded WebUI) │  │  (/v1/messages)  │  │ (Docker orchestration)    │  │
│  └──────────────────┘  └──────────────────┘  └───────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Thiết kế Giao diện mô phỏng VS Code / Cursor Workspace

Bố cục giao diện sẽ được tổ chức thành các khung panel phân cực phẳng đặc trưng của các trình biên dịch mã nguồn hiện đại:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Activity | Primary Sidebar (Explorer) │ Editor Area (Active File Tabs)      │
│   Bar    │                            ├─────────────────────────────────────┤
│          │  ▼ OPENCODE2CLAUDE         │ Tab: dashboard.json ✕ proxy_pool.json │
│   [Ξ]    │    [#] dashboard.json      ├─────────────────────────────────────┤
│ Explorer │    [#] proxy_pool.json     │ {                                   │
│          │    [#] config.toml         │   "status": "online",               │
│   [⚒]    │                            │   "model": "deepseek-v4-flash",     │
│  Doctor  │  ▼ PROXY POOL (PRIMARY)    │   "uptime": "2h 15m"                │
│          │    [●] Node 1 (Alive)      │ }                                   │
│   [🖳]    │    [●] Node 2 (Alive)      │                                     │
│ Terminal │    [◉] Node 3 (Cooldown)   │                                     │
│          │                            │                                     │
├──────────┴────────────────────────────┴─────────────────────────────────────┤
│ Terminal Panel (Live Log Output)                                            │
│ nmhhuei@opencode:~$ tail -f /server/logs                                    │
│ [2026-07-08 12:31:47] INFO: Proxy 40003 entered cooldown.                  │
└─────────────────────────────────────────────────────────────────────────────┘
│ StatusBar: ● Port: 4000  |  ● Model: deepseek-v4  |  ● Active Proxies: 4/5   │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Các thành phần chi tiết của IDE Layout:
1.  **Activity Bar (Thanh hoạt động dọc - góc trái ngoài cùng)**:
    *   Chiều rộng: 48px. Màu nền tối nhất (`#0b0b0e`).
    *   Chứa các icon chuyển đổi góc nhìn chính: Explorer (Giao diện tổng quan), Doctor (Chẩn đoán), Terminal (Dòng lệnh), Settings (Cấu hình).
2.  **Primary Sidebar (Thanh điều hướng tệp tin)**:
    *   Chiều rộng: 260px. Màu nền xám đậm (`#18181c`).
    *   Hiển thị cấu trúc dạng cây (Tree view):
        *   Thư mục ảo chứa các "file" trạng thái (`dashboard.json`, `proxy_pool.json`, `config.toml`).
        *   Cây trạng thái của các nút Proxy (Primary, Standby).
3.  **Editor Area (Vùng nội dung chính)**:
    *   Màu nền editor (`#1e1e24`).
    *   Hỗ trợ duyệt nhiều tab như tệp tin thực tế (Ví dụ: Click vào `config.toml` trong sidebar sẽ mở tab chỉnh sửa tệp cấu hình đó).
    *   Nội dung hiển thị được định dạng màu sắc cú pháp code (syntax highlighting) sạch sẽ, gọn gàng.
4.  **Terminal Panel (Khung console bên dưới)**:
    *   Mô phỏng một terminal tích hợp thực thụ với thanh cuộn tự động và con trỏ nhấp nháy. Ghi nhận logs hệ thống thời gian thực qua kết nối SSE.
5.  **Status Bar (Thanh trạng thái cuối trang)**:
    *   Chiều cao: 22px. Màu xanh dương đặc trưng của VS Code (`#007acc`) hoặc đen tối giản của Cursor (`#121317`).
    *   Hiển thị nhanh các thông số cấu hình: Cổng hoạt động, Tên Model, Trạng thái Proxy Pool.

### Bảng màu đồ họa (IDE Aesthetic Tokens)
*   `--color-text-main`: `#ffffff` (Trắng sáng - Văn bản chính)
*   `--color-text-muted`: `#b7bfc8` (Xám dịu - Chú thích/Tab inactive)
*   `--bg-activity`: `#000000` hoặc `#010000` (Đen tuyền - Activity Bar bên trái)
*   `--bg-sidebar`: `#392d3f` (Xám tím tối - Primary Sidebar)
*   `--bg-editor`: `#121212` (Đen mờ - Vùng soạn thảo chính Editor Area)
*   `--accent-blue`: `#0d1231` (Xanh slate tối - Nút bấm / Chỉ thị)
*   `--accent-purple`: `#390638` (Màu mận chín / Plum - Chỉ thị trạng thái Hoạt động)
*   `--accent-olive`: `#39361e` hoặc `#3c3422` (Xám rêu/Khaki - Chỉ thị trạng thái Cooldown / Hàng đợi)
*   `--accent-red`: `#550102` (Đỏ sậm - Chỉ thị trạng thái Lỗi / Offline)
*   `--border-color`: `#23232b` (Đường kẻ phân tách mỏng 1px)
*   `--font-mono`: `'JetBrains Mono', 'Fira Code', monospace`

---

## 3. Đặc tả API Contract (Backend Endpoints)

Hệ thống API backend phục vụ các thao tác tương tác từ Workspace:

| Endpoint | Method | Trả về | Mô tả |
|----------|--------|---------|-------|
| `/api/dashboard/status` | `GET` | JSON | Trả về dữ liệu cho file ảo `dashboard.json` |
| `/api/dashboard/proxies` | `GET` | JSON | Trả về dữ liệu cho file ảo `proxy_pool.json` |
| `/api/dashboard/config` | `GET` | TOML/JSON | Lấy nội dung cấu hình hiện tại để hiển thị trong file ảo `config.toml` |
| `/api/dashboard/config/save` | `POST` | JSON | Lưu lại cấu hình mới khi lập trình viên chỉnh sửa file `config.toml` |
| `/api/dashboard/events` | `GET` | SSE | Stream log thời gian thực vào tab Terminal tích hợp |
| `/api/dashboard/proxy/restart` | `POST` | JSON | Trigger restart một proxy container |

---

## 4. Các bước triển khai (Execution Steps)

1.  **Rust Embedding**: Nhúng thư mục giao diện Web Workspace tĩnh bằng crate `rust-embed`.
2.  **API Handler Implementation**: Thiết lập thêm handler `/api/dashboard/config` để cho phép đọc và ghi đè file cấu hình TOML trực quan từ giao diện Web.
3.  **Frontend Layout Development**:
    *   Bố trí layout Flexbox/Grid chuẩn mực mô phỏng toàn diện khung của VS Code.
    *   Tích hợp thư viện text-editor mỏng hoặc hiển thị textarea dạng đơn cách với syntax highlighting giả lập.
4.  **Real-time Synchronization**: Lắng nghe thay đổi trạng thái qua SSE và tự động cập nhật cây tệp tin ở Sidebar (ví dụ: chuyển icon từ xanh sang đỏ khi có node offline).
