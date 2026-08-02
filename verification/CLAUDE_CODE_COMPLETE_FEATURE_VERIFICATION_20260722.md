# Claude Code Complete Feature Inventory & Verification — 2026-07-22

## 1. Mục đích

Đây là tài liệu gốc để kiểm tra tương thích Claude Code cho repo:

```text
/home/light/GitHub/opencode2claude
```

Tài liệu này hợp nhất:

- các audit và capture Claude Code trước đây;
- toàn bộ bề mặt CLI được phát hiện từ Claude Code đang cài trên máy;
- các tính năng bridge liên quan trực tiếp đến Claude Code;
- kết quả verify sau khi sửa parser ngày 2026-07-22;
- các tính năng chỉ mới kiểm tra cú pháp/help, chưa được phép gọi là integration-tested;
- checklist bắt buộc phải chạy lại sau mỗi thay đổi parser, mapper, streaming hoặc tool handling.

Không được dùng cụm “đã verify toàn bộ Claude Code” nếu không kèm mức verify. Claude Code có nhiều tính năng cloud, trình duyệt, IDE và side effect nằm ngoài protocol bridge. Tài liệu này phân biệt rõ các mức bằng chứng bên dưới.

## 2. Quy ước trạng thái

| Trạng thái | Ý nghĩa |
|---|---|
| **LIVE REAL CLI VERIFIED** | Đã gọi Claude Code thật, model thật qua bridge release đang chạy và kiểm tra kết quả/tool side effect. |
| **WIRE/CAPTURE VERIFIED** | Đã chạy Claude Code thật tới capture server và kiểm tra request/header/body thực tế. Không chứng minh chất lượng model hoặc tool execution. |
| **UNIT/PROTOCOL VERIFIED** | Đã kiểm tra bằng Rust unit, router fixture, SSE fixture, fuzz hoặc protocol conformance. |
| **HELP/DISCOVERY VERIFIED** | `--help` parse thành công và feature hiện diện trong binary Claude Code đang cài. Chưa chứng minh workflow chạy end-to-end. |
| **PREVIOUS LIVE VERIFIED** | Đã verify bằng CLI/browser thật ở audit trước; vòng 2026-07-22 không chạy lại toàn bộ scenario đó. |
| **NOT INTEGRATION VERIFIED** | Chỉ được inventory hoặc nằm ngoài phạm vi local bridge hiện tại. |

## 3. Môi trường hiện tại

| Thành phần | Giá trị |
|---|---|
| Ngày verify | `2026-07-22` |
| Claude Code | `2.1.217` |
| OpenCode | `1.18.4` |
| OpenCode2API | `0.5.0` |
| Bridge | `http://127.0.0.1:4000` |
| PID sau restart cuối | `1397179` |
| Model pin | `opencode/deepseek-v4-flash-free` |
| Claude model profile gửi vào bridge | `claude-sonnet-4-6` |
| Claude Code integration key | `opencode-bridge` |
| Context cấu hình | `200000` tokens |
| Max output cấu hình | `128000` tokens |
| Fixed thinking đã test | `127000` tokens |
| Auto compact | `90%` của cửa sổ cấu hình |

Bridge hiện bind loopback. `opencode-bridge` là integration credential riêng của Claude Code và không phụ thuộc vòng đời application key trên dashboard. Không được expose cấu hình fallback tĩnh này ra `0.0.0.0` nếu chưa harden auth/TLS.

Lưu ý: `server status` hiện vẫn có thể hiển thị `auth_enabled=false` vì field này phản ánh legacy auth config, trong khi runtime middleware vẫn nhận integration key riêng và managed API-key registry. Hành vi thực tế đã được kiểm tra bằng `/v1/models` và Claude Code CLI.

## 4. Kết luận vòng verify hiện tại

| Nhóm | Kết quả |
|---|---:|
| Claude Code CLI help surface | **44/44 PASS** |
| Top-level options inventoried | **69** |
| Top-level commands inventoried | **12** |
| Claude request-shape audit | **22/22 PASS** |
| Real-model Claude Code matrix | **16/16 PASS** |
| Real CLI core tool matrix | **11/11 PASS** |
| Subagent + WebSearch streaming | **11/11 checks PASS** |
| Stream parser regression suite | **35/35 PASS** |
| Rust test thực thi toàn repo | **474 PASS** |
| Clippy `-D warnings` | **PASS** |
| Release build | **PASS** |
| Health + model discovery + final CLI smoke | **PASS** |
| Real quoted Bash after final restart | **PASS** |

