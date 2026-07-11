//! Shared management-domain primitives used by the browser dashboard and the
//! versioned REST API.
//!
//! Transport-specific handlers must stay thin. Authentication, safe snapshots,
//! and proxy lifecycle validation live here so browser and REST behavior cannot
//! silently diverge.

pub mod auth;
pub mod config_apply;
pub mod dto;
pub mod service;
