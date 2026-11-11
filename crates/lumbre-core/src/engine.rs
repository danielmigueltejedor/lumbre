use std::path::PathBuf;
use std::time::Instant;

use lumbre_api::{
    Answer, BackendStatus, ChatEvent, ChatRequest, GenStats, LoadedModel, ModelInfo, Paths, Role, Status,
    VariantInfo, API,
};
use lumbre_models::{estimate_ram_bytes, Manifest, Registry, Store, Variant};
use lumbre_resources::{
    budget, system_probe, Budget, LoadGate, Policy, Probe, Reason, Residency, ResidencyDecision,
    UnloadReason, Verdict,
};
use lumbre_router::{Decision, Route, Router, Tier};
use lumbre_runtime::{Backend, GenRequest, LlamaServer, LoadSpec, Message, RuntimeError};
use lumbre_tools::{Context, Language, Permission};
use serde_json::Value;

use crate::Config;

const SYSTEM_PROMPT: &str =
    "You are Lumbre, an assistant that runs entirely on the user's computer. Answer in the language the user writes in. Be accurate and concise; say so when you do not know.";

struct Loaded {
    id: String,
    quant: String,
    tier: Tier,
    residency: Residency,
}

struct Choice {
    id: String,
    quant: String,
    tier: Tier,
    path: PathBuf,
    downgraded: bool,
}

/// The coordinator. One per daemon; not `Sync` — callers serialise access.
pub struct Engine {
    config: Config,
    policy: Policy,
    router: Router,
    tools: lumbre_tools::Registry,
    registry: Registry,
    store: Store,
    backend: Box<dyn Backend>,
    probe: Box<dyn Probe>,
    loaded: Option<Loaded>,
    gate: LoadGate,
    last_unload: Option<UnloadReason>,
    started: Instant,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("backend", &self.backend.name())
            .field("loaded", &self.loaded_ref())
            .finish()
    }
}

fn step_down(tier: Tier) -> Tier {
    match tier {
        Tier::Large => Tier::Standard,
        Tier::Standard => Tier::Mini,
        other => other,
    }
}

impl Engine {
    /// The engine for this machine: the system probe, the llama.cpp worker backend, the built-in
    /// manifests plus the user's, and the model store in the data directory.
    pub fn new(config: Config, paths: &Paths) -> Self {
        let mut registry = Registry::builtin();
        for (path, error) in registry.load_dir(&paths.manifests_dir()) {
            tracing::warn!(path = %path.display(), %error, "ignoring a model manifest");
        }
        let backend = Box::new(LlamaServer::new(config.llama_server.clone()));
        Self::with_parts(config, registry, Store::new(paths.models_dir()), backend, system_probe())
    }

