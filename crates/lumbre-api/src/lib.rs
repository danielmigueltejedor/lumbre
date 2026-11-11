//! The contract between the Lumbre daemon and its clients (the desktop app, `lumbre-code`, and
//! anything else on the machine).
//!
//! The API is HTTP on loopback, versioned in the path (`/v1/...`), and every request carries a
//! bearer token that only the user's account can read. Streaming endpoints answer with
//! server-sent events whose `data:` is one JSON value of the documented event type.
//!
//! | Method | Path | Body → answer |
//! | --- | --- | --- |
//! | GET | `/v1/status` | → [`Status`] |
//! | POST | `/v1/route` | [`RouteRequest`] → `Decision` (what would happen; nothing runs) |
//! | POST | `/v1/chat` | [`ChatRequest`] → stream of [`ChatEvent`] |
//! | POST | `/v1/unload` | → [`Status`] |
//! | GET | `/v1/models` | → `[`[`ModelInfo`]`]` |
//! | POST | `/v1/models/pull` | [`ModelRef`] → stream of [`PullEvent`] |
//! | POST | `/v1/models/remove` | [`ModelRef`] → `{"removed": bool}` |
//! | GET | `/v1/conversations?q=` | → `[`[`ConversationSummary`]`]` |
//! | GET, DELETE | `/v1/conversations/{id}` | → [`Conversation`] |

mod auth;
mod client;
mod paths;
mod types;

pub use auth::load_or_create_token;
pub use client::{Client, ClientError};
pub use paths::Paths;
pub use types::*;

/// The API version in request paths.
pub const API: &str = "v1";
/// Loopback port used when nothing else is configured.
pub const DEFAULT_PORT: u16 = 47110;