Một system test WARP thật vẫn `ignored` theo thiết kế vì cần local SOCKS proxies 40001–40003 và Internet. Nó không được tính vào 474 test thực thi.

## 5. Các lỗi parser và streaming được sửa trong vòng này

### 5.1 Thinking bị lặp

#### Hiện tượng

Claude Code hiển thị nhiều đoạn reasoning gần giống nhau, ví dụ model liên tục lặp lại:

- “The user approved the design. Now I need to…”;
- cùng một danh sách bước;
- cùng lời tuyên bố “let me first…” nhưng chưa gọi tool.

#### Chẩn đoán thực tế

Raw SSE trước sửa:

```text
thinking_delta chunks: 932
thinking characters: 4089
paragraphs: 27
final result: FINAL_OK
```

Kiểm tra `/v1/chat/completions` raw qua bridge cho thấy upstream model tự sinh phần reasoning lặp. Bridge không phát lại một cumulative snapshot hoặc nhân đôi nguyên chunk SSE.

#### Cách sửa

Trong `src/opencode/mapper/request.rs`, bridge thêm reasoning-hygiene system guidance chỉ khi đồng thời:

- request đang streaming;
- model map sang DeepSeek V4 Free;
- thinking được bật rõ ràng.

Guidance yêu cầu model:

- không restart/restatement cùng kế hoạch;
- mỗi kế hoạch chỉ nêu một lần;
- sau khi quyết định thì hành động ngay;
- không lặp lại câu chuẩn bị gọi tool.

Không tắt thinking, không cắt reasoning token và không áp dụng cho model khác.

#### Kết quả sau sửa

```text
thinking_delta chunks: 89
thinking characters: 376
paragraphs: 4
near-duplicate paragraph pairs: 0
final result: FINAL_OK
```

Regression tests:

- `deepseek_v4_free_streaming_thinking_adds_reasoning_hygiene`
- `deepseek_v4_free_without_streaming_thinking_skips_reasoning_hygiene`

### 5.2 Bash tool marker có quote không escape bị leak ra màn hình

#### Hiện tượng

Free model có thể trả compatibility marker như sau:

```text
[Requesting Tool execution: 'Bash' with arguments: {"command":"echo "hello"", ...}]
```

Dấu `"` bên trong shell command bị model xuất thành quote literal, cộng thêm raw newline. JSON strict không parse được nên marker bị hiển thị như text thay vì trở thành Anthropic `tool_use`.

#### Cách sửa parser

`src/opencode/forward/common.rs` hiện dùng hai tầng:

1. **Fast path:** giữ nguyên hành vi cho JSON hợp lệ; không sửa hoặc biến đổi payload đúng.
2. **Recovery path:**
   - thử các vị trí `]` có thể là cuối marker;
   - repair quote literal bên trong JSON string;
   - escape raw control characters;
   - chỉ chấp nhận candidate parse thành đúng một JSON value hoàn chỉnh;
   - bracket nằm trong array hoặc shell command không được nhầm là cuối marker.

Quy tắc quote recovery hiện theo dõi ngữ cảnh container JSON. Quote của object key chỉ đóng trước `:`, còn quote của string value chỉ đóng trước `,`, `}`, `]` hoặc EOF. Vì vậy nội dung Write chứa ví dụ như `{"success":true}` được giữ nguyên thay vì bị hiểu nhầm là cấu trúc JSON bên ngoài.

#### Regression tests

- `compat_marker_parser_repairs_unescaped_shell_quotes`
- `malformed_bash_compat_marker_becomes_tool_use_without_leaking_marker`
- toàn bộ stream parser suite: **35/35 PASS**.

#### Verify bằng Claude Code thật

Case mới `bash_quoted_multiline` trong `scripts/manual_verify_tool_calls.py`:

```text
Tool use: Bash
Tool result: received
Final: REAL_QUOTED_BASH_OK
Raw compatibility marker visible: NO
```

Final smoke sau rebuild/restart:

```text
Final: FINAL_QUOTED_BASH_OK
Tool use: Bash
Raw compatibility marker visible: false
```

Evidence:

