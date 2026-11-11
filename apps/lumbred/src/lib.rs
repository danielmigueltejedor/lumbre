//! The HTTP face of Lumbre: a thin layer that turns `lumbre-api` requests into calls on the
//! [`Engine`] and streams what it reports. No decisions are made here.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lumbre_api::{Answer, ChatEvent, ChatRequest, ErrorBody, ModelRef, PullEvent, RouteRequest, Status};
use lumbre_core::{Engine, History};
use lumbre_resources::{system_probe, Probe};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify};

/// How often a loaded model is checked for idleness and memory pressure.
const TICK: Duration = Duration::from_secs(5);

pub struct Shared {
    engine: Mutex<Engine>,
    history: History,
    token: String,
    /// The status as of the last time the engine was free, for when it is busy generating.
    last_status: Mutex<Option<Status>>,
    /// A probe of its own, so live measurements never wait for a generation to finish.
    probe: Mutex<Box<dyn Probe>>,
    last_request: Mutex<Instant>,
    /// Wakes the housekeeping task when a model may have been loaded.
    wake: Notify,
}

pub type App = Arc<Shared>;

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

/// A poisoned lock only means another request panicked; the data is still the best we have.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn shared(engine: Engine, history: History, token: String) -> App {
    Arc::new(Shared {
        engine: Mutex::new(engine),
        history,
        token,
        last_status: Mutex::new(None),
        probe: Mutex::new(system_probe()),
        last_request: Mutex::new(Instant::now()),
        wake: Notify::new(),
    })
}

impl Shared {
    /// True when no model is loaded and no request arrived for `quiet`.
    pub fn idle_for(&self, quiet: Duration) -> bool {
        let quiet_enough = lock(&self.last_request).elapsed() >= quiet;
        quiet_enough && self.engine.try_lock().is_ok_and(|engine| engine.is_idle())
    }

    /// Unloads the model; called on shutdown so no worker outlives the daemon.
    pub fn shutdown(&self) {
        lock(&self.engine).unload();
    }
}

pub fn router(app: App) -> Router {
    let v1 = Router::new()
        .route("/status", get(status))
        .route("/route", post(route))
        .route("/chat", post(chat))
        .route("/unload", post(unload))
        .route("/models", get(models))
        .route("/models/pull", post(pull))
        .route("/models/remove", post(remove))
        .route("/conversations", get(conversations))
        .route("/conversations/{id}", get(conversation).delete(delete_conversation))
        .layer(middleware::from_fn_with_state(Arc::clone(&app), authorize));
    Router::new().nest(&format!("/{}", lumbre_api::API), v1).with_state(app)
}

/// Unloads idle models. Sleeps without a timer while nothing is loaded, so an idle daemon does
/// not wake up at all.
pub async fn housekeeping(app: App) {
    loop {
        app.wake.notified().await;
        loop {
            tokio::time::sleep(TICK).await;
            let worker = Arc::clone(&app);
            let still_loaded = tokio::task::spawn_blocking(move || {
                // A busy engine is generating: the model is in use, check again next time.
                let Ok(mut engine) = worker.engine.try_lock() else { return true };
                engine.tick();
                !engine.is_idle()
            })
            .await
            .unwrap_or(false);
            if !still_loaded {
                break;
            }
        }
    }
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(ErrorBody { error: message.into() })).into_response()
}

fn same(a: &[u8], b: &[u8]) -> bool {
    // Constant time for equal lengths, so the token cannot be guessed byte by byte.
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn authorize(State(app): State<App>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !presented.is_some_and(|token| same(token.as_bytes(), app.token.as_bytes())) {
        return error(StatusCode::UNAUTHORIZED, "missing or wrong API token");
    }
    *lock(&app.last_request) = Instant::now();
    next.run(request).await
}

/// Runs blocking work off the async thread and turns its result into a response.
async fn blocking<T: IntoResponse + Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Response {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value.into_response(),
        Err(join) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("request failed: {join}")),
    }
}

fn fresh_status(app: &Shared) -> Status {
    match app.engine.try_lock() {
        Ok(mut engine) => {
            let status = engine.status();
            *lock(&app.last_status) = Some(status.clone());
            status
        }
        // Generating. Report the model as it was when generation began, with live measurements.
        Err(_) => {
            let snapshot = lock(&app.probe).snapshot();
            let cached = lock(&app.last_status).clone();
            match cached {
                Some(mut status) => {
                    status.snapshot = snapshot;
                    status.busy = true;
                    status
                }
                // No status was ever taken: wait for the engine after all.
                None => lock(&app.engine).status(),
            }
        }
    }
}

async fn status(State(app): State<App>) -> Response {
    blocking(move || Json(fresh_status(&app))).await
}

async fn route(State(app): State<App>, Json(request): Json<RouteRequest>) -> Response {
    blocking(move || Json(lock(&app.engine).route(&request.text)).into_response()).await
}

async fn unload(State(app): State<App>) -> Response {
    blocking(move || {
        let mut engine = lock(&app.engine);
        engine.unload();
        Json(engine.status())
    })
    .await
}

