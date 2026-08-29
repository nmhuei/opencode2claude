//! Local shell delegation protocol.
//!
//! The bridge never executes the command here. It emits a tool_use block for the
//! client and later echoes the matching tool_result back as an assistant response.

use super::prompt::{last_user_shell_cmd, local_shell_result_candidates};
use super::{AnthropicTool, MessagesRequest};
use crate::api_key::{ApiKeyPolicyError, AuthenticatedClient};
use crate::error::BridgeError;
use crate::shell::ShellPolicy;
use crate::sse::SseEventBuilder;
use crate::state::AppState;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::info;

/// Upper bound on echoed local-shell output before truncation.
pub(crate) const MAX_ECHO_BYTES: usize = 64 * 1024;
/// Live window for a delegation ticket: one client round-trip, minutes at most.
const TICKET_TTL: Duration = Duration::from_secs(5 * 60);
/// Maximum concurrently outstanding delegation tickets (bounds memory).
pub(crate) const SHELL_TICKET_CAPACITY: usize = 256;

/// Bounded single-use tickets binding a client-echoed shell result to a
/// delegation this bridge actually issued. Prevents forged tool_result
/// blocks from being rendered as assistant output.
#[derive(Debug, Default)]
pub struct ShellDelegations {
    tickets: Mutex<HashMap<String, Instant>>,
}

impl ShellDelegations {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh single-use ticket id for a delegation being emitted.
    pub(crate) fn issue(&self) -> String {
        let mut tickets = self.lock();
        evict_locked(&mut tickets);
        loop {
            let id = format!(
                "toolu_{}",
                crate::infrastructure::random::secure_random_hex(16)
                    .unwrap_or_else(|_| fallback_ticket_id(unix_nanos()))
            );
            if !tickets.contains_key(&id) {
                tickets.insert(id.clone(), Instant::now());
                return id;
            }
        }
    }

    /// Consume a ticket: true only when it was issued by this store and is
    /// still within its TTL (single use — replays and expired entries fail).
    pub(crate) fn consume(&self, id: &str) -> bool {
        let mut tickets = self.lock();
        match tickets.remove(id) {
            Some(issued) => issued.elapsed() <= TICKET_TTL,
            None => false,
        }
    }

    /// Non-destructive liveness probe: true when `id` was issued by this
    /// store and is still within its TTL. Unlike `consume` it leaves the
    /// ticket intact, letting admission-time decisions inspect echo traffic
    /// without spending single-use tickets on requests that may yet be
    /// rejected.
    pub(crate) fn is_live(&self, id: &str) -> bool {
        self.lock()
            .get(id)
            .is_some_and(|issued| issued.elapsed() <= TICKET_TTL)
    }