```text
artifacts/final-claude-smoke/summary.json
artifacts/final-claude-smoke/bash.jsonl
artifacts/tool-call-manual/summary.json
```

### 5.3 Invalid shell/regex escapes, detached fields và marker fail-closed

Quét toàn bộ Claude project history hiện có tìm được 44 assistant text blocks chứa compatibility marker. Các dạng lỗi thực tế gồm:

```text
33 Bash marker: Invalid \\escape
3 Bash marker: detached/extra fields after argument object
1 Write marker: unescaped JSON example inside a long content string
```

Root cause chính của case ANSER là command thực tế trong session chứa:

```text
grep -rn 'rq\|RQ\|Queue\|enqueue\|redis' ...
```

`\|`, `\.`, `\(` và `\;` có nghĩa trong shell/regex nhưng không phải JSON escape hợp lệ. Recovery parser hiện chỉ nhân đôi backslash trên wire đối với escape không hợp lệ, để serde decode trở lại đúng command gốc. Các JSON escape hợp lệ vẫn đi qua fast path không bị sửa.

Dạng field bị lệch như sau cũng được phục hồi:

```text
{"command":"...","description":"useful"}, "description": null}
```

Suffix phải tự parse được thành object fragment. Field non-null trong object gốc được ưu tiên, nên duplicate `null` không xóa description hữu ích.

Các marker incomplete hoặc malformed tại EOF không còn được phát raw ra Claude Code. Marker vượt giới hạn 64 KiB chuyển sang fail-closed discard mode cho phần còn lại của channel và chỉ hiện placeholder:

```text
[Incomplete tool request omitted]
[Oversized tool request omitted]
```

Regression coverage mới:

- `compat_parser_recovers_detached_duplicate_fields`
- `compat_parser_repairs_unescaped_json_example_inside_write_content`
- `parses_exact_bash_and_read_markers_without_space_after_arguments_colon`
- `exact_bash_then_read_markers_become_two_tool_uses_without_leaking_text`
- `historical_bash_read_markers_survive_every_two_chunk_split`
- `incomplete_compat_marker_is_redacted_at_eof`
- `oversized_compat_marker_is_fail_closed_without_leaking_remainder`

Manual ANSER case:

```text
case: bash_regex_then_read_anser
Bash input contains: rq\|RQ\|Queue\|enqueue\|redis
Read input: /tmp/ANSER/core/automation_engine.py
Final: RQ_REGEX_READ_OK
Raw marker: false
Result: PASS
```

### 5.4 Claude Code TUI chỉ render thinking khi block đóng

Đo ba tầng xác nhận bridge không buffer toàn bộ response:

- upstream SSE được nhận theo chunk;
- Anthropic `thinking_delta`/`text_delta` được enqueue liên tục;
- Claude Code `stream-json` nhận delta theo thời gian.

Tuy nhiên interactive TUI giữ một thinking block đang mở ở dạng spinner và chỉ bung nội dung khi có `content_block_stop`. Bridge hiện segment reasoning thành các thinking block tối đa khoảng 384 byte. Nội dung, thứ tự và accumulated reasoning không đổi; chỉ lifecycle block được chia nhỏ để TUI có thể render phần đã hoàn tất sớm hơn.

TTY thực tế trước segmentation:

```text
screen-reader: reasoning đầu tiên xuất hiện ở 4724.819 ms
normal TUI:    reasoning đầu tiên xuất hiện ở 3989.510 ms
```

TTY thực tế trên release cuối:

```text
screen-reader: block reasoning đầu xuất hiện ở 4306.408 ms,
               block tiếp theo ở 4649.222 ms
normal TUI:    block reasoning đầu xuất hiện ở 3887.818 ms,
               model vẫn tiếp tục thinking/render sau đó
```

Điểm quan trọng không phải tổng latency model, vì mỗi inference dao động, mà là release cuối đã hiển thị một reasoning block trong khi response vẫn tiếp tục thay vì chờ toàn bộ reasoning kết thúc.

Regression:

- `long_reasoning_is_segmented_for_interactive_rendering`

Evidence:

```text
artifacts/parser-real-repro/tty-real-profile-before-segmentation.json
artifacts/parser-real-repro/tty-real-profile-after-segmentation.json
artifacts/parser-real-repro/tty-real-profile-final-release.json
artifacts/parser-real-repro/tty-screen-reader-raw.txt
artifacts/parser-real-repro/tty-normal-raw.txt
```

