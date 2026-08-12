pub mod application;
pub mod config;
pub mod domain;
pub mod github;
pub mod infra;
pub mod instructions;
pub mod runner;
pub mod security;
pub mod terminal;
pub mod workspace;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
