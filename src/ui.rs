use crate::core::{
    self, Action, CopyProgress, FileComparison, Mode, Pair, Plan, RunSummary, ScanMode,
    ScanProgress,
};
use eframe::egui::{
    self, Align, Align2, Color32, FontFamily, FontId, RichText, Stroke, TextureHandle, Ui, Vec2,
};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc,
};
use std::thread;
use std::time::Duration;

const BACKGROUND: Color32 = Color32::from_rgb(243, 245, 242);
const FIELD: Color32 = Color32::from_rgb(249, 250, 248);
const HEADER: Color32 = Color32::from_rgb(24, 45, 40);
const WHITE: Color32 = Color32::WHITE;
const TEXT: Color32 = Color32::from_rgb(25, 43, 38);
const MUTED: Color32 = Color32::from_rgb(80, 99, 92);
const BORDER: Color32 = Color32::from_rgb(210, 222, 215);
const ACCENT: Color32 = Color32::from_rgb(16, 102, 83);
const ACCENT_SOFT: Color32 = Color32::from_rgb(228, 241, 234);
const GREEN: Color32 = Color32::from_rgb(29, 117, 73);
const RED: Color32 = Color32::from_rgb(170, 54, 54);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Language {
    English,
    Russian,
}
fn t(lang: Language, en: &'static str, ru: &'static str) -> &'static str {
    if lang == Language::English {
        en
    } else {
        ru
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Busy {
    Scan,
    Copy,
    Compare,
}
enum WorkerEvent {
    Scanning(ScanProgress),
    Scanned(Result<Pair, String>),
    Copying(CopyProgress),
    Copied(RunSummary),
    Compared(Result<FileComparison, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Folders,
    Files,
    Activity,
    Settings,
}

pub struct ComparatorApp {
    language: Language,
    source: String,
    destination: String,
    mode: Mode,
    scan_mode: ScanMode,
    skip_matches: bool,
    pair: Option<Pair>,
    plan: Option<Plan>,
    last_run: Option<RunSummary>,
    scanned_pair: Option<Pair>,
    scanned_plan: Option<Plan>,
    busy: Option<Busy>,
    status: String,
    progress: f32,
    error: Option<String>,
    page: Page,
    show_files: bool,
    review_open: bool,
    search: String,
    confirm: bool,
    mirror_phrase: String,
    first_file: String,
    second_file: String,
    file_comparison: Option<FileComparison>,
    logs: Vec<String>,
    sender: Sender<WorkerEvent>,
    receiver: Receiver<WorkerEvent>,
    cancel: Arc<AtomicBool>,
    logo: TextureHandle,
}

impl ComparatorApp {
    pub fn new(ctx: &egui::Context) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        for (name, bytes) in [
            (
                "Geist",
                include_bytes!("../assets/Geist-Regular.ttf").as_slice(),
            ),
            (
                "Geist Semibold",
                include_bytes!("../assets/Geist-SemiBold.ttf").as_slice(),
            ),
        ] {
            fonts
                .font_data
                .insert(name.into(), egui::FontData::from_static(bytes).into());
        }
        let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
        proportional.insert(0, "Geist".into());
        let mut semibold = proportional.clone();
        semibold.insert(0, "Geist Semibold".into());
        fonts.families.insert(Self::semibold(), semibold);
        ctx.set_fonts(fonts);
        let mut visuals = egui::Visuals::light();
        visuals.panel_fill = BACKGROUND;
        visuals.window_fill = WHITE;
        visuals.override_text_color = Some(TEXT);
        visuals.extreme_bg_color = FIELD;
        visuals.selection.bg_fill = ACCENT_SOFT;
        visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
        visuals.widgets.inactive.bg_fill = WHITE;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
        visuals.widgets.hovered.bg_fill = ACCENT_SOFT;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
        ctx.set_visuals(visuals);
        ctx.style_mut(|style| {
            style.spacing.item_spacing = egui::vec2(12., 10.);
            style.spacing.interact_size.y = 34.;
            style.spacing.icon_width = 18.;
            style.spacing.button_padding = egui::vec2(14., 8.);
            style.visuals.widgets.inactive.weak_bg_fill = FIELD;
            style.visuals.widgets.hovered.weak_bg_fill = ACCENT_SOFT;
            style.visuals.widgets.active.weak_bg_fill = ACCENT_SOFT;
            style.visuals.widgets.inactive.rounding = egui::Rounding::same(8.);
            style.visuals.widgets.hovered.rounding = egui::Rounding::same(8.);
            style.visuals.widgets.active.rounding = egui::Rounding::same(8.);
            style
                .text_styles
                .insert(egui::TextStyle::Heading, FontId::new(32., Self::semibold()));
            style.text_styles.insert(
                egui::TextStyle::Body,
                FontId::new(17., FontFamily::Proportional),
            );
            style.text_styles.insert(
                egui::TextStyle::Button,
                FontId::new(17., FontFamily::Proportional),
            );
        });
        let picture = egui::ColorImage::from_rgba_unmultiplied(
            [64, 64],
            include_bytes!("../assets/comparator.rgba"),
        );
        let logo = ctx.load_texture("comparator-logo", picture, egui::TextureOptions::LINEAR);
        let (sender, receiver) = mpsc::channel();
        Self {
            language: Language::English,
            source: String::new(),
            destination: String::new(),
            mode: Mode::Changes,
            scan_mode: ScanMode::Smart,
            skip_matches: true,
            pair: None,
            plan: None,
            last_run: None,
            scanned_pair: None,
            scanned_plan: None,
            busy: None,
            status: String::new(),
            progress: 0.,
            error: None,
            page: Page::Folders,
            show_files: false,
            review_open: false,
            search: String::new(),
            confirm: false,
            mirror_phrase: String::new(),
            first_file: String::new(),
            second_file: String::new(),
            file_comparison: None,
            logs: Vec::new(),
            sender,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            logo,
        }
    }

    fn log(&mut self, message: impl Into<String>) {
        self.logs.push(message.into());
        if self.logs.len() > 500 {
            self.logs.drain(..self.logs.len() - 500);
        }
    }
    fn reset_plan(&mut self) {
        self.pair = None;
        self.plan = None;
        self.last_run = None;
        self.error = None;
        self.show_files = false;
        self.review_open = false;
    }
    fn replan(&mut self) {
        self.plan = self
            .pair
            .as_ref()
            .map(|pair| core::plan(pair, self.mode, self.skip_matches));
    }
    fn drain_events(&mut self) {
        let events: Vec<_> = self.receiver.try_iter().collect();
        for event in events {
            match event {
                WorkerEvent::Scanning(p) => {
                    self.status = format!(
                        "{} · {} {}",
                        if p.side == "source" {
                            t(self.language, "Source", "Источник")
                        } else {
                            t(self.language, "Destination", "Назначение")
                        },
                        p.processed_files,
                        t(self.language, "files", "файлов")
                    );
                    if p.hashed_bytes > 0 {
                        self.status.push_str(&format!(
                            " · {} {}",
                            core::format_bytes(p.hashed_bytes),
                            t(self.language, "checked", "прочитано")
                        ));
                    }
                    if !p.current.is_empty() {
                        self.status
                            .push_str(&format!(" · {}", Self::shorten(&p.current, 64)));
                    }
                }
                WorkerEvent::Scanned(result) => {
                    self.busy = None;
                    match result {
                        Ok(pair) => {
                            let warning_count =
                                pair.source.warnings.len() + pair.destination.warnings.len();
                            self.log(format!("Compared {} source files with {} destination files; {warning_count} warnings.",
                                pair.source.files.len(), pair.destination.files.len()));
                            for w in pair.source.warnings.iter().take(100) {
                                self.log(format!("Source warning: {w}"));
                            }
                            for w in pair.destination.warnings.iter().take(100) {
                                self.log(format!("Destination warning: {w}"));
                            }
                            self.pair = Some(pair);
                            self.replan();
                            self.review_open = true;
                            self.error = None;
                        }
                        Err(error) => {
                            self.log(format!("Scan error: {error}"));
                            self.error = Some(error);
                        }
                    }
                }
                WorkerEvent::Copying(p) => {
                    self.progress = if p.total_bytes != 0 {
                        (p.written as f64 / p.total_bytes as f64).clamp(0., 1.) as f32
                    } else if p.total != 0 {
                        (p.completed as f32 / p.total as f32).clamp(0., 1.)
                    } else {
                        1.
                    };
                    self.status = format!("{}/{} · {}", p.completed, p.total, p.current);
                }
                WorkerEvent::Copied(summary) => {
                    self.busy = None;
                    if summary.cancelled {
                        self.error = Some(
                            t(
                                self.language,
                                "Stopped. Files already copied were kept.",
                                "Остановлено. Уже скопированные файлы сохранены.",
                            )
                            .into(),
                        );
                    } else if !summary.failed.is_empty() {
                        self.error = Some(format!(
                            "{} {}",
                            summary.failed.len(),
                            t(
                                self.language,
                                "failed operations. See Activity.",
                                "ошибок. См. журнал."
                            )
                        ));
                    } else {
                        self.error = None;
                    }
                    self.log(format!(
                        "Finished: {} created, {} replaced, {} saved in recovery, {} failures.",
                        summary.created,
                        summary.replaced,
                        summary.moved_to_recovery,
                        summary.failed.len()
                    ));
                    if let Some(path) = &summary.recovery_path {
                        self.log(format!("Recovery folder: {}", path.display()));
                    }
                    for failure in &summary.failed {
                        self.log(format!("ERROR: {failure}"));
                    }
                    self.last_run = Some(summary);
                    self.review_open = true;
                    self.pair = None;
                    self.plan = None; // Never allow another run with a stale scan.
                    self.show_files = false;
                }
                WorkerEvent::Compared(result) => {
                    self.busy = None;
                    match result {
                        Ok(comparison) => {
                            self.file_comparison = Some(comparison);
                            self.error = None;
                        }
                        Err(error) => {
                            self.error = Some(error.clone());
                            self.log(error);
                        }
                    }
                }
            }
        }
    }
    fn scan(&mut self) {
        if self.busy.is_some() {
            return;
        }
        let source = PathBuf::from(&self.source);
        let destination = PathBuf::from(&self.destination);
        if let Err(error) = core::validate_roots(&source, &destination) {
            self.error = Some(error);
            return;
        }
        self.reset_plan();
        self.cancel = Arc::new(AtomicBool::new(false));
        self.busy = Some(Busy::Scan);
        self.status.clear();
        self.log(format!(
            "Checking {} -> {}",
            source.display(),
            destination.display()
        ));
        let tx = self.sender.clone();
        let cancel = self.cancel.clone();
        let scan_mode = self.scan_mode;
        thread::spawn(move || {
            let result =
                core::scan_pair_with_mode(&source, &destination, scan_mode, &cancel, |p| {
                    let _ = tx.send(WorkerEvent::Scanning(p));
                });
            let _ = tx.send(WorkerEvent::Scanned(result));
        });
    }
    fn copy(&mut self) {
        if self.busy.is_some() {
            return;
        }
        let (Some(pair), Some(plan)) = (self.pair.clone(), self.plan.clone()) else {
            return;
        };
        if !plan.permitted(&pair) {
            return;
        }
        self.scanned_pair = Some(pair.clone());
        self.scanned_plan = Some(plan.clone());
        self.cancel = Arc::new(AtomicBool::new(false));
        self.busy = Some(Busy::Copy);
        self.progress = 0.;
        self.status.clear();
        self.error = None;
        self.log(format!(
            "Approved {:?}: {} actions, {}",
            plan.mode,
            plan.actionable(),
            core::format_bytes(plan.bytes_to_copy)
        ));
        let tx = self.sender.clone();
        let cancel = self.cancel.clone();
        thread::spawn(move || {
            let summary = core::execute(&pair, &plan, &cancel, |p| {
                let _ = tx.send(WorkerEvent::Copying(p));
            });
            let _ = tx.send(WorkerEvent::Copied(summary));
        });
    }
    fn compare_files(&mut self) {
        if self.busy.is_some() {
            return;
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        self.busy = Some(Busy::Compare);
        self.status.clear();
        self.error = None;
        self.file_comparison = None;
        let a = PathBuf::from(&self.first_file);
        let b = PathBuf::from(&self.second_file);
        let tx = self.sender.clone();
        let cancel = self.cancel.clone();
        thread::spawn(move || {
            let _ = tx.send(WorkerEvent::Compared(core::compare_files(&a, &b, &cancel)));
        });
    }
    fn save_report(&mut self) {
        let report = serde_json::json!({
            "app": concat!("Comparator Studio ", env!("CARGO_PKG_VERSION")),
            "source": self.source,
            "destination": self.destination,
            "scan": self.pair.as_ref().or(self.scanned_pair.as_ref()),
            "plan": self.plan.as_ref().or(self.scanned_plan.as_ref()),
            "last_run": self.last_run,
            "events": self.logs,
        });
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("comparator-report.json")
            .add_filter("JSON", &["json"])
            .save_file()
        {
            match serde_json::to_string_pretty(&report)
                .map_err(|e| e.to_string())
                .and_then(|data| std::fs::write(&path, data).map_err(|e| e.to_string()))
            {
                Ok(()) => self.log(format!("Report saved: {}", path.display())),
                Err(e) => {
                    self.error = Some(e.clone());
                    self.log(e);
                }
            }
        }
    }

    fn semibold() -> FontFamily {
        FontFamily::Name("Geist Semibold".into())
    }

    fn primary(label: &str) -> egui::Button<'_> {
        egui::Button::new(RichText::new(label).family(Self::semibold()).color(WHITE))
            .fill(ACCENT)
            .stroke(Stroke::NONE)
            .min_size(Vec2::new(144., 46.))
    }

    fn surface<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
        egui::Frame::none()
            .fill(WHITE)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .rounding(egui::Rounding::same(14.))
            .inner_margin(egui::Margin::same(20.))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                add(ui)
            })
            .inner
    }

    fn shorten(path: &str, max: usize) -> String {
        if path.chars().count() <= max {
            return path.to_owned();
        }
        let chars: String = path.chars().rev().take(max.saturating_sub(1)).collect();
        format!("…{}", chars.chars().rev().collect::<String>())
    }

    fn path_field(
        ui: &mut Ui,
        lang: Language,
        label: &str,
        path: &mut String,
        enabled: bool,
    ) -> bool {
        let mut changed = false;
        ui.label(RichText::new(label).size(20.).family(Self::semibold()));
        ui.horizontal(|ui| {
            let browse = if path.is_empty() {
                t(lang, "Choose file…", "Выбрать файл…")
            } else {
                t(lang, "Change…", "Изменить…")
            };
            let button_width = 168.;
            let field_width =
                (ui.available_width() - button_width - ui.spacing().item_spacing.x).max(100.);
            changed = ui
                .add_enabled(
                    enabled,
                    egui::TextEdit::singleline(path)
                        .hint_text(t(lang, "No file selected", "Файл не выбран"))
                        .desired_width((field_width - 8.).max(0.))
                        .min_size(egui::vec2(0., 44.)),
                )
                .on_hover_text(path.as_str())
                .changed();
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(RichText::new(browse).family(Self::semibold()).color(ACCENT))
                        .fill(ACCENT_SOFT)
                        .min_size(egui::vec2(button_width, 44.)),
                )
                .clicked()
            {
                if let Some(selected) = rfd::FileDialog::new().pick_file() {
                    *path = selected.display().to_string();
                    changed = true;
                }
            }
        });
        changed
    }

    fn header(&mut self, ctx: &egui::Context) {
        let lang = self.language;
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::none()
                    .fill(HEADER)
                    .inner_margin(egui::Margin::symmetric(27., 12.)),
            )
            .show(ctx, |ui| {
                ui.visuals_mut().widgets.inactive.weak_bg_fill = WHITE;
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new(&self.logo).fit_to_exact_size(Vec2::splat(34.)));
                    ui.label(
                        RichText::new("Comparator Studio")
                            .color(WHITE)
                            .family(Self::semibold())
                            .size(19.),
                    );
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.add_enabled_ui(!self.confirm && self.busy.is_none(), |ui| {
                            if self.page != Page::Folders
                                && ui
                                    .button(t(lang, "Back to folders", "Назад к папкам"))
                                    .clicked()
                            {
                                self.page = Page::Folders;
                            }
                            ui.menu_button(t(lang, "Tools", "Инструменты"), |ui| {
                                for (page, label) in [
                                    (
                                        Page::Files,
                                        t(lang, "Compare two files", "Сравнить два файла"),
                                    ),
                                    (
                                        Page::Activity,
                                        t(lang, "Activity and recovery", "Журнал и восстановление"),
                                    ),
                                    (Page::Settings, t(lang, "Settings", "Настройки")),
                                ] {
                                    if ui.button(label).clicked() {
                                        self.page = page;
                                        ui.close_menu();
                                    }
                                }
                            });
                        });
                    });
                });
            });
    }

    fn folder_row(ui: &mut Ui, lang: Language, label: &str, path: &mut String, next: bool) -> bool {
        ui.label(RichText::new(label).family(Self::semibold()).size(20.));
        ui.horizontal(|ui| {
            let button_width = 174.;
            let input_padding = 12.;
            let field_width = ui.available_width() - button_width - ui.spacing().item_spacing.x;
            let mut changed = ui
                .add(
                    egui::TextEdit::singleline(path)
                        .hint_text(
                            RichText::new(t(lang, "No folder selected", "Папка не выбрана"))
                                .color(MUTED),
                        )
                        .margin(egui::Margin::symmetric(input_padding, 11.))
                        .desired_width((field_width - input_padding * 2.).max(0.))
                        .min_size(egui::vec2(0., 46.)),
                )
                .on_hover_text(path.as_str())
                .changed();

            let label = if path.is_empty() {
                t(lang, "Choose folder…", "Выбрать папку…")
            } else {
                t(lang, "Change folder…", "Изменить папку…")
            };
            let picker = if next {
                Self::primary(label)
            } else {
                egui::Button::new(RichText::new(label).color(ACCENT).family(Self::semibold()))
                    .fill(ACCENT_SOFT)
                    .stroke(Stroke::NONE)
            };
            let picker = picker.min_size(egui::vec2(button_width, 46.));
            if ui.add(picker).clicked() {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    *path = folder.display().to_string();
                    changed = true;
                }
            }
            changed
        })
        .inner
    }

    fn folders(&mut self, ui: &mut Ui) {
        let lang = self.language;
        if self.busy.is_some() {
            self.busy_state(ui);
            self.review_locations(ui);
            return;
        }
        if self.show_files {
            if let Some(plan) = &self.plan {
                Self::file_list(ui, lang, plan, &mut self.search);
            }
            return;
        }
        if self.review_open {
            self.review_locations(ui);
            self.results(ui);
            return;
        }
        Self::surface(ui, |ui| {
            let choose_source = self.source.is_empty();
            let source_changed = Self::folder_row(
                ui,
                lang,
                t(lang, "Copy from", "Копировать из"),
                &mut self.source,
                choose_source,
            );
            ui.add_space(6.);
            let choose_destination = !self.source.is_empty() && self.destination.is_empty();
            let destination_changed = Self::folder_row(
                ui,
                lang,
                t(lang, "Copy to", "Копировать в"),
                &mut self.destination,
                choose_destination,
            );
            if source_changed || destination_changed {
                self.reset_plan();
            }
            ui.add_space(6.);
            ui.separator();
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 46.),
                egui::Layout::right_to_left(Align::Center),
                |ui| {
                    let has_results = self.plan.is_some() || self.last_run.is_some();
                    let ready = !self.source.is_empty() && !self.destination.is_empty();
                    if has_results
                        && ui
                            .add(Self::primary(t(lang, "Back to results", "К результатам")))
                            .clicked()
                    {
                        self.review_open = true;
                    }
                    let compare = if has_results {
                        egui::Button::new(t(lang, "Compare again", "Сравнить снова"))
                            .min_size(egui::vec2(168., 46.))
                    } else if ready {
                        Self::primary(t(lang, "Compare folders", "Сравнить папки"))
                            .min_size(egui::vec2(168., 46.))
                    } else {
                        egui::Button::new(t(lang, "Compare folders", "Сравнить папки"))
                            .min_size(egui::vec2(168., 46.))
                    };
                    if ui.add_enabled(ready, compare).clicked() {
                        self.scan();
                    }
                    if self.mode == Mode::Mirror {
                        ui.label(
                            RichText::new(t(lang, "Mirror mode", "Зеркалирование")).color(RED),
                        );
                    }
                },
            );
        });
    }

    fn review_locations(&mut self, ui: &mut Ui) {
        let lang = self.language;
        Self::surface(ui, |ui| {
            ui.columns(2, |columns| {
                for (column, label, path) in [
                    (
                        0,
                        t(lang, "Copy from", "Копировать из"),
                        self.source.as_str(),
                    ),
                    (
                        1,
                        t(lang, "Copy to", "Копировать в"),
                        self.destination.as_str(),
                    ),
                ] {
                    columns[column]
                        .label(RichText::new(label).family(Self::semibold()).color(MUTED));
                    columns[column]
                        .add(egui::Label::new(path).truncate().selectable(true))
                        .on_hover_text(path);
                }
            });
            if self.busy.is_some() {
                return;
            }
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(t(lang, "Change folders", "Изменить папки"))
                    .clicked()
                {
                    self.review_open = false;
                }
                if ui
                    .button(t(lang, "Compare again", "Сравнить снова"))
                    .clicked()
                {
                    self.scan();
                }
            });
        });
    }

    fn scan_mode_name(lang: Language, mode: ScanMode) -> &'static str {
        match mode {
            ScanMode::Smart => t(lang, "Quick check", "Быстрая проверка"),
            ScanMode::Verified => t(lang, "Full verification", "Полная проверка"),
        }
    }

    fn busy_state(&mut self, ui: &mut Ui) {
        let lang = self.language;
        let busy = self.busy;
        let Some(busy) = busy else {
            return;
        };
        let stopping = self.cancel.load(Ordering::Relaxed);
        Self::surface(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(20.).color(ACCENT));
                let heading = if stopping {
                    t(lang, "Stopping…", "Остановка…")
                } else {
                    match busy {
                        Busy::Scan => t(lang, "Comparing folders…", "Сравниваем папки…"),
                        Busy::Copy => t(lang, "Copying files…", "Копируем файлы…"),
                        Busy::Compare => t(lang, "Comparing files…", "Сравниваем файлы…"),
                    }
                };
                ui.label(RichText::new(heading).family(Self::semibold()).size(18.));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(!stopping, egui::Button::new(t(lang, "Stop", "Остановить")))
                        .clicked()
                    {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                });
            });
            if busy == Busy::Copy {
                ui.add_space(6.);
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .fill(ACCENT)
                        .desired_width(ui.available_width()),
                );
            }
            if !self.status.is_empty() {
                ui.add(egui::Label::new(RichText::new(&self.status).color(MUTED)).truncate())
                    .on_hover_text(&self.status);
            }
        });
    }
    fn results(&mut self, ui: &mut Ui) {
        let lang = self.language;
        if let Some(run) = &self.last_run {
            ui.add_space(9.);
            Self::surface(ui, |ui| {
                ui.label(
                    RichText::new(if run.cancelled {
                        t(lang, "Copy stopped", "Копирование остановлено")
                    } else if run.failed.is_empty() {
                        t(lang, "Copy complete", "Копирование завершено")
                    } else {
                        t(
                            lang,
                            "Copy completed with errors",
                            "Копирование завершено с ошибками",
                        )
                    })
                    .size(18.)
                    .strong(),
                );
                ui.label(format!(
                    "{} {} · {}",
                    run.created + run.replaced,
                    t(lang, "files copied", "файлов скопировано"),
                    core::format_bytes(run.copied_bytes)
                ));
                if let Some(path) = &run.recovery_path {
                    ui.label(t(lang, "Recovery folder", "Папка восстановления"));
                    ui.add(
                        egui::Label::new(RichText::new(path.display().to_string()).monospace())
                            .selectable(true),
                    );
                }
            });
            return;
        }
        let (Some(plan), Some(pair)) = (&self.plan, &self.pair) else {
            return;
        };
        let warnings = pair.source.warnings.len() + pair.destination.warnings.len();
        let can_apply = plan.actionable() != 0 && plan.permitted(pair);
        let mut choose_partial = false;
        let mut review = false;
        ui.add_space(9.);
        Self::surface(ui, |ui| {
            let stats = [
                (plan.new_count, t(lang, "New files", "Новые файлы")),
                (plan.replace_count, t(lang, "To replace", "К замене")),
                (plan.same_count, t(lang, "Unchanged", "Без изменений")),
                (
                    plan.target_only,
                    t(lang, "Destination only", "Только в назначении"),
                ),
            ];
            ui.columns(4, |columns| {
                for (column, (count, label)) in columns.iter_mut().zip(stats) {
                    column.label(
                        RichText::new(count.to_string())
                            .size(27.)
                            .family(Self::semibold()),
                    );
                    column.label(RichText::new(label).size(15.).color(MUTED));
                }
            });
            ui.add_space(4.);
            for (count, detail) in [
                (
                    plan.matching_elsewhere,
                    t(
                        lang,
                        "content matches elsewhere",
                        "совпадений в других папках",
                    ),
                ),
                (
                    plan.metadata_matches,
                    t(
                        lang,
                        "matches based on size and date",
                        "совпадений по размеру и дате",
                    ),
                ),
                (
                    plan.new_folders,
                    t(lang, "folders to create", "папок к созданию"),
                ),
                (
                    plan.old_folders,
                    t(
                        lang,
                        "empty destination folders to remove",
                        "пустых папок к удалению",
                    ),
                ),
                (
                    plan.quarantine_count,
                    t(
                        lang,
                        "files to move to recovery",
                        "файлов к переносу в резерв",
                    ),
                ),
                (
                    plan.no_space_count,
                    t(
                        lang,
                        "files skipped for lack of space",
                        "файлов пропущено из-за нехватки места",
                    ),
                ),
                (
                    warnings,
                    t(
                        lang,
                        "scan warnings. See Activity.",
                        "предупреждений. См. журнал.",
                    ),
                ),
            ] {
                if count > 0 {
                    ui.label(RichText::new(format!("{count} {detail}")).color(MUTED));
                }
            }
            if plan.space_blocked {
                ui.label(
                    RichText::new(t(lang, "Insufficient disk space", "Недостаточно места"))
                        .color(RED),
                );
                if let Some(free) = plan.available_bytes {
                    ui.label(format!(
                        "{} {} · {} {}",
                        core::format_bytes(plan.required_bytes_before_partial),
                        t(lang, "required", "требуется"),
                        core::format_bytes(free),
                        t(lang, "free before reserve", "свободно до резерва")
                    ));
                }
                if plan.mode != Mode::Mirror {
                    choose_partial = ui
                        .button(t(lang, "Copy what fits", "Копировать по месту"))
                        .clicked();
                }
            } else if !plan.permitted(pair) {
                ui.label(
                    RichText::new(t(
                        lang,
                        "Mirroring requires a complete scan.",
                        "Для зеркалирования нужна полная проверка.",
                    ))
                    .color(RED),
                );
            } else if plan.actionable() == 0 {
                ui.label(t(lang, "No pending changes", "Нет изменений"));
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(t(lang, "Show files", "Показать файлы")).clicked() {
                    self.show_files = true;
                    self.search.clear();
                }
                if can_apply {
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        review = ui
                            .add(Self::primary(if plan.mode == Mode::Mirror {
                                t(lang, "Review mirror", "Проверить зеркало")
                            } else {
                                t(lang, "Review and copy", "Проверить и копировать")
                            }))
                            .clicked();
                        ui.label(
                            RichText::new(core::format_bytes(plan.bytes_to_copy)).color(MUTED),
                        );
                    });
                }
            });
        });
        if choose_partial {
            self.mode = Mode::Partial;
            self.replan();
        }
        if review {
            self.confirm = true;
            self.mirror_phrase.clear();
        }
    }

    fn settings(&mut self, ui: &mut Ui) {
        let lang = self.language;
        ui.label(
            RichText::new(t(lang, "Copy mode", "Режим копирования"))
                .strong()
                .size(17.),
        );
        let mut changed = ui
            .radio_value(
                &mut self.mode,
                Mode::Changes,
                t(
                    lang,
                    "Copy new and changed files",
                    "Копировать новые и изменённые файлы",
                ),
            )
            .changed();
        changed |= ui
            .radio_value(
                &mut self.mode,
                Mode::Partial,
                t(lang, "Copy what fits", "Копировать по месту"),
            )
            .changed();
        changed |= ui
            .radio_value(
                &mut self.mode,
                Mode::Mirror,
                t(lang, "Mirror source", "Зеркалировать источник"),
            )
            .changed();
        if self.mode == Mode::Mirror {
            ui.label(
                RichText::new(t(
                    lang,
                    "Extra destination files move to the recovery folder.",
                    "Лишние файлы назначения перемещаются в папку восстановления.",
                ))
                .color(RED),
            );
        }
        let skip = ui.add_enabled(
            self.mode != Mode::Mirror,
            egui::Checkbox::new(
                &mut self.skip_matches,
                t(
                    lang,
                    "Skip matching content at other paths",
                    "Пропускать совпадения по другим путям",
                ),
            ),
        );
        if changed || skip.changed() {
            self.replan();
        }
        ui.separator();
        ui.label(RichText::new(t(lang, "Verification", "Проверка")).family(Self::semibold()));
        let mut scan_mode = self.scan_mode;
        for mode in [ScanMode::Smart, ScanMode::Verified] {
            ui.radio_value(&mut scan_mode, mode, Self::scan_mode_name(lang, mode));
        }
        ui.label(
            RichText::new(t(
                lang,
                "Quick check can match files by size and date. Full verification reads every file.",
                "Быстрая проверка учитывает размер и дату. Полная проверка читает все файлы.",
            ))
            .color(MUTED),
        );
        if scan_mode != self.scan_mode {
            self.scan_mode = scan_mode;
            self.reset_plan();
        }
        ui.separator();
        ui.label(
            RichText::new(t(lang, "Language", "Язык"))
                .strong()
                .size(17.),
        );
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.language, Language::English, "English");
            ui.selectable_value(&mut self.language, Language::Russian, "Русский");
        });
    }

    fn action_name(lang: Language, a: Action) -> &'static str {
        match a {
            Action::CopyNew => t(lang, "Copy", "Копировать"),
            Action::Replace => t(lang, "Update", "Обновить"),
            Action::Identical => t(lang, "Unchanged", "Без изменений"),
            Action::FoundElsewhere => t(lang, "Same content found", "Найдено совпадение"),
            Action::KeepTarget => t(lang, "Keep", "Оставить"),
            Action::Quarantine => t(lang, "Save in recovery", "В резерв"),
            Action::NoSpace => t(lang, "Won't fit", "Не поместится"),
        }
    }
    fn file_list(ui: &mut Ui, lang: Language, plan: &Plan, search: &mut String) {
        Self::surface(ui, |ui| {
            let search_changed = ui
                .add(
                    egui::TextEdit::singleline(search)
                        .desired_width((ui.available_width() - 20.).max(0.))
                        .margin(egui::Margin::symmetric(10., 8.))
                        .hint_text(t(
                            lang,
                            "Search files or folders…",
                            "Поиск по файлам и папкам…",
                        )),
                )
                .changed();
            let query = search.to_lowercase();
            const LIMIT: usize = 2000;
            let entries: Vec<_> = plan
                .entries
                .iter()
                .filter(|entry| {
                    query.is_empty()
                        || entry
                            .relative
                            .to_string_lossy()
                            .to_lowercase()
                            .contains(&query)
                })
                .take(LIMIT + 1)
                .collect();
            let visible = entries.len().min(LIMIT);
            if visible == 0 {
                ui.label(t(lang, "No matching files", "Совпадений нет"));
                return;
            }
            let count = if entries.len() > LIMIT {
                t(
                    lang,
                    "First 2,000 matches. Search to narrow the list.",
                    "Первые 2000 совпадений. Уточните поиск.",
                )
                .to_owned()
            } else {
                format!("{}  {visible}", t(lang, "Files shown", "Файлов показано"))
            };
            ui.label(RichText::new(count).size(14.).color(MUTED));

            ui.spacing_mut().scroll.floating = false;
            ui.spacing_mut().scroll.bar_width = 8.;
            ui.spacing_mut().scroll.bar_inner_margin = 4.;
            ui.spacing_mut().scroll.bar_outer_margin = 0.;
            let table_width = (ui.available_width() - 12.).max(0.);
            let action_width = 190.;
            let file_width = (table_width - action_width).max(0.);
            ui.allocate_ui_with_layout(
                egui::vec2(table_width, 29.),
                egui::Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 0.;
                    for (width, label) in [
                        (file_width, t(lang, "File", "Файл")),
                        (action_width, t(lang, "Action", "Действие")),
                    ] {
                        ui.allocate_ui_with_layout(
                            egui::vec2(width, 29.),
                            egui::Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.set_min_width(width);
                                ui.add_space(10.);
                                ui.label(
                                    RichText::new(label).family(Self::semibold()).color(MUTED),
                                );
                            },
                        );
                    }
                },
            );
            ui.separator();
            let height = (ui.clip_rect().bottom() - ui.cursor().top() - 22.).max(125.);
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt("review_file_rows")
                .max_height(height)
                .auto_shrink([false, true]);
            if search_changed {
                scroll = scroll.vertical_scroll_offset(0.);
            }
            ui.spacing_mut().item_spacing.y = 4.;
            const ROW_HEIGHT: f32 = 49.;
            scroll.show_rows(ui, ROW_HEIGHT, visible, |ui, range| {
                for index in range {
                    let entry = entries[index];
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(table_width, ROW_HEIGHT),
                        egui::Sense::hover(),
                    );
                    if index % 2 == 0 {
                        ui.painter().rect_filled(rect, 4., FIELD);
                    }
                    let file_rect = egui::Rect::from_min_max(
                        rect.min,
                        egui::pos2(rect.left() + file_width, rect.bottom()),
                    );
                    let path = entry.relative.to_string_lossy();
                    let file = entry
                        .relative
                        .file_name()
                        .map(|name| name.to_string_lossy());
                    let mut file_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt(("file", index))
                            .max_rect(file_rect.shrink2(egui::vec2(10., 5.)))
                            .layout(egui::Layout::top_down(Align::Min)),
                    );
                    {
                        let ui = &mut file_ui;
                        ui.spacing_mut().item_spacing.y = 0.;
                        let label = ui.add(
                            egui::Label::new(
                                RichText::new(file.as_deref().unwrap_or(&path))
                                    .family(Self::semibold()),
                            )
                            .truncate()
                            .sense(egui::Sense::click()),
                        );
                        label.on_hover_text(path.as_ref()).context_menu(|ui| {
                            if ui
                                .button(t(
                                    lang,
                                    "Copy relative path",
                                    "Копировать относительный путь",
                                ))
                                .clicked()
                            {
                                ui.ctx().copy_text(path.to_string());
                                ui.close_menu();
                            }
                        });
                        if let Some(parent) = entry
                            .relative
                            .parent()
                            .filter(|p| !p.as_os_str().is_empty())
                        {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(parent.to_string_lossy())
                                        .size(14.)
                                        .color(MUTED),
                                )
                                .truncate(),
                            )
                            .on_hover_text(path.as_ref());
                        }
                    }
                    let mut action_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt(("action", index))
                            .max_rect(
                                egui::Rect::from_min_max(
                                    egui::pos2(file_rect.right(), rect.top()),
                                    rect.max,
                                )
                                .shrink2(egui::vec2(10., 4.)),
                            )
                            .layout(egui::Layout::left_to_right(Align::Center)),
                    );
                    {
                        let ui = &mut action_ui;
                        let color = match entry.action {
                            Action::CopyNew | Action::Replace => ACCENT,
                            Action::Quarantine | Action::NoSpace => RED,
                            _ => MUTED,
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(Self::action_name(lang, entry.action))
                                    .color(color)
                                    .size(16.),
                            )
                            .truncate(),
                        );
                    }
                }
            });
        });
    }
    fn file_comparison_ui(&mut self, ui: &mut Ui) {
        let lang = self.language;
        let enabled = self.busy.is_none();
        let first = Self::path_field(
            ui,
            lang,
            t(lang, "First file", "Первый файл"),
            &mut self.first_file,
            enabled,
        );
        let second = Self::path_field(
            ui,
            lang,
            t(lang, "Second file", "Второй файл"),
            &mut self.second_file,
            enabled,
        );
        if first || second {
            self.file_comparison = None;
            self.error = None;
        }
        if ui
            .add_enabled(
                enabled && !self.first_file.is_empty() && !self.second_file.is_empty(),
                Self::primary(t(lang, "Compare", "Сравнить")),
            )
            .clicked()
        {
            self.compare_files();
        }
        if let Some(result) = &self.file_comparison {
            ui.label(
                RichText::new(if result.identical {
                    t(lang, "Identical content", "Содержимое совпадает")
                } else {
                    t(lang, "Different content", "Содержимое отличается")
                })
                .size(17.)
                .strong()
                .color(if result.identical { GREEN } else { RED }),
            );
            egui::ScrollArea::vertical()
                .max_height(180.)
                .show(ui, |ui| {
                    ui.label(RichText::new(&result.detail).monospace().size(12.));
                });
        }
    }
    fn activity_ui(&mut self, ui: &mut Ui) {
        let lang = self.language;
        ui.add_space(7.);
        if ui
            .button(t(lang, "Export report…", "Экспорт отчёта…"))
            .clicked()
        {
            self.save_report();
        }
        egui::ScrollArea::vertical()
            .max_height(420.)
            .show(ui, |ui| {
                if self.logs.is_empty() {
                    ui.label(t(lang, "No activity yet", "Журнал пуст"));
                }
                for log in self.logs.iter().rev().take(120) {
                    ui.label(RichText::new(log).color(MUTED).size(12.));
                }
            });
    }
    fn confirmation(&mut self, ctx: &egui::Context) {
        if !self.confirm {
            return;
        }
        let lang = self.language;
        let Some(plan) = self.plan.as_ref() else {
            self.confirm = false;
            return;
        };
        let mode = plan.mode;
        let count = plan.new_count + plan.replace_count;
        let recovered = plan.quarantine_count;
        let new_folders = plan.new_folders;
        let old_folders = plan.old_folders;
        let replaced = plan.replace_count;
        let copied = core::format_bytes(plan.bytes_to_copy);
        egui::Window::new(t(lang, "Confirm changes", "Подтвердить изменения"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .title_bar(false)
            .resizable(false)
            .min_width(424.)
            .max_width(424.)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(WHITE)
                    .rounding(egui::Rounding::same(14.))
                    .inner_margin(egui::Margin::same(22.)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(t(lang, "Confirm changes", "Подтвердить изменения"))
                        .family(Self::semibold())
                        .size(25.),
                );
                ui.label(
                    RichText::new(format!(
                        "{}  {} · {}",
                        t(lang, "Files to copy", "Файлов к копированию"),
                        count,
                        copied
                    ))
                    .color(MUTED),
                );
                ui.add_space(5.);
                ui.label(RichText::new(t(lang, "Copy to", "Копировать в")).color(MUTED));
                ui.add(egui::Label::new(&self.destination).wrap().selectable(true));
                ui.add_space(4.);
                for (count, label) in [
                    (
                        replaced,
                        t(
                            lang,
                            "files to replace and back up",
                            "файлов к замене с резервной копией",
                        ),
                    ),
                    (
                        recovered,
                        t(
                            lang,
                            "extra files to move to recovery",
                            "лишних файлов к переносу в резерв",
                        ),
                    ),
                    (
                        new_folders,
                        t(lang, "folders to create", "папок к созданию"),
                    ),
                    (
                        old_folders,
                        t(lang, "empty folders to remove", "пустых папок к удалению"),
                    ),
                ] {
                    if count > 0 {
                        ui.label(format!("{count} {label}"));
                    }
                }
                if mode == Mode::Mirror {
                    ui.label(t(
                        lang,
                        "Type MIRROR to continue.",
                        "Введите MIRROR для подтверждения.",
                    ));
                    ui.add(egui::TextEdit::singleline(&mut self.mirror_phrase).desired_width(180.));
                }
                ui.add_space(8.);
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 46.),
                    egui::Layout::right_to_left(Align::Center),
                    |ui| {
                        let allowed = mode != Mode::Mirror || self.mirror_phrase == "MIRROR";
                        if ui
                            .add_enabled(
                                allowed,
                                Self::primary(if mode == Mode::Mirror {
                                    t(lang, "Apply mirror changes", "Применить зеркалирование")
                                } else {
                                    t(lang, "Start copying", "Начать копирование")
                                }),
                            )
                            .clicked()
                        {
                            self.confirm = false;
                            self.copy();
                        }
                        if ui
                            .add(
                                egui::Button::new(t(lang, "Cancel", "Отмена"))
                                    .min_size(egui::vec2(90., 46.)),
                            )
                            .clicked()
                        {
                            self.confirm = false;
                        }
                    },
                );
            });
    }
}
impl eframe::App for ComparatorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let previously_reviewing = self.review_open;
        self.drain_events();
        let previous_page = self.page;
        self.header(ctx);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(28., 24.)),
            )
            .show(ctx, |ui| {
                ui.add_enabled_ui(!self.confirm, |ui| {
                    let mut scroll = egui::ScrollArea::vertical()
                        .id_salt((self.page as u8, self.show_files))
                        .animated(false)
                        .auto_shrink([false, false]);
                    if self.page != previous_page || self.review_open != previously_reviewing {
                        scroll = scroll.vertical_scroll_offset(0.);
                    }
                    scroll.show(ui, |ui| {
                        let width = ui.available_width().min(760.);
                        ui.horizontal_top(|ui| {
                            ui.add_space(
                                ((ui.available_width() - width) / 2. - ui.spacing().item_spacing.x)
                                    .max(0.),
                            );
                            ui.vertical(|ui| {
                                ui.set_width(width);
                                let lang = self.language;
                                let heading = match self.page {
                                    Page::Folders if self.show_files => t(lang, "Files", "Файлы"),
                                    Page::Folders if self.review_open && self.busy.is_none() => {
                                        if self.last_run.is_some() {
                                            t(lang, "Copy results", "Результаты копирования")
                                        } else {
                                            t(lang, "Review changes", "Проверка изменений")
                                        }
                                    }
                                    Page::Folders => t(lang, "Copy files", "Копирование файлов"),
                                    Page::Files => t(lang, "Compare files", "Сравнение файлов"),
                                    Page::Activity => t(lang, "Activity", "Журнал"),
                                    Page::Settings => t(lang, "Settings", "Настройки"),
                                };
                                ui.horizontal(|ui| {
                                    ui.heading(heading);
                                    if self.page == Page::Folders && self.show_files {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if ui
                                                    .button(t(
                                                        lang,
                                                        "Back to results",
                                                        "К результатам",
                                                    ))
                                                    .clicked()
                                                {
                                                    self.show_files = false;
                                                }
                                            },
                                        );
                                    }
                                });
                                ui.add_space(7.);
                                match self.page {
                                    Page::Folders => self.folders(ui),
                                    Page::Files => {
                                        Self::surface(ui, |ui| self.file_comparison_ui(ui))
                                    }
                                    Page::Activity => Self::surface(ui, |ui| self.activity_ui(ui)),
                                    Page::Settings => Self::surface(ui, |ui| self.settings(ui)),
                                }
                                if self.page == Page::Files {
                                    self.busy_state(ui);
                                }
                                if let Some(error) = &self.error {
                                    ui.label(RichText::new(error).color(RED));
                                }
                            });
                        });
                    });
                });
            });
        if self.confirm && ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.confirm = false;
        }
        self.confirmation(ctx);
        if self.busy.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}