## 6. Inventory toàn bộ bề mặt Claude Code CLI 2.1.217

Script discovery:

```text
scripts/audit_claude_code_surface.py
```

Kết quả:

```text
44 help surfaces / 44 PASS
69 top-level options
12 top-level commands
```

Evidence:

```text
artifacts/claude-code-surface/summary.json
artifacts/claude-code-surface/REPORT.md
artifacts/claude-code-surface/raw/
```

### 6.1 Toàn bộ 69 top-level options

Mỗi option dưới đây đã được phát hiện từ `claude --help` của binary 2.1.217. Trạng thái chung của phần này là **HELP/DISCOVERY VERIFIED**; những option có integration test riêng được ghi ở các mục sau.

#### Session, model và execution

```text
-p
--print
-c
--continue
-r
--resume
--session-id
--fork-session
--no-session-persistence
-n
--name
--model
--fallback-model
--effort
--max-budget-usd
--agent
--agents
--bg
--background
--brief
--prompt-suggestions
```

#### Input, output và event stream

```text
--input-format
--output-format
--json-schema
--include-partial-messages
--replay-user-messages
--include-hook-events
--forward-subagent-text
--verbose
--ax-screen-reader
```

#### Prompt và context source

```text
--system-prompt
--append-system-prompt
--add-dir
--file
--exclude-dynamic-system-prompt-sections
```

#### Tools, skills và permission

```text
--tools
--allowedTools
--allowed-tools
--disallowedTools
--disallowed-tools
--permission-mode
--dangerously-skip-permissions
--allow-dangerously-skip-permissions
--disable-slash-commands
--safe-mode
--bare
```

#### Settings và integrations

```text
--settings
--setting-sources
--mcp-config
--strict-mcp-config
--plugin-dir
--plugin-url
--betas
--chrome
--no-chrome
--ide
```

#### Workspace, remote và debug

```text
-w
--worktree
--tmux
--from-pr
--remote-control
--remote-control-session-name-prefix
-d
--debug
--debug-file
```

#### General

```text
-h
--help
-v
--version
```

### 6.2 Command tree được phát hiện

Các command/help surface sau đều exit code 0 ở mức discovery:

```text
claude agents
claude auth
  login
  logout
  status
claude auto-mode
  config
  critique
  defaults
  reset
claude doctor
claude gateway
claude install
claude mcp
  add
  add-from-claude-desktop
  add-json
  get
  list
  login
  logout
  remove
  reset-project-choices
  serve
claude plugin
  details
  disable
  enable
  eval
  init
  install
  list
  marketplace
  prune
  tag
  uninstall
  update
  validate
claude project
  purge
claude setup-token
claude ultrareview
claude update
```

Các command cấp sâu hơn được help output phát hiện nhưng audit chỉ recurse tối đa hai cấp:

```text
claude plugin marketplace add|list|remove|update
claude plugin eval init
```

Chúng là **discovered**, không được tính là help surface riêng trong 44/44.

## 7. Feature matrix liên quan trực tiếp đến bridge

### 7.1 Anthropic Messages API

| Feature | Trạng thái hiện tại | Evidence |
|---|---|---|
| `POST /v1/messages` sync | UNIT/PROTOCOL + LIVE VERIFIED | `tests/protocol_conformance.rs`, real CLI matrices |
| Anthropic SSE streaming | UNIT/PROTOCOL + LIVE VERIFIED | 12 protocol tests, stream-json matrices |
| `POST /v1/messages/count_tokens` | UNIT/ROUTER; PREVIOUS LIVE VERIFIED | prior external-agent audit |
| `GET /v1/models` | LIVE VERIFIED | final HTTP 200 smoke |
| Model requested by Claude remapped to configured DeepSeek | UNIT + LIVE VERIFIED | middleware/mapper tests and logs |
| Invalid request/error shape | UNIT/ROUTER VERIFIED | fast/integration suites |
| Body/SSE/sync response bounds | UNIT/PROTOCOL VERIFIED | overflow tests |
| Client disconnect cancellation | PROTOCOL VERIFIED | `dropping_client_stream_cancels_upstream_body` |
| Premature EOF and duplicate `[DONE]` handling | PROTOCOL VERIFIED | protocol suite |
| Fragmented UTF-8 | PROTOCOL VERIFIED | protocol suite |
| Future/unknown request fields preserved | WIRE + UNIT VERIFIED | serde flatten and 22-case audit |

