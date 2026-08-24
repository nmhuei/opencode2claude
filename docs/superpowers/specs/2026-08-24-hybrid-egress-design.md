# Design: Hybrid egress — direct-first startup, proxy-preferred runtime

Ngày: 2026-08-24
Trạng thái: đã duyệt trong brainstorming
Phạm vi rollout đầu tiên: isolated test instance `127.0.0.1:4010`; main `127.0.0.1:4000` không restart hoặc thay route trong giai đoạn kiểm thử.

## 1. Bối cảnh

OpenCode2API hiện có hai mode egress tách biệt:

- `direct`: gateway đi thẳng tới upstream; proxy pool không tham gia routing.
- `proxy`: gateway phụ thuộc proxy pool; nếu không có eligible proxy route thì request fail closed.

`opencode2api server start` hiện gọi proxy bootstrap trước khi supervisor khởi động daemon. Vì vậy Docker/WARP startup, SOCKS verification hoặc một proxy bị treo có thể làm user phải chờ dù direct egress hoàn toàn sử dụng được.

Topology chuẩn hiện tại là **1 primary + 1 protected warm standby**:

```text
primary  = socks5h://127.0.0.1:40001
standby  = socks5h://127.0.0.1:40004
active_proxy_count = 1
```

Live isolated testing trên port `4010` đã chứng minh:

- cả `40001` và `40004` đạt 80/80 SOCKS requests ở concurrency 24;
- model traffic qua primary thành công;
- model burst đạt 24/24 HTTP 200 ở các batch concurrency 4 và 8;
- khi primary test bị giả lập unavailable, verified standby `40004` được chọn và model request vẫn thành công;
- main `4000` không bị ảnh hưởng trong các test trên.

Vấn đề còn lại là startup coupling và proxy readiness semantics: container `Up` không đồng nghĩa proxy đủ điều kiện route.

## 2. Mục tiêu

Thêm `hybrid` egress mode với hành vi:

1. Gateway lên nhanh bằng direct egress, không block trên Docker/WARP bootstrap.
2. Proxy lifecycle và verification chạy background sau khi gateway đã bind/listen.
3. Request mới ưu tiên proxy **chỉ khi proxy đã qua strict verification**.
4. Khi proxy unavailable/degraded/recovering, fresh requests dùng direct ngay thay vì chờ proxy recovery.
5. Khi proxy trở lại READY, fresh requests tự quay về proxy, user không phải restart server hoặc sửa env.
6. Primary/standby vẫn giữ topology 1+1 và protected lifecycle semantics hiện tại.
7. Health/status/dashboard phải tách rõ gateway readiness và proxy readiness.
8. Không dùng route switching để circumvent provider rate limit, quota, account restriction hoặc application-level errors.
9. Background proxy startup/recovery phải bounded: có timeout, backoff và observable state; không được treo vô hạn hoặc busy-loop.

## 3. Non-goals

- Không load-balance direct và proxy theo phần trăm.
- Không đổi egress giữa các retry của cùng request chỉ để tìm quota/rate-limit tốt hơn.
- Không mutate host-wide WARP (`warp-cli`).
- Không cho phép direct fallback trong strict `proxy` mode hiện có.
- Không đổi semantics protected standby: không purge/remove/recreate standby tự động.
- Không restart main `4000` trong giai đoạn implementation/test ban đầu.
- Không yêu cầu 2 public exit IP khác nhau để gateway hybrid hoạt động; duplicate exit làm giảm redundancy và phải được báo degraded, nhưng direct path vẫn giữ gateway available.

## 4. Egress modes sau thay đổi

```rust
pub enum EgressMode {
    Direct,
    Proxy,
    Hybrid,
}
```

### `direct`

Giữ nguyên:

- không tạo live proxy routing pool;
- không spawn proxy lifecycle workers cho request routing;
- mọi upstream request dùng direct client.

### `proxy`

Giữ fail-closed semantics hiện tại:

- proxy pool bắt buộc usable;
- không có eligible proxy route => request error;
- direct fallback vẫn bị security validation từ chối.

### `hybrid`

Mode mới:

