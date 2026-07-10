//! HTTP server composition and process lifecycle.

mod args;
mod routes;
mod runtime;

pub use args::ServeArgsBridge;
pub use routes::build_router;
pub use runtime::run_server;