### 7.2 Thinking, effort và token policy

| Feature | Trạng thái | Kết quả |
|---|---|---|
| Thinking disabled | LIVE REAL CLI VERIFIED | 0 thinking delta, correct final |
| Adaptive thinking | LIVE REAL CLI VERIFIED | reasoning blocks present |
| Fixed thinking 127K | LIVE REAL CLI VERIFIED | fixed budget request accepted |
| Effort low | LIVE REAL CLI VERIFIED | PASS |
| Effort medium | LIVE REAL CLI VERIFIED | PASS |
| Effort high | LIVE REAL CLI VERIFIED | PASS |
| Effort xhigh | LIVE REAL CLI VERIFIED | normalized and PASS |
| Effort max | LIVE REAL CLI VERIFIED | PASS |
| `reasoning_content`, `reasoning`, `thinking` aliases | UNIT VERIFIED | stream parser tests |
| Thinking before text block ordering | PROTOCOL VERIFIED | conformance + tracker tests |
| No implicit non-reasoning fallback for reasoning stream | UNIT VERIFIED | retry policy tests |
| DeepSeek conflicting sampling/tool-choice removal | UNIT VERIFIED | mapper/OpenAI handler tests |
| Duplicate-thinking hygiene | UNIT + LIVE HTTP VERIFIED | before/after metrics in this report |
| Max output 128K request shape | WIRE VERIFIED | 22-case audit |
| Fixed thinking 127K safety value | WIRE + LIVE VERIFIED | audit + real matrix |

### 7.3 Prompt, output và request modes

| Feature | Trạng thái |
|---|---|
| Default system prompt passthrough | UNIT + LIVE VERIFIED |
| `--system-prompt` | WIRE + LIVE REAL CLI VERIFIED |
| `--append-system-prompt` | WIRE + LIVE REAL CLI VERIFIED |
| JSON Schema / structured output | WIRE + LIVE REAL CLI VERIFIED |
| `CLAUDE_CODE_EXTRA_BODY` | WIRE VERIFIED |
| metadata | WIRE/TYPE VERIFIED |
| service tier | WIRE/TYPE VERIFIED |
| context management field preservation | WIRE/TYPE VERIFIED |
| beta header | WIRE VERIFIED |
| prompt suggestions request mode | WIRE VERIFIED; not separately asserted with model semantics |
| `--input-format stream-json` | LIVE REAL CLI VERIFIED |
| `--output-format stream-json` | LIVE REAL CLI VERIFIED |
| partial message events | LIVE REAL CLI VERIFIED |
| replayed user messages | LIVE REAL CLI VERIFIED |
| text/json output formats | LIVE REAL CLI VERIFIED |
| image/document input blocks | NOT ADVERTISED / NOT INTEGRATION VERIFIED |

### 7.4 Context, history và session

| Feature | Trạng thái | Evidence |
|---|---|---|
| 200K configured context | WIRE + runtime config verified | settings/captures |
| Max output 128K | WIRE verified | request audit |
| Auto compact at configured threshold | WIRE/CLAUDE HARNESS VERIFIED | forced autocompact case |
| History actually reduced after compact | WIRE VERIFIED | old long turns absent after compact |
| Assistant thinking history preservation | UNIT VERIFIED | mapper regression |
| Tool-use/tool-result history ordering | UNIT + LIVE VERIFIED | mapper and tool matrices |
| Session persistence | LIVE REAL CLI VERIFIED | real matrix |
| `--resume` same session | LIVE REAL CLI VERIFIED | session ID and remembered marker |
| `--continue`, fork-session | HELP/DISCOVERY only in current cycle |
| Background sessions/agents | HELP/DISCOVERY only |

Forced autocompact evidence từ audit trước/current capture harness:

```text
trigger: auto
compact_result: success
pre_tokens: 277513
post_tokens: 208
cumulative_dropped_tokens: 277305
```

### 7.5 Built-in tools và file operations

Real tool matrix sau parser fix:

