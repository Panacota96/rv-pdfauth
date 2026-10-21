//! The window. Drawing only: rules live in `state.rs`, work lives in the core (on a worker thread).
//! Rakvor dark theme; interface language EN/ES/FR switchable at any time (texts from i18n "ui").

use crate::state::{GuiState, OptionChange, Phase, Viewer, decode_png, verify_digest};
use eframe::egui;
use rv_pdfauth_core::analyzer::Severity;
use rv_pdfauth_core::{Event, HashAlgo, RunConfig, RunSummary, TOOL_VERSION};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

// Rakvor palette
const NIGHT: egui::Color32 = egui::Color32::from_rgb(0x0D, 0x11, 0x17);
const NIGHT2: egui::Color32 = egui::Color32::from_rgb(0x1A, 0x1F, 0x26);
const BLUE: egui::Color32 = egui::Color32::from_rgb(0x2A, 0x5C, 0xFF);
const VIOLET: egui::Color32 = egui::Color32::from_rgb(0x6B, 0x00, 0xFF);
const CYAN: egui::Color32 = egui::Color32::from_rgb(0x00, 0xE5, 0xFF);
const MIST: egui::Color32 = egui::Color32::from_rgb(0xE6, 0xE6, 0xE6);
const UI_LANGS: [(&str, &str); 3] = [("en", "English"), ("es", "Español"), ("fr", "Français")];
const RED: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x6B, 0x6B);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xB0, 0x40);
const YELLOW: egui::Color32 = egui::Color32::from_rgb(0xF2, 0xD3, 0x5B);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x4C, 0xD0, 0x8A);
const GREY: egui::Color32 = egui::Color32::from_rgb(0x9A, 0xA4, 0xB2);
// Preset values for the numeric options (one click = one audited change; the current value is always listed).
const DPI_PRESETS: [u32; 6] = [72, 96, 110, 150, 200, 300];
const PAGE_PRESETS: [u32; 6] = [10, 25, 50, 100, 250, 500];
const TIMEOUT_PRESETS: [u64; 5] = [60, 120, 300, 600, 1800];
const MEMORY_PRESETS: [u64; 5] = [1024, 2048, 4096, 8192, 16384];

fn severity_color(s: Severity) -> egui::Color32 {
    match s {
        Severity::High => RED,
        Severity::Medium => AMBER,
        Severity::Low => YELLOW,
        Severity::Info => GREY,
    }
}

fn status_color(s: &str) -> egui::Color32 {
    match s {
        "problem" => RED,
        "attention" => AMBER,
        _ => GREEN,
    }
}

/// Zoom by a factor at an anchor (panel px), for a panel size and an image size.
type ZoomRequest = (f32, [f32; 2], [f32; 2], [f32; 2]);
/// One picture panel: screen rect, origin and size of its region in the PNG, caption, draw box overlay.
type Panel = (egui::Rect, [f32; 2], [f32; 2], String, bool);

enum Msg {
    Event(Event),
    Done(Result<RunSummary, String>),
}

/// The diff picture currently on screen (decoded once per selection).
struct ViewerTexture {
    path: PathBuf,
    handle: Option<egui::TextureHandle>,
    size: [u32; 2],
    /// Some(true) = matches the digest recorded in the audit log.
    digest_ok: Option<bool>,
    error: Option<String>,
}

pub struct HashInvApp {
    state: GuiState,
    rx: Option<Receiver<Msg>>,
    cancel: Arc<AtomicBool>,
    started: Option<Instant>,
    notice: Option<String>,
    themed: bool,
    texture: Option<ViewerTexture>,
}

/// A ComboBox of preset numbers that also lists the current value; returns the picked one.
fn preset_combo<T: Copy + PartialEq + Ord + std::fmt::Display>(
    ui: &mut egui::Ui,
    id: &str,
    current: T,
    presets: &[T],
) -> Option<T> {
    let mut values: Vec<T> = presets.to_vec();
    if !values.contains(&current) {
        values.push(current);
        values.sort();
    }
    let mut picked = None;
    egui::ComboBox::from_id_salt(id)
        .selected_text(current.to_string())
        .width(70.0)
        .show_ui(ui, |ui| {
            for v in values {
                if ui.selectable_label(v == current, v.to_string()).clicked() && v != current {
                    picked = Some(v);
                }
            }
        });
    picked
}

/// Applies the Rakvor dark theme.
pub fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = NIGHT;
    v.window_fill = NIGHT2;
    v.extreme_bg_color = NIGHT2; // text fields
    v.widgets.inactive.bg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(0x2B, 0x32, 0x40));
    v.widgets.noninteractive.bg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(0x2B, 0x32, 0x40));
    v.faint_bg_color = NIGHT2;
    v.override_text_color = Some(MIST);
    v.selection.bg_fill = BLUE;
    v.hyperlink_color = CYAN;
    v.widgets.inactive.bg_fill = NIGHT2;
    v.widgets.inactive.weak_bg_fill = NIGHT2;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, CYAN);
    v.widgets.active.bg_fill = BLUE;
    ctx.set_visuals(v);
}

