# Design: `server start` tự khởi động standby proxies (40004–40005)

Ngày: 2026-08-03
Trạng thái: đã duyệt (brainstorming, phương án A)

## Mục tiêu

Khi chạy `opencode2api server start` (cả foreground lẫn daemon), bridge phải đảm bảo **cả 5 proxy container** đang chạy: 3 primary (40001–40003) + 2 standby (40004–40005). Hiện tại primary đã được tự xử lý, nhưng standby đang tắt thì bị bỏ qua với cảnh báo.

Nguyên tắc: **protected = không bao giờ bị destroy/purge/stop, nhưng luôn được khởi động**. `docker start`/`docker restart` là thao tác không phá hủy (giữ WARP registration volume), nên được phép trên standby.

## Hành vi mong muốn

| Tình huống (trong `server start`) | Trước | Sau |
|---|---|---|
| Standby 40004/40005 đang tắt | Cảnh báo, không làm gì | `docker start` (resume, giữ volume) |
| Standby chạy lên nhưng verify fail | "no restart attempted" | `docker restart` 1 lần (best-effort) |
| Standby chưa tồn tại | `docker run` (create) | giữ nguyên |
| Standby đang chạy | — | giữ nguyên |
| `server stop` / `server stop --purge` | không đụng standby | **giữ nguyên** |
| `proxy restart` / `proxy purge` | không đụng standby | **giữ nguyên** |
| `server restart` | không đụng proxy | **giữ nguyên** |

## Thay đổi kiến trúc

### 1. `src/docker/types.rs` — port validation cho start/restart

- Thêm `validate_startable_port(port)`: chỉ check port thuộc dải đã biết (40001–40005), cho phép port protected.
- `start_managed` và `restart_managed` chuyển từ `validate_managed_port` → `validate_startable_port`.
- `recreate_managed`, `rotate_managed`, `remove_managed`, `stop_managed` **giữ nguyên** `validate_managed_port` (chặn protected).
- Xóa variant `ContainerSetupState::ProtectedStopped` — đã grep xác nhận chỉ còn 4 chỗ dùng (types.rs:90 định nghĩa, lifecycle.rs:310 trả về, lifecycle.rs:442 test, bootstrap.rs:55 match arm), tất cả nằm trong phạm vi thay đổi này.

### 2. `src/docker/lifecycle.rs` — nhánh protected trong `ensure_proxy`

```text
protected + exists:false            → create_missing → New            (giữ nguyên)
protected + running:true            → Running / ProtectedLegacy       (giữ nguyên)
protected + running:false           → start_managed → Resumed         (THAY ĐỔI)
```

Quy tắc mới: protected mà **tồn tại và đang tắt** thì luôn `docker start`, bất kể volume có đúng hay không (volume không ảnh hưởng việc start).

### 3. `src/docker/bootstrap.rs` — `bootstrap_proxy_pool_with_runtime`

- Bỏ nhánh cảnh báo `ProtectedStopped` (không còn xảy ra).
- Vòng verification: standby offline → gọi `runtime.restart_managed(spec)` kèm cảnh báo `restarting offline standby` (best-effort, lỗi chỉ in cảnh báo và continue — khác primary đang dùng `?`).
- Vòng setup lỗi protected: giữ nguyên (in lỗi, continue, không recover destructive).

## Luồng dữ liệu

```text
opencode2api server start (foreground/daemon)
  → maybe_bootstrap_proxies(no_proxy, quiet)         (app/server.rs, không đổi)
    → bootstrap_proxy_pool → bootstrap_proxy_pool_with_runtime
      → ensure_proxy cho cả 5 port (40001–40005)
          standby tắt → docker start (resume)
      → verify từng proxy qua SOCKS5 → cloudflare.com/cdn-cgi/trace
          primary offline → docker restart (fatal, giữ nguyên)
          standby offline  → docker restart (best-effort)
  → set BRIDGE_PRIMARY_PROXIES + BRIDGE_WARM_STANDBY_PROXIES cho serve child (không đổi)
```

## Xử lý lỗi

- Docker daemon không khả dụng → bỏ qua bootstrap (giữ nguyên).
- Standby start lỗi → in `✗ proxy port ... setup failed`, continue (giữ nguyên).
- Standby restart lỗi sau verify → chỉ cảnh báo, **không abort bootstrap** (standby là failover; hỏng nó không nên chặn bridge chạy với 3 primary).
- Standby vẫn offline sau tất cả → vẫn set env vars; pool routing bỏ qua standby offline như hiện tại.

## Kiểm thử

### Unit (sửa 3, thêm 2)

1. `lifecycle.rs::protected_destructive_actions_do_not_reach_runner` — đổi: `start`/`restart` được phép trên protected; `recreate`/`rotate`/`remove`/`stop` vẫn chặn.
2. `lifecycle.rs::protected_existing_container_is_never_mutated_by_ensure` — đổi: stopped protected → `Resumed` + đúng 1 mutation `start`.
3. `bootstrap.rs::protected_stopped_or_legacy_nodes_are_not_mutated` — đổi: stopped protected → được start; legacy vẫn không đụng.
4. Mới: protected stopped + volume cũ → vẫn `docker start`.
5. Mới: bootstrap verify — standby offline → `restart_managed` được gọi; lỗi restart không abort bootstrap.

`health.rs` tests không đổi (bulk stop/purge không bao giờ chạm standby — đã có test phủ).

### Live

1. `cargo test --lib`, `cargo clippy --lib`, `cargo fmt --check`.
2. `docker stop opencode-warp-4 opencode-warp-5` → chạy bootstrap path (foreground `server start -f` hoặc `start_daemon`) → xác nhận 2 container resumed.
3. `opencode2api proxy status` → 5/5 healthy.
4. `/health/ready` → ready, verified unique exit IPs ≥ 3.
5. Cập nhật `REPO_WORKLOG.md` (snapshot + journal entry).

## Scope ngoài (không đổi)

- `server stop` / `stop_proxy_containers` — chỉ docker-stop primary, không bao giờ chạm standby.
- `proxy restart` / `proxy purge` — chỉ tác động primary.
- `server restart` — không gọi bootstrap.
- Cấu hình mới: không thêm (YAGNI — hành vi mặc định là đúng thứ người dùng cần).