| Case | Tool | Kết quả |
|---|---|---:|
| `read_single` | Read | PASS |
| `read_parallel` | Read ×2 | PASS |
| `bash` | Bash | PASS |
| `bash_quoted_multiline` | Bash với quote/newline | PASS |
| `bash_regex_then_read_anser` | Bash regex escape + Read | PASS |
| `glob` | Glob | PASS |
| `grep` | Grep | PASS |
| `write` | Write + side effect | PASS |
| `edit` | Read + Edit + side effect | PASS |
| `webfetch` | WebFetch | PASS |
| `mcp_echo` | MCP stdio tool | PASS |

Mọi case đều kiểm tra:

- process exit 0;
- final result thành công;
- đúng tool name được phát ra;
- có tool result;
- expected result token có trong final;
- raw `[Requesting Tool execution: ...]` không bị leak;
- side effect đúng với Write/Edit.

### 7.6 Các dạng tool call mà bridge hỗ trợ

| Dạng | Trạng thái |
|---|---|
| Native OpenAI function tool call | UNIT/PROTOCOL + LIVE VERIFIED |
| Streamed native arguments theo `tool_call.index` | UNIT/PROTOCOL VERIFIED |
| Arguments đến trước function name | UNIT VERIFIED |
| Parallel tool fragments không trộn nhau | UNIT VERIFIED |
| Anthropic tool name casing resolution | UNIT VERIFIED |
| DSML tool envelope | UNIT/PROTOCOL VERIFIED |
| DSML tag bị chia giữa SSE chunks | UNIT VERIFIED |
| Compatibility text marker | UNIT + LIVE VERIFIED |
| Nhiều compatibility markers liên tiếp | UNIT VERIFIED |
| Raw multiline JSON string | UNIT VERIFIED |
| Unescaped quote trong Bash/Write JSON string | UNIT + LIVE VERIFIED |
| Invalid shell/regex escape (`\\|`, `\\.`, `\\;`) | UNIT + LIVE VERIFIED |
| Detached duplicate fields ngoài argument object | UNIT VERIFIED |
| Incomplete/oversized marker fail-closed | UNIT VERIFIED |
| Unavailable/hallucinated tool | UNIT VERIFIED; không báo malformed tool-use |
| Search marker trong reasoning channel | UNIT VERIFIED |
| Shell command bắt đầu bằng `!` theo bridge shell policy | UNIT/INTEGRATION VERIFIED; disabled by default |

### 7.7 Skill, slash command và MCP

| Feature | Trạng thái |
|---|---|
| Custom `SKILL.md` slash invocation | WIRE + LIVE REAL CLI VERIFIED |
| Skill expansion trong request | WIRE VERIFIED |
| Disable slash commands option | HELP/DISCOVERY VERIFIED |
| MCP stdio server | LIVE REAL CLI VERIFIED |
| MCP strict config | LIVE REAL CLI VERIFIED |
| MCP allowed tool name | LIVE REAL CLI VERIFIED |
| MCP CLI add/get/list/login/logout/remove/serve | HELP/DISCOVERY; operations không chạy toàn bộ |
| Plugin directory loading | HELP/DISCOVERY only |
| Plugin URL/marketplace install/update | NOT INTEGRATION VERIFIED |

### 7.8 Agent, subagent và WebSearch

Current real test:

```text
Agent calls: 2
Successful agent results: 2
Distinct WebSearch queries: 5
Real source URLs: 10
content_block_delta: 2242
text_delta: 818
text streaming span: 33703.5 ms
Raw tool marker: absent
Search loop error: absent
API error event: absent
```

Các check PASS:

1. process exit 0;
2. successful final result;
3. nhiều Agent calls;
4. Agent nhận đủ tool results;
5. nhiều query khác nhau;
6. có URL thật;
7. không raw marker;
8. không `search_loop_protection`;
9. không API error event;
10. nhiều content deltas;
11. text stream diễn ra theo thời gian, không bị buffer đến cuối.

Search improvements đã có từ các vòng trước:

- giữ argument theo từng `tool_call.index`;
- không mất fragment tới trước function name;
- không trộn parallel calls;
- query extraction recursive;
- fallback từ user prompt khi args malformed;
- native, DSML và compatibility search marker dùng chung interception path;
- duplicate query cache;
- bounded search budget và final synthesis;
- không kết thúc bằng empty-content API error;
- provider chain:

```text
Tavily → Exa → Serper → SearXNG → DuckDuckGo → Yahoo
```

