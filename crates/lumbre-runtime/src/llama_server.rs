//! llama.cpp as a worker process, driven through its bundled HTTP server on loopback.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::sse::{parse_line, Chunk};
use crate::{Backend, GenRequest, LoadSpec, RuntimeError, Stats};

const BINARY: &str = "llama-server";
/// Large models on slow disks take a while to map; beyond this something is wrong.
const START_TIMEOUT: Duration = Duration::from_secs(180);

/// Runs `llama-server` (from llama.cpp) as a child process and talks to it over loopback.
///
/// The binary is looked for, in order, at the configured path, in `$LUMBRE_LLAMA_SERVER`, and on
/// `$PATH`. Lumbre 0.1 does not bundle it.
#[derive(Debug)]
pub struct LlamaServer {
    configured: Option<PathBuf>,
    worker: Option<Worker>,
    agent: ureq::Agent,
}

#[derive(Debug)]
struct Worker {
    /// `None` when attached to a server Lumbre did not start.
    child: Option<Child>,
    base_url: String,
    spec: LoadSpec,
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).map(|dir| dir.join(name)).find(|candidate| candidate.is_file())
}

fn free_port() -> std::io::Result<u16> {
    // Ask the kernel for an unused port; the worker binds it a moment later.
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

impl LlamaServer {
    pub fn new(configured: Option<PathBuf>) -> Self {
        // Never route loopback traffic through a proxy from the environment.
        let config =
            ureq::Agent::config_builder().proxy(None).timeout_connect(Some(Duration::from_secs(5))).build();
        Self { configured, worker: None, agent: ureq::Agent::new_with_config(config) }
    }

    /// Uses a llama.cpp server that is already running (started by the user or by a test) instead
    /// of spawning one. `spec` describes what that server has loaded.
    pub fn attach(base_url: impl Into<String>, spec: LoadSpec) -> Result<Self, RuntimeError> {
        let mut backend = Self::new(None);
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if !backend.healthy(&base_url) {
            return Err(RuntimeError::Http(format!("{base_url}/health did not answer")));
        }
        backend.worker = Some(Worker { child: None, base_url, spec });
        Ok(backend)
    }

    /// The binary that would be run, if one can be found.
    pub fn binary(&self) -> Option<PathBuf> {
        let from_env = std::env::var_os("LUMBRE_LLAMA_SERVER").map(PathBuf::from);
        [self.configured.clone(), from_env]
            .into_iter()
            .flatten()
            .find(|path| path.is_file())
            .or_else(|| find_on_path(BINARY))
    }

    fn healthy(&self, base_url: &str) -> bool {
        self.agent.get(format!("{base_url}/health")).call().is_ok()
    }

    fn command(binary: &Path, spec: &LoadSpec, port: u16) -> Command {
        let mut command = Command::new(binary);
        command
            .arg("--model")
            .arg(&spec.model_path)
            .args(["--host", "127.0.0.1"])
            .args(["--port", &port.to_string()])
            .args(["--ctx-size", &spec.context_tokens.to_string()])
            .args(["--threads", &spec.threads.to_string()])
            .args(["--n-gpu-layers", &spec.gpu_layers.to_string()])
            // One conversation at a time: parallel slots would split the context between them.
            .args(["--parallel", "1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}

impl Backend for LlamaServer {
    fn name(&self) -> &'static str {
        "llama.cpp (llama-server worker)"
    }

    fn available(&self) -> Result<(), RuntimeError> {
        if self.worker.is_some() || self.binary().is_some() {
            return Ok(());
        }
        Err(RuntimeError::BackendMissing(format!(
            "`{BINARY}` (llama.cpp) was not found on PATH; install llama.cpp or set `llama_server` in the configuration"
        )))
    }

    fn load(&mut self, spec: &LoadSpec) -> Result<(), RuntimeError> {
        self.unload();
        self.available()?;
        let binary = self.binary().ok_or_else(|| RuntimeError::BackendMissing(BINARY.into()))?;
        let port = free_port().map_err(RuntimeError::Spawn)?;
        let mut child = Self::command(&binary, spec, port).spawn().map_err(RuntimeError::Spawn)?;
        let base_url = format!("http://127.0.0.1:{port}");
        tracing::info!(pid = child.id(), model = %spec.model_path.display(), ctx = spec.context_tokens, threads = spec.threads, "worker started");

        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().map_err(RuntimeError::Spawn)? {
                return Err(RuntimeError::Exited(status.to_string()));
            }
            if self.healthy(&base_url) {
                break;
            }
            if started.elapsed() > START_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RuntimeError::StartTimeout(START_TIMEOUT.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        tracing::info!(pid = child.id(), load_ms = started.elapsed().as_millis() as u64, "worker ready");
        self.worker = Some(Worker { child: Some(child), base_url, spec: spec.clone() });
        Ok(())
    }

    fn unload(&mut self) {
        if let Some(Worker { child: Some(mut child), .. }) = self.worker.take() {
            let _ = child.kill();
            // Reap it: the memory is only really gone once the process is.
            let _ = child.wait();
            tracing::info!(pid = child.id(), "worker stopped");
        }
    }

    fn loaded(&self) -> Option<&LoadSpec> {
        self.worker.as_ref().map(|worker| &worker.spec)
    }

    fn memory_bytes(&self) -> Option<u64> {
        let child = self.worker.as_ref()?.child.as_ref()?;
        lumbre_resources::process_rss(child.id())
    }

    fn pids(&self) -> Vec<u32> {
        self.worker.iter().filter_map(|worker| worker.child.as_ref()).map(Child::id).collect()
    }

    fn generate(
        &mut self,
        request: &GenRequest,
        on_token: &mut dyn FnMut(&str) -> bool,
    ) -> Result<Stats, RuntimeError> {
        let worker = self.worker.as_ref().ok_or(RuntimeError::NotLoaded)?;
        let body = json!({
            "messages": request.messages,
            "max_tokens": request.max_tokens,
            "temperature": request.temperature,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        let started = Instant::now();
        let mut response = self
            .agent
            .post(format!("{}/v1/chat/completions", worker.base_url))
            .send_json(&body)
            .map_err(|error| RuntimeError::Http(error.to_string()))?;

        let mut stats = Stats::default();
        let reader = BufReader::new(response.body_mut().as_reader());
        'stream: for line in reader.lines() {
            let line = line.map_err(|error| RuntimeError::Http(error.to_string()))?;
            for chunk in parse_line(&line).map_err(RuntimeError::Protocol)? {
                match chunk {
                    Chunk::Text(text) => {
                        stats.first_token_ms.get_or_insert(started.elapsed().as_millis() as u64);
                        if !on_token(&text) {
                            // Dropping the response closes the connection, which stops generation.
                            break 'stream;
                        }
                    }
                    Chunk::Usage { prompt_tokens, completion_tokens } => {
                        stats.prompt_tokens = prompt_tokens;
                        stats.completion_tokens = completion_tokens;
                    }
                    Chunk::Done => break 'stream,
                }
            }
        }
        stats.total_ms = started.elapsed().as_millis() as u64;
        Ok(stats)
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        self.unload();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, Role};
    use std::io::{Read, Write};
    use std::sync::mpsc;

    fn spec() -> LoadSpec {
        LoadSpec {
            model_path: "/nonexistent/model.gguf".into(),
            context_tokens: 2048,
            threads: 2,
            gpu_layers: 0,
        }
    }

    /// A stand-in for llama-server that speaks just enough of its protocol: `GET /health` and a
    /// streamed `POST /v1/chat/completions`. It forwards each request body to the test.
    ///
    /// This checks Lumbre's side of the protocol. It is not evidence about llama.cpp itself; that
    /// needs the real binary and a model (see `tests/` and the README).
    fn fake_server(events: &'static [&'static str]) -> (String, mpsc::Receiver<serde_json::Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (bodies, received) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                if request_line.starts_with("GET /health") {
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"status\":\"ok\"}").unwrap();
                    continue;
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let _ = bodies.send(serde_json::from_slice(&body).unwrap());
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
                for event in events {
                    if stream.write_all(format!("data: {event}\n\n").as_bytes()).is_err() {
                        break;
                    }
                }
            }
        });
        (url, received)
    }

    fn request() -> GenRequest {
        GenRequest {
            messages: vec![Message { role: Role::User, content: "Hola".into() }],
            max_tokens: 64,
            temperature: 0.5,
        }
    }

    #[test]
    fn streams_tokens_and_reports_usage() {
        let (url, bodies) = fake_server(&[
            r#"{"choices":[{"delta":{"role":"assistant"}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hola"}}]}"#,
            r#"{"choices":[{"delta":{"content":", mundo"}}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":3}}"#,
            "[DONE]",
        ]);
        let mut backend = LlamaServer::attach(url, spec()).unwrap();
        assert_eq!(backend.loaded(), Some(&spec()));
        let mut text = String::new();
        let stats = backend
            .generate(&request(), &mut |token| {
                text.push_str(token);
                true
            })
            .unwrap();
        assert_eq!(text, "Hola, mundo");
        assert_eq!((stats.prompt_tokens, stats.completion_tokens), (Some(9), Some(3)));
        assert!(stats.first_token_ms.is_some());

        let sent = bodies.recv().unwrap();
        assert_eq!(sent["messages"], json!([{"role": "user", "content": "Hola"}]));
        assert_eq!((sent["stream"].as_bool(), sent["max_tokens"].as_u64()), (Some(true), Some(64)));
    }

    #[test]
    fn the_caller_can_stop_generation() {
        let (url, _bodies) = fake_server(&[
            r#"{"choices":[{"delta":{"content":"uno"}}]}"#,
            r#"{"choices":[{"delta":{"content":"dos"}}]}"#,
            "[DONE]",
        ]);
        let mut backend = LlamaServer::attach(url, spec()).unwrap();
        let mut seen = Vec::new();
        backend
            .generate(&request(), &mut |token| {
                seen.push(token.to_owned());
                false
            })
            .unwrap();
        assert_eq!(seen, ["uno"]);
    }

    #[test]
    fn a_server_error_is_an_error_not_an_empty_answer() {
        let (url, _bodies) =
            fake_server(&[r#"{"error":{"message":"the request exceeds the context size"}}"#]);
        let mut backend = LlamaServer::attach(url, spec()).unwrap();
        let error = backend.generate(&request(), &mut |_| true).unwrap_err();
        assert!(
            matches!(&error, RuntimeError::Protocol(message) if message.contains("context size")),
            "{error}"
        );
    }

    #[test]
    fn generating_without_a_model_fails_cleanly() {
        let mut backend = LlamaServer::new(None);
        assert!(matches!(backend.generate(&request(), &mut |_| true), Err(RuntimeError::NotLoaded)));
        assert_eq!(backend.memory_bytes(), None);
        assert!(backend.pids().is_empty());
        backend.unload();
    }

    #[test]
    #[cfg(unix)]
    fn a_worker_that_dies_while_loading_is_reported() {
        // `false` exits at once with a failure status, like a llama-server given a broken model.
        let Some(binary) =
            ["/usr/bin/false", "/bin/false"].into_iter().map(PathBuf::from).find(|path| path.is_file())
        else {
            return;
        };
        let mut backend = LlamaServer::new(Some(binary));
        let error = backend.load(&spec()).unwrap_err();
        assert!(matches!(error, RuntimeError::Exited(_)), "{error}");
        assert!(backend.loaded().is_none());
    }

    #[test]
    fn the_worker_command_line_carries_the_load_spec() {
        let command = LlamaServer::command(Path::new("llama-server"), &spec(), 4242);
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let expected = [
            "--model",
            "/nonexistent/model.gguf",
            "--host",
            "127.0.0.1",
            "--port",
            "4242",
            "--ctx-size",
            "2048",
            "--threads",
            "2",
            "--n-gpu-layers",
            "0",
            "--parallel",
            "1",
        ];
        assert_eq!(args, expected);
    }
}
