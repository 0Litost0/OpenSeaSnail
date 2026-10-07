//! SeaSnail 守护进程库（纯数据面）。
//!
//! M1 逐步填充：lifecycle（ST-M1.1）、bootstrap（ST-M1.2）、singleton（ST-M1.3）、
//! server + api（ST-M1.4）、logging（ST-M1.5）、error（ST-M1.6）已落地。

pub mod account;
pub mod api;
pub mod app_resources;
pub mod application;
pub mod bootstrap;
pub mod cleanup;
pub mod composer;
pub mod daemon_resources;
pub mod desktop_auth;
pub mod downloader;
pub mod error;
pub mod exit_code;
mod harness;
pub mod launch;
pub mod lifecycle;
pub mod logging;
pub(crate) mod media_cache;
pub(crate) mod model_runtime;
pub mod model_settings;
pub mod reasoning;
pub mod resource;
pub mod server;
pub mod singleton;

pub use bootstrap::{health_probe, Bootstrap, BootstrapInfo};
pub use error::AppError;
pub use harness::{
    http_get, http_get_with_headers, http_post, spawn_daemon, wait_health, wait_ready,
};
pub use logging::{init_logging, redact_line, trace_id_middleware};
pub use server::build_app;
pub use singleton::{AcquireError, SingletonLock};

// 账户编排层（ST-M2.4/2.5）+ API 共享态（ST-M2.6）。
pub use account::{Auth, Crypto};
pub use api::AppState;
