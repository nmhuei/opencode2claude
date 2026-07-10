//! Upstream retry, model fallback, and egress-failure handling.

mod execute;
mod policy;
mod warp;

pub(crate) use execute::execute_with_warp_retry;

#[cfg(test)]
mod tests;
