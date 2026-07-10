//! Docker-backed WARP proxy management.

mod bootstrap;
mod health;
mod lifecycle;
mod types;

pub use bootstrap::bootstrap_proxy_pool;
pub use health::stop_proxy_containers;
pub use lifecycle::{
    check_daemon, container_logs, create_container, is_docker_available, list_containers,
    remove_container, ContainerSetupState,
};
pub use types::{container_name, DockerError, DockerResult};
