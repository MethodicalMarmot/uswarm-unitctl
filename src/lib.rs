pub mod config;
pub mod context;
pub mod env;
pub mod mavlink;
pub mod messages;
pub mod net;
pub mod sensors;
pub mod services;

use std::sync::Arc;

pub trait Task: Send + Sync {
    fn run(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>>;
}