    #[cfg(test)]
    pub(crate) fn expire_for_test(&self, id: &str) {
        let mut tickets = self.lock();
        if let Some(slot) = tickets.get_mut(id) {
            *slot = Instant::now()
                .checked_sub(TICKET_TTL + Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
        self.tickets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Drop expired entries and, when full, the oldest tickets to stay bounded.
fn evict_locked(tickets: &mut HashMap<String, Instant>) {
    tickets.retain(|_, issued| issued.elapsed() <= TICKET_TTL);
    while tickets.len() >= SHELL_TICKET_CAPACITY {
        let Some(oldest) = tickets
            .iter()
            .min_by_key(|(_, issued)| **issued)
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        tickets.remove(&oldest);
    }
}

/// Wall-clock nanos since the epoch; 0 when the clock is unset or pre-epoch.
fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default()
}

/// Monotonic salt for entropy-failure fallbacks: guarantees successive mints
/// never repeat even when the wall clock is frozen (or pre-epoch), so the
/// collision loop in `issue()` always terminates.
static FALLBACK_SALT: AtomicU64 = AtomicU64::new(0);

/// Entropy failure fallback: wall-clock nanos still beat a static id.
fn fallback_ticket_id(nanos: u128) -> String {
    let salt = FALLBACK_SALT.fetch_add(1, Ordering::Relaxed);
    format!("toolu_{nanos:032x}{salt:016x}")
}

pub(super) async fn try_handle(
    state: &AppState,
    client: Option<&AuthenticatedClient>,
    payload: &MessagesRequest,
    model: String,
) -> Result<Option<Response>, BridgeError> {
    // Echo leg: only render when the client returns a live single-use ticket
    // that this bridge issued for a preceding delegation. Unknown, replayed,
    // or expired ids fall through to normal handling instead of rendering.
    for (ticket_id, output) in local_shell_result_candidates(&payload.messages) {
        if !state.shell_delegations.consume(&ticket_id) {
            info!(%ticket_id, "ignoring unverifiable local shell result");
            continue;
        }
        ensure_shell_allowed(&state.config.shell_policy, client)?;
        let output = cap_echo_output(output);
        info!(length = output.len(), "echoing verified local shell result");
        return Ok(Some(render_shell_result(output, model, payload.stream)));
    }

    let Some(command) = last_user_shell_cmd(&payload.messages) else {
        return Ok(None);
    };

    ensure_shell_allowed(&state.config.shell_policy, client)?;
    state
        .config
        .shell_policy
        .check(&command)
        .map_err(|_| BridgeError::ShellDisabled)?;

    let ticket_id = state.shell_delegations.issue();
    info!(%command, "delegating local shell command to client");
    let target = resolve_shell_tool(payload.tools.as_deref());
    Ok(Some(render_shell_request(
        command,
        ticket_id,
        target,
        model,
        payload.stream,
    )))
}

/// Global switch: the bridge-level shell policy forbids everything.
fn global_shell_rejection(policy: &ShellPolicy) -> Option<BridgeError> {
    if matches!(policy, ShellPolicy::Disabled) {
        Some(BridgeError::ShellDisabled)
    } else {
        None
    }
}

/// Per-key switch: an API key may individually forfeit shell delegation.
/// Produces the exact same error construction as messages.rs
/// `apply_client_policy`'s shell clause (`Forbidden(ApiKeyPolicyError::
/// ShellDisabled)`), so the two gates can never emit divergent responses.
fn per_key_shell_rejection(client: Option<&AuthenticatedClient>) -> Option<BridgeError> {
    match client {
        Some(client) if !client.policy.permissions.shell => Some(BridgeError::Forbidden(
            ApiKeyPolicyError::ShellDisabled.to_string(),
        )),
        _ => None,
    }
}

/// Policy gate applied before any shell interaction (echo or delegation):
/// global Disabled policy and per-key `permissions.shell = false` both reject,
/// so an echoed result is never rendered for a disallowed caller.
fn ensure_shell_allowed(
    policy: &ShellPolicy,
    client: Option<&AuthenticatedClient>,
) -> Result<(), BridgeError> {
    global_shell_rejection(policy)
        .or_else(|| per_key_shell_rejection(client))
        .map_or(Ok(()), Err)
}

/// Live shell-delegation tickets act as externally-known tool_use IDs for
/// request-history validation: Claude Code may return the tool_result in a
/// compact request that omits the prior synthetic assistant turn. Only IDs
/// that are currently present in the single-use ticket store receive this
/// exemption; guessed/expired/replayed IDs remain ordinary orphan results.
pub(super) fn live_shell_result_ticket_ids(
    payload: &MessagesRequest,
    delegations: &ShellDelegations,
) -> Vec<String> {
    local_shell_result_candidates(&payload.messages)
        .into_iter()
        .filter_map(|(ticket_id, _)| delegations.is_live(&ticket_id).then_some(ticket_id))
        .collect()
}

/// Admission-time shell verdict: everything about this request's shell fate
/// that is decidable from data already in hand, without touching the
/// concurrency permit, single-use tickets, or any I/O. `Some(rejection)`
/// means the request is doomed and should fail fast — before rate-limit
/// admission queues it behind saturated upstream streams; `None` means shell
/// handling (if any) must proceed normally inside `try_handle`.
///
/// Reproduces — from identical inputs — exactly the verdicts and error
/// variants `try_handle` would eventually produce, in the same effective
/// precedence order, so hoisting changes only *when* a doomed request fails,
/// never *how*:
///
/// 1. an authenticated shell-forbidden key sending a `!cmd` prompt gets the
///    per-key `Forbidden` verdict, mirroring `apply_client_policy`, whose
///    clause runs first today and therefore wins even when the global policy
///    is also Disabled;
/// 2. a request carrying a live echo ticket is judged in the echo leg's own
///    order (global switch, then per-key) and, once admitted, owns the
///    request — the delegation leg never runs for it;
/// 3. otherwise a `!cmd` prompt is judged by the delegation leg (global
///    switch, then allowlist, preserving the legacy mapping of allowlist
///    violations onto `BridgeError::ShellDisabled`).
pub(super) fn shell_admission_rejection(
    policy: &ShellPolicy,
    client: Option<&AuthenticatedClient>,
    payload: &MessagesRequest,
    delegations: &ShellDelegations,
) -> Option<BridgeError> {
    let delegation_command = last_user_shell_cmd(&payload.messages);

    // Shape 1: `!cmd` prompt. For authenticated callers this reproduces
    // apply_client_policy's clause verbatim (same inputs, same verdict).
    if delegation_command.is_some() {
        if let Some(rejection) = per_key_shell_rejection(client) {
            return Some(rejection);
        }
    }

    // Shape 2: verified echo pending. A live ticket means try_handle's echo
    // leg will claim this request, so only the echo leg's own gates apply;
    // peeked, never consumed — rejection must not spend single-use tickets.
    let echo_pending = local_shell_result_candidates(&payload.messages)
        .iter()
        .any(|(ticket_id, _)| delegations.is_live(ticket_id));
    if echo_pending {
        return global_shell_rejection(policy).or_else(|| per_key_shell_rejection(client));
    }

    // Shape 3: delegation leg rules the request.
    if let Some(command) = delegation_command {
        if let Some(rejection) = global_shell_rejection(policy) {
            return Some(rejection);
        }
        if policy.check(&command).is_err() {
            return Some(BridgeError::ShellDisabled);
        }
    }

    None
}

/// Hard cap on echoed content so a hostile client cannot push unbounded bytes
/// through the verified echo path.
fn cap_echo_output(output: String) -> String {
    if output.len() <= MAX_ECHO_BYTES {
        return output;
    }
    let mut cut = MAX_ECHO_BYTES;
    while cut > 0 && !output.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut truncated = output[..cut].to_string();
    truncated.push_str("\n[truncated by opencode2api]");
    truncated
}

#[derive(Debug)]
struct ShellToolTarget {
    name: String,
    parameter: String,
}

fn resolve_shell_tool(tools: Option<&[AnthropicTool]>) -> ShellToolTarget {
    let Some(tool) = tools.and_then(|tools| {
        tools.iter().find(|tool| {
            matches!(
                tool.name.to_ascii_lowercase().as_str(),
                "bash" | "execute_command" | "run_command"
            )
        })
    }) else {
        return ShellToolTarget {
            name: "bash".to_string(),
            parameter: "command".to_string(),
        };
    };

    let parameter = tool
        .input_schema
        .get("properties")
        .and_then(|value| value.as_object())
        .and_then(|properties| {
            if properties.contains_key("command") {
                Some("command".to_string())
            } else if properties.contains_key("cmd") {
                Some("cmd".to_string())
            } else {
                properties.keys().next().cloned()
            }
        })
        .unwrap_or_else(|| "command".to_string());

    ShellToolTarget {
        name: tool.name.clone(),
        parameter,
    }
}

fn render_shell_result(output: String, model: String, stream: bool) -> Response {
    let output_tokens = estimate_output_tokens(&output, 10);
    let builder = SseEventBuilder::new("msg_local_shell_result".to_string(), model);

    if !stream {
        return Json(builder.non_streaming_response(&output, 10, output_tokens)).into_response();
    }

    let (tx, rx) = tokio::sync::mpsc::channel(10);
    tokio::spawn(async move {
        for event in [
            builder.message_start(10),
            builder.content_block_start(),
            builder.text_delta(&output),
            builder.content_block_stop(),
            builder.message_delta(output_tokens),
            builder.message_stop(),
        ] {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });
    sse_response(rx)
}

fn render_shell_request(
    command: String,
    ticket_id: String,
    target: ShellToolTarget,
    model: String,
    stream: bool,
) -> Response {
    let output_tokens = estimate_output_tokens(&command, 15);
    let input = json!({ target.parameter.clone(): command.clone() });

    if !stream {
        return Json(json!({
            "id": "msg_local_shell",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{
                "type": "tool_use",
                "id": ticket_id,
                "name": target.name,
                "input": input
            }],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": {"input_tokens": 50, "output_tokens": output_tokens}
        }))
        .into_response();
    }

    let (tx, rx) = tokio::sync::mpsc::channel(10);
    let builder = SseEventBuilder::new("msg_local_shell".to_string(), model);
    tokio::spawn(async move {
        let events = [
            builder.message_start(50),
            json_event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": ticket_id,
                        "name": target.name,
                        "input": {}
                    }
                }),
            ),
            json_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": input.to_string()
                    }
                }),
            ),
            json_event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
            json_event(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "tool_use", "stop_sequence": null},
                    "usage": {"output_tokens": output_tokens}
                }),
            ),
            json_event("message_stop", json!({"type": "message_stop"})),
        ];

        for event in events {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });
    sse_response(rx)
}

