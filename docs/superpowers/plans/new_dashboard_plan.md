# New Dashboard Design & Implementation Plan

We are redesigning the dashboard from scratch. Based on the user's feedback, the UI should look like a **normal, professional web console**, not an IDE or a terminal simulation. It must have high visual aesthetics (dark mode, clean typography, vibrant neon/gold gradients), a robust routing path setup to prevent layout breaking, and include the core features of `ds2api` (Secure Login, Basic Config Form, and API Test stream panel with DeepSeek-style reasoning).

---

## 1. Core Visual Architecture (Aesthetics & Layout)

Instead of side-by-side IDE structures, the dashboard will follow a modern **Admin Panel Layout**:
- **Fixed Sidebar (Left)**: Width of `260px`, very dark matte `#0c0c0e` background. Contains brand info, primary views switcher, system online status with a pulsing neon green dot, basic uptime/version metrics, and a "Sign Out" button.
- **Main Scrollable Viewport (Right)**: Space gray `#121216` background. Max content width restricted to `1200px` for optimal readability.
- **Unified Color Theme**:
  - Background: Black/Dark-Gray (`#0c0c0e` and `#121216`)
  - Primary Accent: Neon Golden/Amber (`#f59e0b` and `#d97706`)
  - Status Indicators: Emerald Green (`#10b981`), Ruby Red (`#ef4444`), Slate Gray (`#71717a`)
  - Cards: Dark surface `#18181f` with subtle 1px border `#27272a`.
- **Absolute Static Asset Resolution**:
  - The HTML must resolve assets using absolute routes:
    - `/dashboard/style.css` instead of `style.css`
    - `/dashboard/app.js` instead of `app.js`
  - This guarantees that visiting `http://127.0.0.1:4000/dashboard` or `http://127.0.0.1:4000/dashboard/` will load CSS and JS correctly without getting 404 errors.

---

## 2. Integrated Features & Workflows

### Phase A: Secure Gatekeeper (Đăng nhập)
- **Automatic Discovery**: Upon loading, the client fetches the `/api/dashboard/status`.
- **Login Screen**: If `admin_token_configured` is true and the browser does not have a valid token stored in `localStorage`:
  - The main app layout is blurred and disabled.
  - A clean, centered Login card is displayed asking for the "Admin Authorization Token".
  - Submitting sends a POST request to `/api/dashboard/login` containing the `X-Dashboard-Token` header.
  - If successful, the token is saved, the overlay vanishes, and the app fetches config, proxies, and starts the SSE event logs channel.

### Phase B: Basic Configuration Editor (Đổi model config cơ bản)
- Provides a clean, styled HTML Form for basic inputs:
  - **Server Settings**: Bind Host (IP), Bind Port, Default Model Identifier, and Shell Policy dropdown.
  - **Search Fallbacks**: Inputs for Tavily, Exa, Serper keys (rendered as password fields), and SearXNG instance URL.
  - **Auth Tokens**: Input field for Bearer tokens.
- Automatic TOML generator: Submitting the form collects all values, formats them into a clean, annotated TOML file, and posts them to `/api/dashboard/config/save`.
- Offers a toggle button to switch to "Advanced Mode" (Raw TOML text editor) for advanced power users.

### Phase C: API Tester (Lựa chọn test model)
- A specialized view featuring a two-column playground:
  - **Control Column (Left)**: Input or select model (pre-filled with default model), temperature, max tokens, and prompt text area.
  - **Output Column (Right)**: Displays the model's streamed response.
- **DeepSeek Reasoning Delta support**:
  - Detects incoming thinking/reasoning chunks (`thinking_delta` or within `<thinking>` tags).
  - Renders them in a clean, yellow-bordered, shaded box labeled **"Thinking Process"** that can be collapsed or expanded.
- **Latency & Speed Indicators**:
  - Measures TTFT (Time-to-first-token) in milliseconds.
  - Displays total elapsed time upon response completion.

---

## 3. Implementation Checklist

- [ ] Overwrite `src/webui/index.html` with absolute asset routing and structural layout.
- [ ] Overwrite `src/webui/style.css` with non-IDE clean card and form styles.
- [ ] Overwrite `src/webui/app.js` with login flow, config builder, and SSE API tester.
- [ ] Rebuild Rust binary to bundle new assets.
- [ ] Test in browser at `/dashboard` and `/dashboard/`.
