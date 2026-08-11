//! Host WARP mutation is intentionally unsupported.
//!
//! Upstream retry logic must never call the machine-wide `warp-cli`, because
//! disconnecting or reconnecting host WARP interrupts unrelated user traffic.
//! Managed egress recovery lives exclusively in the Docker proxy pool.
