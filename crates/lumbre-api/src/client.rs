use std::io::{BufRead, BufReader};
use std::time::Duration;

use lumbre_router::Decision;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{
    load_or_create_token, Answer, ChatEvent, ChatRequest, Conversation, ConversationSummary, ErrorBody,
    ModelInfo, ModelRef, Paths, PullEvent, RouteRequest, Status, API, DEFAULT_PORT,
};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Nothing is listening: the daemon is not running.
    #[error("the Lumbre daemon is not running at {0}")]
    NotRunning(String),
    #[error("{0}")]
    Api(String),
    #[error("could not read the API token: {0}")]
    Token(std::io::Error),
    #[error("unexpected response from the daemon: {0}")]
    Protocol(String),
}

/// A blocking client for the local API. Cheap to create; holds no connection while idle.
#[derive(Clone, Debug)]
pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl Client {
    /// Finds the daemon: `$LUMBRE_ADDR`, else the address the daemon recorded, else the default
    /// loopback port. Does not connect yet.
    pub fn discover(paths: &Paths) -> Result<Self, ClientError> {
        let recorded =
            || std::fs::read_to_string(paths.endpoint_file()).ok().map(|text| text.trim().to_owned());
        let address = std::env::var("LUMBRE_ADDR")
            .ok()
            .or_else(recorded)
            .filter(|address| !address.is_empty())
            .unwrap_or_else(|| format!("127.0.0.1:{DEFAULT_PORT}"));
        let token = load_or_create_token(&paths.token_file()).map_err(ClientError::Token)?;
        Ok(Self::new(&address, token))
    }

    pub fn new(address: &str, token: String) -> Self {
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(3)))
            .build();
        Self { base: format!("http://{address}/{API}"), token, agent: ureq::Agent::new_with_config(config) }
    }

    pub fn address(&self) -> &str {
        self.base.trim_start_matches("http://").trim_end_matches(API).trim_end_matches('/')
    }

    fn transport(&self, error: ureq::Error) -> ClientError {
        match error {
            ureq::Error::ConnectionFailed | ureq::Error::Io(_) | ureq::Error::Timeout(_) => {
                ClientError::NotRunning(self.address().to_owned())
            }
            other => ClientError::Protocol(other.to_string()),
        }
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&dyn ErasedBody>,
    ) -> Result<ureq::http::Response<ureq::Body>, ClientError> {
        let url = format!("{}{path}", self.base);
        let auth = format!("Bearer {}", self.token);
        let result = match (method, body) {
            ("GET", _) => self.agent.get(&url).header("Authorization", &auth).call(),
            ("DELETE", _) => self.agent.delete(&url).header("Authorization", &auth).call(),
            (_, Some(body)) => self.agent.post(&url).header("Authorization", &auth).send_json(body.value()?),
            (_, None) => self.agent.post(&url).header("Authorization", &auth).send_empty(),
        };
        let mut response = result.map_err(|error| self.transport(error))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.body_mut().read_to_string().unwrap_or_default();
            let message = serde_json::from_str::<ErrorBody>(&text).map_or(text, |body| body.error);
            return Err(ClientError::Api(format!("{status}: {message}")));
        }
        Ok(response)
    }

    fn json<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&dyn ErasedBody>,
    ) -> Result<T, ClientError> {
        let mut response = self.send(method, path, body)?;
        response.body_mut().read_json().map_err(|error| ClientError::Protocol(error.to_string()))
    }

    /// Reads a server-sent event stream, handing each event to `on_event` until it returns
    /// `false` or the stream ends.
    fn stream<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &dyn ErasedBody,
        on_event: &mut dyn FnMut(T) -> bool,
    ) -> Result<(), ClientError> {
        let mut response = self.send("POST", path, Some(body))?;
        for line in BufReader::new(response.body_mut().as_reader()).lines() {
            let line = line.map_err(|error| ClientError::Protocol(error.to_string()))?;
            let Some(data) = line.strip_prefix("data:") else { continue };
            let event = serde_json::from_str(data.trim())
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            if !on_event(event) {
                break;
            }
        }
        Ok(())
    }

    pub fn status(&self) -> Result<Status, ClientError> {
        self.json("GET", "/status", None)
    }

    /// What the router would do with a request. Nothing is executed.
    pub fn route(&self, text: &str) -> Result<Decision, ClientError> {
        self.json("POST", "/route", Some(&RouteRequest { text: text.to_owned() }))
    }

    /// Sends a request and streams the events. Returns the final answer and the id of the
    /// conversation it was stored in, if any. `on_event` returns `false` to stop early, in which
    /// case the answer is `None`.
    pub fn chat(
        &self,
        request: &ChatRequest,
        on_event: &mut dyn FnMut(&ChatEvent) -> bool,
    ) -> Result<Option<(Answer, Option<String>)>, ClientError> {
        let mut done = None;
        self.stream("/chat", request, &mut |event: ChatEvent| {
            let keep_going = on_event(&event);
            if let ChatEvent::Done { answer, conversation } = event {
                done = Some((answer, conversation));
            }
            keep_going
        })?;
        Ok(done)
    }

    pub fn unload(&self) -> Result<Status, ClientError> {
        self.json("POST", "/unload", None)
    }

    pub fn models(&self) -> Result<Vec<ModelInfo>, ClientError> {
        self.json("GET", "/models", None)
    }

    pub fn pull(
        &self,
        model: &ModelRef,
        on_event: &mut dyn FnMut(PullEvent) -> bool,
    ) -> Result<(), ClientError> {
        self.stream("/models/pull", model, on_event)
    }

    pub fn remove_model(&self, model: &ModelRef) -> Result<bool, ClientError> {
        let value: serde_json::Value = self.json("POST", "/models/remove", Some(model))?;
        Ok(value["removed"].as_bool().unwrap_or(false))
    }

    /// Stored conversations, newest first; with a query, only those containing it.
    pub fn conversations(&self, query: Option<&str>) -> Result<Vec<ConversationSummary>, ClientError> {
        let path = match query.filter(|query| !query.trim().is_empty()) {
            Some(query) => format!("/conversations?q={}", percent_encode(query)),
            None => "/conversations".to_owned(),
        };
        self.json("GET", &path, None)
    }

    pub fn conversation(&self, id: &str) -> Result<Conversation, ClientError> {
        self.json("GET", &format!("/conversations/{}", percent_encode(id)), None)
    }

    pub fn delete_conversation(&self, id: &str) -> Result<(), ClientError> {
        self.send("DELETE", &format!("/conversations/{}", percent_encode(id)), None).map(drop)
    }
}

/// Lets `send` take any serialisable body without being generic itself.
trait ErasedBody {
    fn value(&self) -> Result<serde_json::Value, ClientError>;
}

impl<T: Serialize> ErasedBody for T {
    fn value(&self) -> Result<serde_json::Value, ClientError> {
        serde_json::to_value(self).map_err(|error| ClientError::Protocol(error.to_string()))
    }
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_encoded() {
        assert_eq!(percent_encode("año nuevo & más"), "a%C3%B1o%20nuevo%20%26%20m%C3%A1s");
    }

    #[test]
    fn a_missing_daemon_is_reported_as_such() {
        // Port 1 on loopback: nothing listens there.
        let client = Client::new("127.0.0.1:1", "t".repeat(64));
        assert_eq!(client.address(), "127.0.0.1:1");
        let error = client.status().unwrap_err();
        assert!(matches!(error, ClientError::NotRunning(_)), "{error}");
    }
}
