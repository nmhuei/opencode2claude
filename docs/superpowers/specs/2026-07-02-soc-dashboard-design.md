# SOC Dashboard — Design Proposal

## OpenCode2API Cyber/SOC Operations Console

## 1. Executive Summary

Đề xuất này mô tả thiết kế tổng thể cho **SOC Dashboard** dành cho `OpenCode2API`, định hướng theo phong cách **Cyber/SOC Command Center**: nền tối, dữ liệu realtime, cảnh báo theo mức độ nghiêm trọng, bản đồ luồng hệ thống, log timeline và trạng thái proxy pool.

Mục tiêu của dashboard không phải chỉ là "làm giao diện đẹp", mà là tạo ra một **operations console** giúp người vận hành nhìn nhanh được:

- Bridge server có đang hoạt động ổn định không.
- Request có bị lỗi, nghẽn hoặc tăng bất thường không.
- WARP proxy pool có node nào degraded, cooldown hoặc failover không.
- Token estimation, latency và throughput có đang ở ngưỡng an toàn không.
- Auth, rate limit và structured logs có phát sinh sự kiện rủi ro không.

Repo hiện đã có nền tảng phù hợp cho dashboard như `/health`, structured logging, Prometheus metrics, WARP proxy pool, auth và rate limiting.

## 2. Product Objective

### 2.1. Primary Goal

Thiết kế một **SOC-style dashboard** để giám sát realtime toàn bộ hoạt động của `OpenCode2API` dưới góc nhìn vận hành, bảo mật và độ ổn định hệ thống.

### 2.2. Secondary Goals

- Chuẩn hóa cách hiển thị trạng thái bridge, proxy pool và upstream model.
- Hỗ trợ phát hiện sớm lỗi hệ thống hoặc bất thường.
- Cung cấp một màn hình command center dễ đọc, phù hợp dùng trên desktop hoặc màn hình lớn.
- Tạo nền tảng để sau này mở rộng thành web UI thực thụ.

## 3. Target Users

### 3.1. Primary Users

**System Operator / Developer**

Người chạy `opencode2api` local hoặc server, cần theo dõi bridge, proxy pool, request và log.

### 3.2. Secondary Users

**Security / SOC Analyst**

Người quan sát rủi ro liên quan đến auth, rate limit, upstream failure, proxy failover và request bất thường.

### 3.3. Tertiary Users

**Maintainer / Contributor**

Người phát triển repo, cần dashboard để debug, benchmark, demo hoặc kiểm tra nhanh health của hệ thống.

## 4. Design Direction

### 4.1. Visual Style

Dashboard sử dụng phong cách:

**Cyber / SOC / Command Center**

Đặc điểm nhận diện:

- Dark mode làm nền chính.
- Accent màu cyan, blue, green.
- Màu cảnh báo: yellow, orange, red.
- Card dạng glassmorphism nhẹ.
- Bố cục grid rõ ràng.
- Dữ liệu realtime, log timeline, alert severity.
- Trạng thái hệ thống hiển thị bằng badge/pill.
- Cảm giác kỹ thuật, hiện đại, phù hợp sản phẩm infrastructure/security.

### 4.2. Mood Keywords

- Secure
- Realtime
- Technical
- Operational
- High contrast
- Command center
- Observability-first
- Low-noise, high-signal

## 5. Information Architecture

Dashboard chia thành 6 nhóm nội dung chính:

### 5.1. Command Header

Hiển thị tổng quan:

- Product name: `OpenCode2API SOC Console`
- Bridge status: Online / Offline / Degraded
- Current host/port
- Runtime mode
- Active model
- Auth status
- Last updated time

### 5.2. KPI Overview

Các KPI card quan trọng:

| KPI            | Ý nghĩa                     |
| -------------- | --------------------------- |
| Bridge Health  | Tình trạng server           |
| Request Volume | Tổng request theo thời gian |
| Avg Latency    | Độ trễ trung bình           |
| Token Estimate | Token/cost dự kiến          |
| Proxy Health   | Tình trạng proxy pool       |
| Risk Events    | Số cảnh báo theo severity   |

### 5.3. Network Flow Map

Mô phỏng luồng hệ thống:

`OpenCode CLI → OpenCode2API Bridge → Token Estimator → Casing Resolver → WARP Proxy Pool → Upstream LLM API`

Repo hiện có server route chính như `/v1/messages`, `/v1/messages/count_tokens`, `/v1/models` và `/health`, phù hợp để đưa vào dashboard monitoring.

### 5.4. Alert Severity Panel

Hiển thị danh sách cảnh báo theo mức độ:

- Critical
- High
- Medium
- Low
- Info

### 5.5. Realtime Log Timeline

Dòng sự kiện gần nhất:

