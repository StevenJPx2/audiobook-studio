//! egui/eframe GUI for Audiobook Studio. Owns the screen state machine and runs
//! the (async) pipeline on a background thread, polling results/progress each
//! frame over channels so the UI never blocks.
use crate::model::{BookInfo, Chapter, GenerateRequest, Progress, VoiceConfig};
use crate::pipeline;
use eframe::egui;
use std::sync::mpsc::{Receiver, Sender};

/// A few Kokoro voices for the picker. `bm_george` (British) at 1.0× is the
/// proven default and stays first.
const VOICES: &[(&str, &str, &str)] = &[
    ("bm_george", "b", "George — British male"),
    ("bf_emma", "b", "Emma — British female"),
    ("am_adam", "a", "Adam — American male"),
    ("af_heart", "a", "Heart — American female"),
];

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Drop,
    Review,
    Running,
    Done,
}

/// Messages the background pipeline thread sends back to the UI.
enum Msg {
    Inspected(BookInfo),
    Chapters(Vec<Chapter>),
    Progress(Progress),
    Done(String),
    Error(String),
}

pub struct App {
    stage: Stage,
    rt: tokio::runtime::Runtime,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    busy: bool,
    error: Option<String>,
    status: String,

    book: Option<BookInfo>,
    models: Vec<String>,
    model: String,
    chapters: Vec<Chapter>,
    voice: VoiceConfig,
    title: String,
    author: String,
    out_dir: String,
    polish: bool,
    ollama_up: bool,

    progress: Option<Progress>,
    result_path: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let mut app = Self {
            stage: Stage::Drop,
            rt,
            tx,
            rx,
            busy: false,
            error: None,
            status: String::new(),
            book: None,
            models: Vec::new(),
            model: String::new(),
            chapters: Vec::new(),
            voice: VoiceConfig::default(),
            title: String::new(),
            author: String::new(),
            out_dir: String::new(),
            polish: true,
            ollama_up: false,
            progress: None,
            result_path: None,
        };
        app.refresh_models();
        app
    }
}

impl App {
    /// Spawn an async job on the runtime; it sends `Msg`s back over the channel.
    fn spawn<F>(&self, fut: impl FnOnce(Sender<Msg>) -> F + Send + 'static)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let tx = self.tx.clone();
        let handle = self.rt.handle().clone();
        std::thread::spawn(move || {
            handle.block_on(fut(tx));
        });
    }

    fn refresh_models(&mut self) {
        self.spawn(|tx| async move {
            match pipeline::list_models().await {
                Ok(models) => {
                    // Reuse the Progress channel for a lightweight signal.
                    let _ = tx.send(Msg::Progress(Progress::new(
                        "__models__",
                        models.join(","),
                        0,
                        0,
                    )));
                }
                Err(_) => {
                    let _ = tx.send(Msg::Progress(Progress::new("__models__", "", 0, 0)));
                }
            }
        });
    }

