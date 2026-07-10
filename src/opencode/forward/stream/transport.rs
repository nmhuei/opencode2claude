//! SSE transport plumbing and client-disconnect cancellation.

use axum::response::sse::Event;
use futures_util::Stream;
use std::convert::Infallible;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Timeout for sending a single SSE event through the mpsc channel.
/// Prevents the stream task from hanging forever if the receiver is slow.
const SSE_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Wraps a stream and cancels a CancellationToken when dropped.
/// This ensures the spawned task is notified when the client disconnects.
pub(super) struct DropCancel {
    pub(super) token: CancellationToken,
    pub(super) inner: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>,
}

impl Drop for DropCancel {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

impl Stream for DropCancel {
    type Item = Result<Event, Infallible>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// Send an SSE event through the mpsc channel with a timeout.
/// Returns `true` if the event was sent, `false` if the channel timed out or closed.
pub(super) async fn send_sse(tx: &tokio::sync::mpsc::Sender<Event>, event: Event) -> bool {
    match tokio::time::timeout(SSE_SEND_TIMEOUT, tx.send(event)).await {
        Ok(Ok(())) => true,
        Ok(Err(_)) => {
            warn!("SSE send failed because receiver was closed");
            false
        }
        Err(_) => {
            warn!("SSE send timed out after {:?}", SSE_SEND_TIMEOUT);
            false
        }
    }
}