impl HashInvApp {
    pub fn new() -> Self {
        HashInvApp {
            state: GuiState::default(),
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            started: None,
            notice: None,
            themed: false,
            texture: None,
        }
    }

    /// Starts with evidence already selected (paths given to the executable).
    pub fn with_inputs(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut app = Self::new();
        for p in paths {
            app.state.add_input(p);
        }
        app
    }

    fn start(&mut self, ctx: &egui::Context) {
        let cfg: RunConfig = match self.state.to_config() {
            Ok(c) => c,
            Err(e) => {
                self.notice = Some(e);
                return;
            }
        };
        self.notice = None;
        self.state.start();
        self.cancel.store(false, Ordering::SeqCst);
        self.started = Some(Instant::now());
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let cancel = self.cancel.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let tx2 = tx.clone();
            let r2 = repaint.clone();
            let mut sink = move |e: &Event| {
                let _ = tx2.send(Msg::Event(e.clone()));
                r2.request_repaint();
            };
            let result = rv_pdfauth_core::run(&cfg, &mut sink, &cancel);
            let _ = tx.send(Msg::Done(result));
            repaint.request_repaint();
        });
    }

    fn drain(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut finished = false;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Event(e) => {
                    if let Event::Timestamp { status, reason, .. } = &e {
                        if status != "ok" {
                            self.state
                                .warnings
                                .push(format!("timestamp: {}", reason.as_deref().unwrap_or("-")));
                        }
                    }
                    self.state.apply(&e)
                }
                Msg::Done(Ok(summary)) => {
                    self.state.load_results(&summary);
                    self.texture = None;
                    finished = true;
                }
                Msg::Done(Err(e)) => {
                    self.state.fail(e);
                    finished = true;
                }
            }
        }
        if finished {
            self.rx = None;
            self.started = None;
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(38.0, 38.0), egui::Sense::hover());
            ui.painter().rect(
                rect,
                8.0,
                NIGHT2,
                egui::Stroke::new(2.0, BLUE),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                rect.center() - egui::vec2(0.0, 3.0),
                egui::Align2::CENTER_CENTER,
                "RV",
                egui::FontId::proportional(16.0),
                MIST,
            );
            ui.painter().line_segment(
                [
                    rect.center() + egui::vec2(-8.0, 10.0),
                    rect.center() + egui::vec2(8.0, 10.0),
                ],
                egui::Stroke::new(2.0, CYAN),
            );
            ui.vertical(|ui| {
                ui.label(egui::RichText::new("R A K V O R").strong().size(16.0));
                let desc =
                    rv_pdfauth_core::i18n::catalog(&self.state.ui_lang).brand()["descriptor"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                ui.label(egui::RichText::new(desc).color(CYAN).size(10.0));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let current = UI_LANGS
                    .iter()
                    .find(|(k, _)| *k == self.state.ui_lang)
                    .map(|(_, v)| *v)
                    .unwrap_or("English");
                let mut chosen = None;
                egui::ComboBox::from_id_salt("ui-lang")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (k, v) in UI_LANGS {
                            if ui.selectable_label(self.state.ui_lang == k, v).clicked() {
                                chosen = Some(k);
                            }
                        }
                    });
                if let Some(k) = chosen {
                    self.state.set_ui_lang(k);
                }
                ui.label(self.state.tr("ui_lang"));
                ui.weak(format!("v{}", TOOL_VERSION));
            });
        });
        ui.weak(self.state.tr("audit_note"));
        let (r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 3.0), egui::Sense::hover());
        let third = r.width() / 3.0;
        for (i, c) in [BLUE, VIOLET, CYAN].iter().enumerate() {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    r.min + egui::vec2(third * i as f32, 0.0),
                    egui::vec2(third, 3.0),
                ),
                0.0,
                *c,
            );
        }
    }

    fn section_title(ui: &mut egui::Ui, text: String) {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(text).color(CYAN).strong().size(15.0));
    }

    fn evidence_section(&mut self, ui: &mut egui::Ui, enabled: bool) {
        Self::section_title(ui, self.state.tr("evidence"));
        ui.label(self.state.tr("evidence_help"));
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                #[cfg(windows)]
                {
                    if ui.button(self.state.tr("add_files")).clicked() {
                        if let Some(files) = rfd::FileDialog::new().pick_files() {
                            for f in files {
                                self.state.add_input(f);
                            }
                        }
                    }
                    if ui.button(self.state.tr("add_folder")).clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            self.state.add_input(d);
                        }
                    }
                }
                let hint = self.state.tr("path_hint");
                let entry = ui.add(
                    egui::TextEdit::singleline(&mut self.state.path_entry)
                        .hint_text(hint)
                        .desired_width(320.0),
                );
                let enter = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button(self.state.tr("add_path")).clicked() || enter {
                    self.state.add_typed_path();
                }
            });
            let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .filter(|p| !p.as_os_str().is_empty())
                    .collect()
            });
            for p in dropped {
                self.state.add_input(p);
            }
            let mut remove = None;
            egui::ScrollArea::vertical()
                .max_height(100.0)
                .id_salt("inputs")
                .show(ui, |ui| {
                    if self.state.inputs.is_empty() {
                        ui.weak(self.state.tr("no_evidence"));
                    }
                    for (i, p) in self.state.inputs.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.small_button("x").clicked() {
                                remove = Some(i);
                            }
                            ui.monospace(p.display().to_string());
                        });
                    }
                });
            if let Some(i) = remove {
                self.state.remove_input(i);
            }
            let mut rec = self.state.recursive;
            if ui.checkbox(&mut rec, self.state.tr("subfolders")).changed() {
                self.state.set_recursive(rec);
            }
        });
    }

    fn hashing_section(&mut self, ui: &mut egui::Ui, enabled: bool) {
        Self::section_title(ui, self.state.tr("hashes"));
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal_wrapped(|ui| {
                for a in HashAlgo::ALL {
                    let mut on = self.state.selected.contains(&a);
                    let tip = if self.state.can_toggle(a) {
                        String::new()
                    } else if on {
                        self.state.tr("hash_tip_one")
                    } else {
                        self.state.tr("hash_tip_two")
                    };
                    let r = ui.add_enabled(
                        self.state.can_toggle(a),
                        egui::Checkbox::new(&mut on, a.name()),
                    );
                    let r = if tip.is_empty() {
                        r
                    } else {
                        r.on_disabled_hover_text(tip)
                    };
                    if r.changed() {
                        if let Err(e) = self.state.toggle_algo(a) {
                            self.notice = Some(e);
                        }
                    }
                }
            });
        });
    }

    fn options_section(&mut self, ui: &mut egui::Ui, enabled: bool) {
        Self::section_title(ui, self.state.tr("options"));
        let o = self.state.options.clone();
        let mut fp_entry = self.state.fp_db_entry.clone();
        let tr = |k: &str| self.state.tr(k);
        let mut changes: Vec<OptionChange> = vec![];
        ui.add_enabled_ui(enabled, |ui| {
            egui::Grid::new("options-grid")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    let mut v = o.extract_revisions;
                    if ui.checkbox(&mut v, tr("opt_revisions")).changed() {
                        changes.push(OptionChange::ExtractRevisions(v));
                    }
                    let mut v = o.objects_dump;
                    if ui.checkbox(&mut v, tr("opt_objects")).changed() {
                        changes.push(OptionChange::ObjectsDump(v));
                    }
                    ui.end_row();

                    ui.horizontal(|ui| {
                        let mut v = o.visual_diff;
                        if ui.checkbox(&mut v, tr("opt_visual")).changed() {
                            changes.push(OptionChange::VisualDiff(v));
                        }
                        ui.add_enabled_ui(o.visual_diff, |ui| {
                            ui.label(tr("opt_dpi"));
                            if let Some(d) = preset_combo(ui, "opt-dpi", o.dpi, &DPI_PRESETS) {
                                changes.push(OptionChange::Dpi(d));
                            }
                            ui.label(tr("opt_max_pages"));
                            if let Some(p) =
                                preset_combo(ui, "opt-pages", o.max_pages, &PAGE_PRESETS)
                            {
                                changes.push(OptionChange::MaxPages(p));
                            }
                        });
                    });
                    let mut v = o.extract_embedded;
                    if ui.checkbox(&mut v, tr("opt_embedded")).changed() {
                        changes.push(OptionChange::ExtractEmbedded(v));
                    }
                    ui.end_row();

                    ui.horizontal(|ui| {
                        ui.label(tr("opt_worker"));
                        ui.label(tr("opt_timeout"));
                        if let Some(t) =
                            preset_combo(ui, "opt-timeout", o.worker_timeout_s, &TIMEOUT_PRESETS)
                        {
                            changes.push(OptionChange::WorkerTimeout(t));
                        }
                        ui.label(tr("opt_memory"));
                        if let Some(m) =
                            preset_combo(ui, "opt-memory", o.worker_memory_mb, &MEMORY_PRESETS)
                        {
                            changes.push(OptionChange::WorkerMemory(m));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(tr("opt_item_reports"));
                        if ui
                            .radio(!o.item_reports_flagged_only, tr("opt_reports_all"))
                            .clicked()
                        {
                            changes.push(OptionChange::ItemReportsFlaggedOnly(false));
                        }
                        if ui
                            .radio(o.item_reports_flagged_only, tr("opt_reports_flagged"))
                            .clicked()
                        {
                            changes.push(OptionChange::ItemReportsFlaggedOnly(true));
                        }
                    });
                    ui.end_row();
                });
            ui.horizontal(|ui| {
                ui.label(tr("opt_fp_db"));
                let r = ui.add(
                    egui::TextEdit::singleline(&mut fp_entry)
                        .hint_text(tr("opt_fp_hint"))
                        .desired_width(360.0),
                );
                if r.lost_focus() {
                    changes.push(OptionChange::FingerprintsDb(fp_entry.clone()));
                }
                #[cfg(windows)]
                if ui.button(tr("browse")).clicked() {
                    if let Some(f) = rfd::FileDialog::new()
                        .add_filter("JSON", &["json"])
                        .pick_file()
                    {
                        changes.push(OptionChange::FingerprintsDb(f.display().to_string()));
                    }
                }
            });
        });
        self.state.fp_db_entry = fp_entry;
        for c in changes {
            self.state.set_option(c);
        }
    }

    fn results_table(&mut self, ui: &mut egui::Ui) {
        if self.state.results.is_empty() {
            return;
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(self.state.tr("results"))
                .color(CYAN)
                .strong(),
        );
        let mut open = None;
        egui::ScrollArea::vertical()
            .id_salt("results")
            .max_height(220.0)
            .show(ui, |ui| {
                egui::Grid::new("results-grid")
                    .num_columns(6)
                    .striped(true)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong("#");
                        ui.strong(self.state.tr("col_file"));
                        ui.strong(self.state.tr("col_status"));
                        ui.strong(self.state.tr("col_findings"));
                        ui.strong(self.state.tr("col_revisions"));
                        ui.label("");
                        ui.end_row();
                        for (i, r) in self.state.results.iter().enumerate() {
                            ui.monospace(r.index.to_string());
                            ui.label(&r.name);
                            ui.colored_label(
                                status_color(&r.status),
                                self.state.tr(&format!("status_{}", r.status)),
                            );
                            let txt = match r.top {
                                Some(s) => {
                                    format!("{} ({})", r.findings, self.state.severity_name(s))
                                }
                                None => r.findings.to_string(),
                            };
                            ui.colored_label(r.top.map_or(MIST, severity_color), txt);
                            ui.label(r.revisions.to_string());
                            if ui
                                .add_enabled(
                                    r.viewer_item.is_some(),
                                    egui::Button::new(self.state.tr("review")),
                                )
                                .clicked()
                            {
                                open = Some(i);
                            }
                            ui.end_row();
                        }
                    });
            });
        if let Some(i) = open {
            self.state.open_viewer(i);
        }
    }

    /// Loads (once per selection) the diff PNG of the viewer's page and checks it against the audit log.
    fn sync_texture(&mut self, ctx: &egui::Context) {
        let Some(path) = self.state.viewer.png_path() else {
            self.texture = None;
            return;
        };
        if self.texture.as_ref().is_some_and(|t| t.path == path) {
            return;
        }
        let digest = self.state.viewer.png_digest();
        let mut t = ViewerTexture {
            path: path.clone(),
            handle: None,
            size: [0, 0],
            digest_ok: None,
            error: None,
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                t.digest_ok = digest.as_ref().and_then(|d| verify_digest(&bytes, d));
                match decode_png(&bytes) {
                    Ok((w, h, rgba)) => {
                        let img = egui::ColorImage::from_rgba_unmultiplied(
                            [w as usize, h as usize],
                            &rgba,
                        );
                        t.handle = Some(ctx.load_texture(
                            path.display().to_string(),
                            img,
                            egui::TextureOptions::LINEAR,
                        ));
                        t.size = [w, h];
                    }
                    Err(e) => t.error = Some(e),
                }
            }
            Err(e) => t.error = Some(e.to_string()),
        }
        self.texture = Some(t);
    }

    fn viewer_ui(&mut self, ui: &mut egui::Ui) {
        self.sync_texture(ui.ctx());
        let tr = |k: &str| self.state.tr(k);
        // Title + selectors.
        let mut back = false;
        let (mut sel_item, mut sel_pair, mut sel_page) = (None, None, None);
        let mut zoom_factor: Option<f32> = None;
        let mut fit = false;
        let v = &self.state.viewer;
        let Some(item) = v.current_item().cloned() else {
            self.state.viewer.close();
            return;
        };
        ui.horizontal(|ui| {
            if ui.button(tr("back")).clicked() {
                back = true;
            }
            ui.label(
                egui::RichText::new(format!("{} \u{00B7} {}", tr("viewer_title"), item.name))
                    .color(CYAN)
                    .strong()
                    .size(15.0),
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("item"));
            egui::ComboBox::from_id_salt("viewer-item")
                .selected_text(&item.name)
                .show_ui(ui, |ui| {
                    for (i, it) in v.items.iter().enumerate() {
                        if ui.selectable_label(i == v.item, &it.name).clicked() {
                            sel_item = Some(i);
                        }
                    }
                });
            ui.label(tr("pair"));
            let pair_label = |p: &crate::state::PairView| {
                format!(
                    "{} {} vs {} \u{00B7} {}",
                    tr("rev"),
                    p.from,
                    p.to,
                    if p.has_changes() {
                        tr("changed")
                    } else {
                        tr("unchanged")
                    }
                )
            };
            egui::ComboBox::from_id_salt("viewer-pair")
                .selected_text(v.current_pair().map(&pair_label).unwrap_or_default())
                .show_ui(ui, |ui| {
                    for (i, p) in item.pairs.iter().enumerate() {
                        if ui.selectable_label(i == v.pair, pair_label(p)).clicked() {
                            sel_pair = Some(i);
                        }
                    }
                });
            ui.label(tr("page"));
            let page_label = |p: &crate::state::PageView| {
                if p.changed {
                    format!(
                        "{} \u{00B7} {} {}",
                        p.page,
                        tr("changed"),
                        p.changed_percent()
                    )
                } else {
                    p.page.to_string()
                }
            };
            egui::ComboBox::from_id_salt("viewer-page")
                .selected_text(
                    v.current_page()
                        .map(&page_label)
                        .unwrap_or("\u{2014}".into()),
                )
                .show_ui(ui, |ui| {
                    if let Some(pr) = v.current_pair() {
                        for (i, p) in pr.pages.iter().enumerate() {
                            if ui.selectable_label(i == v.page, page_label(p)).clicked() {
                                sel_page = Some(i);
                            }
                        }
                    }
                });
            ui.separator();
            ui.label(tr("zoom"));
            if ui.button(" - ").clicked() {
                zoom_factor = Some(1.0 / crate::state::ZOOM_STEP);
            }
            ui.monospace(format!("{:>4.0}%", v.zoom * 100.0));
            if ui.button(" + ").clicked() {
                zoom_factor = Some(crate::state::ZOOM_STEP);
            }
            if ui.button(tr("fit")).clicked() {
                fit = true;
            }
        });
        ui.weak(tr("viewer_help"));

        // Timeline strip.
        let (hl_from, hl_to) = v.pair_revisions().unwrap_or((0, 0));
        let mut pick_rev = None;
        ui.label(egui::RichText::new(tr("timeline")).color(CYAN).strong());
        egui::ScrollArea::horizontal()
            .id_salt("timeline")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, r) in item.revisions.iter().enumerate() {
                        if i > 0 {
                            ui.label(egui::RichText::new(">").color(GREY));
                        }
                        let on = r.index == hl_from || r.index == hl_to;
                        let resp = egui::Frame::new()
                            .fill(NIGHT2)
                            .stroke(egui::Stroke::new(
                                if on { 2.0 } else { 1.0 },
                                if on {
                                    CYAN
                                } else {
                                    egui::Color32::from_rgb(0x2B, 0x32, 0x40)
                                },
                            ))
                            .corner_radius(6)
                            .inner_margin(egui::Margin::same(6))
                            .show(ui, |ui| {
                                ui.set_max_width(230.0);
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.strong(format!("{} {}", tr("rev"), r.index));
                                        if !r.signed_by.is_empty() {
                                            ui.colored_label(GREEN, tr("signed"));
                                        }
                                    });
                                    ui.small(r.date.clone().unwrap_or_else(|| tr("no_date")));
                                    ui.small(
                                        r.producer.clone().unwrap_or_else(|| tr("no_producer")),
                                    );
                                    if i > 0 {
                                        let gap = GuiState::since_text(r.since_prev_s);
                                        let neg = r.since_prev_s.is_some_and(|s| s < 0);
                                        ui.colored_label(
                                            if neg { RED } else { GREY },
                                            format!("{} {}", gap, tr("since_prev")),
                                        );
                                    }
                                    ui.horizontal_wrapped(|ui| {
                                        for (sev, code) in &r.badges {
                                            ui.label(
                                                egui::RichText::new(code)
                                                    .small()
                                                    .color(NIGHT)
                                                    .background_color(severity_color(*sev)),
                                            );
                                        }
                                    });
                                });
                            })
                            .response;
                        if i > 0
                            && ui
                                .interact(
                                    resp.rect,
                                    ui.make_persistent_id(("rv-timeline-rev", i)),
                                    egui::Sense::click(),
                                )
                                .clicked()
                        {
                            pick_rev = Some(r.index);
                        }
                    }
                });
            });

        // Picture area (two synchronised halves) + findings below.
        let findings = self.state.viewer_findings();
        let avail = ui.available_size();
        let find_h = (avail.y * 0.30).clamp(110.0, 240.0);
        let pic_h = (avail.y - find_h - 40.0).max(160.0);
        let tex = self.texture.as_ref();
        ui.horizontal(|ui| {
            if let Some(t) = tex {
                match t.digest_ok {
                    Some(true) => ui.colored_label(GREEN, tr("digest_ok")),
                    Some(false) => ui.colored_label(RED, tr("digest_bad")),
                    None => ui.colored_label(GREY, tr("digest_none")),
                };
                if let Some(e) = &t.error {
                    ui.colored_label(RED, e);
                }
            }
            if let Some(p) = v.current_page() {
                if p.changed {
                    ui.weak(format!("{} {}", p.boxes.len(), tr("regions")));
                }
            }
            if let Some(pr) = v.current_pair() {
                if !pr.pages_added.is_empty() {
                    ui.colored_label(
                        AMBER,
                        format!("{}: {:?}", tr("pages_added"), pr.pages_added),
                    );
                }
                if !pr.pages_removed.is_empty() {
                    ui.colored_label(
                        AMBER,
                        format!("{}: {:?}", tr("pages_removed"), pr.pages_removed),
                    );
                }
            }
        });
        let (area, _) = ui.allocate_exact_size(egui::vec2(avail.x, pic_h), egui::Sense::hover());
        let mut pan: Option<egui::Vec2> = None;
        let mut zoom_at: Option<ZoomRequest> = None;
        let mut reset = false;
        match (tex, v.current_page()) {
            (Some(t), Some(page)) if t.handle.is_some() => {
                let id = t.handle.as_ref().map(|h| h.id()).unwrap_or_default();
                let tsize = [t.size[0] as f32, t.size[1] as f32];
                let panels: Vec<Panel> = match Viewer::halves(t.size, [page.width, page.height]) {
                    Some(h) => {
                        let w = (area.width() - 8.0) / 2.0;
                        vec![
                            (
                                egui::Rect::from_min_size(area.min, egui::vec2(w, area.height())),
                                h.left,
                                h.size,
                                format!("{} \u{00B7} {} {}", tr("previous"), tr("rev"), hl_from),
                                true,
                            ),
                            (
                                egui::Rect::from_min_size(
                                    area.min + egui::vec2(w + 8.0, 0.0),
                                    egui::vec2(w, area.height()),
                                ),
                                h.right,
                                h.size,
                                format!("{} \u{00B7} {} {}", tr("current"), tr("rev"), hl_to),
                                true,
                            ),
                        ]
                    }
                    None => vec![(area, [0.0, 0.0], tsize, String::new(), false)],
                };
                for (n, (rect, origin, size, label, overlay)) in panels.into_iter().enumerate() {
                    let painter = ui.painter_at(rect);
                    painter.rect_filled(rect, 4.0, NIGHT2);
                    let (sc, off) = v.transform([rect.width(), rect.height()], size);
                    let img = egui::Rect::from_min_size(
                        rect.min + egui::vec2(off[0], off[1]),
                        egui::vec2(size[0] * sc, size[1] * sc),
                    );
                    let uv = egui::Rect::from_min_max(
                        egui::pos2(origin[0] / tsize[0], origin[1] / tsize[1]),
                        egui::pos2(
                            (origin[0] + size[0]) / tsize[0],
                            (origin[1] + size[1]) / tsize[1],
                        ),
                    );
                    painter.image(id, img, uv, egui::Color32::WHITE);
                    if overlay {
                        for b in &page.boxes {
                            let r = egui::Rect::from_min_size(
                                img.min + egui::vec2(b[0] as f32 * sc, b[1] as f32 * sc),
                                egui::vec2(b[2] as f32 * sc, b[3] as f32 * sc),
                            );
                            painter.rect_filled(
                                r,
                                0.0,
                                egui::Color32::from_rgba_unmultiplied(0x00, 0xE5, 0xFF, 40),
                            );
                            painter.rect_stroke(
                                r.expand(3.0),
                                2.0,
                                egui::Stroke::new(2.0, CYAN),
                                egui::StrokeKind::Outside,
                            );
                        }
                    }
                    if !label.is_empty() {
                        let g =
                            painter.layout_no_wrap(label, egui::FontId::proportional(13.0), MIST);
                        let bg = egui::Rect::from_min_size(
                            rect.min + egui::vec2(6.0, 6.0),
                            g.size() + egui::vec2(10.0, 6.0),
                        );
                        painter.rect_filled(bg, 4.0, NIGHT.gamma_multiply(0.85));
                        painter.galley(bg.min + egui::vec2(5.0, 3.0), g, MIST);
                    }
                    let resp = ui.interact(
                        rect,
                        ui.make_persistent_id(("rv-viewer-half", n)),
                        egui::Sense::click_and_drag(),
                    );
                    if resp.dragged() {
                        pan = Some(resp.drag_delta());
                    }
                    if resp.double_clicked() {
                        reset = true;
                    }
                    if resp.hovered() {
                        let (scroll, pinch, pointer) = ui.input(|i| {
                            (
                                i.smooth_scroll_delta.y,
                                i.zoom_delta(),
                                i.pointer.hover_pos(),
                            )
                        });
                        let f = pinch * (scroll / 240.0).exp();
                        if (f - 1.0).abs() > 1e-4 {
                            let a = pointer.unwrap_or(rect.center()) - rect.min;
                            zoom_at = Some((f, [a.x, a.y], [rect.width(), rect.height()], size));
                        }
                    }
                }
            }
            _ => {
                let painter = ui.painter_at(area);
                painter.rect_filled(area, 4.0, NIGHT2);
                let msg = if item.visual_status != "done" {
                    format!(
                        "{}: {}",
                        tr("visual_unavailable"),
                        item.visual_reason
                            .clone()
                            .unwrap_or_else(|| item.visual_status.clone())
                    )
                } else {
                    tr("no_picture")
                };
                let g = painter.layout(
                    msg,
                    egui::FontId::proportional(14.0),
                    GREY,
                    (area.width() - 48.0).max(100.0),
                );
                painter.galley(area.center() - g.size() / 2.0, g, GREY);
            }
        }

        // Findings for this item.
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(tr("item_findings"))
                .color(CYAN)
                .strong(),
        );
        egui::ScrollArea::vertical()
            .id_salt("viewer-findings")
            .max_height(find_h)
            .show(ui, |ui| {
                if findings.is_empty() {
                    ui.weak(tr("no_findings"));
                }
                for f in &findings {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                " {} ",
                                self.state.severity_name(f.severity)
                            ))
                            .color(NIGHT)
                            .strong()
                            .background_color(severity_color(f.severity)),
                        );
                        ui.strong(&f.title);
                        ui.weak(&f.code);
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(GREEN, format!("{}:", tr("shows")));
                        ui.label(&f.shows);
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(AMBER, format!("{}:", tr("not_proves")));
                        ui.label(&f.not_proves);
                    });
                    ui.add_space(4.0);
                }
            });

        // Apply interactions (state rules live in Viewer).
        let vw = &mut self.state.viewer;
        if back {
            vw.close();
        }
        if let Some(i) = sel_item {
            vw.select_item(i);
        }
        if let Some(p) = sel_pair {
            vw.select_pair(p);
        }
        if let Some(r) = pick_rev {
            if let Some(p) = vw
                .current_item()
                .and_then(|it| it.pairs.iter().position(|p| p.to == r))
            {
                vw.select_pair(p);
            }
        }
        if let Some(p) = sel_page {
            vw.select_page(p);
        }
        if let Some(f) = zoom_factor {
            vw.set_zoom(vw.zoom * f);
        }
        if fit || reset {
            vw.reset_view();
        }
        if let Some(d) = pan {
            vw.pan_by([d.x, d.y]);
        }
        if let Some((f, a, p, s)) = zoom_at {
            vw.zoom_at(f, a, p, s);
        }
    }

    fn report_section(&mut self, ui: &mut egui::Ui, enabled: bool) {
        Self::section_title(ui, self.state.tr("reports"));
        let tr = |k: &str| self.state.tr(k);
        let (l_lang, l_out, l_browse, l_exam, l_case, l_hint, l_all) = (
            tr("report_lang"),
            tr("out_dir"),
            tr("browse"),
            tr("examiner"),
            tr("case"),
            tr("case_hint"),
            tr("lang_all"),
        );
        ui.add_enabled_ui(enabled, |ui| {
            egui::Grid::new("report-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(l_lang);
                    let choices = [
                        ("en", "English".to_string()),
                        ("es", "Español".to_string()),
                        ("fr", "Français".to_string()),
                        ("all", l_all),
                    ];
                    let current = choices
                        .iter()
                        .find(|(k, _)| *k == self.state.lang)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt("lang")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (k, v) in &choices {
                                if ui.selectable_label(self.state.lang == *k, v).clicked() {
                                    chosen = Some(*k);
                                }
                            }
                        });
                    if let Some(k) = chosen {
                        self.state.set_lang(k);
                    }
                    ui.end_row();
                    ui.label(l_out);
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.state.out_dir)
                                .desired_width(300.0),
                        );
                        #[cfg(windows)]
                        if ui.button(&l_browse).clicked() {
                            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                                self.state.out_dir = d.display().to_string();
                            }
                        }
                        let _ = &l_browse;
                    });
                    ui.end_row();
                    ui.label(l_exam);
                    ui.add(egui::TextEdit::singleline(&mut self.state.actor).desired_width(300.0));
                    ui.end_row();
                    ui.label(l_case);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.state.case_id)
                            .hint_text(l_hint)
                            .desired_width(300.0),
                    );
                    ui.end_row();
                });
        });
    }

    fn trust_section(&mut self, ui: &mut egui::Ui, enabled: bool) {
        Self::section_title(ui, self.state.tr("trust"));
        let tr = |k: &str| self.state.tr(k);
        let (l_jur, l_co, l_fr, l_tsa, l_cert, l_pass, l_browse) = (
            tr("jurisdiction"),
            tr("jur_co"),
            tr("jur_fr"),
            tr("tsa"),
            tr("sign_cert"),
            tr("sign_pass"),
            tr("browse"),
        );
        ui.add_enabled_ui(enabled, |ui| {
            egui::Grid::new("trust-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(l_jur);
                    let current = if self.state.jurisdiction == "fr" {
                        l_fr.clone()
                    } else {
                        l_co.clone()
                    };
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt("jur")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(self.state.jurisdiction == "co", &l_co)
                                .clicked()
                            {
                                chosen = Some("co");
                            }
                            if ui
                                .selectable_label(self.state.jurisdiction == "fr", &l_fr)
                                .clicked()
                            {
                                chosen = Some("fr");
                            }
                        });
                    if let Some(j) = chosen {
                        self.state.set_jurisdiction(j);
                    }
                    ui.end_row();
                    ui.label(l_tsa);
                    ui.horizontal(|ui| {
                        let mut on = self.state.timestamp;
                        let mut url = self.state.tsa_url.clone();
                        let c = ui.checkbox(&mut on, "");
                        let t = ui.add_enabled(
                            on,
                            egui::TextEdit::singleline(&mut url).desired_width(280.0),
                        );
                        if c.changed() || t.lost_focus() {
                            self.state.set_timestamp(on, &url);
                        } else {
                            self.state.tsa_url = url;
                        }
                    });
                    ui.end_row();
                    ui.label(l_cert);
                    ui.horizontal(|ui| {
                        let mut path = self.state.sign_cert.clone();
                        let r = ui.add(egui::TextEdit::singleline(&mut path).desired_width(280.0));
                        if r.lost_focus() || r.changed() {
                            self.state.set_sign_cert(&path);
                        }
                        #[cfg(windows)]
                        if ui.button(&l_browse).clicked() {
                            if let Some(f) = rfd::FileDialog::new()
                                .add_filter("PKCS#12", &["p12", "pfx"])
                                .pick_file()
                            {
                                self.state.set_sign_cert(&f.display().to_string());
                            }
                        }
                        let _ = &l_browse;
                    });
                    ui.end_row();
                    ui.label(l_pass);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.state.sign_password)
                            .password(true)
                            .desired_width(280.0),
                    );
                    ui.end_row();
                });
        });
    }

    fn run_section(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if self.state.is_running() {
                if ui.button(self.state.tr("cancel")).clicked() {
                    self.cancel.store(true, Ordering::SeqCst);
                    self.notice = Some(self.state.tr("cancelling"));
                }
            } else {
                let start = egui::Button::new(
                    egui::RichText::new(self.state.tr("start"))
                        .strong()
                        .color(egui::Color32::WHITE),
                )
                .fill(BLUE);
                if ui.add_enabled(self.state.can_start(), start).clicked() {
                    let ctx = ui.ctx().clone();
                    self.start(&ctx);
                }
            }
            if let Some(n) = &self.notice {
                ui.colored_label(egui::Color32::from_rgb(0xFF, 0xB0, 0x40), n);
            }
        });
        ui.add_space(6.0);
        ui.add(
            egui::ProgressBar::new(self.state.fraction())
                .text(self.state.progress_text())
                .fill(BLUE)
                .animate(self.state.is_running()),
        );
        let live = self.started.map(|s| s.elapsed().as_millis() as u64);
        ui.label(self.state.timing_text(live));
        if !self.state.current_item.is_empty() {
            ui.weak(format!(
                "{}: {}",
                self.state.tr("now"),
                self.state.current_item
            ));
        }
        match &self.state.phase {
            Phase::Failed(e) => {
                ui.colored_label(
                    egui::Color32::from_rgb(0xFF, 0x6B, 0x6B),
                    format!("{}: {}", self.state.tr("failed"), e),
                );
            }
            Phase::Finished | Phase::Cancelled => {
                if let Some(o) = self.state.outcome.clone() {
                    ui.separator();
                    let title = if self.state.phase == Phase::Cancelled {
                        self.state.tr("cancelled")
                    } else {
                        self.state.tr("done")
                    };
                    ui.label(egui::RichText::new(title).color(CYAN).strong());
                    ui.label(format!(
                        "{} {} \u{00B7} {} {} \u{00B7} {}: {} / {} / {} / {}",
                        o.items,
                        self.state.tr("files"),
                        o.errors,
                        self.state.tr("errors"),
                        self.state.tr("findings"),
                        o.high,
                        o.medium,
                        o.low,
                        o.info
                    ));
                    ui.horizontal(|ui| {
                        if ui.button(self.state.tr("open_report")).clicked() {
                            open_path(&o.report);
                        }
                        if ui.button(self.state.tr("open_logs")).clicked() {
                            open_path(&o.log_dir);
                        }
                    });
                    self.results_table(ui);
                }
            }
            _ => {}
        }
        if !self.state.warnings.is_empty() {
            egui::CollapsingHeader::new(format!(
                "{} ({})",
                self.state.tr("warnings"),
                self.state.warnings.len()
            ))
            .show(ui, |ui| {
                for w in &self.state.warnings {
                    ui.label(w);
                }
            });
        }
    }
}

impl Default for HashInvApp {
    fn default() -> Self {
        Self::new()
    }
}

fn open_path(p: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(p).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(p).spawn();
}

impl eframe::App for HashInvApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.themed {
            apply_theme(ui.ctx());
            self.themed = true;
        }
        self.drain();
        if self.state.is_running() {
            // Keep the elapsed timer moving even when no event arrives (large files).
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if self.state.viewer.open {
                // No outer scroll area: the mouse wheel zooms the picture.
                self.header(ui);
                self.viewer_ui(ui);
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.header(ui);
                let enabled = !self.state.is_running();
                self.evidence_section(ui, enabled);
                self.hashing_section(ui, enabled);
                self.options_section(ui, enabled);
                // Reports and trust side by side: a finished run and its results fit on one screen.
                ui.columns(2, |cols| {
                    self.report_section(&mut cols[0], enabled);
                    self.trust_section(&mut cols[1], enabled);
                });
                self.run_section(ui);
            });
        });
    }
}