    fn load_pdf(&mut self, path: String) {
        self.busy = true;
        self.error = None;
        self.status = format!("Reading {path}…");
        let p = path.clone();
        self.spawn(move |tx| async move {
            match tokio::task::spawn_blocking(move || pipeline::inspect_pdf(&p)).await {
                Ok(Ok(info)) => {
                    let _ = tx.send(Msg::Inspected(info));
                }
                Ok(Err(e)) => {
                    let _ = tx.send(Msg::Error(e.to_string()));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(e.to_string()));
                }
            }
        });
    }

    fn detect_chapters(&mut self) {
        let Some(book) = &self.book else { return };
        self.busy = true;
        self.error = None;
        let path = book.path.clone();
        let model = self.model.clone();
        self.spawn(move |tx| async move {
            let cb_tx = tx.clone();
            let progress = move |p: Progress| {
                let _ = cb_tx.send(Msg::Progress(p));
            };
            match pipeline::detect_chapters(&path, &model, &progress).await {
                Ok(chs) => {
                    let _ = tx.send(Msg::Chapters(chs));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(e.to_string()));
                }
            }
        });
    }

    fn generate(&mut self) {
        let Some(book) = &self.book else { return };
        self.busy = true;
        self.error = None;
        self.progress = None;
        let req = GenerateRequest {
            pdf_path: book.path.clone(),
            out_dir: self.out_dir.clone(),
            chapters: self.chapters.clone(),
            voice: self.voice.clone(),
            book_title: self.title.clone(),
            author: self.author.clone(),
            polish: self.polish,
            polish_model: if self.model.is_empty() {
                None
            } else {
                Some(self.model.clone())
            },
        };
        self.stage = Stage::Running;
        self.spawn(move |tx| async move {
            let cb_tx = tx.clone();
            let progress = move |p: Progress| {
                let _ = cb_tx.send(Msg::Progress(p));
            };
            match pipeline::generate_audiobook(req, &progress).await {
                Ok(path) => {
                    let _ = tx.send(Msg::Done(path));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(e.to_string()));
                }
            }
        });
    }

    /// Drain background messages once per frame.
    fn pump(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Progress(p) if p.stage == "__models__" => {
                    self.models = if p.message.is_empty() {
                        Vec::new()
                    } else {
                        p.message.split(',').map(String::from).collect()
                    };
                    self.ollama_up = !self.models.is_empty();
                    if self.model.is_empty() {
                        self.model = pick_model_default(&self.models);
                    }
                }
                Msg::Progress(p) => self.progress = Some(p),
                Msg::Inspected(info) => {
                    self.busy = false;
                    if self.title.is_empty() {
                        self.title = title_from_filename(&info.file_name);
                    }
                    if self.out_dir.is_empty() {
                        self.out_dir = default_out_dir(&info.path);
                    }
                    self.book = Some(info);
                    // Auto-advance: detect chapters right away.
                    self.detect_chapters();
                }
                Msg::Chapters(chs) => {
                    self.busy = false;
                    self.chapters = chs;
                    self.stage = Stage::Review;
                }
                Msg::Done(path) => {
                    self.busy = false;
                    self.result_path = Some(path);
                    self.stage = Stage::Done;
                }
                Msg::Error(e) => {
                    self.busy = false;
                    self.error = Some(e);
                    if self.stage == Stage::Running {
                        self.stage = Stage::Review;
                    }
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        // Repaint while work is in flight so progress animates.
        if self.busy || self.stage == Stage::Running {
            ctx.request_repaint();
        }

        // Native file drop.
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !self.busy {
            if let Some(path) = dropped.into_iter().find_map(|f| f.path) {
                if path.extension().map(|e| e == "pdf").unwrap_or(false) {
                    self.load_pdf(path.to_string_lossy().to_string());
                }
            }
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Audiobook Studio");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (dot, label) = if !self.ollama_up && self.models.is_empty() {
                        (egui::Color32::GRAY, "Ollama: checking…")
                    } else if self.ollama_up {
                        (egui::Color32::from_rgb(60, 180, 90), "Ollama: online")
                    } else {
                        (egui::Color32::from_rgb(210, 90, 90), "Ollama: offline")
                    };
                    ui.label(label);
                    // Painted dot — no font glyph, so it can never render as tofu.
                    let r = 5.0;
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(r * 2.0, r * 2.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), r, dot);
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &self.error {
                ui.colored_label(
                    egui::Color32::from_rgb(210, 90, 90),
                    format!("{}  {err}", egui_phosphor::regular::WARNING),
                );
                ui.separator();
            }
            match self.stage {
                Stage::Drop => self.view_drop(ui),
                Stage::Review => self.view_review(ui),
                Stage::Running => self.view_running(ui),
                Stage::Done => self.view_done(ui),
            }
        });
    }
}