- direct client luôn sẵn sàng ngay khi server start;
- proxy pool + workers được tạo nếu có proxy config;
- proxy bootstrap/reconciliation chạy background;
- proxy chưa READY => route direct;
- proxy READY => fresh requests route qua proxy;
- proxy mất READY => fresh requests route direct;
- proxy recover READY => fresh requests tự route proxy trở lại.

## 5. Startup architecture

### Trước

```text
server start
  -> maybe_bootstrap_proxies()       [blocking]
       -> inspect/create/start Docker
       -> verify proxy
  -> supervisor.start()
  -> gateway listens
```

### Sau trong `hybrid`

```text
server start
  -> resolve config
  -> supervisor.start()
  -> gateway binds/listens immediately
  -> AppState starts direct-capable request path
  -> background proxy bootstrap/reconcile worker
       -> inspect Docker
       -> ensure permitted containers
       -> transport verify
       -> identity verify
       -> route verify
       -> publish proxy state READY or DEGRADED
```

`direct` và strict `proxy` không bị silently đổi semantics. Có thể refactor bootstrap primitives dùng chung, nhưng behavior compatibility của hai mode phải có regression tests.

## 6. Proxy lifecycle state machine

Thêm explicit runtime state cho proxy subsystem, độc lập với per-node `HealthState`/`CircuitState`:

```text
Disabled
   |
   v
Starting
   |
   v
TransportVerifying
   |
   v
IdentityVerifying
   |
   v
RouteVerifying
   |
   +------ PASS ------> Ready
   |
   +------ FAIL ------> Degraded
                           |
                     bounded backoff
                           |
                           +----> Starting
```

Các state tối thiểu:

- `Disabled`: mode không dùng proxy hoặc không có proxy config.
- `Starting`: đang inspect/start allowed proxy containers.
- `TransportVerifying`: đang verify SOCKS/TCP/HTTP connectivity.
- `IdentityVerifying`: đang verify WARP signal + public exit identity.
- `RouteVerifying`: đang chạy một end-to-end routing probe qua proxy candidate.
- `Ready`: ít nhất một eligible route đạt full verification policy.
- `Degraded`: proxy subsystem không usable nhưng hybrid vẫn phục vụ direct.

State transition phải timestamped và có `last_error`/`last_success` observable nhưng secret-safe.

## 7. Strict verification gates

Một proxy không được route chỉ vì Docker báo `running`.

### Gate A — process/container

Pass khi:

- expected container tồn tại;
- container đang running;
- không OOM-killed;
- port mapping đúng;
- protected/managed lifecycle policy khớp topology.

Mọi Docker call phải có timeout; Docker CLI treo không được block gateway startup.

### Gate B — transport

Pass khi:

- SOCKS port accept kết nối;
- HTTP(S) request thực tế qua SOCKS thành công;
- DNS semantics dùng `socks5h` và không bypass proxy ngoài ý muốn.

### Gate C — WARP identity

Pass khi:

- Cloudflare trace xác nhận `warp=on`;
- identity endpoints đạt consensus theo logic hiện có;
- public IP parse hợp lệ;
- identity timestamp fresh trong configured TTL.

### Gate D — duplicate handling

- duplicate exit không được tính thành independent redundancy;
- chỉ deterministic owner của một exit IP được normal-routable;
- duplicate standby có trạng thái transport healthy nhưng redundancy degraded;
- status/dashboard phải phân biệt `transport_healthy` và `unique_exit_healthy`.

### Gate E — route probe

Trước khi subsystem chuyển `Ready`, một real HTTP route probe qua selected proxy phải thành công sau khi identity verify. Mục tiêu là bắt trường hợp SOCKS connect được nhưng upstream path thực tế unusable.

Probe không được gửi user payload hoặc credential nhạy cảm; dùng endpoint benign/identity hiện có hoặc một lightweight controlled upstream probe.

## 8. Timeout và backoff

Không operation nào được chờ vô hạn.

Initial defaults đề xuất:

| Stage | Timeout |
|---|---:|
| Docker inspect/start | 8s |
| SOCKS/TCP connectivity | 3s |
| HTTP-through-SOCKS verify | 5s |
| Identity consensus | 10s |
| Route verification | 10s |
| One full bootstrap attempt | <= 30s wall-clock target |