fn json_event(name: &'static str, payload: serde_json::Value) -> Event {
    Event::default()
        .event(name)
        .json_data(payload)
        .unwrap_or_else(|_| Event::default().event(name).data("{}"))
}

fn sse_response(rx: tokio::sync::mpsc::Receiver<Event>) -> Response {
    let response = Sse::new(
        tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, std::convert::Infallible>),
    )
    .keep_alive(KeepAlive::default())
    .into_response();
    super::messages::disable_proxy_buffering(response)
}

fn estimate_output_tokens(text: &str, overhead: u32) -> u32 {
    (text.len() as f32 / 3.5).round() as u32 + overhead
}

#[cfg(test)]
mod admission_equivalence_tests {
    use super::*;
    use crate::api_key::{ApiKeyPermissions, ApiKeyPolicy};
    use crate::handlers::{ContentVal, Message, MessageContent};

    fn client(shell: bool) -> Option<AuthenticatedClient> {
        Some(AuthenticatedClient {
            key_id: "key_gate_equiv".to_string(),
            name: "Gate Equivalence".to_string(),
            environment: "development".to_string(),
            policy: ApiKeyPolicy {
                permissions: ApiKeyPermissions {
                    shell,
                    ..Default::default()
                },
                ..Default::default()
            },
        })
    }