- Request accepted
- Token estimation completed
- Proxy node cooldown
- Auth middleware denied
- Upstream request failed
- Health check passed

### 5.6. Proxy Pool Status

Theo dõi WARP proxy pool:

- Primary nodes
- Warm-standby nodes
- Health status
- Latency
- Fail count
- Last check
- Cooldown state

README mô tả WARP pool có primary pool, warm-standby pool, failover, health check và protected standby, nên đây là một module trọng tâm của dashboard.

## 6. Proposed Layout

### 6.1. Desktop Layout

Dashboard desktop nên dùng layout 12-column grid.

```text
┌─────────────────────────────────────────────────────────────┐
│ Header: OpenCode2API SOC Console                         │
│ Bridge Online | Model | Auth | Last Updated                 │
└─────────────────────────────────────────────────────────────┘

┌──────────┬──────────┬──────────┬──────────┐
│ Health   │ Requests │ Latency  │ Risk     │
└──────────┴──────────┴──────────┴──────────┘

┌───────────────────────────────┬─────────────────────────────┐
│ Network Flow Map              │ Alert Severity Panel         │
│ CLI → Bridge → Proxy → LLM     │ Critical / High / Medium     │
└───────────────────────────────┴─────────────────────────────┘

┌───────────────────────────────┬─────────────────────────────┐
│ Realtime Log Timeline         │ Proxy Pool Status            │
│ Structured events             │ Primary / Standby / Health   │
└───────────────────────────────┴─────────────────────────────┘
```

### 6.2. Tablet Layout

- Header full width.
- KPI cards hiển thị 2 cột.
- Network map full width.
- Alerts và logs xếp dọc.

### 6.3. Mobile Layout

- Ưu tiên trạng thái và cảnh báo.
- KPI card xếp 1 cột.
- Network map rút gọn thành flow list.
- Log timeline hiển thị dạng compact.

## 7. Core Screens

### Screen 1 — SOC Command Center

#### Purpose

Màn hình chính để xem toàn bộ tình trạng hệ thống.

#### Main Components

- Header status bar.
- KPI overview.
- Network flow map.
- Alert severity panel.
- Realtime log timeline.
- Proxy pool summary.

#### Key UX Requirement

Người dùng phải trả lời được trong 5 giây:

> "Hệ thống có đang ổn không, nếu không ổn thì lỗi nằm ở đâu?"

### Screen 2 — Proxy Pool Operations

#### Purpose

Theo dõi chi tiết WARP proxy pool.

#### Components

- Proxy node table.
- Node role: Primary / Standby.
- Health state: Healthy / Degraded / Cooldown / Down.
- Latency chart.
- Failover history.
- Protected standby indicator.
- Action buttons: restart primary, view logs, purge primary.

#### Important Note

Các action nhạy cảm như restart/purge phải có confirmation modal để tránh thao tác nhầm.

### Screen 3 — Security & Auth Events

#### Purpose

Giám sát auth, rate limit và các request bất thường.

#### Components

- Auth status.
- Failed auth attempts.
- Rate limit pressure.
- Blocked requests.
- Suspicious IP/session.
- Middleware denied events.
- Severity distribution.

### Screen 4 — Token & Cost Telemetry

#### Purpose

Theo dõi token estimation, latency và cost prediction.

#### Components

- Estimated input tokens.
- Estimated output tokens.
- Confidence score.
- Predicted latency.
- Model comparison.
- Historical token trend.

### Screen 5 — Logs & Incident Timeline

#### Purpose

Hiển thị log có cấu trúc để debug và điều tra sự cố.

#### Components

- Realtime log stream.
- Severity filter.
- Event type filter.
- Search by request/session ID.
- Expandable log detail.
- Copy JSON.
- Export incident report.

## 8. Component Design System

### 8.1. Color Tokens

| Token        | Value                    | Usage                   |
| ------------ | ------------------------ | ----------------------- |
| Background   | `#050914`                | Main background         |
| Panel        | `rgba(12, 22, 38, 0.88)` | Card background         |
| Text Primary | `#E6F6FF`                | Main text               |
| Text Muted   | `#7F94AB`                | Secondary text          |
| Cyan         | `#42D9FF`                | Active state / realtime |
| Green        | `#35F29A`                | Healthy                 |
| Yellow       | `#FFD166`                | Low warning             |
| Orange       | `#FF8C42`                | Medium warning          |
| Red          | `#FF4D6D`                | High/Critical           |
| Violet       | `#A78BFA`                | Secondary accent        |

### 8.2. Typography