Background retry dùng exponential backoff + jitter, ví dụ:

```text
2s -> 5s -> 10s -> 30s -> 60s -> cap 120s
```

Sau một success, backoff reset.

Yêu cầu:

- không busy-loop;
- không log cùng một failure mỗi health tick ở WARN nếu không có state transition;
- repeated identical failures được rate-limit/coalesce trong logs;
- worker heartbeat vẫn cập nhật trong thời gian backoff;
- cancellation token/shutdown phải interrupt timeout/backoff nhanh.

## 9. Route selection semantics

### Fresh request trong hybrid

```text
if proxy_subsystem == Ready
    && pool has eligible verified route:
        choose proxy route
else:
        choose direct route
```

Không chờ tối đa 30 giây như strict proxy selection hiện tại khi hybrid không có eligible route. Hybrid fallback direct phải gần như immediate.

### In-flight request stability

Một attempt đã chọn route nào thì giữ route đó theo retry policy hiện tại, trừ transport failure classification được design riêng dưới đây. Không đổi route chỉ vì một worker background vừa chuyển state.

### Transport failure

Cho fresh retry sau genuine proxy transport failure:

1. đánh dấu node/pool failure theo logic hiện tại;
2. ưu tiên another eligible proxy (standby) nếu có;
3. nếu hybrid không còn eligible proxy, route direct cho **transport recovery only**.

Phải giữ correlation/history cho biết attempt nào là `proxy`, `standby`, hoặc `direct-hybrid-fallback`.

### Provider/application failure

Các response như provider 429/quota/account restriction/application 4xx không được trigger proxy -> direct switch để né limit.

Rules:

- provider rate-limit penalty tiếp tục được record theo route hiện tại;
- không rotate proxy/direct chỉ vì 429;
- không dùng direct như quota bypass;
- model fallback và retry budget vẫn tuân theo policy hiện tại.

## 10. Background reconciliation

Hybrid cần một worker chịu trách nhiệm đảm bảo topology mong muốn mà không block startup.

Responsibilities:

1. discover/inspect configured 1+1 containers;
2. start permitted stopped containers;
3. không destructive-mutate protected standby;
4. run staged verification;
5. publish subsystem state;
6. enqueue managed primary recovery khi cần;
7. preserve standby as protected;
8. retry bounded/backoff;
9. recover từ Docker daemon unavailable sau này mà không restart gateway.

Existing `proxy-health`, `proxy-identity`, `proxy-restart` workers được reuse/refactor nơi hợp lý; tránh tạo hai worker cùng ownership một lifecycle mutation.

## 11. Readiness / liveness contract

### `/health/live`

Không đổi: process/event loop sống => 200.

### `/health/ready`

Trong hybrid, gateway ready khi:

- critical gateway workers healthy;
- **ít nhất direct egress path operational theo local state**.

Proxy degraded không làm toàn gateway 503.

Response mở rộng:

```json
{
  "status": "ready",
  "checks": {
    "critical_workers": true,
    "egress": true,
    "proxy": false
  },
  "egress": {
    "mode": "hybrid",
    "active_route": "direct",
    "direct": {
      "ready": true
    },
    "proxy": {
      "state": "starting",
      "ready": false,
      "verified_unique_exit_ips": 0,
      "last_error": null
    }
  }
}
```

Khi proxy READY:

```json
{
  "egress": {
    "mode": "hybrid",
    "active_route": "proxy",
    "direct": { "ready": true },
    "proxy": {
      "state": "ready",
      "ready": true,
      "verified_unique_exit_ips": 1
    }
  }
}
```

`active_route` mô tả preference cho fresh requests tại thời điểm snapshot; không khẳng định mọi in-flight request dùng cùng route.

## 12. Status, metrics, history và dashboard

Thêm metrics tối thiểu:

- `egress_direct_requests`
- `egress_proxy_requests`
- `egress_hybrid_fallbacks`
- `proxy_bootstrap_attempts`
- `proxy_bootstrap_successes`
- `proxy_bootstrap_failures`
- `proxy_state_transitions`
- `proxy_route_probe_failures`
- `proxy_duplicate_exit_events`