    pub fn with_parts(
        config: Config,
        registry: Registry,
        store: Store,
        backend: Box<dyn Backend>,
        probe: Box<dyn Probe>,
    ) -> Self {
        Self {
            policy: config.policy(),
            config,
            router: Router::new(),
            tools: lumbre_tools::Registry::builtin(),
            registry,
            store,
            backend,
            probe,
            loaded: None,
            gate: LoadGate::default(),
            last_unload: None,
            started: Instant::now(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The manifest and variant behind `id` and `quant`, for downloads and removals.
    pub fn model(&self, id: &str, quant: &str) -> Option<(Manifest, Variant)> {
        let manifest = self.registry.get(id)?;
        Some((manifest.clone(), manifest.variant(quant)?.clone()))
    }

    /// `(id, quant)` of the resident model.
    pub fn loaded_ref(&self) -> Option<(&str, &str)> {
        self.loaded.as_ref().map(|loaded| (loaded.id.as_str(), loaded.quant.as_str()))
    }

    /// What the router would do. Runs nothing.
    pub fn route(&self, text: &str) -> Decision {
        self.router.route(text)
    }

    fn budget_now(&mut self) -> (lumbre_resources::Snapshot, Budget) {
        let snapshot = self.probe.snapshot();
        let held = self.backend.memory_bytes().unwrap_or(0);
        let mut own = self.backend.pids();
        own.push(std::process::id());
        let budget = budget(&self.policy, &snapshot, held, &own);
        (snapshot, budget)
    }

    /// Handles one request from start to finish. Events are streamed to `on_event`, which returns
    /// `false` to stop generation; the final [`Answer`] is returned (the caller sends `Done`).
    pub fn chat(&mut self, request: &ChatRequest, on_event: &mut dyn FnMut(ChatEvent) -> bool) -> Answer {
        let decision = self.router.route(&request.text);
        tracing::info!(stage = ?decision.stage, route = ?decision.route, confidence = decision.confidence, "routed");
        on_event(ChatEvent::Routed { decision: decision.clone() });
        match (decision.route, &request.model) {
            (Route::Tool { name, args }, None) => self.run_tool(&name, &args, &request.text),
            // The user forced a model: honour it even for something a tool could answer.
            (Route::Tool { .. }, Some(_)) => self.run_model(Tier::Standard, request, on_event),
            (Route::Model { tier, .. }, _) => self.run_model(tier, request, on_event),
        }
    }

    fn run_tool(&self, name: &str, args: &Value, text: &str) -> Answer {
        let context = Context {
            // Configuring search folders is the user's grant to read file names in them.
            granted: if self.config.search_roots.is_empty() {
                Vec::new()
            } else {
                vec![Permission::ReadFiles]
            },
            roots: self.config.search_roots.clone(),
            language: Language::detect(text),
        };
        match self.tools.invoke(name, args, &context) {
            Ok(output) => Answer::Tool { tool: name.to_owned(), text: output.text, data: output.data },
            Err(error) => Answer::Error { message: error.to_string() },
        }
    }

    fn choose(&self, tier: Tier, forced: Option<&str>, budget: &Budget) -> Result<Choice, Answer> {
        let context = self.config.context_tokens;
        let installed = |manifest: &Manifest, variant: &Variant| self.store.is_installed(manifest, variant);
        let choice = |manifest: &Manifest, variant: &Variant, downgraded| Choice {
            id: manifest.id.clone(),
            quant: variant.quant.clone(),
            tier: manifest.tier,
            path: self.store.path(manifest, variant),
            downgraded,
        };

        if let Some(spec) = forced {
            let (id, quant) = spec.split_once(':').map_or((spec, None), |(id, quant)| (id, Some(quant)));
            let error = |message: String| Answer::Error { message };
            let manifest = self.registry.get(id).ok_or_else(|| error(format!("unknown model `{id}`")))?;
            let mut variants: Vec<&Variant> = match quant {
                Some(quant) => vec![manifest
                    .variant(quant)
                    .ok_or_else(|| error(format!("`{id}` has no `{quant}` quantisation")))?],
                None => manifest.variants.iter().collect(),
            };
            variants.retain(|variant| installed(manifest, variant));
            variants.sort_by_key(|variant| std::cmp::Reverse(variant.size_bytes));
            if variants.is_empty() {
                return Err(error(format!("`{spec}` is not installed")));
            }
            // A manual choice still has to fit: the budget is a promise to the rest of the system.
            return match variants
                .iter()
                .find(|variant| estimate_ram_bytes(variant, context) <= budget.model_bytes)
            {
                Some(variant) => Ok(choice(manifest, variant, false)),
                None => Err(Answer::NoModel {
                    wanted: manifest.tier,
                    budget_bytes: budget.model_bytes,
                    suggestion: None,
                    detail: format!(
                        "`{spec}` needs about {} MiB and the current budget is {} MiB",
                        variants.last().map_or(0, |variant| estimate_ram_bytes(variant, context)) >> 20,
                        budget.model_bytes >> 20
                    ),
                }),
            };
        }

        let wanted = if budget.verdict == Verdict::Degrade { step_down(tier) } else { tier };
        if let Some(pick) = self.registry.pick(wanted, budget.model_bytes, context, installed) {
            return Ok(choice(pick.manifest, pick.variant, pick.downgraded || wanted != tier));
        }
        let suggestion = self
            .registry
            .pick(wanted, budget.model_bytes, context, |_, _| true)
            .map(|pick| format!("{}:{}", pick.manifest.id, pick.variant.quant));
        let detail = match (&suggestion, self.store.installed(&self.registry).is_empty()) {
            (Some(suggestion), true) => {
                format!("no model is installed yet; `{suggestion}` fits this machine right now")
            }
            (Some(suggestion), false) => {
                format!("no installed model fits the current budget; `{suggestion}` would")
            }
            (None, _) => "the current memory budget is too small for any known model".to_owned(),
        };
        Err(Answer::NoModel { wanted: tier, budget_bytes: budget.model_bytes, suggestion, detail })
    }

    fn run_model(
        &mut self,
        tier: Tier,
        request: &ChatRequest,
        on_event: &mut dyn FnMut(ChatEvent) -> bool,
    ) -> Answer {
        if let Err(error) = self.backend.available() {
            return Answer::BackendMissing { detail: error.to_string() };
        }
        let (_, budget) = self.budget_now();
        if budget.verdict == Verdict::Defer {
            tracing::info!(reasons = ?budget.reasons, "deferred");
            return Answer::Deferred { reasons: budget.reasons, retry_after_secs: None };
        }
        let choice = match self.choose(tier, request.model.as_deref(), &budget) {
            Ok(choice) => choice,
            Err(answer) => return answer,
        };

        let resident =
            self.loaded_ref() == Some((&choice.id, &choice.quant)) && self.backend.loaded().is_some();
        if !resident {
            let now = Instant::now();
            if let Some(left) = self.gate.closed_for(now) {
                return Answer::Deferred {
                    reasons: vec![Reason::RecentPressure],
                    retry_after_secs: Some(left.as_secs().max(1)),
                };
            }
            on_event(ChatEvent::Loading { model: choice.id.clone(), quant: choice.quant.clone() });
            // Never two models at once: the old one goes before the new one comes.
            self.unload();
            let spec = LoadSpec {
                model_path: choice.path.clone(),
                context_tokens: self.config.context_tokens,
                threads: budget.threads,
                gpu_layers: self.config.gpu_layers,
            };
            if let Err(error) = self.backend.load(&spec) {
                tracing::error!(%error, model = %choice.id, "load failed");
                return match error {
                    RuntimeError::BackendMissing(detail) => Answer::BackendMissing { detail },
                    other => Answer::Error { message: other.to_string() },
                };
            }
            self.loaded = Some(Loaded {
                id: choice.id.clone(),
                quant: choice.quant.clone(),
                tier: choice.tier,
                residency: Residency::new(Instant::now()),
            });
        }

        let mut messages =
            vec![Message { role: lumbre_runtime::Role::System, content: SYSTEM_PROMPT.to_owned() }];
        messages.extend(request.history.iter().map(|message| Message {
            role: match message.role {
                Role::System => lumbre_runtime::Role::System,
                Role::User => lumbre_runtime::Role::User,
                Role::Assistant => lumbre_runtime::Role::Assistant,
            },
            content: message.content.clone(),
        }));
        messages.push(Message { role: lumbre_runtime::Role::User, content: request.text.clone() });
        let generation = GenRequest {
            messages,
            max_tokens: request.max_tokens.unwrap_or(self.config.max_tokens),
            temperature: 0.7,
        };

        let mut text = String::new();
        let result = self.backend.generate(&generation, &mut |token| {
            text.push_str(token);
            on_event(ChatEvent::Token { text: token.to_owned() })
        });
        if let Some(loaded) = &mut self.loaded {
            loaded.residency.touch(Instant::now());
        }
        match result {
            Ok(stats) => {
                tracing::info!(model = %choice.id, quant = %choice.quant, total_ms = stats.total_ms, first_token_ms = ?stats.first_token_ms, completion_tokens = ?stats.completion_tokens, "generated");
                Answer::Model {
                    model: choice.id,
                    quant: choice.quant,
                    tier: choice.tier,
                    downgraded: choice.downgraded,
                    text,
                    stats: GenStats {
                        prompt_tokens: stats.prompt_tokens,
                        completion_tokens: stats.completion_tokens,
                        first_token_ms: stats.first_token_ms,
                        total_ms: stats.total_ms,
                    },
                }
            }
            Err(error) => {
                tracing::error!(%error, "generation failed");
                // The worker may be in an unknown state; a fresh one is cheaper than guessing.
                self.unload();
                Answer::Error { message: error.to_string() }
            }
        }
    }

    /// Unloads the resident model now, if there is one.
    pub fn unload(&mut self) {
        self.backend.unload();
        self.loaded = None;
    }

    /// Housekeeping; call every few seconds while a model is loaded. Unloads it when it has been
    /// idle too long or the system is under memory pressure.
    pub fn tick(&mut self) -> Option<UnloadReason> {
        self.tick_at(Instant::now())
    }

    fn tick_at(&mut self, now: Instant) -> Option<UnloadReason> {
        self.loaded.as_ref()?;
        let pressure = self.probe.snapshot().memory.pressure;
        let policy = self.policy.clone();
        let decision = self.loaded.as_mut()?.residency.observe(now, pressure, &policy);
        let ResidencyDecision::Unload(reason) = decision else { return None };
        tracing::info!(?reason, "unloading the model");
        self.unload();
        self.last_unload = Some(reason);
        if reason == UnloadReason::Pressure {
            self.gate.close(now, &self.policy);
        }
        Some(reason)
    }

    /// True when nothing is loaded, so the daemon has no reason to stay awake.
    pub fn is_idle(&self) -> bool {
        self.loaded.is_none()
    }

    pub fn status(&mut self) -> Status {
        let (snapshot, budget) = self.budget_now();
        let now = Instant::now();
        let spec = self.backend.loaded().cloned();
        let loaded = self.loaded.as_ref().map(|loaded| LoadedModel {
            id: loaded.id.clone(),
            quant: loaded.quant.clone(),
            tier: loaded.tier,
            memory_bytes: self.backend.memory_bytes(),
            idle_secs: loaded.residency.idle_for(now).as_secs(),
            context_tokens: spec.as_ref().map_or(0, |spec| spec.context_tokens),
            threads: spec.as_ref().map_or(0, |spec| spec.threads),
        });
        let availability = self.backend.available();
        Status {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            api: API.to_owned(),
            mode: self.policy.mode,
            snapshot,
            budget,
            loaded,
            backend: BackendStatus {
                name: self.backend.name().to_owned(),
                available: availability.is_ok(),
                detail: availability.err().map(|error| error.to_string()),
            },
            uptime_secs: self.started.elapsed().as_secs(),
            last_unload: self.last_unload,
            load_blocked_secs: self.gate.closed_for(now).map(|left| left.as_secs().max(1)),
            busy: false,
        }
    }

    /// Every known model with what is installed and what fits right now.
    pub fn models(&mut self) -> Vec<ModelInfo> {
        let (_, budget) = self.budget_now();
        let context = self.config.context_tokens;
        self.registry
            .all()
            .iter()
            .map(|manifest| ModelInfo {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                tier: manifest.tier,
                publisher: manifest.publisher.clone(),
                parameters: manifest.parameters.clone(),
                license: manifest.license.clone(),
                license_url: manifest.license_url.clone(),
                upstream: manifest.upstream.clone(),
                variants: manifest
                    .variants
                    .iter()
                    .map(|variant| {
                        let estimated_ram_bytes = estimate_ram_bytes(variant, context);
                        VariantInfo {
                            quant: variant.quant.clone(),
                            size_bytes: variant.size_bytes,
                            estimated_ram_bytes,
                            installed: self.store.is_installed(manifest, variant),
                            fits_budget: estimated_ram_bytes <= budget.model_bytes,
                            download_url: manifest.url(variant),
                        }
                    })
                    .collect(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumbre_resources::{Cpu, Memory, Mode, Power, Pressure, Snapshot};
    use lumbre_router::Stage;
    use lumbre_runtime::Stats;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const GIB: u64 = 1 << 30;

    #[derive(Default)]
    struct Log {
        loads: Vec<PathBuf>,
        unloads: u32,
        prompts: Vec<Vec<Message>>,
        fail_generation: bool,
    }

    /// Stands in for llama.cpp so the coordinator's decisions can be tested without weights.
    /// It is only compiled into the tests; no shipped code path uses it.
    struct FakeBackend {
        log: Arc<Mutex<Log>>,
        spec: Option<LoadSpec>,
        available: bool,
    }

    impl Backend for FakeBackend {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn available(&self) -> Result<(), RuntimeError> {
            if self.available {
                Ok(())
            } else {
                Err(RuntimeError::BackendMissing("not installed".into()))
            }
        }
        fn load(&mut self, spec: &LoadSpec) -> Result<(), RuntimeError> {
            self.log.lock().unwrap().loads.push(spec.model_path.clone());
            self.spec = Some(spec.clone());
            Ok(())
        }
        fn unload(&mut self) {
            if self.spec.take().is_some() {
                self.log.lock().unwrap().unloads += 1;
            }
        }
        fn loaded(&self) -> Option<&LoadSpec> {
            self.spec.as_ref()
        }
        fn memory_bytes(&self) -> Option<u64> {
            self.spec.as_ref().map(|_| 3 * GIB)
        }
        fn pids(&self) -> Vec<u32> {
            Vec::new()
        }
        fn generate(
            &mut self,
            request: &GenRequest,
            on_token: &mut dyn FnMut(&str) -> bool,
        ) -> Result<Stats, RuntimeError> {
            let mut log = self.log.lock().unwrap();
            log.prompts.push(request.messages.clone());
            if log.fail_generation {
                return Err(RuntimeError::Http("connection reset".into()));
            }
            for token in ["Hola", ", ", "mundo"] {
                if !on_token(token) {
                    break;
                }
            }
            Ok(Stats {
                prompt_tokens: Some(5),
                completion_tokens: Some(3),
                first_token_ms: Some(1),
                total_ms: 2,
            })
        }
    }

    struct FakeProbe(Arc<Mutex<Snapshot>>);

    impl Probe for FakeProbe {
        fn snapshot(&mut self) -> Snapshot {
            self.0.lock().unwrap().clone()
        }
    }

    struct Rig {
        engine: Engine,
        log: Arc<Mutex<Log>>,
        machine: Arc<Mutex<Snapshot>>,
        root: PathBuf,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Three tiny "models" (nano, standard, code). `installed` names the ones present on disk.
    fn rig(name: &str, installed: &[&str], config: Config) -> Rig {
        let root = std::env::temp_dir().join(format!("lumbre-engine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::new(&root);
        let mut registry = Registry::default();
        for (id, tier, size) in [("tiny", "nano", 1000), ("mid", "standard", 2000), ("coder", "code", 3000)] {
            let manifest = Manifest::parse(&format!(
                "schema = 1\nid = \"{id}\"\nname = \"{id}\"\ntier = \"{tier}\"\npublisher = \"tests\"\n\
                 upstream = \"https://example.invalid/u\"\nsource = \"https://example.invalid/s\"\nrevision = \"abc\"\n\
                 license = \"Apache-2.0\"\nlicense_url = \"https://example.invalid/l\"\nformat = \"gguf\"\nparameters = \"0\"\n\
                 [[variant]]\nquant = \"Q4\"\nfile = \"{id}.gguf\"\nsize_bytes = {size}\nsha256 = \"{}\"\n",
                "0".repeat(64)
            ))
            .unwrap();
            if installed.contains(&id) {
                let path = store.path(&manifest, &manifest.variants[0]);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, vec![0u8; size]).unwrap();
            }
            registry.add(manifest);
        }
        let log = Arc::new(Mutex::new(Log::default()));
        let machine = Arc::new(Mutex::new(Snapshot {
            memory: Memory {
                total_bytes: 16 * GIB,
                available_bytes: 10 * GIB,
                pressure: Pressure::Normal,
                psi: None,
            },
            cpu: Cpu { logical: 8, load1: None, busy: None },
            power: Power::Ac,
            ..Default::default()
        }));
        let backend = Box::new(FakeBackend { log: Arc::clone(&log), spec: None, available: true });
        let engine =
            Engine::with_parts(config, registry, store, backend, Box::new(FakeProbe(Arc::clone(&machine))));
        Rig { engine, log, machine, root }
    }

    fn ask(engine: &mut Engine, text: &str) -> (Answer, Vec<ChatEvent>) {
        let mut events = Vec::new();
        let request = ChatRequest { text: text.to_owned(), ..Default::default() };
        let answer = engine.chat(&request, &mut |event| {
            events.push(event);
            true
        });
        (answer, events)
    }

    const QUESTION: &str = "Recomiéndame tres novelas cortas para leer este verano en la playa";

    #[test]
    fn a_tool_request_never_touches_the_backend() {
        let mut rig = rig("tool", &["mid"], Config::default());
        let (answer, events) = ask(&mut rig.engine, "¿Cuánto es 12 * (3 + 4)?");
        assert_eq!(answer.text(), Some("12 * (3 + 4) = 84"));
        assert!(matches!(&events[0], ChatEvent::Routed { decision } if decision.stage == Stage::Rule));
        assert_eq!(events.len(), 1);
        assert!(rig.log.lock().unwrap().loads.is_empty());
        assert!(rig.engine.is_idle());
    }

    #[test]
    fn file_search_is_off_until_the_user_configures_folders() {
        let mut rig = rig("files-off", &[], Config::default());
        let (answer, _) = ask(&mut rig.engine, "busca el archivo tiny.gguf");
        assert!(matches!(&answer, Answer::Error { message } if message.contains("permission")), "{answer:?}");

        let root = rig.root.clone();
        let config = Config { search_roots: vec![root], ..Default::default() };
        let mut rig = rig_with_files(config);
        let (answer, _) = ask(&mut rig.engine, "busca el archivo tiny.gguf");
        assert!(answer.text().unwrap().contains("tiny.gguf"), "{answer:?}");
    }

    fn rig_with_files(config: Config) -> Rig {
        let mut rig = rig("files-on", &["tiny"], Config::default());
        rig.engine.config = Config { search_roots: vec![rig.root.clone()], ..config };
        rig
    }

    #[test]
    fn a_question_loads_the_right_model_once_and_streams() {
        let mut rig = rig("load", &["tiny", "mid", "coder"], Config::default());
        let (answer, events) = ask(&mut rig.engine, QUESTION);
        match &answer {
            Answer::Model { model, quant, tier, downgraded, text, stats } => {
                assert_eq!(
                    (model.as_str(), quant.as_str(), *tier, *downgraded),
                    ("mid", "Q4", Tier::Standard, false)
                );
                assert_eq!(text, "Hola, mundo");
                assert_eq!(stats.completion_tokens, Some(3));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&events[1], ChatEvent::Loading { model, .. } if model == "mid"));
        assert_eq!(events.iter().filter(|event| matches!(event, ChatEvent::Token { .. })).count(), 3);

        // Second question: same model stays resident, nothing is reloaded.
        let (_, events) = ask(&mut rig.engine, QUESTION);
        assert!(!events.iter().any(|event| matches!(event, ChatEvent::Loading { .. })));
        let log = rig.log.lock().unwrap();
        assert_eq!(log.loads.len(), 1);
        assert_eq!(log.prompts[0].len(), 2);
        assert_eq!(log.prompts[0][0].role, lumbre_runtime::Role::System);
    }

    #[test]
    fn only_one_model_is_ever_resident() {
        let mut rig = rig("swap", &["tiny", "mid", "coder"], Config::default());
        ask(&mut rig.engine, QUESTION);
        let (answer, _) =
            ask(&mut rig.engine, "Fix this bug: fn main() { let x: u8 = 300; } does not compile");
        assert!(matches!(&answer, Answer::Model { model, .. } if model == "coder"), "{answer:?}");
        let log = rig.log.lock().unwrap();
        assert_eq!(log.loads.len(), 2);
        assert_eq!(log.unloads, 1);
        assert_eq!(rig.engine.loaded_ref(), Some(("coder", "Q4")));
    }

    #[test]
    fn a_missing_tier_falls_back_and_says_so() {
        let mut rig = rig("fallback", &["tiny"], Config::default());
        let (answer, _) = ask(&mut rig.engine, QUESTION);
        assert!(
            matches!(&answer, Answer::Model { model, downgraded: true, .. } if model == "tiny"),
            "{answer:?}"
        );
    }

    #[test]
    fn nothing_installed_gives_a_suggestion_not_a_fake_answer() {
        let mut rig = rig("empty", &[], Config::default());
        let (answer, _) = ask(&mut rig.engine, QUESTION);
        match answer {
            Answer::NoModel { wanted, suggestion, detail, .. } => {
                assert_eq!(wanted, Tier::Standard);
                assert_eq!(suggestion.as_deref(), Some("mid:Q4"));
                assert!(detail.contains("no model is installed"), "{detail}");
            }
            other => panic!("{other:?}"),
        }
        assert!(rig.log.lock().unwrap().loads.is_empty());
    }

    #[test]
    fn the_user_limit_is_never_exceeded_even_by_a_manual_choice() {
        let config = Config { max_ram_mb: Some(100), ..Default::default() };
        let mut rig = rig("cap", &["tiny", "mid"], config);
        let (answer, _) = ask(&mut rig.engine, QUESTION);
        assert!(matches!(answer, Answer::NoModel { budget_bytes, .. } if budget_bytes == 100 << 20));

        let request = ChatRequest { text: "hola".into(), model: Some("mid".into()), ..Default::default() };
        let answer = rig.engine.chat(&request, &mut |_| true);
        assert!(
            matches!(&answer, Answer::NoModel { detail, .. } if detail.contains("needs about")),
            "{answer:?}"
        );
        assert!(rig.log.lock().unwrap().loads.is_empty());
    }

    #[test]
    fn a_manual_choice_is_honoured_and_checked() {
        let mut rig = rig("manual", &["tiny", "mid"], Config::default());
        let mut request =
            ChatRequest {
                text: "¿Qué hora es?".into(), model: Some("tiny:Q4".into()), ..Default::default()
            };
        let answer = rig.engine.chat(&request, &mut |_| true);
        assert!(matches!(&answer, Answer::Model { model, .. } if model == "tiny"), "{answer:?}");
        for (spec, expected) in
            [("nope", "unknown model"), ("tiny:Q9", "no `Q9`"), ("coder", "not installed")]
        {
            request.model = Some(spec.into());
            let answer = rig.engine.chat(&request, &mut |_| true);
            assert!(
                matches!(&answer, Answer::Error { message } if message.contains(expected)),
                "{spec}: {answer:?}"
            );
        }
    }

    #[test]
    fn critical_pressure_defers_instead_of_loading() {
        let mut rig = rig("defer", &["mid"], Config::default());
        rig.machine.lock().unwrap().memory.pressure = Pressure::Critical;
        let (answer, _) = ask(&mut rig.engine, QUESTION);
        assert_eq!(
            answer,
            Answer::Deferred { reasons: vec![Reason::MemoryPressureCritical], retry_after_secs: None }
        );
        assert!(rig.log.lock().unwrap().loads.is_empty());
        // Tools still work while models are deferred.
        assert_eq!(ask(&mut rig.engine, "2+2").0.text(), Some("2+2 = 4"));
    }

    #[test]
    fn elevated_pressure_steps_down_a_tier() {
        let mut rig = rig("degrade", &["tiny", "mid"], Config::default());
        rig.machine.lock().unwrap().memory.pressure = Pressure::Elevated;
        let (answer, _) = ask(&mut rig.engine, QUESTION);
        // Standard steps down to Mini; no Mini is installed, so Nano answers.
        assert!(
            matches!(&answer, Answer::Model { model, downgraded: true, .. } if model == "tiny"),
            "{answer:?}"
        );
        assert_eq!(rig.log.lock().unwrap().loads.len(), 1);
    }

    #[test]
    fn an_idle_model_is_unloaded_and_the_next_question_reloads_it() {
        let mut rig = rig("idle", &["mid"], Config::default());
        ask(&mut rig.engine, QUESTION);
        let now = Instant::now();
        assert_eq!(rig.engine.tick_at(now + Duration::from_secs(60)), None);
        assert_eq!(rig.engine.tick_at(now + Duration::from_secs(301)), Some(UnloadReason::Idle));
        assert!(rig.engine.is_idle());
        assert_eq!(rig.engine.status().last_unload, Some(UnloadReason::Idle));
        assert_eq!(rig.engine.tick_at(now + Duration::from_secs(400)), None);

        ask(&mut rig.engine, QUESTION);
        let log = rig.log.lock().unwrap();
        assert_eq!((log.loads.len(), log.unloads), (2, 1));
    }

    #[test]
    fn a_pressure_unload_blocks_reloading_for_the_cooldown() {
        let mut rig = rig("gate", &["mid"], Config::default());
        ask(&mut rig.engine, QUESTION);
        rig.machine.lock().unwrap().memory.pressure = Pressure::Critical;
        let later = Instant::now() + Duration::from_secs(60);
        assert_eq!(rig.engine.tick_at(later), None);
        assert_eq!(rig.engine.tick_at(later + Duration::from_secs(5)), Some(UnloadReason::Pressure));

        // Pressure is gone, but the gate stays closed: no load/unload ping-pong.
        rig.machine.lock().unwrap().memory.pressure = Pressure::Normal;
        let (answer, _) = ask(&mut rig.engine, QUESTION);
        match answer {
            Answer::Deferred { reasons, retry_after_secs } => {
                assert_eq!(reasons, vec![Reason::RecentPressure]);
                assert!(retry_after_secs.is_some_and(|secs| secs > 60));
            }
            other => panic!("{other:?}"),
        }
        assert!(rig.engine.status().load_blocked_secs.is_some());
        assert_eq!(rig.log.lock().unwrap().loads.len(), 1);
    }

    #[test]
    fn a_failed_generation_is_an_error_and_drops_the_worker() {
        let mut rig = rig("fail", &["mid"], Config::default());
        rig.log.lock().unwrap().fail_generation = true;
        let (answer, _) = ask(&mut rig.engine, QUESTION);
        assert!(
            matches!(&answer, Answer::Error { message } if message.contains("connection reset")),
            "{answer:?}"
        );
        assert!(rig.engine.is_idle());
    }

    #[test]
    fn a_missing_backend_is_reported_before_anything_else() {
        let mut rig = rig("nobackend", &["mid"], Config::default());
        rig.engine.backend =
            Box::new(FakeBackend { log: Arc::clone(&rig.log), spec: None, available: false });
        let (answer, _) = ask(&mut rig.engine, QUESTION);
        assert!(matches!(answer, Answer::BackendMissing { .. }));
        let status = rig.engine.status();
        assert!(!status.backend.available && status.backend.detail.is_some());
    }

    #[test]
    fn status_and_models_reflect_the_engine_state() {
        let config = Config { mode: Mode::Saver, ..Default::default() };
        let mut rig = rig("status", &["mid"], config);
        let before = rig.engine.status();
        assert_eq!((before.mode, before.loaded.is_none(), before.api.as_str()), (Mode::Saver, true, "v1"));

        ask(&mut rig.engine, QUESTION);
        let after = rig.engine.status();
        let loaded = after.loaded.unwrap();
        assert_eq!(
            (loaded.id.as_str(), loaded.memory_bytes, loaded.context_tokens),
            ("mid", Some(3 * GIB), 4096)
        );
        // Saver mode on 8 logical cores: two threads.
        assert_eq!(loaded.threads, 2);
        // The loaded model's memory counts as Lumbre's own, so holding it does not shrink the budget.
        assert!(after.budget.model_bytes >= before.budget.model_bytes);

        let models = rig.engine.models();
        let installed: Vec<_> = models
            .iter()
            .filter(|model| model.variants.iter().any(|variant| variant.installed))
            .map(|model| model.id.as_str())
            .collect();
        assert_eq!(installed, ["mid"]);
        assert!(models.iter().all(|model| model.variants.iter().all(|variant| variant.fits_budget)));
    }
}