impl App {
    fn view_drop(&mut self, ui: &mut egui::Ui) {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            if self.busy {
                ui.spinner();
                ui.label(&self.status);
                ui.weak("Extracting and analyzing pages. Large books can take a moment.");
            } else {
                ui.heading("Drop a book PDF here");
                ui.label("A local model finds the chapters; Kokoro narrates them.");
                ui.add_space(12.0);
                if ui
                    .button(format!(
                        "{}  Browse for a PDF…",
                        egui_phosphor::regular::FOLDER_OPEN
                    ))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("PDF", &["pdf"])
                        .pick_file()
                    {
                        self.load_pdf(path.to_string_lossy().to_string());
                    }
                }
            }
        });
    }

    fn view_review(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Chapters");
            ui.weak(format!(
                "{} chapters detected. Edit titles; page ranges follow the boundaries.",
                self.chapters.len()
            ));
            ui.add_space(6.0);
            // Give the title column a generous, fixed width so the edit boxes
            // don't shrink to their content (which truncated titles to a few
            // characters). The box also grows with the window via the fraction.
            let title_w = (ui.available_width() - 160.0).clamp(220.0, 560.0);
            egui::Grid::new("chapters")
                .striped(true)
                .num_columns(3)
                .min_col_width(40.0)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.strong("#");
                    ui.strong("Title");
                    ui.strong("Pages");
                    ui.end_row();
                    for ch in &mut self.chapters {
                        ui.label(ch.order.to_string());
                        ui.add(
                            egui::TextEdit::singleline(&mut ch.title).desired_width(title_w),
                        );
                        ui.label(format!("{}–{}", ch.start_page, ch.end_page));
                        ui.end_row();
                    }
                });

            ui.add_space(12.0);
            ui.heading("Narration");
            egui::Grid::new("opts").num_columns(2).show(ui, |ui| {
                ui.label("Voice");
                egui::ComboBox::from_id_salt("voice")
                    .selected_text(voice_label(&self.voice.voice))
                    .show_ui(ui, |ui| {
                        for (id, lang, label) in VOICES {
                            if ui
                                .selectable_label(self.voice.voice == *id, *label)
                                .clicked()
                            {
                                self.voice.voice = (*id).into();
                                self.voice.lang = (*lang).into();
                            }
                        }
                    });
                ui.end_row();

                ui.label(format!("Speed · {:.2}×", self.voice.speed));
                ui.add(egui::Slider::new(&mut self.voice.speed, 0.7..=1.3));
                ui.end_row();

                ui.label("Model");
                egui::ComboBox::from_id_salt("model")
                    .selected_text(if self.model.is_empty() {
                        "—".into()
                    } else {
                        self.model.clone()
                    })
                    .show_ui(ui, |ui| {
                        for m in &self.models {
                            ui.selectable_value(&mut self.model, m.clone(), m);
                        }
                    });
                ui.end_row();

                let field_w = (ui.available_width() - 150.0).clamp(220.0, 560.0);
                ui.label("Title");
                ui.add(egui::TextEdit::singleline(&mut self.title).desired_width(field_w));
                ui.end_row();
                ui.label("Author");
                ui.add(egui::TextEdit::singleline(&mut self.author).desired_width(field_w));
                ui.end_row();
                ui.label("Output folder");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.out_dir)
                            .desired_width(field_w - 36.0),
                    );
                    if ui.button("…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.out_dir = dir.to_string_lossy().to_string();
                        }
                    }
                });
                ui.end_row();
            });

            ui.add_space(6.0);
            ui.add_enabled_ui(self.ollama_up, |ui| {
                ui.checkbox(
                    &mut self.polish,
                    "Polish transcripts with the local model (recommended)",
                );
            });
            ui.weak(if self.ollama_up {
                "Removes front-matter, cover/title boilerplate and stray headings that vary per book. Deletion-only; falls back to the raw transcript if unsure."
            } else {
                "Requires Ollama. Without it, the built-in cleaner is used."
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .button(format!("{}  Back", egui_phosphor::regular::ARROW_LEFT))
                    .clicked()
                {
                    self.stage = Stage::Drop;
                    self.book = None;
                    self.chapters.clear();
                }
                let can_go = !self.chapters.is_empty() && !self.out_dir.is_empty() && !self.busy;
                if ui
                    .add_enabled(
                        can_go,
                        egui::Button::new(format!(
                            "{}  Generate Audiobook",
                            egui_phosphor::regular::PLAY
                        )),
                    )
                    .clicked()
                {
                    self.generate();
                }
            });
        });
    }

    fn view_running(&mut self, ui: &mut egui::Ui) {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.heading("Generating…");
            if let Some(p) = &self.progress {
                ui.add_space(8.0);
                ui.add(egui::ProgressBar::new(p.pct / 100.0).show_percentage());
                ui.label(format!("{} · {}", p.stage, p.message));
                if p.total > 1 {
                    ui.weak(format!("{}/{}", p.current, p.total));
                }
            } else {
                ui.spinner();
                ui.label("Starting…");
            }
        });
    }

    fn view_done(&mut self, ui: &mut egui::Ui) {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.heading(format!(
                "{}  Audiobook ready",
                egui_phosphor::regular::CHECK_CIRCLE
            ));
            if let Some(path) = self.result_path.clone() {
                ui.add_space(8.0);
                ui.label(&path);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(format!(
                            "{}  Reveal in Finder",
                            egui_phosphor::regular::FOLDER_OPEN
                        ))
                        .clicked()
                    {
                        let _ = pipeline::reveal_in_os(&path);
                    }
                    if ui.button("Make another").clicked() {
                        self.stage = Stage::Drop;
                        self.book = None;
                        self.chapters.clear();
                        self.result_path = None;
                        self.progress = None;
                    }
                });
            }
        });
    }
}

fn voice_label(id: &str) -> String {
    VOICES
        .iter()
        .find(|(v, _, _)| *v == id)
        .map(|(_, _, l)| (*l).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// Prefer small/fast instruct models for structured extraction.
fn pick_model_default(models: &[String]) -> String {
    for pref in [
        "gemma4:e2b",
        "gemma",
        "llama",
        "qwen",
        "mistral",
        "deepseek",
    ] {
        if let Some(m) = models.iter().find(|m| m.contains(pref)) {
            return m.clone();
        }
    }
    models.first().cloned().unwrap_or_default()
}

fn title_from_filename(name: &str) -> String {
    name.trim_end_matches(".pdf").replace(['_', '-'], " ")
}

fn default_out_dir(pdf_path: &str) -> String {
    let p = std::path::Path::new(pdf_path);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    p.parent()
        .map(|d| {
            d.join(format!("{stem} - Audiobook"))
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_default()
}