    fn delegate_payload(command: &str) -> MessagesRequest {
        MessagesRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Single(format!("!{command}")),
            }],
            max_tokens: Some(64),
            ..Default::default()
        }
    }

    fn echo_payload(ticket_id: &str) -> MessagesRequest {
        MessagesRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![MessageContent {
                    content_type: "tool_result".to_string(),
                    tool_use_id: Some(ticket_id.to_string()),
                    content: Some(serde_json::json!("out")),
                    ..Default::default()
                }]),
            }],
            max_tokens: Some(64),
            ..Default::default()
        }
    }

    /// The admission gate must reproduce — from identical inputs — exactly
    /// the verdicts the in-`try_handle` cascade would eventually produce,
    /// including error variants and precedence. This table pins every input
    /// class so a future edit to either side that breaks agreement fails here
    /// instead of changing observable responses.
    #[test]
    fn gate_reproduces_the_legacy_cascade_verdict_for_every_input_class() {
        let disabled = ShellPolicy::Disabled;
        let open = ShellPolicy::Unrestricted;
        let ls_only = ShellPolicy::AllowList(std::collections::HashSet::from(["ls".to_string()]));
        let store = ShellDelegations::new();

        // Shape 1 precedence: for authenticated keys the per-key Forbidden
        // verdict wins even when the global policy is also Disabled — this is
        // what apply_client_policy (running first today) produces.
        assert!(matches!(
            shell_admission_rejection(
                &disabled,
                client(false).as_ref(),
                &delegate_payload("ls"),
                &store,
            ),
            Some(BridgeError::Forbidden(_))
        ));
        // Delegation leg: global switch rejects anonymously.
        assert!(matches!(
            shell_admission_rejection(&disabled, None, &delegate_payload("ls"), &store),
            Some(BridgeError::ShellDisabled)
        ));
        // Delegation leg: allowlist violations keep the legacy ShellDisabled
        // mapping (not a distinct error).
        assert!(matches!(
            shell_admission_rejection(&ls_only, None, &delegate_payload("cat /etc/passwd"), &store),
            Some(BridgeError::ShellDisabled)
        ));
        // Admissible delegations pass.
        assert!(
            shell_admission_rejection(&ls_only, None, &delegate_payload("ls -la"), &store)
                .is_none()
        );
        assert!(shell_admission_rejection(
            &open,
            client(true).as_ref(),
            &delegate_payload("anything"),
            &store
        )
        .is_none());

        // Echo leg owns requests carrying live tickets: global first, then
        // per-key; an admitted echo falls through to normal handling.
        let live_store = ShellDelegations::new();
        let ticket = live_store.issue();
        assert!(matches!(
            shell_admission_rejection(
                &open,
                client(false).as_ref(),
                &echo_payload(&ticket),
                &live_store
            ),
            Some(BridgeError::Forbidden(_))
        ));
        assert!(matches!(
            shell_admission_rejection(&disabled, None, &echo_payload(&ticket), &live_store),
            Some(BridgeError::ShellDisabled)
        ));
        assert!(
            shell_admission_rejection(&open, None, &echo_payload(&ticket), &live_store).is_none()
        );
        // Deciding admission never spends single-use tickets.
        assert!(live_store.is_live(&ticket));

        // Combined shapes: when a live ticket AND a `!cmd` text block are both
        // present, try_handle's echo leg runs first and claims the request —
        // the delegation leg (and its allowlist check) never executes. The
        // gate must therefore pass an allowlisted-out command through untouched
        // whenever a live ticket is pending, or it would diverge from the
        // rendered echo response callers receive today.
        let both = MessagesRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![
                    MessageContent {
                        content_type: "tool_result".to_string(),
                        tool_use_id: Some(ticket.clone()),
                        content: Some(serde_json::json!("out")),
                        ..Default::default()
                    },
                    MessageContent {
                        content_type: "text".to_string(),
                        text: Some("!cat /etc/passwd".to_string()),
                        ..Default::default()
                    },
                ]),
            }],
            max_tokens: Some(64),
            ..Default::default()
        };
        assert!(
            shell_admission_rejection(&ls_only, None, &both, &live_store).is_none(),
            "echo leg owns the request; allowlist must not fire"
        );
        assert!(matches!(
            shell_admission_rejection(&disabled, None, &both, &live_store),
            Some(BridgeError::ShellDisabled)
        ));

        // Expired tickets do not put a request into the echo leg: it falls
        // through to normal handling exactly as try_handle's consume loop
        // does today (consume returns false and the loop continues).
        let stale_store = ShellDelegations::new();
        let stale = stale_store.issue();
        stale_store.expire_for_test(&stale);
        assert!(
            shell_admission_rejection(&disabled, None, &echo_payload(&stale), &stale_store)
                .is_none()
        );

        // No shell shape at all: the gate never rejects.
        let plain = MessagesRequest {
            max_tokens: Some(8),
            ..Default::default()
        };
        assert!(
            shell_admission_rejection(&disabled, client(false).as_ref(), &plain, &store).is_none()
        );
    }

    fn rejection_kind(result: Result<(), BridgeError>) -> &'static str {
        match result {
            Ok(()) => "pass",
            Err(BridgeError::ShellDisabled) => "global_disabled",
            Err(BridgeError::Forbidden(_)) => "per_key_forbidden",
            Err(_) => "other",
        }
    }

    /// Double-rejection proof: when a request passes pre-permit admission but
    /// still reaches `try_handle`'s own gate (echo leg), both gates return
    /// the identical verdict from identical inputs, because they are composed
    /// from the same predicate helpers.
    #[test]
    fn ensure_shell_allowed_agrees_with_the_admission_gate_on_echo_traffic() {
        let policies = [
            ShellPolicy::Disabled,
            ShellPolicy::Unrestricted,
            ShellPolicy::AllowList(std::collections::HashSet::from(["ls".to_string()])),
        ];
        let store = ShellDelegations::new();
        let ticket = store.issue();

        for policy in &policies {
            let gate_verdict = shell_admission_rejection(
                policy,
                client(false).as_ref(),
                &echo_payload(&ticket),
                &store,
            )
            .map_or(Ok(()), Err);
            assert_eq!(
                rejection_kind(gate_verdict),
                rejection_kind(ensure_shell_allowed(policy, client(false).as_ref())),
                "gates disagree for policy {policy:?}"
            );

            let gate_verdict =
                shell_admission_rejection(policy, None, &echo_payload(&ticket), &store)
                    .map_or(Ok(()), Err);
            assert_eq!(
                rejection_kind(gate_verdict),
                rejection_kind(ensure_shell_allowed(policy, None)),
                "gates disagree for anonymous caller under {policy:?}"
            );
        }
    }
}