async fn models(State(app): State<App>) -> Response {
    blocking(move || Json(lock(&app.engine).models()).into_response()).await
}

/// Adapts a channel of already-formatted events to an HTTP body.
struct Events(mpsc::Receiver<String>);

impl futures_core::Stream for Events {
    type Item = Result<String, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx).map(|event| event.map(Ok))
    }
}

fn event_stream(events: mpsc::Receiver<String>) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(Events(events)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Sends one event; `false` when the client has gone away.
fn emit<T: Serialize>(events: &mpsc::Sender<String>, event: &T) -> bool {
    match serde_json::to_string(event) {
        Ok(json) => events.blocking_send(format!("data: {json}\n\n")).is_ok(),
        Err(_) => false,
    }
}

async fn chat(State(app): State<App>, Json(request): Json<ChatRequest>) -> Response {
    let (events, stream) = mpsc::channel(64);
    tokio::task::spawn_blocking(move || {
        let mut engine = lock(&app.engine);
        // Remember what was resident before generating, for status requests that arrive meanwhile.
        *lock(&app.last_status) = Some(engine.status());
        let answer = engine.chat(&request, &mut |event| emit(&events, &event));
        *lock(&app.last_status) = Some(engine.status());
        drop(engine);
        app.wake.notify_one();

        let via = match &answer {
            Answer::Tool { tool, .. } => format!("tool:{tool}"),
            Answer::Model { model, quant, .. } => format!("{model}:{quant}"),
            _ => String::new(),
        };
        // Only real answers are stored; a deferral or an error is not part of the conversation.
        let conversation = match (request.conversation.as_deref(), answer.text()) {
            (Some(id), Some(text)) => match app.history.append(Some(id), &request.text, text, &via) {
                Ok(id) => Some(id),
                Err(error) => {
                    tracing::error!(%error, "could not store the conversation");
                    None
                }
            },
            _ => None,
        };
        emit(&events, &ChatEvent::Done { answer, conversation });
    });
    event_stream(stream)
}

async fn pull(State(app): State<App>, Json(model): Json<ModelRef>) -> Response {
    let found = {
        let engine = lock(&app.engine);
        engine
            .model(&model.id, &model.quant)
            .map(|(manifest, variant)| (manifest, variant, engine.store().clone()))
    };
    let Some((manifest, variant, store)) = found else {
        return error(StatusCode::NOT_FOUND, format!("unknown model `{}:{}`", model.id, model.quant));
    };
    let (events, stream) = mpsc::channel(16);
    tokio::task::spawn_blocking(move || {
        tracing::info!(model = %manifest.id, quant = %variant.quant, url = %manifest.url(&variant), "download started");
        let mut last = Instant::now();
        let result = store.download(&manifest, &variant, &mut |progress| {
            // A few updates per second are plenty for a progress bar.
            if last.elapsed() < Duration::from_millis(250) && progress.done_bytes < progress.total_bytes {
                return !events.is_closed();
            }
            last = Instant::now();
            emit(
                &events,
                &PullEvent::Progress { done_bytes: progress.done_bytes, total_bytes: progress.total_bytes },
            )
        });
        let event = match result {
            Ok(path) => PullEvent::Done { path: path.display().to_string() },
            Err(error) => PullEvent::Error { message: error.to_string() },
        };
        tracing::info!(model = %manifest.id, outcome = ?event, "download finished");
        emit(&events, &event);
    });
    event_stream(stream)
}

async fn remove(State(app): State<App>, Json(model): Json<ModelRef>) -> Response {
    blocking(move || {
        let mut engine = lock(&app.engine);
        let Some((manifest, variant)) = engine.model(&model.id, &model.quant) else {
            return error(StatusCode::NOT_FOUND, format!("unknown model `{}:{}`", model.id, model.quant));
        };
        if engine.loaded_ref() == Some((&model.id, &variant.quant)) {
            engine.unload();
        }
        match engine.store().remove(&manifest, &variant) {
            Ok(removed) => Json(serde_json::json!({ "removed": removed })).into_response(),
            Err(failure) => error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()),
        }
    })
    .await
}

#[derive(Debug, Deserialize)]
struct Search {
    q: Option<String>,
}

async fn conversations(State(app): State<App>, Query(search): Query<Search>) -> Response {
    blocking(move || Json(app.history.list(search.q.as_deref())).into_response()).await
}

fn history_result<T: Serialize>(result: std::io::Result<T>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
            error(StatusCode::NOT_FOUND, failure.to_string())
        }
        Err(failure) => error(StatusCode::INTERNAL_SERVER_ERROR, failure.to_string()),
    }
}

async fn conversation(State(app): State<App>, Path(id): Path<String>) -> Response {
    blocking(move || history_result(app.history.get(&id))).await
}

async fn delete_conversation(State(app): State<App>, Path(id): Path<String>) -> Response {
    blocking(move || history_result(app.history.delete(&id))).await
}