### 7.9 API key và Claude Code integration identity

| Feature | Trạng thái |
|---|---|
| Claude Code key tách khỏi managed application keys | UNIT VERIFIED hiện tại + PREVIOUS LIVE VERIFIED |
| Managed key create/disable/rotate/revoke hot reload | UNIT + PREVIOUS BROWSER/LIVE VERIFIED |
| Claude key còn hoạt động khi application key active | PREVIOUS LIVE VERIFIED |
| Claude key còn hoạt động sau khi application key revoke | PREVIOUS LIVE VERIFIED |
| Requested Claude model bị pin về global DeepSeek model | UNIT + LIVE VERIFIED |
| Hash managed secret at rest | PREVIOUS BROWSER VERIFIED |
| Placeholder client config không leak secret | UNIT + PREVIOUS BROWSER VERIFIED |

Current regression:

```text
middleware::tests::claude_code_key_survives_managed_key_lifecycle PASS
```

Prior live evidence đã xác nhận:

```text
application key before revoke: 200
Claude Code key while application key active: 200
application key after revoke: 403
Claude Code key after application key revoke: 200
actual Claude CLI before and after revoke: success
```

Browser lifecycle không được chạy lại trong vòng parser 2026-07-22, nên trạng thái của phần browser là **PREVIOUS LIVE VERIFIED**, không phải current rerun.

### 7.10 OpenAI-compatible API

Bridge cũng có inbound:

```text
POST /v1/chat/completions
```

Đã verify trước đây:

- non-streaming JSON;
- SSE + `[DONE]`;
- function tool call;
- tool-result continuation;
- OpenAI error shape;
- model pinning;
- DeepSeek thinking normalization.

Current Rust suite vẫn bao phủ OpenAI handler/policy tests. Vòng parser này không chạy lại toàn bộ external OpenAI live matrix.

## 8. Các feature Claude Code chưa được gọi là end-to-end verified

Các feature sau có thể đã xuất hiện trong help inventory, nhưng không được phép ghi “integration PASS” trong cấu hình local hiện tại:

| Feature | Mức hiện tại / lý do |
|---|---|
| Interactive TUI đầy đủ | Không tự động hóa terminal interaction trong vòng này |
| Chrome integration | Không chạy browser extension/session thật |
| IDE integration | Không kết nối IDE thật |
| Remote Control | Không mở remote session |
| Claude auth/login/logout/setup-token | Không thay đổi tài khoản hoặc credential người dùng |
| Enterprise gateway | Chỉ help smoke |
| Claude install/update | Không tự thay binary đang dùng |
| Cloud ultrareview | Không gọi dịch vụ cloud/cost-bearing workflow |
| `--from-pr` | Không dùng PR thật |
| Worktree + tmux integration | Chỉ help discovery |
| Background agent management | Không chạy lifecycle đầy đủ |
| `--file` remote file resource download | Không có file ID thật |
| Plugin URL download/marketplace mutations | Tránh network/side effect |
| Plugin eval/publish report | Không chạy judge/cost workflow |
| Auto-mode critique/reset/config actions | Help only; reset có side effect |
| Hook event semantics | Option discovered; chưa có hook integration matrix riêng |
| Prompt suggestions semantic quality | Wire mode captured; không đánh giá chất lượng suggestion |
| Browser CORS/public remote deployment | Active bridge loopback-only; không quảng cáo CORS |
| TLS/reverse proxy production deployment | Ngoài phạm vi local bridge |
| Image/document content blocks | Không nằm trong verified compatibility contract |
| Official Anthropic SDK matrix | Raw HTTP và Claude Code CLI là bằng chứng chính hiện tại |

## 9. Toàn bộ bằng chứng cần giữ lại

### Current 2026-07-22

```text
artifacts/claude-code-surface/
artifacts/claude-cli-audit/
artifacts/claude-cli-real-matrix/
artifacts/tool-call-manual/
artifacts/subagent-stream-timing/summary-parsefix-20260722.json
artifacts/subagent-stream-timing/REPORT-parsefix-20260722.md
artifacts/final-claude-smoke/
```

### Historical

