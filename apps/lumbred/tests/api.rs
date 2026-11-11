//! End-to-end tests of the local API: a real server on a loopback port, the real engine with the
//! real system probe, and the real blocking client. No model is installed, so everything that
//! needs one must say so honestly instead of answering.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use lumbre_api::{Answer, ChatEvent, ChatRequest, Client, ClientError, ModelRef, Paths, PullEvent};
use lumbre_core::{Config, Engine, History};

struct Server {
    client: Client,
    address: String,
    home: PathBuf,
    _runtime: tokio::runtime::Runtime,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn start(name: &str) -> Server {
    let home = std::env::temp_dir().join(format!("lumbred-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let paths = Paths::at(&home);
    let config = Config { search_roots: vec![home.clone()], ..Config::default() };
    let token = lumbre_api::load_or_create_token(&paths.token_file()).unwrap();
    let app =
        lumbred::shared(Engine::new(config, &paths), History::new(paths.conversations_dir()), token.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    runtime.spawn(lumbred::housekeeping(Arc::clone(&app)));
    runtime.spawn(async move { axum::serve(listener, lumbred::router(app)).await });
    Server { client: Client::new(&address, token), address, home, _runtime: runtime }
}

fn chat(server: &Server, request: &ChatRequest) -> (Answer, Option<String>, Vec<ChatEvent>) {
    let mut events = Vec::new();
    let (answer, conversation) = server
        .client
        .chat(request, &mut |event| {
            events.push(event.clone());
            true
        })
        .unwrap()
        .expect("the stream ends with Done");
    (answer, conversation, events)
}

fn ask(server: &Server, text: &str) -> Answer {
    chat(server, &ChatRequest { text: text.to_owned(), ..Default::default() }).0
}

#[test]
fn status_reports_the_real_machine_and_an_empty_engine() {
    let server = start("status");
    let status = server.client.status().unwrap();
    assert_eq!(status.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(status.api, "v1");
    assert!(status.loaded.is_none() && !status.busy);
    assert!(status.snapshot.cpu.logical >= 1);
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert!(status.snapshot.memory.total_bytes > 0);
        assert!(status.budget.model_bytes < status.snapshot.memory.total_bytes);
    }
}

#[test]
fn requests_without_the_token_are_refused() {
    let server = start("auth");
    let stranger = Client::new(&server.address, "0".repeat(64));
    let error = stranger.status().unwrap_err();
    assert!(matches!(&error, ClientError::Api(message) if message.contains("401")), "{error}");
    let error =
        stranger.chat(&ChatRequest { text: "2+2".into(), ..Default::default() }, &mut |_| true).unwrap_err();
    assert!(matches!(error, ClientError::Api(_)));
}

#[test]
fn tools_answer_end_to_end_without_a_model() {
    let server = start("tools");
    let (answer, conversation, events) =
        chat(&server, &ChatRequest { text: "¿Cuánto es 6 * 7?".into(), ..Default::default() });
    assert_eq!(answer.text(), Some("6 * 7 = 42"));
    assert_eq!(conversation, None);
    assert!(matches!(events.first(), Some(ChatEvent::Routed { .. })));
    assert!(matches!(events.last(), Some(ChatEvent::Done { .. })));

    let time = ask(&server, "what time is it");
    assert!(
        matches!(&time, Answer::Tool { tool, text, .. } if tool == "clock.now" && text.starts_with("It is ")),
        "{time:?}"
    );

    std::fs::write(server.home.join("informe-anual.txt"), "x").unwrap();
    let found = ask(&server, "busca el archivo informe-anual");
    assert!(found.text().is_some_and(|text| text.contains("informe-anual.txt")), "{found:?}");

    // Nothing was loaded for any of this.
    assert!(server.client.status().unwrap().loaded.is_none());
}

#[test]
fn a_question_without_a_usable_model_says_why() {
    let server = start("nomodel");
    let decision = server.client.route("Explica paso a paso por qué el cielo es azul").unwrap();
    assert!(matches!(decision.route, lumbre_router::Route::Model { .. }));

    let answer = ask(&server, "Explica paso a paso por qué el cielo es azul");
    // Which of the two depends on whether llama.cpp is installed on the machine running the
    // tests; either way there is no invented answer.
    match answer {
        Answer::BackendMissing { detail } => assert!(detail.contains("llama-server"), "{detail}"),
        Answer::NoModel { suggestion, detail, .. } => {
            assert!(suggestion.is_some() || detail.contains("budget"), "{detail}")
        }
        Answer::Deferred { reasons, .. } => assert!(!reasons.is_empty()),
        other => panic!("expected an honest non-answer, got {other:?}"),
    }
}

#[test]
fn the_model_list_carries_licence_provenance_and_live_fit() {
    let server = start("models");
    let models = server.client.models().unwrap();
    assert_eq!(models.len(), 6);
    for model in &models {
        assert_eq!(model.license, "Apache-2.0");
        assert!(model.upstream.starts_with("https://huggingface.co/Qwen/"));
        for variant in &model.variants {
            assert!(!variant.installed);
            assert!(variant.estimated_ram_bytes > variant.size_bytes);
            assert!(variant.download_url.contains("/resolve/"));
        }
    }
    // Unknown models are rejected before any network access.
    let missing = ModelRef { id: "nope".into(), quant: "Q4".into() };
    assert!(
        matches!(server.client.remove_model(&missing), Err(ClientError::Api(message)) if message.contains("404"))
    );
    assert!(server.client.pull(&missing, &mut |_: PullEvent| true).is_err());
    let known = ModelRef { id: "qwen3-4b".into(), quant: "Q4_K_M".into() };
    assert!(!server.client.remove_model(&known).unwrap());
}

#[test]
fn conversations_are_stored_listed_searched_and_deleted() {
    let server = start("history");
    let first = ChatRequest {
        text: "¿Cuánto es 2^10?".into(),
        conversation: Some("new".into()),
        ..Default::default()
    };
    let (_, id, _) = chat(&server, &first);
    let id = id.expect("a stored conversation has an id");
    let follow_up =
        ChatRequest { text: "calcula 1024 / 4".into(), conversation: Some(id.clone()), ..Default::default() };
    assert_eq!(chat(&server, &follow_up).1.as_deref(), Some(id.as_str()));

    let stored = server.client.conversation(&id).unwrap();
    assert_eq!(stored.messages.len(), 4);
    assert_eq!(stored.messages[3].content, "1024 / 4 = 256");
    assert_eq!(stored.messages[3].via.as_deref(), Some("tool:calc.eval"));

    assert_eq!(server.client.conversations(None).unwrap().len(), 1);
    assert_eq!(server.client.conversations(Some("1024")).unwrap()[0].id, id);
    assert!(server.client.conversations(Some("nothing like this")).unwrap().is_empty());

    server.client.delete_conversation(&id).unwrap();
    assert!(
        matches!(server.client.conversation(&id), Err(ClientError::Api(message)) if message.contains("404"))
    );
}
