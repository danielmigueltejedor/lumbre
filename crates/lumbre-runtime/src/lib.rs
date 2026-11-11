//! Lumbre's inference runtime.
//!
//! [`Backend`] is the seam between Lumbre and whatever runs the weights. 0.1 ships one
//! implementation, [`LlamaServer`], which runs llama.cpp as a **worker process**:
//!
//! * unloading a model is ending a process, so the memory really returns to the system — there
//!   is no allocator fragmentation to keep a "freed" model resident;
//! * the idle daemon links no inference code and stays small;
//! * a crash in native code cannot take the daemon or the user's conversation with it;
//! * the worker can be placed in its own cgroup with a hard memory limit.
//!
//! The runtime holds at most one model. Deciding *which* model, with how much context and how
//! many threads, belongs to `lumbre-core`.

mod llama_server;
mod sse;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use llama_server::LlamaServer;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// Everything a backend needs to bring a model up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadSpec {
    pub model_path: PathBuf,
    pub context_tokens: u32,
    pub threads: u32,
    /// Layers to offload to a GPU; 0 keeps everything on the CPU.
    pub gpu_layers: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenRequest {
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    pub temperature: f32,
}

/// Measured, not estimated. Token counts are `None` when the backend did not report them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    /// Time until the first token arrived.
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("no inference backend: {0}")]
    BackendMissing(String),
    #[error("could not start the worker: {0}")]
    Spawn(std::io::Error),
    #[error("the worker exited while loading the model ({0})")]
    Exited(String),
    #[error("the worker did not become ready within {0} s")]
    StartTimeout(u64),
    #[error("no model is loaded")]
    NotLoaded,
    #[error("worker request failed: {0}")]
    Http(String),
    #[error("unexpected worker response: {0}")]
    Protocol(String),
}

pub trait Backend: Send {
    fn name(&self) -> &'static str;

    /// Whether the backend can run at all on this machine (binary present, etc.).
    fn available(&self) -> Result<(), RuntimeError>;

    /// Brings a model up, replacing any loaded one. Blocks until it can generate.
    fn load(&mut self, spec: &LoadSpec) -> Result<(), RuntimeError>;

    /// Releases the model and its memory. Idempotent.
    fn unload(&mut self);

    fn loaded(&self) -> Option<&LoadSpec>;

    /// Resident memory of the loaded model as the operating system reports it.
    fn memory_bytes(&self) -> Option<u64>;

    /// Process ids that belong to the backend, so the governor does not mistake them for the
    /// user's own workload.
    fn pids(&self) -> Vec<u32>;

    /// Generates a reply, calling `on_token` for each piece of text. `on_token` returns `false`
    /// to stop early.
    fn generate(
        &mut self,
        request: &GenRequest,
        on_token: &mut dyn FnMut(&str) -> bool,
    ) -> Result<Stats, RuntimeError>;
}
