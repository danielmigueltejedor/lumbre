use lumbre_resources::{Budget, Mode, Reason, Snapshot, UnloadReason};
use lumbre_router::{Decision, Tier};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// Everything the interface shows about the backend. Every field is measured or decided by the
/// daemon at the moment of the request; nothing is cached for display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// Lumbre version (`CARGO_PKG_VERSION` of the daemon).
    pub version: String,
    pub api: String,
    pub mode: Mode,
    pub snapshot: Snapshot,
    /// What a model may use right now, and why.
    pub budget: Budget,
    pub loaded: Option<LoadedModel>,
    pub backend: BackendStatus,
    pub uptime_secs: u64,
    /// Why the last model was unloaded, if one was.
    pub last_unload: Option<UnloadReason>,
    /// Seconds until loading is allowed again after a pressure unload.
    pub load_blocked_secs: Option<u64>,
    /// A request is being processed right now. `snapshot` is still live; the other fields are
    /// from the moment that request started.
    #[serde(default)]
    pub busy: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoadedModel {
    pub id: String,
    pub quant: String,
    pub tier: Tier,
    /// Resident memory of the worker as the OS reports it.
    pub memory_bytes: Option<u64>,
    pub idle_secs: u64,
    pub context_tokens: u32,
    pub threads: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendStatus {
    pub name: String,
    pub available: bool,
    /// Why it is not available.
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRequest {
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub text: String,
    /// Earlier turns of the conversation, oldest first.
    #[serde(default)]
    pub history: Vec<Message>,
    /// Force a model: `id` or `id:QUANT`. `None` lets the router and governor choose.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Append this exchange to a stored conversation; `"new"` creates one.
    #[serde(default)]
    pub conversation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum ChatEvent {
    /// The routing decision, sent first.
    Routed {
        decision: Decision,
    },
    /// A model is being brought up; this can take seconds.
    Loading {
        model: String,
        quant: String,
    },
    Token {
        text: String,
    },
    /// Always the last event.
    Done {
        answer: Answer,
        conversation: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GenStats {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
}

impl GenStats {
    /// Generation speed after the first token, when the backend reported token counts.
    pub fn tokens_per_sec(&self) -> Option<f32> {
        let tokens = self.completion_tokens?;
        let millis = self.total_ms.checked_sub(self.first_token_ms?)?;
        (millis > 0 && tokens > 1).then(|| (tokens - 1) as f32 * 1000.0 / millis as f32)
    }
}

/// How a request ended. Every non-answer says why, so the interface never has to guess.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Answer {
    /// Answered by a tool; no model ran.
    Tool {
        tool: String,
        text: String,
        data: Value,
    },
    Model {
        model: String,
        quant: String,
        tier: Tier,
        /// A smaller tier than the router asked for was used.
        downgraded: bool,
        text: String,
        stats: GenStats,
    },
    /// The machine is busy; nothing was loaded.
    Deferred {
        reasons: Vec<Reason>,
        retry_after_secs: Option<u64>,
    },
    /// No installed model fits the budget.
    NoModel {
        wanted: Tier,
        budget_bytes: u64,
        suggestion: Option<String>,
        detail: String,
    },
    BackendMissing {
        detail: String,
    },
    Error {
        message: String,
    },
}

impl Answer {
    /// The text to show the user, for answers that have one.
    pub fn text(&self) -> Option<&str> {
        match self {
            Answer::Tool { text, .. } | Answer::Model { text, .. } => Some(text),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub tier: Tier,
    pub publisher: String,
    pub parameters: String,
    pub license: String,
    pub license_url: String,
    pub upstream: String,
    pub variants: Vec<VariantInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantInfo {
    pub quant: String,
    pub size_bytes: u64,
    /// Heuristic memory need at the configured context size (see `lumbre-models`).
    pub estimated_ram_bytes: u64,
    pub installed: bool,
    /// Whether it fits the budget at the moment of the request.
    pub fits_budget: bool,
    pub download_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub id: String,
    pub quant: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum PullEvent {
    Progress { done_bytes: u64, total_bytes: u64 },
    Done { path: String },
    Error { message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: Role,
    pub content: String,
    /// RFC 3339 local time.
    pub at: String,
    /// For assistant messages: what produced it, e.g. `tool:clock.now` or `qwen3-4b:Q4_K_M`.
    #[serde(default)]
    pub via: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created: String,
    pub updated: String,
    pub messages: Vec<StoredMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub updated: String,
    pub messages: usize,
    /// The matching passage, when the listing came from a search.
    pub snippet: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_have_a_stable_wire_shape() {
        let event = ChatEvent::Token { text: "hola".into() };
        assert_eq!(serde_json::to_string(&event).unwrap(), r#"{"event":"token","text":"hola"}"#);
        let answer =
            Answer::Deferred { reasons: vec![Reason::MemoryPressureCritical], retry_after_secs: Some(30) };
        assert_eq!(
            serde_json::to_string(&answer).unwrap(),
            r#"{"kind":"deferred","reasons":[{"kind":"memory_pressure_critical"}],"retry_after_secs":30}"#
        );
        let request: ChatRequest = serde_json::from_str(r#"{"text":"hi"}"#).unwrap();
        assert_eq!(request, ChatRequest { text: "hi".into(), ..Default::default() });
    }

    #[test]
    fn speed_needs_token_counts() {
        let stats = GenStats {
            completion_tokens: Some(21),
            first_token_ms: Some(500),
            total_ms: 2500,
            ..Default::default()
        };
        assert_eq!(stats.tokens_per_sec(), Some(10.0));
        assert_eq!(GenStats::default().tokens_per_sec(), None);
    }
}
