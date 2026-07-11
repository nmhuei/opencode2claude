//! Host-level WARP reconnection used only when explicit proxy mode is disabled.

use crate::infrastructure::warp::WarpController;
use tracing::{info, warn};

pub(super) async fn reconnect_warp(controller: &dyn WarpController) -> bool {
    match controller.reconnect().await {
        Ok(()) => {
            info!("host WARP client reconnected; exit-IP change was not assumed");
            true
        }
        Err(error) => {
            warn!(%error, "host WARP reconnection failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::warp::{WarpError, WarpStatus};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct FakeWarp {
        reconnects: AtomicUsize,
    }

    #[async_trait]
    impl WarpController for FakeWarp {
        async fn connect(&self) -> Result<(), WarpError> {
            Ok(())
        }
        async fn disconnect(&self) -> Result<(), WarpError> {
            Ok(())
        }
        async fn status(&self) -> Result<WarpStatus, WarpError> {
            Ok(WarpStatus::Connected)
        }
        async fn reconnect(&self) -> Result<(), WarpError> {
            self.reconnects.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn reconnect_delegates_to_controller() {
        let controller = FakeWarp::default();
        assert!(reconnect_warp(&controller).await);
        assert_eq!(controller.reconnects.load(Ordering::SeqCst), 1);
    }
}