#[cfg(test)]
mod admission_order_tests {
    use super::*;
    use crate::api_key::{ApiKeyPermissions, ApiKeyPolicy};
    use crate::config::{BridgeConfig, EgressConfig, EgressMode};
    use axum::body::Body;
    use axum::extract::Extension;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::Router;
    use std::collections::HashSet;
    use tower::util::ServiceExt;

    fn shell_denied_client() -> AuthenticatedClient {
        AuthenticatedClient {
            key_id: "key_shell_denied".to_string(),
            name: "Shell Denied".to_string(),
            environment: "development".to_string(),
            policy: ApiKeyPolicy {
                permissions: ApiKeyPermissions {
                    shell: false,
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    /// Bridge config with a saturated-slot setup: exactly one concurrency
    /// permit, and an upstream address that nothing should ever reach because
    /// every request in these tests is rejected before forwarding.
    fn bridge_config(policy: ShellPolicy) -> BridgeConfig {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);

        let defaults = BridgeConfig::default();
        let mut config = BridgeConfig {
            model: Some("fixture-model".to_string()),
            shell_policy: policy,
            retry: crate::config::RetryConfig {
                upstream_base_url: format!("http://{address}"),
                max_network_attempts: 1,
                base_backoff: Duration::ZERO,
                ..defaults.retry
            },
            egress: EgressConfig {
                mode: EgressMode::Direct,
                ..defaults.egress
            },
            ..defaults
        };
        config.observability.max_concurrent_requests = Some(1);
        config.management.config_path = std::env::temp_dir().join(format!(
            "opencode2api-shell-admission-{}-{}.toml",
            std::process::id(),
            crate::api_key::unix_timestamp(),
        ));
        config
    }

    async fn drain_sole_permit(state: &AppState) -> tokio::sync::OwnedSemaphorePermit {
        state
            .rate_limiter
            .as_ref()
            .expect("test config enables the rate limiter")
            .clone()
            .acquire_owned()
            .await
            .unwrap()
    }

    fn app_with(state: AppState, client: Option<AuthenticatedClient>) -> Router {
        let router = Router::new().route("/v1/messages", post(crate::handlers::handle_messages));
        match client {
            Some(client) => router.layer(Extension(client)),
            None => router,
        }
        .with_state(state)
    }

    fn post_messages(body: String) -> axum::http::Request<Body> {
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    fn delegate_body(command: &str) -> String {
        serde_json::json!({
            "stream": false,
            "max_tokens": 64,
            "messages": [{"role": "user", "content": format!("!{command}")}]
        })
        .to_string()
    }

    fn echo_body(ticket_id: &str) -> String {
        serde_json::json!({
            "stream": false,
            "max_tokens": 64,
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": ticket_id, "content": "file-a"}
            ]}]
        })
        .to_string()
    }

    async fn response_text(response: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// A policy-doomed request must fail fast instead of queueing behind a
    /// saturated semaphore: with the single global slot held elsewhere, a key
    /// whose `permissions.shell = false` sending a `!cmd` prompt still gets
    /// its 403 immediately instead of waiting for a slot it can never use.
    #[tokio::test]
    async fn shell_policy_rejections_do_not_queue_for_a_concurrency_slot() {
        let state = AppState::new(bridge_config(ShellPolicy::Unrestricted));
        let held = drain_sole_permit(&state).await;
        let app = app_with(state.clone(), Some(shell_denied_client()));

        let response = tokio::time::timeout(
            Duration::from_secs(5),
            app.oneshot(post_messages(delegate_body("pwd"))),
        )
        .await
        .expect("shell-policy-rejected request must not wait for a concurrency slot")
        .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "shell-forbidden key must be rejected without needing a slot"
        );
        let body = response_text(response).await;
        assert!(
            body.contains("shell execution is disabled for this API key"),
            "expected the per-key shell policy error, got: {body}"
        );
        drop(held);
    }

    /// With auth disabled (anonymous caller) nothing runs apply_client_policy,
    /// so a `!cmd` prompt under the default global Disabled policy must be
    /// rejected at admission time instead of queueing behind saturated
    /// upstream streams.
    #[tokio::test]
    async fn anonymous_delegation_rejects_without_a_slot_when_globally_disabled() {
        let state = AppState::new(bridge_config(ShellPolicy::Disabled));
        let held = drain_sole_permit(&state).await;
        let app = app_with(state, None);

        let response = tokio::time::timeout(
            Duration::from_secs(5),
            app.oneshot(post_messages(delegate_body("ls"))),
        )
        .await
        .expect("globally-disabled shell rejection must not wait for a concurrency slot")
        .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = response_text(response).await;
        assert!(
            body.contains("Shell commands are disabled by policy"),
            "expected the global shell policy error, got: {body}"
        );
        drop(held);
    }

    /// An allowlisted-out command is rejected by a pure check against static
    /// config, so that rejection must never wait for the concurrency slot.
    #[tokio::test]
    async fn allowlist_violations_reject_without_a_slot() {
        let state = AppState::new(bridge_config(ShellPolicy::AllowList(HashSet::from([
            "ls".to_string()
        ]))));
        let held = drain_sole_permit(&state).await;
        let app = app_with(state, None);

        let response = tokio::time::timeout(
            Duration::from_secs(5),
            app.oneshot(post_messages(delegate_body("cat /etc/passwd"))),
        )
        .await
        .expect("allowlist-violating shell request must not wait for a concurrency slot")
        .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = response_text(response).await;
        assert!(
            body.contains("Shell commands are disabled by policy"),
            "allowlist violations keep the legacy ShellDisabled mapping, got: {body}"
        );
        drop(held);
    }

    /// The echo leg (client returning a delegated result) checks the very same
    /// policies inside try_handle. A shell-forbidden key must be rejected
    /// before admission AND its ticket must survive the rejection instead of
    /// being irreversibly consumed post-permit.
    #[tokio::test]
    async fn echo_leg_rejections_do_not_queue_or_burn_tickets() {
        let state = AppState::new(bridge_config(ShellPolicy::Unrestricted));
        let ticket = state.shell_delegations.issue();
        let held = drain_sole_permit(&state).await;
        let app = app_with(state.clone(), Some(shell_denied_client()));

        let response = tokio::time::timeout(
            Duration::from_secs(5),
            app.oneshot(post_messages(echo_body(&ticket))),
        )
        .await
        .expect("echo-leg policy rejection must not wait for a concurrency slot")
        .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = response_text(response).await;
        assert!(
            body.contains("shell execution is disabled for this API key"),
            "expected the per-key shell policy error, got: {body}"
        );
        assert!(
            state.shell_delegations.is_live(&ticket),
            "a policy-rejected echo must not consume the single-use ticket"
        );
        drop(held);
    }
}

#[cfg(test)]
mod fallback_minting_tests {
    use super::*;

    /// `issue()` regenerates in a loop until it mints an id absent from the
    /// store. If the entropy source fails AND the clock fallback ever repeats
    /// an id across successive mints, every retry collides with the already-
    /// inserted entry and the loop spins forever while holding the store
    /// mutex — deadlocking all shell delegation traffic. The fallback must
    /// therefore never repeat, even when the clock is frozen (or pre-epoch,
    /// where `unix_nanos()` collapses to 0 on every call).
    #[test]
    fn fallback_ids_never_repeat_when_the_clock_is_frozen() {
        let first = fallback_ticket_id(0);
        let second = fallback_ticket_id(0);
        assert_ne!(
            first, second,
            "frozen-clock fallback repeated an id; issue() would livelock on this input"
        );
        assert!(first.starts_with("toolu_"));
        assert_eq!(first.len(), second.len());
    }
}
