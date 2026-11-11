//! Everything that talks to the daemon, off the interface thread.
//!
//! The interface reads [`State`]; background threads fill it in and ask for a repaint. Nothing in
//! `State` is made up locally: each field is the daemon's last answer, or the error it gave.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use eframe::egui::Context;
use lumbre_api::{
    Answer, ChatEvent, ChatRequest, Client, ClientError, ConversationSummary, Message, ModelInfo, ModelRef,
    Paths, PullEvent, Role, Status,
};

#[derive(Clone, Debug)]
pub enum Bubble {
    User(String),
    /// An answer and what produced it.
    Assistant {
        text: String,
        via: String,
    },
    /// A request that was not answered, with the daemon's reason.
    Unanswered(Answer),
}

#[derive(Clone, Debug)]
pub enum Pull {
    Running { done_bytes: u64, total_bytes: u64 },
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct State {
    /// `Ok` is the daemon's status; `Err` is why there is none.
    pub status: Option<Result<Status, String>>,
    pub daemon_running: bool,
    pub conversations: Vec<ConversationSummary>,
    pub models: Vec<ModelInfo>,
    pub conversation: Option<String>,
    pub bubbles: Vec<Bubble>,
    /// Text of the answer being generated.
    pub streaming: Option<String>,
    /// What the daemon is doing for the current request ("routing", "loading qwen3-4b…").
    pub phase: Option<String>,
    pub pulls: BTreeMap<(String, String), Pull>,
    /// The last failed action, shown until dismissed.
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct Worker {
    pub paths: Paths,
    state: Arc<Mutex<State>>,
    ctx: Context,
    refresh: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    query: Arc<Mutex<String>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Worker {
    pub fn start(ctx: Context) -> Self {
        let worker = Self {
            paths: Paths::discover(),
            state: Arc::default(),
            ctx,
            refresh: Arc::new(AtomicBool::new(true)),
            stop: Arc::default(),
            query: Arc::default(),
        };
        let poller = worker.clone();
        std::thread::spawn(move || loop {
            poller.poll();
            // Wake early when something asked for a refresh.
            for _ in 0..10 {
                if poller.refresh.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        worker
    }

    pub fn state(&self) -> State {
        lock(&self.state).clone()
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        change(&mut lock(&self.state));
        self.ctx.request_repaint();
    }

    fn client(&self) -> Result<Client, ClientError> {
        // Rediscovered each time: the daemon may have been (re)started on another port.
        Client::discover(&self.paths)
    }

    pub fn refresh(&self) {
        self.refresh.store(true, Ordering::Relaxed);
    }

    pub fn set_query(&self, query: &str) {
        *lock(&self.query) = query.to_owned();
        self.refresh();
    }

    fn poll(&self) {
        let lists_wanted = self.refresh.swap(false, Ordering::Relaxed);
        let client = match self.client() {
            Ok(client) => client,
            Err(error) => return self.update(|state| state.status = Some(Err(error.to_string()))),
        };
        match client.status() {
            Ok(status) => {
                let first_contact = !lock(&self.state).daemon_running;
                self.update(|state| {
                    state.status = Some(Ok(status));
                    state.daemon_running = true;
                });
                // Model fit depends on live memory, so the lists follow the status.
                if lists_wanted || first_contact || !lock(&self.state).pulls.is_empty() {
                    let query = lock(&self.query).clone();
                    let conversations = client.conversations(Some(&query));
                    let models = client.models();
                    self.update(|state| {
                        match conversations {
                            Ok(conversations) => state.conversations = conversations,
                            Err(error) => state.error = Some(error.to_string()),
                        }
                        match models {
                            Ok(models) => state.models = models,
                            Err(error) => state.error = Some(error.to_string()),
                        }
                    });
                }
            }
            Err(error) => self.update(|state| {
                state.daemon_running = !matches!(error, ClientError::NotRunning(_));
                state.status = Some(Err(error.to_string()));
            }),
        }
    }

    /// Starts `lumbred` (the copy next to this program, else the one on `PATH`).
    pub fn start_daemon(&self) {
        let sibling = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("lumbred")));
        let program = sibling.filter(|path| path.is_file()).unwrap_or_else(|| "lumbred".into());
        let spawned = std::process::Command::new(&program)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(_) => self.refresh(),
            Err(error) => self.update(|state| state.error = Some(format!("{}: {error}", program.display()))),
        }
    }

    pub fn new_conversation(&self) {
        self.update(|state| {
            state.conversation = None;
            state.bubbles.clear();
        });
    }

    pub fn open_conversation(&self, id: String) {
        let worker = self.clone();
        std::thread::spawn(move || match worker.client().and_then(|client| client.conversation(&id)) {
            Ok(conversation) => worker.update(|state| {
                state.conversation = Some(conversation.id);
                state.bubbles = conversation
                    .messages
                    .into_iter()
                    .map(|message| match message.role {
                        Role::User => Bubble::User(message.content),
                        _ => {
                            Bubble::Assistant { text: message.content, via: message.via.unwrap_or_default() }
                        }
                    })
                    .collect();
            }),
            Err(error) => worker.update(|state| state.error = Some(error.to_string())),
        });
    }

    pub fn delete_conversation(&self, id: String) {
        let worker = self.clone();
        std::thread::spawn(move || {
            let result = worker.client().and_then(|client| client.delete_conversation(&id));
            worker.update(|state| {
                if let Err(error) = result {
                    state.error = Some(error.to_string());
                } else if state.conversation.as_deref() == Some(&id) {
                    state.conversation = None;
                    state.bubbles.clear();
                }
            });
            worker.refresh();
        });
    }

    pub fn stop_generation(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn send(&self, text: String, model: Option<String>) {
        let worker = self.clone();
        self.stop.store(false, Ordering::Relaxed);
        // Earlier turns go along as context; unanswered requests are not part of the conversation.
        let (history, conversation) = {
            let state = lock(&self.state);
            let history = state
                .bubbles
                .iter()
                .filter_map(|bubble| match bubble {
                    Bubble::User(content) => Some(Message { role: Role::User, content: content.clone() }),
                    Bubble::Assistant { text, .. } => {
                        Some(Message { role: Role::Assistant, content: text.clone() })
                    }
                    Bubble::Unanswered(_) => None,
                })
                .collect();
            (history, state.conversation.clone())
        };
        self.update(|state| {
            state.bubbles.push(Bubble::User(text.clone()));
            state.streaming = Some(String::new());
            state.phase = Some("routing".into());
        });
        std::thread::spawn(move || {
            let request = ChatRequest {
                text,
                history,
                model,
                max_tokens: None,
                conversation: Some(conversation.unwrap_or_else(|| "new".into())),
            };
            let result = worker.client().and_then(|client| {
                client.chat(&request, &mut |event| {
                    match event {
                        ChatEvent::Routed { decision } => {
                            let phase = decision.explanation.clone();
                            worker.update(|state| state.phase = Some(phase));
                        }
                        ChatEvent::Loading { model, quant } => {
                            let phase = format!("loading {model}:{quant}");
                            worker.update(|state| state.phase = Some(phase));
                        }
                        ChatEvent::Token { text } => worker.update(|state| {
                            state.phase = None;
                            if let Some(partial) = &mut state.streaming {
                                partial.push_str(text);
                            }
                        }),
                        ChatEvent::Done { .. } => {}
                    }
                    !worker.stop.load(Ordering::Relaxed)
                })
            });
            worker.update(|state| {
                let partial = state.streaming.take().unwrap_or_default();
                state.phase = None;
                match result {
                    Ok(Some((answer, conversation))) => {
                        if conversation.is_some() {
                            state.conversation = conversation;
                        }
                        state.bubbles.push(match answer {
                            Answer::Tool { tool, text, .. } => {
                                Bubble::Assistant { text, via: format!("tool:{tool}") }
                            }
                            Answer::Model { model, quant, text, stats, downgraded, .. } => {
                                let speed = stats
                                    .tokens_per_sec()
                                    .map_or(String::new(), |speed| format!(" · {speed:.1} tok/s"));
                                let note = if downgraded { " · smaller tier" } else { "" };
                                Bubble::Assistant { text, via: format!("{model}:{quant}{speed}{note}") }
                            }
                            other => Bubble::Unanswered(other),
                        });
                    }
                    // Stopped by the user: keep what was generated, marked as partial.
                    Ok(None) => {
                        state.bubbles.push(Bubble::Assistant { text: partial, via: "stopped".into() })
                    }
                    Err(error) => {
                        state.bubbles.push(Bubble::Unanswered(Answer::Error { message: error.to_string() }))
                    }
                }
            });
            worker.refresh();
        });
    }

    pub fn pull(&self, id: String, quant: String) {
        let worker = self.clone();
        let key = (id.clone(), quant.clone());
        self.update(|state| {
            state.pulls.insert(key.clone(), Pull::Running { done_bytes: 0, total_bytes: 0 });
        });
        std::thread::spawn(move || {
            let mut failure = None;
            let result = worker.client().and_then(|client| {
                client.pull(&ModelRef { id, quant }, &mut |event| {
                    match event {
                        PullEvent::Progress { done_bytes, total_bytes } => worker.update(|state| {
                            state.pulls.insert(key.clone(), Pull::Running { done_bytes, total_bytes });
                        }),
                        PullEvent::Done { .. } => {}
                        PullEvent::Error { message } => failure = Some(message),
                    }
                    true
                })
            });
            let failure = failure.or_else(|| result.err().map(|error| error.to_string()));
            worker.update(|state| match failure {
                Some(message) => {
                    state.pulls.insert(key.clone(), Pull::Failed(message));
                }
                None => {
                    state.pulls.remove(&key);
                }
            });
            worker.refresh();
        });
    }

    pub fn remove_model(&self, id: String, quant: String) {
        let worker = self.clone();
        std::thread::spawn(move || {
            if let Err(error) =
                worker.client().and_then(|client| client.remove_model(&ModelRef { id, quant }))
            {
                worker.update(|state| state.error = Some(error.to_string()));
            }
            worker.refresh();
        });
    }

    pub fn unload(&self) {
        let worker = self.clone();
        std::thread::spawn(move || {
            if let Err(error) = worker.client().and_then(|client| client.unload()) {
                worker.update(|state| state.error = Some(error.to_string()));
            }
            worker.refresh();
        });
    }

    pub fn dismiss_error(&self) {
        self.update(|state| state.error = None);
    }

    pub fn dismiss_pull(&self, key: &(String, String)) {
        self.update(|state| {
            state.pulls.remove(key);
        });
    }
}
