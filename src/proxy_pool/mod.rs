//! Multi-agent proxy routing with primary and protected warm-standby tiers.

pub mod maintenance;
mod pool;
pub mod routing;
pub mod types;

pub use maintenance::*;
pub use routing::*;
pub use types::*;

#[cfg(test)]
mod tests;
