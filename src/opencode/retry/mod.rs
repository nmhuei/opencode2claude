//! Upstream retry, model fallback, and egress-failure handling.

mod execute;
mod policy;
mod response;
mod warp;

pub(crate) use execute::execute_with_warp_retry;
pub(crate) use policy::cancellation_failure;
pub(crate) use response::LeasedResponse;

#[cfg(test)]
mod tests;
