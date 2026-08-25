//! Multi-agent proxy routing with primary and protected warm-standby tiers.

pub mod identity;
pub mod maintenance;
mod pool;
pub mod reconcile;
pub mod routing;
pub mod subsystem;
pub mod types;

pub use identity::*;
pub use maintenance::*;
pub use reconcile::*;
pub use subsystem::*;
pub use types::*;

#[cfg(test)]
mod tests;
