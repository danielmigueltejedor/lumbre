use eframe::egui::{
    self, Align, Button, CentralPanel, Color32, ComboBox, Frame, Key, KeyboardShortcut, Layout, Modifiers,
    Panel, ProgressBar, RichText, ScrollArea, TextEdit, Ui,
};
use lumbre_api::{Answer, ModelInfo, Status};
use lumbre_resources::{Pressure, Reason, Verdict};

use crate::capture::Capture;
use crate::worker::{Bubble, Pull, State, Worker};

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Chat,
    Models,
    Privacy,
}

pub struct Lumbre {
    worker: Worker,
    view: View,
    input: String,
    search: String,
    /// `None` is automatic selection.
    model: Option<String>,
    capture: Capture,
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / f64::from(1u32 << 30))
}

fn mib(bytes: u64) -> String {
    format!("{:.0} MiB", bytes as f64 / f64::from(1u32 << 20))
}

fn reason_text(reason: &Reason) -> String {
    match reason {
        Reason::MemoryUnknown => "available memory could not be read".into(),
        Reason::MemoryPressureElevated => "the system is short of memory".into(),
        Reason::MemoryPressureCritical => "the system is critically short of memory".into(),
        Reason::RecentPressure => "a model was just unloaded because memory ran short".into(),
        Reason::OnBattery => "running on battery".into(),
        Reason::LowBattery { percent } => format!("battery at {percent} %"),
        Reason::BusyProcess { name, cores } => format!("`{name}` is using {cores:.1} cores"),
        Reason::UserLimit { bytes } => format!("your limit of {}", gib(*bytes)),
        Reason::CgroupLimit { bytes } => format!("the system limit of {} for this session", gib(*bytes)),
        Reason::SaverMode => "saver mode".into(),
    }
}

fn reasons_text(reasons: &[Reason]) -> String {
    reasons.iter().map(reason_text).collect::<Vec<_>>().join("; ")
}

/// A framed block with the panel's border, used for messages and cards.
fn card(ui: &mut Ui, fill: Color32, add: impl FnOnce(&mut Ui)) {
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    Frame::new().fill(fill).stroke(stroke).corner_radius(6).inner_margin(10).show(ui, add);
}

impl Lumbre {
    pub fn new(ctx: egui::Context) -> Self {
        let view = match Capture::view().as_deref() {
            Some("models") => View::Models,
            Some("privacy") => View::Privacy,
            _ => View::Chat,
        };
        Self {
            worker: Worker::start(ctx),
            view,
            input: String::new(),
            search: String::new(),
            model: None,
            capture: Capture::from_env(),
        }
    }