Status/node view cần hiển thị:

- mode: `direct | proxy | hybrid`;
- subsystem state;
- preferred active route;
- primary/standby topology;
- transport health;
- identity health;
- unique exit count;
- duplicate relationship;
- last verification timestamp;
- last transition/error;
- current recovery backoff/deadline.

History attempt cần ghi route loại nào được dùng:

```text
proxy_node = opencode-warp-1 | opencode-warp-4 | null
route_kind = proxy | standby | direct | direct-hybrid-fallback
```

Nếu schema migration cho `route_kind` quá invasive, phase đầu có thể derive từ `proxy_node` + explicit event metadata, nhưng final implementation phải có cách query đáng tin cậy.

## 13. Configuration

`BRIDGE_EGRESS_MODE` nhận:

```text
direct
proxy
hybrid
```

Sau rollout và live verification, mục tiêu UX là `hybrid` trở thành recommended/default user mode. Việc đổi default trong loader chỉ thực hiện sau test 4010 và regression suite; không silently đổi main running instance.

Config bổ sung đề xuất:

```text
BRIDGE_PROXY_BOOTSTRAP_TIMEOUT_SECS=30
BRIDGE_PROXY_VERIFY_TIMEOUT_SECS=10
BRIDGE_PROXY_RECOVERY_BACKOFF_MAX_SECS=120
```

Không thêm flag nếu existing timeout knobs có thể reuse rõ nghĩa; ưu tiên ít config và default an toàn.

`--no-proxy` vẫn force `direct` và không bootstrap/mutate Docker.

## 14. Security / safety invariants

- Hybrid direct fallback chỉ phục vụ availability/transport failure.
- Không route-switch để bypass provider quota/rate limit.
- Host WARP không bao giờ bị mutate.
- Public bind security rules giữ nguyên.
- Protected standby không bị remove/purge/recreate tự động.
- Probe payload không chứa user prompt, API secret hoặc conversation data.
- Log/status không lộ proxy credential hoặc auth token.
- Docker/proxy worker failure không được crash gateway request plane.

## 15. Compatibility

- Existing `direct` tests phải tiếp tục pass nguyên semantics.
- Existing strict `proxy` fail-closed tests phải tiếp tục pass.
- `allow_direct_fallback` legacy config không được tự trở thành hybrid; hybrid là explicit mode.
- Existing `.env`/TOML dùng `direct` hoặc `proxy` vẫn parse như cũ.
- `opencode2api server start --no-proxy` giữ behavior cũ.
- Shell integration `opencode2api set env` không cần user thêm proxy-specific exports để dùng hybrid default.

## 16. Test strategy — TDD

Implementation bắt đầu bằng failing tests.

### Config tests

1. `hybrid` parse thành `EgressMode::Hybrid`.
2. direct/proxy parse không regression.
3. `--no-proxy` vẫn override thành direct.
4. strict proxy + direct fallback vẫn bị security reject.
5. hybrid config với 1+1 hợp lệ.

### State/worker tests

1. hybrid tạo direct client + proxy pool/workers.
2. direct không tạo live proxy pool.
3. proxy giữ strict behavior.
4. proxy worker failure không stop request worker registry.
5. background bootstrap state transitions đúng thứ tự.

### Startup tests

1. Docker bootstrap fake sleep 60s; gateway start path không đợi 60s.
2. Docker unavailable; gateway hybrid vẫn start/direct-ready.
3. proxy sau đó recover; subsystem đổi `Degraded -> ... -> Ready` không restart gateway.
4. cancellation/shutdown interrupt bootstrap/backoff.

### Verification tests

1. container running nhưng SOCKS dead => không READY.
2. SOCKS TCP live nhưng HTTP-through-proxy fail => không READY.
3. transport pass nhưng `warp != on` => không READY.
4. identity endpoint disagreement => không READY.
5. stale identity => không READY.
6. route probe fail => không READY.
7. duplicate exit => chỉ owner routable; redundancy degraded.
8. full staged pass => READY.

### Routing tests