```text
verification/CLAUDE_CODE_CLI_MANUAL_AUDIT_20260721.md
verification/WEB_SEARCH_LOOP_MANUAL_VERIFY_20260721.md
verification/OPENAI_API_AND_API_KEY_MANUAL_VERIFY_20260721.md
verification/DASHBOARD_API_WORKSPACE_MANUAL_VERIFY_20260721.md
verification/EXTERNAL_AGENT_API_MANUAL_VERIFY_20260721.md
verification/FEATURE_MATRIX.md
docs/compatibility.md
tmp/claude-mode-captures/
```

Historical docs có thể chứa phiên bản hoặc trạng thái cũ. Tài liệu hiện tại là index mới hơn; raw artifact vẫn là nguồn bằng chứng chi tiết.

## 10. Lệnh tái kiểm tra chuẩn

Chạy từ repo root:

```bash
cd /home/light/GitHub/opencode2claude
```

### 10.1 Inventory Claude Code đang cài

```bash
python3 scripts/audit_claude_code_surface.py
```

### 10.2 Capture toàn bộ request modes

```bash
python3 scripts/audit_claude_code_cli.py
```

### 10.3 Real model feature matrix

```bash
python3 scripts/manual_claude_code_real_matrix.py
```

### 10.4 Real core tool matrix

```bash
python3 scripts/manual_verify_tool_calls.py
```

### 10.5 Agent + WebSearch + streaming

```bash
python3 scripts/manual_verify_subagent_streaming.py \
  --label <unique-label> \
  --bridge-log /home/light/.opencode2api/opencode2api.log
```

Script phải đọc log từ offset bắt đầu của chính run hiện tại, không dùng dữ liệu cũ.

### 10.6 Rust quality gates

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release --bins
```

### 10.7 Deploy và live smoke

```bash
./target/release/opencode2api server restart
./target/release/opencode2api server status --json
curl -sS http://127.0.0.1:4000/health
curl -sS -H 'x-api-key: opencode-bridge' \
  http://127.0.0.1:4000/v1/models
```

Sau đó chạy ít nhất:

- một Claude Code no-tool exact response;
- một Bash command chứa quote + newline;
- xác nhận `tool_use=Bash`;
- xác nhận raw compatibility marker không xuất hiện.

## 11. Checklist bắt buộc sau mỗi lần sửa parser

Không được kết luận xong chỉ vì unit test mới PASS.

1. Thêm exact regression payload đại diện cho lỗi người dùng.
2. Test parser trả về JSON arguments parse được.
3. Test streaming conversion phát `tool_use` và không leak marker.
4. Chạy toàn bộ stream parser suite.
5. Chạy request-shape audit 22 case hoặc phiên bản mới hơn.
6. Chạy real-model matrix.
7. Chạy real core tool matrix, bắt buộc có quoted/multiline Bash và Bash regex escape + Read.
8. Nếu đụng Agent/WebSearch/tool fragments, chạy subagent streaming matrix.
9. Chạy `cargo fmt`, Clippy `-D warnings`, full tests và release build.
10. Restart release service.
11. Chạy final Claude Code CLI smoke trên service vừa restart.
12. Cập nhật file này với version, pass count, evidence và feature mới.
13. Không nâng trạng thái help-only thành live-verified nếu chưa có bằng chứng thực tế.

## 12. Trạng thái cuối vòng 2026-07-22

```text
Bridge: running
PID: 1397179
Bind: 127.0.0.1:4000
Health: {"status":"ok","version":"0.5.0"}
/v1/models with Claude integration key: HTTP 200
Configured model: opencode/deepseek-v4-flash-free
Request-shape audit: 22/22 PASS
Real feature matrix: 16/16 PASS
Core tool matrix: 11/11 PASS
ANSER Bash regex + Read: RQ_REGEX_READ_OK
Agent/WebSearch: 11/11 checks PASS, 30 real URLs
Raw marker leaked: false
TTY segmented thinking: PASS
```

Tóm lại: toàn bộ bề mặt CLI hiện có đã được inventory; toàn bộ feature liên quan trực tiếp đến bridge đã được verify ở mức phù hợp. Parser hiện phục hồi invalid shell/regex escapes, unescaped quotes, raw control characters và detached fields; marker incomplete/oversized fail-closed; exact ANSER Bash + Read đã PASS bằng Claude Code thật. Duplicate thinking được giảm bằng targeted reasoning hygiene mà không tắt thinking, và reasoning dài được segment để interactive TUI có thể render tiến trình trước khi toàn bộ block hoàn tất.
