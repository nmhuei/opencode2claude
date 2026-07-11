//! Host-level WARP reconnection used only when no explicit proxy route exists.

use tracing::{info, warn};

pub(super) async fn reconnect_warp(binary: &str) -> bool {
    info!("reconnecting host WARP client");

    let disconnect = tokio::process::Command::new(binary)
        .arg("disconnect")
        .output()
        .await;
    match disconnect {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            warn!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "warp-cli disconnect returned a non-zero status"
            );
        }
        Err(error) => {
            warn!(%error, "warp-cli disconnect failed");
            return false;
        }
    }

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let connect = tokio::process::Command::new(binary)
        .arg("connect")
        .output()
        .await;
    match connect {
        Ok(output) if output.status.success() => {
            tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
            info!("host WARP client reconnected; exit-IP change was not assumed");
            true
        }
        Ok(output) => {
            warn!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "warp-cli connect returned a non-zero status"
            );
            false
        }
        Err(error) => {
            warn!(%error, "warp-cli connect failed");
            false
        }
    }
}
