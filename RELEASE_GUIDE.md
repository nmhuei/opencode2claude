# Hướng dẫn phát hành phiên bản mới (Release Guide)

Tài liệu này lưu trữ quy trình từng bước để phát hành một phiên bản mới của `opencode2api` lên GitHub và Crates.io.

---

## Quy trình 4 bước phát hành

### Bước 1: Cập nhật phiên bản trong code
1. Mở file Cargo.toml.
2. Sửa giá trị `version` trong phần `[package]` (ví dụ từ `0.2.1` lên `0.2.2`).

### Bước 2: Đồng bộ hóa file Cargo.lock
Chạy lệnh sau để Rust tự động đồng bộ hóa phiên bản mới vào file lock:
```bash
cargo check
```

### Bước 3: Commit và Push mã nguồn lên GitHub
Đưa các thay đổi vào git commit và đẩy lên nhánh `main`:
```bash
git add Cargo.toml Cargo.lock
git commit -m "bump: version 0.2.2"
git push origin main
```

### Bước 4: Tạo và đẩy Git Tag (Kích hoạt Tự động hóa)
Tạo một Git Tag khớp với phiên bản mới của bạn. Khi bạn đẩy tag này lên GitHub, GitHub Actions sẽ tự động biên dịch binary Linux cho x86_64 và ARM64, tạo GitHub Release, phát hành Docker image lên ghcr.io và push lên crates.io:
```bash
# Thay thế v0.2.2 bằng phiên bản tương ứng
git tag -a v0.2.2 -m "Release v0.2.2"
git push origin v0.2.2
```

---

## Kiểm tra tiến độ Release
Bạn có thể theo dõi quy trình build và release trực tiếp tại trang GitHub Actions của dự án:
https://github.com/nmhuei/opencode2api/actions