| Element       |    Size |  Weight | Usage           |
| ------------- | ------: | ------: | --------------- |
| Page Title    | 32-40px |     800 | Dashboard title |
| Section Title | 13-15px |     700 | Card heading    |
| KPI Number    | 34-44px |     800 | Big metric      |
| Body Text     | 13-15px | 400-500 | Descriptions    |
| Log Text      | 12-13px |     400 | Timeline/logs   |
| Badge Text    | 10-12px |     800 | Severity labels |

### 8.3. Card Style

- Border radius: 18-28px.
- Border: subtle cyan line.
- Background: dark translucent panel.
- Shadow: soft large shadow.
- Optional blur: light glassmorphism.
- Padding: 16-24px.

### 8.4. Status Badges

| Status    | Color         | Meaning       |
| --------- | ------------- | ------------- |
| Online    | Green         | System normal |
| Degraded  | Yellow/Orange | Partial issue |
| Offline   | Red           | Not reachable |
| Protected | Blue/Cyan     | Standby/safe  |
| Unknown   | Gray          | No data       |

### 8.5. Severity Badges

| Severity | Visual            |
| -------- | ----------------- |
| Critical | Red filled badge  |
| High     | Red outline badge |
| Medium   | Orange badge      |
| Low      | Yellow badge      |
| Info     | Cyan badge        |

## 9. Data Requirements

### 9.1. Dashboard Status Contract

Recommended endpoint:

`GET /dashboard/status`

Example response:

```json
{
  "bridge": {
    "status": "online",
    "host": "127.0.0.1",
    "port": 4000,
    "uptime_seconds": 45210,
    "version": "0.3.2"
  },
  "runtime": {
    "model": "auto",
    "auth_enabled": true,
    "shell_policy": "disabled"
  },
  "traffic": {
    "requests_last_minute": 184,
    "requests_last_hour": 8204,
    "error_rate": 0.012,
    "avg_latency_ms": 2.8
  },
  "tokens": {
    "estimated_input": 1847,
    "estimated_output": 2341,
    "confidence": 0.94,
    "predicted_latency_ms": 1300
  },
  "proxy_pool": {
    "enabled": true,
    "healthy": 3,
    "degraded": 1,
    "standby": 2,
    "cooldown": 1
  },
  "alerts": {
    "critical": 0,
    "high": 1,
    "medium": 3,
    "low": 8,
    "info": 22
  }
}
```

### 9.2. Proxy Nodes Contract

Recommended endpoint:

`GET /dashboard/proxies`

```json
{
  "nodes": [
    {
      "id": "warp-1",
      "role": "primary",
      "status": "healthy",
      "latency_ms": 8,
      "fail_count": 0,
      "last_check": "2026-07-02T14:21:08Z"
    },
    {
      "id": "warp-4",
      "role": "standby",
      "status": "protected",
      "latency_ms": 18,
      "fail_count": 0,
      "last_check": "2026-07-02T14:21:08Z"
    }
  ]
}
```

### 9.3. Realtime Events Contract

Recommended endpoint:

`GET /dashboard/events`

```json
{
  "events": [
    {
      "timestamp": "2026-07-02T14:21:08Z",
      "severity": "info",
      "type": "bridge.request.accepted",
      "message": "POST /v1/messages routed successfully",
      "request_id": "req_01"
    },
    {
      "timestamp": "2026-07-02T14:21:12Z",
      "severity": "medium",
      "type": "auth.middleware.denied",
      "message": "Bearer token missing for protected route",
      "request_id": "req_02"
    }
  ]
}
```

## 10. UX States

### 10.1. Normal State

- Bridge online.
- Proxy pool healthy.
- Low error rate.
- No critical alerts.
- Green/cyan dominant visuals.

### 10.2. Degraded State

- Một phần hệ thống gặp vấn đề.
- KPI card chuyển sang yellow/orange.
- Alert panel tự động đưa issue lên đầu.
- Network map highlight node bị ảnh hưởng.

### 10.3. Critical State

- Bridge down hoặc upstream unreachable.
- Header chuyển sang red status.
- Critical alert sticky ở đầu dashboard.
- Hiển thị recommended actions.

### 10.4. Empty State

Khi chưa có dữ liệu:

- Hiển thị "No events yet".
- Không dùng màn hình trống.
- Gợi ý chạy bridge hoặc bật metrics.

### 10.5. Loading State

- Skeleton cards.
- Subtle pulse animation.
- Không dùng spinner quá nhiều.

### 10.6. Error State

- Nêu rõ endpoint nào không đọc được.
- Ví dụ: "Unable to fetch `/metrics`".
- Có nút retry.

## 11. Interaction Design

### 11.1. Global Refresh

- Auto refresh mặc định: 5 giây.
- Có nút pause realtime.
- Có timestamp "Last updated".

### 11.2. Alert Interaction

Khi click alert:

- Mở side panel.
- Hiển thị chi tiết:
  - severity
  - timestamp
  - affected component
  - related logs
  - recommended action

### 11.3. Log Interaction

Log timeline hỗ trợ:

- Filter severity.
- Search event type.
- Copy JSON.
- Expand detail.
- Jump to related proxy/request.

### 11.4. Proxy Interaction

Proxy node hỗ trợ:

- Hover xem latency/fail count.
- Click mở node detail.
- Action chỉ hiện nếu user có quyền.

## 12. Security Considerations

Dashboard có thể hiển thị thông tin nhạy cảm, nên cần:

- Mặc định chỉ bind local.
- Không hiển thị raw API keys.
- Mask token, secret, auth header.
- Cần auth nếu expose ra LAN/public.
- Action nguy hiểm cần confirmation.
- Log viewer phải sanitize nội dung.
- Không lưu secret trong frontend.

## 13. Accessibility Requirements

- Contrast đạt chuẩn WCAG AA.
- Không dùng màu là tín hiệu duy nhất; badge phải có text.
- Font log đủ lớn, tối thiểu 12px.
- Keyboard navigation cho filter, modal, table.
- Focus state rõ ràng.
- Animation có thể giảm nếu user bật reduced motion.

## 14. Technical Implementation Plan

### Phase 1 — Product Planning

Deliverables:

- Final dashboard scope.
- Screen list.
- KPI definition.
- Data source mapping.
- Severity taxonomy.

Output:

- `docs/soc-dashboard-proposal.md`
- `docs/soc-dashboard-data-contract.md`

### Phase 2 — Backend Data Contract

Deliverables:

- Chuẩn hóa endpoint dashboard.
- Mapping `/health`, `/metrics`, proxy pool và structured logs.
- Tạo mock JSON để frontend phát triển độc lập.

Recommended endpoints:

- `/dashboard/status`
- `/dashboard/proxies`
- `/dashboard/events`
- `/dashboard/security`
- `/dashboard/tokens`

### Phase 3 — UI Prototype

Deliverables:

- Wireframe low-fidelity.
- Visual design high-fidelity.
- Component library.
- Responsive layout.
- Empty/loading/error states.

Recommended stack:

- Vite + React
- TypeScript
- CSS variables hoặc Tailwind
- Chart library nhẹ
- SSE/WebSocket optional cho realtime

### Phase 4 — Integration

Deliverables:

- Connect API thật.
- Auto refresh.
- Event polling.
- Log filtering.
- Proxy node detail.
- Alert drilldown.

### Phase 5 — Hardening

Deliverables:

- Auth protection.
- Secret masking.
- Error handling.
- Performance optimization.
- Accessibility pass.
- Documentation.

## 15. MVP Scope

### Must Have

- Bridge status.
- KPI cards.
- Proxy pool summary.
- Alert severity panel.
- Realtime log timeline.
- `/health` integration.
- Static/mock fallback.

### Should Have

- `/metrics` integration.
- Proxy node detail.
- Token estimation stats.
- Auth/rate limit events.
- Responsive layout.

### Could Have

- Interactive network map.
- Incident report export.
- WebSocket/SSE realtime.
- Historical charts.
- Theme switcher.

### Won't Have in MVP

- User management.
- Multi-tenant dashboard.
- Full SIEM replacement.
- Long-term log storage.
- Advanced threat detection engine.

## 16. Acceptance Criteria

Dashboard được xem là đạt yêu cầu khi:

1. Người dùng thấy ngay bridge đang online/offline/degraded.
2. Dashboard hiển thị tối thiểu 4 KPI chính.
3. Proxy pool có trạng thái rõ ràng theo node.
4. Alert được phân loại theo severity.
5. Log timeline hiển thị sự kiện gần nhất.
6. Không có secret/token bị lộ trên UI.
7. Giao diện desktop rõ ràng, không rối.
8. Mobile vẫn xem được thông tin cốt lõi.
9. Có trạng thái loading, empty và error.
10. Có tài liệu data contract cho backend/frontend.

## 17. Recommended Final Direction

Nên triển khai dashboard theo hướng:

**Dark Cyber Command Center + Observability Dashboard + SOC Alert Panel**

Không nên biến dashboard thành một trang admin thông thường. Điểm mạnh của `OpenCode2API` nằm ở bridge, proxy, token estimation, observability và runtime operations, nên dashboard cần ưu tiên:

- realtime health
- request flow
- proxy resilience
- alert severity
- structured logs
- token/cost visibility

Thiết kế cuối cùng nên tạo cảm giác giống một **mini SOC console cho AI bridge infrastructure**: gọn, tối, sắc nét, nhiều tín hiệu vận hành nhưng không bị rối.
