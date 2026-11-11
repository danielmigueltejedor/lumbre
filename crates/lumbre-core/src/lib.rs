//! Lumbre Core: the coordinator.
//!
//! For each request the [`Engine`] asks, in this order:
//!
//! 1. *What does this need?* — `lumbre-router` answers with a tool or a model tier.
//! 2. *What can the machine give right now?* — `lumbre-resources` answers with a budget.
//! 3. *Which installed model fits both?* — `lumbre-models` answers with a file.
//!
//! Only then is anything loaded, and never more than one model at a time. A request that a tool
//! can answer never touches steps 2 and 3.

mod config;
mod engine;
mod history;

pub use config::{Config, ConfigError};
pub use engine::Engine;
pub use history::History;