1. hybrid proxy Starting => direct.
2. hybrid proxy Degraded => direct.
3. hybrid proxy Ready => proxy.
4. primary unavailable + verified standby => standby.
5. no eligible proxy after proxy transport failure => direct-hybrid-fallback.
6. provider 429 => không switch route để bypass.
7. strict proxy unavailable => fail closed, không direct fallback.
8. in-flight route ownership/lease giữ nguyên qua stream lifetime.

### Readiness/observability tests

1. hybrid + proxy down => `/health/ready` 200 với proxy degraded.
2. strict proxy + proxy down => `/health/ready` 503 như hiện tại.
3. hybrid + proxy ready => active route `proxy`.
4. metrics increment đúng direct/proxy/fallback/bootstrap.
5. state transition logs bounded, không spam mỗi tick.

## 17. Isolated live rollout plan

Không chạm main `4000` cho tới khi tất cả bước 4010 pass.

### Phase A — build/test local

- `cargo fmt --check`
- targeted tests mới
- `cargo test --lib`
- relevant integration/protocol tests
- `cargo clippy --lib`

### Phase B — isolated `4010`

Runtime riêng:

```text
port = 4010
runtime_dir = ~/Downloads/bqa/opencode2api-hybrid-test-4010/runtime
history = ~/Downloads/bqa/opencode2api-hybrid-test-4010/history.sqlite3
primary = 40001
standby = 40004
mode = hybrid
```

Không bootstrap/restart proxy container từ một second competing instance nếu có nguy cơ hai runtime cùng ownership lifecycle. Live test phải dùng một explicit test ownership mode hoặc fake/unmanaged proxy lifecycle adapter để tránh 4010 và 4000 cùng restart `40001`.

### Phase C — failure injection trên 4010

Không stop proxy container đang phục vụ workload main.

Dùng safe failure injection:

- fake dead primary URL;
- delayed fake Docker adapter;
- fake stale/duplicate identities;
- isolated test proxy/container nếu cần lifecycle destructive test.

Verify:

1. gateway usable direct trước proxy READY;
2. proxy READY tự động được preferred;
3. primary fail => standby;
4. all proxy fail => direct;
5. proxy recover => proxy preferred trở lại;
6. 429 không route-switch;
7. no worker hangs/leaks.

### Phase D — soak

Chạy 4010 hybrid soak nhiều giờ với synthetic traffic:

- concurrent streaming + sync requests;
- periodic proxy probe failures;
- inspect memory/FD/task growth;
- verify no unbounded log growth pattern;
- verify no restart thrash;
- verify direct fallback latency thấp.

### Phase E — promotion

Chỉ sau khi 4010 pass:

1. build/install release binary;
2. cập nhật docs/default config nếu quyết định promotion;
3. main 4000 **không tự restart**;
4. báo user explicit command/time để chuyển main khi họ chủ động cho phép;
5. sau promotion verify health, model, route, Claude Code smoke và rollback path.

## 18. Acceptance criteria

Thiết kế hoàn thành khi implementation chứng minh được:

```text
Docker/WARP startup treo/chậm
=> gateway hybrid vẫn usable bằng direct trong vài giây.

Proxy chưa qua full staged verification
=> tuyệt đối không được route user request.

Proxy READY
=> fresh requests tự ưu tiên proxy.

Proxy mất READY
=> fresh requests tự fallback direct, không chờ recovery timeout.

Proxy recover
=> fresh requests tự quay proxy, không restart server.

Primary fail + standby verified
=> standby được dùng.

All proxies fail
=> direct tiếp tục phục vụ.

Provider 429/quota
=> không switch egress để circumvent limit.

Background recovery
=> bounded timeout + exponential backoff + cancellation; không hang/busy-loop/restart-thrash.

Main 4000
=> không bị thay đổi/restart trong isolated implementation/test phase.
```

## 19. Rollback

Rollback đơn giản và explicit:

- set `BRIDGE_EGRESS_MODE=direct` để bỏ proxy khỏi route plane;
- hoặc set `BRIDGE_EGRESS_MODE=proxy` để quay về strict fail-closed proxy behavior;
- không cần xóa proxy containers;
- không mutate host WARP;
- release promotion phải giữ previous binary backup theo installer/update convention hiện tại.