    fn sidebar(&mut self, ui: &mut Ui, state: &State) {
        ui.add_space(4.0);
        ui.heading("Lumbre");
        ui.add_space(8.0);
        if ui.add_sized([ui.available_width(), 0.0], Button::new("New conversation")).clicked() {
            self.worker.new_conversation();
            self.view = View::Chat;
        }
        ui.add_space(6.0);
        let search = TextEdit::singleline(&mut self.search)
            .hint_text("Search conversations")
            .desired_width(f32::INFINITY);
        if ui.add(search).changed() {
            self.worker.set_query(&self.search);
        }
        ui.add_space(6.0);
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if state.conversations.is_empty() {
                let empty =
                    if self.search.trim().is_empty() { "No conversations yet." } else { "Nothing matches." };
                ui.weak(empty);
            }
            for conversation in &state.conversations {
                let selected =
                    state.conversation.as_deref() == Some(&conversation.id) && self.view == View::Chat;
                let response = ui.selectable_label(selected, &conversation.title);
                if let Some(snippet) = &conversation.snippet {
                    ui.weak(RichText::new(snippet).small());
                }
                if response.clicked() {
                    self.worker.open_conversation(conversation.id.clone());
                    self.view = View::Chat;
                }
                response.context_menu(|ui| {
                    if ui.button("Delete conversation").clicked() {
                        self.worker.delete_conversation(conversation.id.clone());
                        ui.close();
                    }
                });
            }
        });
    }

    /// The discreet indicator: what is loaded and what it costs, straight from the daemon.
    fn indicator(ui: &mut Ui, status: &Status) {
        let mut parts = Vec::new();
        match &status.loaded {
            Some(model) => parts.push(format!(
                "{} {} · {}",
                model.id,
                model.quant,
                model.memory_bytes.map_or_else(|| "memory unknown".into(), gib)
            )),
            None => parts.push("no model loaded".into()),
        }
        match status.snapshot.memory.pressure {
            Pressure::Elevated => parts.push("memory pressure".into()),
            Pressure::Critical => parts.push("critical memory pressure".into()),
            Pressure::Normal | Pressure::Unknown => {}
        }
        if let Some(rss) = status.snapshot.self_rss_bytes {
            parts.push(format!("daemon {}", mib(rss)));
        }
        let hover = format!(
            "Mode: {:?}\nMemory available: {} of {}\nBudget for a model: {} ({:?})\n{}",
            status.mode,
            gib(status.snapshot.memory.available_bytes),
            gib(status.snapshot.memory.total_bytes),
            gib(status.budget.model_bytes),
            status.budget.verdict,
            if status.budget.reasons.is_empty() {
                "No limits apply.".into()
            } else {
                format!("Because: {}", reasons_text(&status.budget.reasons))
            },
        );
        ui.weak(RichText::new(parts.join("   ·   ")).small()).on_hover_text(hover);
    }

    fn top_bar(&mut self, ui: &mut Ui, state: &State) {
        ui.horizontal(|ui| {
            for (view, label) in [(View::Chat, "Chat"), (View::Models, "Models"), (View::Privacy, "Privacy")]
            {
                if ui.selectable_label(self.view == view, label).clicked() {
                    self.view = view;
                    self.worker.refresh();
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(Ok(status)) = &state.status {
                    Self::indicator(ui, status);
                }
            });
        });
    }

    fn unanswered(&mut self, ui: &mut Ui, answer: &Answer) {
        let warn = ui.visuals().warn_fg_color;
        match answer {
            Answer::Deferred { reasons, retry_after_secs } => {
                ui.colored_label(warn, "Not answered: nothing was loaded, to leave the computer to you.");
                ui.label(format!("Why: {}.", reasons_text(reasons)));
                if let Some(secs) = retry_after_secs {
                    ui.label(format!("Try again in about {secs} s."));
                }
            }
            Answer::NoModel { detail, suggestion, budget_bytes, .. } => {
                ui.colored_label(warn, "Not answered: no installed model fits right now.");
                ui.label(format!("{detail} (budget {}).", gib(*budget_bytes)));
                if suggestion.is_some() && ui.button("Open Models").clicked() {
                    self.view = View::Models;
                    self.worker.refresh();
                }
            }
            Answer::BackendMissing { detail } => {
                ui.colored_label(warn, "Not answered: the inference backend is missing.");
                ui.label(detail);
            }
            Answer::Error { message } => {
                ui.colored_label(ui.visuals().error_fg_color, "Not answered: the request failed.");
                ui.label(message);
            }
            Answer::Tool { .. } | Answer::Model { .. } => {}
        }
    }

    fn chat(&mut self, ui: &mut Ui, state: &State) {
        let busy = state.streaming.is_some();
        Panel::bottom("composer").show_separator_line(false).show(ui, |ui| {
            ui.add_space(6.0);
            let installed: Vec<String> = state
                .models
                .iter()
                .flat_map(|model| {
                    model
                        .variants
                        .iter()
                        .filter(|variant| variant.installed)
                        .map(move |variant| format!("{}:{}", model.id, variant.quant))
                })
                .collect();
            ui.horizontal(|ui| {
                ui.weak("Model");
                ComboBox::from_id_salt("model")
                    .selected_text(self.model.clone().unwrap_or_else(|| "Automatic".into()))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.model, None, "Automatic");
                        for name in &installed {
                            ui.selectable_value(&mut self.model, Some(name.clone()), name);
                        }
                    });
                if installed.is_empty() {
                    ui.weak("(no model installed; tools still answer)");
                }
                if let Some(phase) = &state.phase {
                    ui.spinner();
                    ui.weak(phase);
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let send_width = 72.0;
                let editor = TextEdit::multiline(&mut self.input)
                    .hint_text("Ask something. Enter sends, Shift+Enter adds a line.")
                    .desired_rows(2)
                    .desired_width(ui.available_width() - send_width - ui.spacing().item_spacing.x)
                    .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter));
                let response = ui.add(editor);
                let enter = response.has_focus()
                    && ui.input(|input| input.key_pressed(Key::Enter) && !input.modifiers.shift);
                let clicked = if busy {
                    if ui.add_sized([send_width, 32.0], Button::new("Stop")).clicked() {
                        self.worker.stop_generation();
                    }
                    false
                } else {
                    ui.add_enabled_ui(state.daemon_running, |ui| {
                        ui.add_sized([send_width, 32.0], Button::new("Send")).clicked()
                    })
                    .inner
                };
                if (enter || clicked) && !busy && state.daemon_running && !self.input.trim().is_empty() {
                    self.worker.send(std::mem::take(&mut self.input).trim().to_owned(), self.model.clone());
                    response.request_focus();
                }
            });
            ui.add_space(8.0);
        });

        CentralPanel::default().show(ui, |ui| {
            ScrollArea::vertical().auto_shrink(false).stick_to_bottom(true).show(ui, |ui| {
                if state.bubbles.is_empty() && !busy {
                    ui.add_space(24.0);
                    ui.weak("Ask for the time, a sum or a file and a tool answers at once, with no model.\nAnything else is routed to the smallest installed model that can handle it.");
                }
                let own = ui.visuals().selection.bg_fill.gamma_multiply(0.25);
                let other = ui.visuals().faint_bg_color;
                for bubble in &state.bubbles {
                    match bubble {
                        Bubble::User(text) => {
                            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                ui.set_max_width(ui.available_width() * 0.8);
                                card(ui, own, |ui| {
                                    ui.label(text);
                                });
                            });
                        }
                        Bubble::Assistant { text, via } => {
                            card(ui, other, |ui| {
                                ui.label(text);
                                if !via.is_empty() {
                                    ui.weak(RichText::new(via).small());
                                }
                            });
                        }
                        Bubble::Unanswered(answer) => card(ui, other, |ui| self.unanswered(ui, answer)),
                    }
                    ui.add_space(6.0);
                }
                if let Some(partial) = state.streaming.as_ref().filter(|partial| !partial.is_empty()) {
                    card(ui, other, |ui| {
                        ui.label(partial);
                    });
                }
            });
        });
    }

    fn model_card(&mut self, ui: &mut Ui, state: &State, model: &ModelInfo) {
        card(ui, ui.visuals().faint_bg_color, |ui| {
            ui.horizontal(|ui| {
                ui.strong(&model.name);
                ui.weak(format!(
                    "{} · {} parameters · {}",
                    model.tier.as_str(),
                    model.parameters,
                    model.publisher
                ));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.hyperlink_to(&model.license, &model.license_url);
                    ui.hyperlink_to("model card", &model.upstream);
                });
            });
            ui.add_space(4.0);
            for variant in &model.variants {
                let key = (model.id.clone(), variant.quant.clone());
                ui.horizontal(|ui| {
                    ui.monospace(format!("{:<7}", variant.quant));
                    ui.label(format!("{} on disk", gib(variant.size_bytes)));
                    ui.weak(format!("≈ {} in memory", gib(variant.estimated_ram_bytes))).on_hover_text(
                        "Estimated from the file size and the configured context; not yet a measurement.",
                    );
                    if !variant.fits_budget {
                        ui.colored_label(ui.visuals().warn_fg_color, "does not fit right now");
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| match state.pulls.get(&key) {
                        Some(Pull::Running { done_bytes, total_bytes }) => {
                            let fraction = if *total_bytes == 0 {
                                0.0
                            } else {
                                *done_bytes as f32 / *total_bytes as f32
                            };
                            ui.add(ProgressBar::new(fraction).desired_width(180.0).text(format!(
                                "{} / {}",
                                gib(*done_bytes),
                                gib(*total_bytes)
                            )));
                        }
                        Some(Pull::Failed(message)) => {
                            if ui.small_button("Dismiss").clicked() {
                                self.worker.dismiss_pull(&key);
                            }
                            ui.colored_label(ui.visuals().error_fg_color, message);
                        }
                        None if variant.installed => {
                            if ui.button("Remove").clicked() {
                                self.worker.remove_model(key.0.clone(), key.1.clone());
                            }
                            ui.weak("installed");
                        }
                        None => {
                            let download = ui.button("Download").on_hover_text(format!(
                                "Downloads {} from\n{}\nand checks its SHA-256 before installing.",
                                gib(variant.size_bytes),
                                variant.download_url
                            ));
                            if download.clicked() {
                                self.worker.pull(key.0.clone(), key.1.clone());
                            }
                        }
                    });
                });
            }
        });
    }

    fn models(&mut self, ui: &mut Ui, state: &State) {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if let Some(Ok(status)) = &state.status {
                ui.label(format!(
                    "Right now a model may use up to {} ({} of {} memory available).",
                    gib(status.budget.model_bytes),
                    gib(status.snapshot.memory.available_bytes),
                    gib(status.snapshot.memory.total_bytes)
                ));
                if status.budget.verdict != Verdict::Run || !status.budget.reasons.is_empty() {
                    ui.weak(format!("Limited because: {}.", reasons_text(&status.budget.reasons)));
                }
                if !status.backend.available {
                    ui.colored_label(ui.visuals().warn_fg_color, status.backend.detail.clone().unwrap_or_default());
                }
                if let Some(model) = &status.loaded {
                    ui.horizontal(|ui| {
                        ui.label(format!("Loaded: {} {} — idle {} s.", model.id, model.quant, model.idle_secs));
                        if ui.small_button("Unload now").clicked() {
                            self.worker.unload();
                        }
                    });
                }
                ui.add_space(8.0);
            }
            ui.weak("Open-weight models. Nothing is downloaded until you press Download; files go to the Lumbre data folder.");
            ui.add_space(8.0);
            for model in &state.models {
                self.model_card(ui, state, model);
                ui.add_space(6.0);
            }
        });
    }

    fn privacy(&self, ui: &mut Ui, state: &State) {
        let paths = &self.worker.paths;
        let facts = [
            ("Where it runs", "On this computer. There is no account, no telemetry and no cloud service.".to_owned()),
            ("Network", "Lumbre only connects to the internet to download a model you asked for, from the address shown on the Download button.".to_owned()),
            ("Local API", format!("Loopback only, protected by a token only your user can read: {}", paths.token_file().display())),
            ("Conversations", format!("Plain JSON files in {}", paths.conversations_dir().display())),
            ("Models", format!("Stored in {}", paths.models_dir().display())),
            ("Files", format!("File search reads file names only, and only in the folders listed under `search_roots` in {}. None are configured by default.", paths.config_file().display())),
        ];
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (title, body) in facts {
                ui.strong(title);
                ui.label(body);
                ui.add_space(10.0);
            }
            if let Some(Ok(status)) = &state.status {
                ui.strong("Daemon");
                ui.label(format!(
                    "Lumbre {} · API {} · up {} s",
                    status.version, status.api, status.uptime_secs
                ));
            }
        });
    }

    fn offline(&self, ui: &mut Ui, state: &State) {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.heading("The Lumbre daemon is not running");
            ui.add_space(6.0);
            if let Some(Err(error)) = &state.status {
                ui.weak(error);
            }
            ui.add_space(12.0);
            if ui.button("Start it").clicked() {
                self.worker.start_daemon();
            }
            ui.add_space(6.0);
            ui.weak("It uses a few megabytes while idle and loads a model only when a request needs one.");
        });
    }
}

impl eframe::App for Lumbre {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let state = self.worker.state();
        self.capture.tick(ui.ctx());
        // A capture of the chat view shows the latest stored conversation rather than an empty one.
        if self.capture.wants_conversation() && self.view == View::Chat {
            if let Some(latest) = state.conversations.first() {
                self.worker.open_conversation(latest.id.clone());
                self.capture.conversation_opened();
            }
        }

        Panel::left("conversations")
            .default_size(230.0)
            .min_size(180.0)
            .show(ui, |ui| self.sidebar(ui, &state));
        Panel::top("views").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui, &state);
            ui.add_space(4.0);
        });
        if let Some(error) = &state.error {
            Panel::top("error").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    if ui.small_button("Dismiss").clicked() {
                        self.worker.dismiss_error();
                    }
                });
            });
        }
        CentralPanel::default().show(ui, |ui| match (state.daemon_running, self.view) {
            (false, View::Privacy) => self.privacy(ui, &state),
            (false, _) => self.offline(ui, &state),
            (true, View::Chat) => self.chat(ui, &state),
            (true, View::Models) => self.models(ui, &state),
            (true, View::Privacy) => self.privacy(ui, &state),
        });
    }
}
