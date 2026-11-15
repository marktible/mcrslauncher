use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, Stroke, Vec2,
};
use mcrslauncher::{
    engine::{self, Output},
    github::{self, Catalog},
    model::{
        Account, BuildProfile, Channel, Paths, Receipt, Settings, Snapshot, UpdateCheckInterval,
    },
    profiles::{ProfileCommand, ProfileEvent, ProfileWorker, SkinFile, SkinModel},
    progress::{TransferProgress, format_duration},
    updater::{self, LauncherRelease, UpdateOutcome},
    worker::{Action, Event, Worker},
};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant, SystemTime},
};

const BG: Color32 = Color32::from_rgb(18, 21, 22);
const PANEL: Color32 = Color32::from_rgb(23, 27, 28);
const CARD: Color32 = Color32::from_rgb(28, 33, 33);
const LINE: Color32 = Color32::from_rgb(44, 51, 50);
const TEXT: Color32 = Color32::from_rgb(236, 239, 232);
const MUTED: Color32 = Color32::from_rgb(155, 168, 161);
const ACCENT: Color32 = Color32::from_rgb(238, 166, 111);
const GREEN: Color32 = Color32::from_rgb(160, 195, 153);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Play,
    Mods,
    Updates,
    Settings,
}

impl Page {
    const ALL: [Self; 4] = [Self::Play, Self::Mods, Self::Updates, Self::Settings];

    fn label(self) -> &'static str {
        match self {
            Self::Play => "Play",
            Self::Mods => "Mods",
            Self::Updates => "Updates",
            Self::Settings => "Settings",
        }
    }
}

enum Confirm {
    UpdateMods,
    Remove(String),
}

#[derive(Clone, PartialEq)]
enum AccountChoice {
    Guest,
    Offline(String),
    Microsoft(String),
}

struct SkinEditor {
    account: Account,
    model: SkinModel,
    skin: Option<SkinFile>,
    preview: Option<egui::TextureHandle>,
    choosing: bool,
    uploading: bool,
}

pub struct Launcher {
    worker: Worker,
    profiles: ProfileWorker,
    heads: HashMap<String, egui::TextureHandle>,
    skin_editor: Option<SkinEditor>,
    paths: Paths,
    settings: Settings,
    snapshot: Snapshot,
    receipt: Receipt,
    catalog: Option<Catalog>,
    catalog_channel: Channel,
    update_unavailable: Option<String>,
    launcher_release: Option<LauncherRelease>,
    launcher_update: Option<UpdateOutcome>,
    page: Page,
    busy: bool,
    stage: String,
    progress: Option<TransferProgress>,
    job_started: Option<Instant>,
    last_activity: String,
    error: Option<String>,
    notice: String,
    login: Option<(String, String)>,
    mod_url: String,
    trust_mod: bool,
    confirm: Option<Confirm>,
    logs: VecDeque<String>,
    show_logs: bool,
    pending_check: bool,
    checked_at: Option<SystemTime>,
    last_check: Instant,
    playing: bool,
    new_offline_account: bool,
    offline_account_name: String,
    offline_account_error: Option<String>,
    pending_account: Option<String>,
}

impl Launcher {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_page: &str) -> Self {
        configure(&cc.egui_ctx);
        let paths = Paths::discover().unwrap_or_else(|_| Paths {
            root: std::env::temp_dir().join("mcrslauncher"),
        });
        let (settings, error) = match paths.load_settings() {
            Ok(settings) => (settings, None),
            Err(error) => (
                Settings::default(),
                Some(format!("Settings could not be loaded: {error:#}")),
            ),
        };
        let pending_check = settings.auto_check && !settings.offline_mode;
        let mut app = Self {
            worker: Worker::new(paths.clone(), cc.egui_ctx.clone()),
            profiles: ProfileWorker::new(paths.clone(), cc.egui_ctx.clone()),
            heads: HashMap::new(),
            skin_editor: None,
            paths,
            catalog_channel: settings.channel,
            update_unavailable: None,
            launcher_release: None,
            launcher_update: None,
            settings,
            snapshot: Snapshot::default(),
            receipt: Receipt::default(),
            catalog: None,
            page: match initial_page {
                "mods" => Page::Mods,
                "updates" => Page::Updates,
                "settings" => Page::Settings,
                _ => Page::Play,
            },
            busy: false,
            stage: String::new(),
            progress: None,
            job_started: None,
            last_activity: String::new(),
            error: None,
            notice: String::new(),
            login: None,
            mod_url: String::new(),
            trust_mod: false,
            confirm: None,
            logs: VecDeque::with_capacity(500),
            show_logs: false,
            pending_check,
            checked_at: None,
            last_check: Instant::now(),
            playing: false,
            new_offline_account: false,
            offline_account_name: String::new(),
            offline_account_error: None,
            pending_account: None,
        };
        app.start(Action::Refresh);
        app.error = error;
        app
    }

    fn start(&mut self, action: Action) {
        if self.busy {
            return;
        }
        let label = action.label().to_string();
        if matches!(action, Action::CheckLauncherUpdate) {
            self.launcher_release = None;
        }
        let mut settings = self.settings.clone();
        // Setup is an explicit download request, including for a locally selected
        // offline profile. Keep the saved offline-play preference unchanged.
        if matches!(action, Action::Setup) {
            settings.offline_mode = false;
        }
        match self.worker.start(action, settings) {
            Ok(()) => {
                self.busy = true;
                self.stage = label;
                self.progress = None;
                self.job_started = Some(Instant::now());
                self.last_activity.clear();
                self.error = None;
                self.notice.clear();
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        // Bound work per frame even if a compiler floods the output pipe.
        for index in 0..128 {
            let Ok(event) = self.worker.events.try_recv() else {
                break;
            };
            match event {
                Event::Output(Output::Log(line)) => {
                    self.last_activity = line.trim().to_string();
                    if self.logs.len() == 500 {
                        self.logs.pop_front();
                    }
                    self.logs.push_back(line);
                }
                Event::Output(Output::Stage(stage)) => {
                    self.stage = stage;
                    self.progress = None;
                    self.last_activity.clear();
                }
                Event::Output(Output::Progress(progress)) => self.progress = Some(progress),
                Event::Output(Output::Login { url, code }) => {
                    if let Err(error) = open::that(&url) {
                        self.error = Some(format!("Open the sign-in link below: {error}"));
                    }
                    self.login = Some((url, code));
                }
                Event::Output(Output::Playing) => {
                    self.playing = true;
                    if self.settings.minimize_on_play {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                }
                Event::Snapshot(snapshot) => {
                    if self.pending_account.as_ref().is_some_and(|uuid| {
                        snapshot
                            .accounts
                            .iter()
                            .any(|account| account.selected && &account.uuid == uuid)
                    }) {
                        self.pending_account = None;
                        let mut settings = self.settings.clone();
                        settings.offline_mode = false;
                        self.save_account_settings(settings);
                    }
                    let mut accounts = snapshot.accounts.clone();
                    accounts.sort_by_key(|account| !account.selected);
                    let _ = self
                        .profiles
                        .send(ProfileCommand::Load(accounts, self.settings.offline_mode));
                    self.snapshot = snapshot;
                }
                Event::Catalog(catalog) => {
                    self.update_unavailable = None;
                    self.catalog = Some(*catalog);
                    self.catalog_channel = self.settings.channel;
                    self.checked_at = Some(SystemTime::now());
                    self.last_check = Instant::now();
                }
                Event::UpdateUnavailable(message) => self.update_unavailable = Some(message),
                Event::LauncherRelease(release) => self.launcher_release = Some(release),
                Event::LauncherUpdated(outcome) => self.launcher_update = Some(outcome),
                Event::Receipt(receipt) => self.receipt = receipt,
                Event::Done(result) => {
                    self.busy = false;
                    self.login = None;
                    self.progress = None;
                    self.job_started = None;
                    if self.playing {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        self.playing = false;
                    }
                    match result {
                        Ok(message) => self.notice = message,
                        Err(error) => {
                            self.pending_account = None;
                            if self.logs.len() >= 500 {
                                self.logs.pop_front();
                            }
                            self.logs.push_back(error.clone());
                            self.error = Some(error);
                        }
                    }
                }
            }
            if index == 127 {
                ctx.request_repaint();
            }
        }
        for _ in 0..16 {
            let Ok(event) = self.profiles.events.try_recv() else {
                break;
            };
            match event {
                ProfileEvent::Head { uuid, rgba } => {
                    let image = egui::ColorImage::from_rgba_unmultiplied([8, 8], &rgba);
                    self.heads.insert(
                        uuid.clone(),
                        ctx.load_texture(
                            format!("profile-{uuid}"),
                            image,
                            egui::TextureOptions::NEAREST,
                        ),
                    );
                }
                ProfileEvent::Chosen { uuid, skin } => {
                    if let Some(editor) = &mut self.skin_editor
                        && editor.account.uuid == uuid
                    {
                        editor.choosing = false;
                        if let Some(skin) = skin {
                            editor.preview = Some(ctx.load_texture(
                                "skin-preview",
                                egui::ColorImage::from_rgba_unmultiplied([8, 8], &skin.head),
                                egui::TextureOptions::NEAREST,
                            ));
                            editor.skin = Some(skin);
                        }
                    }
                }
                ProfileEvent::Uploaded { uuid } => {
                    if self
                        .skin_editor
                        .as_ref()
                        .is_some_and(|e| e.account.uuid == uuid)
                    {
                        self.skin_editor = None;
                    }
                    self.notice = "Minecraft skin updated. New game sessions will use it.".into();
                    if !self.busy {
                        self.start(Action::Refresh);
                    }
                }
                ProfileEvent::Failed { message } => {
                    if let Some(editor) = &mut self.skin_editor {
                        editor.choosing = false;
                        editor.uploading = false;
                    }
                    self.error = Some(message);
                }
            }
        }
        if !self.busy
            && !self.settings.offline_mode
            && self.settings.auto_check
            && (self.pending_check
                || self
                    .settings
                    .automatic_update_interval()
                    .is_some_and(|interval| self.last_check.elapsed() >= interval))
        {
            self.pending_check = false;
            self.last_check = Instant::now();
            self.start(Action::CheckUpdates);
        }
        if let Some(interval) = self.settings.automatic_update_interval() {
            ctx.request_repaint_after(
                interval
                    .saturating_sub(self.last_check.elapsed())
                    .max(Duration::from_secs(1)),
            );
        }
    }

    fn save(&mut self) {
        if let Err(error) = self.paths.save_settings(&self.settings) {
            self.error = Some(format!("Could not save settings: {error:#}"));
        }
    }

    fn open(&mut self, target: impl AsRef<std::ffi::OsStr>) {
        if let Err(error) = open::that(target) {
            self.error = Some(format!("Could not open: {error}"));
        }
    }

    fn engine_ready(&self) -> bool {
        !self.receipt.executable_sha256.is_empty()
    }
    fn has_update(&self) -> bool {
        self.catalog.as_ref().is_some_and(|c| {
            c.game_version().ok().as_deref() != Some(self.receipt.game_tag.as_str())
                || engine::game_fingerprint(c) != self.receipt.game_digest
                || c.engine.tag_name != self.receipt.tag
                || c.engine_asset.digest.as_deref() != Some(self.receipt.digest.as_str())
        })
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        if ctx.content_rect().width() < 900. {
            egui::TopBottomPanel::top("compact_navigation")
                .frame(
                    Frame::new()
                        .fill(PANEL)
                        .inner_margin(Margin::symmetric(12, 10)),
                )
                .show(ctx, |ui| {
                    ui.spacing_mut().item_spacing.x = 5.;
                    ui.horizontal_wrapped(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(26., 30.), egui::Sense::hover());
                        cube(
                            ui.painter(),
                            rect.center() - egui::vec2(0., 5.),
                            10.,
                            ACCENT,
                        );
                        ui.label(RichText::new("MCRS").size(19.).strong());
                        ui.add_space(8.);
                        for page in Page::ALL {
                            if ui
                                .selectable_label(self.page == page, page.label())
                                .clicked()
                            {
                                self.page = page;
                            }
                        }
                    });
                });
            return;
        }
        egui::SidePanel::left("navigation")
            .default_width(200.)
            .width_range(180. ..=250.)
            .resizable(true)
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(16, 26)),
            )
            .show(ctx, |ui| {
                let short = ui.available_height() < 540.;
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(30.), egui::Sense::hover());
                    cube(ui.painter(), r.center() - egui::vec2(0., 4.), 13., ACCENT);
                    ui.label(RichText::new("MCRS").size(23.).strong().color(TEXT));
                });
                ui.add_space(3.);
                ui.label(RichText::new("Horizon Launcher").size(11.).color(MUTED));
                ui.add_space(if short { 18. } else { 36. });
                for page in Page::ALL {
                    if navigation_item(ui, page, self.page == page).clicked() {
                        self.page = page;
                    }
                    ui.add_space(2.);
                }
                if ui.available_height() >= if short { 64. } else { 150. } {
                    ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                        ui.label(
                            RichText::new(concat!("MCRS v", env!("CARGO_PKG_VERSION")))
                                .size(11.)
                                .color(MUTED),
                        );
                        ui.add_space(8.);
                        if ui
                            .link(RichText::new("Horizon on GitHub ↗").size(12.).color(MUTED))
                            .clicked()
                        {
                            self.open(github::REPOSITORY);
                        }
                        if !short {
                            ui.add_space(18.);
                            ui.separator();
                            ui.add_space(10.);
                            ui.label(
                                RichText::new("A little lighter.\nA new horizon.")
                                    .size(12.)
                                    .color(MUTED),
                            );
                        }
                    });
                }
            });
    }

    fn footer(&mut self, ctx: &egui::Context) {
        let working = self.busy && !self.playing;
        egui::TopBottomPanel::bottom("status")
            .exact_height(if working { 95. } else { 45. })
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(20, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    dot(ui, if self.busy { ACCENT } else { GREEN });
                    let text = if self.busy {
                        self.stage.as_str()
                    } else if !self.notice.is_empty() {
                        &self.notice
                    } else {
                        "Ready"
                    };
                    ui.allocate_ui_with_layout(
                        egui::vec2(
                            (ui.available_width() - if self.busy { 192. } else { 110. }).max(0.),
                            18.,
                        ),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(text).size(11.).color(MUTED))
                                    .truncate(),
                            );
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button(if self.show_logs {
                                "Hide activity"
                            } else {
                                "Activity log"
                            })
                            .clicked()
                        {
                            self.show_logs = !self.show_logs;
                        }
                        if self.busy
                            && ui
                                .small_button(if self.playing { "Stop game" } else { "Cancel" })
                                .clicked()
                        {
                            self.worker.cancel();
                        }
                    });
                });
                if working {
                    if let Some(progress) = &self.progress {
                        ui.add(
                            egui::ProgressBar::new(progress.fraction())
                                .fill(ACCENT)
                                .desired_height(12.),
                        );
                    } else {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 9.),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect_filled(rect, 4., LINE);
                        let elapsed = self
                            .job_started
                            .map(|t| t.elapsed().as_secs_f32())
                            .unwrap_or_default();
                        let width = rect.width() * 0.22;
                        let x = rect.left()
                            + (rect.width() - width) * (elapsed * 1.5).sin().mul_add(0.5, 0.5);
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(x, rect.top()),
                                egui::vec2(width, rect.height()),
                            ),
                            4.,
                            ACCENT,
                        );
                        // Animate independently of the worker's much slower progress reports.
                        // Use a short animation timer only while the bar is visible.
                        if ui.is_rect_visible(rect)
                            && !ctx.input(|i| i.viewport().minimized.unwrap_or(false))
                        {
                            ctx.request_repaint_after(Duration::from_secs_f64(1. / 60.));
                        }
                    }
                    let elapsed =
                        format_duration(self.job_started.map(|t| t.elapsed()).unwrap_or_default());
                    let detail = self
                        .progress
                        .as_ref()
                        .map(TransferProgress::description)
                        .unwrap_or_else(|| {
                            if self.last_activity.is_empty() {
                                "Working…".into()
                            } else {
                                self.last_activity.clone()
                            }
                        });
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("{detail} · elapsed {elapsed}"))
                                .size(11.)
                                .color(MUTED),
                        )
                        .truncate(),
                    );
                    // Measured progress repaints on worker events; only the elapsed clock
                    // needs a timer when there is no animation to draw.
                    ctx.request_repaint_after(Duration::from_secs(1));
                }
            });
    }

    fn play(&mut self, ui: &mut egui::Ui) {
        let short = ui.ctx().content_rect().height() < 560.;
        if short {
            ui.label(RichText::new("Play").size(24.).strong());
        } else {
            title(ui, "Play", "Your next adventure starts here.");
        }
        ui.add_space(if short { 10. } else { 24. });
        if ui.ctx().content_rect().height() >= 600. {
            landscape(ui);
            ui.add_space(20.);
        }
        let width = ui.available_width();
        if width >= 720. {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 16.;
                self.game_card(ui, (width - 16.) * 0.57);
                self.account_card(ui, (width - 16.) * 0.43);
            });
        } else {
            self.game_card(ui, width);
            ui.add_space(16.);
            self.account_card(ui, width);
        }
        ui.add_space(20.);
        Frame::new()
            .inner_margin(Margin::symmetric(4, 0))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    dot(ui, if self.has_update() { ACCENT } else { GREEN });
                    ui.label(
                        RichText::new(if self.has_update() {
                            "A new Horizon build is available."
                        } else {
                            "Discover the latest Horizon builds."
                        })
                        .size(12.)
                        .color(MUTED),
                    );
                    if ui.link("View updates").clicked() {
                        self.page = Page::Updates;
                    }
                });
            });
        ui.add_space(20.);
        ui.label(
            RichText::new("Unofficial launcher. Not affiliated with Mojang or Microsoft.")
                .size(10.)
                .color(MUTED),
        );
    }

    fn game_card(&mut self, ui: &mut egui::Ui, width: f32) {
        card().show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width((width - card().total_margin().sum().x).max(0.));
                if ui.ctx().content_rect().height() >= 560. {
                    ui.set_min_height(250.);
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Horizon").size(21.).strong());
                    badge(ui, self.settings.channel.label(), ACCENT);
                    if self.snapshot.native_build {
                        badge(ui, "MODDED", GREEN);
                    }
                });
                if ui.ctx().content_rect().height() < 560. {
                    ui.add_space(4.);
                    self.play_button(ui);
                    let version = self
                        .snapshot
                        .selected_version
                        .as_deref()
                        .unwrap_or("Not installed");
                    ui.add(
                        egui::Label::new(RichText::new(version).size(11.).color(MUTED)).truncate(),
                    )
                    .on_hover_text(version);
                    return;
                }
                ui.label(
                    RichText::new("Minecraft, powered by Rust.")
                        .size(13.)
                        .color(MUTED),
                );
                ui.add_space(12.);
                ui.separator();
                ui.add_space(10.);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Installed build").size(12.).color(MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let version = self
                            .snapshot
                            .selected_version
                            .as_deref()
                            .unwrap_or("Not installed");
                        ui.add(egui::Label::new(RichText::new(version).size(12.)).truncate())
                            .on_hover_text(version);
                    });
                });
                ui.add_space(12.);
                self.play_button(ui);
                ui.add_space(4.);
                ui.label(
                    RichText::new(if !self.snapshot.resources_ready {
                        "Downloads the game and resources. No account required."
                    } else if self.settings.offline_mode || !self.snapshot.owner {
                        "Ready for single-player and offline servers."
                    } else if self.settings.update_before_play {
                        "Checks for updates before playing."
                    } else {
                        "Using your locally installed game build"
                    })
                    .size(11.)
                    .color(MUTED),
                );
                ui.add_space(4.);
                if ui
                    .link(RichText::new("Open game folder ↗").size(12.))
                    .clicked()
                {
                    self.open(self.game_folder());
                }
            });
        });
    }

    fn play_button(&mut self, ui: &mut egui::Ui) {
        let (label, action) = if !self.engine_ready() || !self.snapshot.resources_ready {
            ("Download & set up Horizon", Action::Setup)
        } else if self.settings.offline_mode || !self.snapshot.owner {
            ("Play offline", Action::Play)
        } else {
            (
                if self.receipt.mods_dirty {
                    "Build & play"
                } else {
                    "Play Horizon"
                },
                Action::Play,
            )
        };
        if primary(ui, label, !self.busy).clicked() {
            self.start(action);
        }
    }

    fn save_account_settings(&mut self, settings: Settings) -> bool {
        match self.paths.save_settings(&settings) {
            Ok(()) => {
                self.settings = settings;
                self.pending_check = false;
                true
            }
            Err(error) => {
                self.error = Some(format!("Could not save accounts: {error:#}"));
                false
            }
        }
    }

    fn choose_account(&mut self, choice: AccountChoice) {
        if self.busy {
            return;
        }
        let mut settings = self.settings.clone();
        match choice {
            AccountChoice::Guest | AccountChoice::Offline(_) => {
                let name = match &choice {
                    AccountChoice::Offline(name) => Some(name.as_str()),
                    _ => None,
                };
                match settings.select_offline_account(name) {
                    Ok(()) => {
                        self.save_account_settings(settings);
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            AccountChoice::Microsoft(uuid) => {
                let selected = self
                    .snapshot
                    .accounts
                    .iter()
                    .find(|account| account.selected)
                    .or(self.snapshot.accounts.first());
                if selected.is_some_and(|account| account.uuid == uuid) {
                    settings.offline_mode = false;
                    self.save_account_settings(settings);
                } else {
                    self.start(Action::SelectAccount(uuid.clone()));
                    if self.busy {
                        self.pending_account = Some(uuid);
                    }
                }
            }
        }
    }

    fn account_menu(&mut self, ui: &mut egui::Ui, id: &str) {
        let active = if self.settings.offline_mode || !self.snapshot.owner {
            self.settings
                .selected_offline_account
                .clone()
                .map(AccountChoice::Offline)
                .unwrap_or(AccountChoice::Guest)
        } else {
            self.snapshot
                .accounts
                .iter()
                .find(|account| account.selected)
                .or(self.snapshot.accounts.first())
                .map(|account| AccountChoice::Microsoft(account.uuid.clone()))
                .unwrap_or(AccountChoice::Guest)
        };
        let label = match &active {
            AccountChoice::Guest => "Guest · no account".into(),
            AccountChoice::Offline(name) => format!("{name} · offline"),
            AccountChoice::Microsoft(uuid) => self
                .snapshot
                .accounts
                .iter()
                .find(|account| &account.uuid == uuid)
                .map(|account| format!("{} · Microsoft", account.name))
                .unwrap_or_default(),
        };
        let mut choice = None;
        ui.add_enabled_ui(!self.busy, |ui| {
            egui::ComboBox::from_id_salt(id)
                .selected_text(label)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(active == AccountChoice::Guest, "Guest · no account")
                        .clicked()
                    {
                        choice = Some(AccountChoice::Guest);
                    }
                    for name in &self.settings.offline_accounts {
                        let account = AccountChoice::Offline(name.clone());
                        if ui
                            .selectable_label(active == account, format!("{name} · offline"))
                            .clicked()
                        {
                            choice = Some(account);
                        }
                    }
                    if !self.snapshot.accounts.is_empty() {
                        ui.separator();
                    }
                    for account in &self.snapshot.accounts {
                        let item = AccountChoice::Microsoft(account.uuid.clone());
                        if ui
                            .selectable_label(
                                active == item,
                                format!("{} · Microsoft", account.name),
                            )
                            .clicked()
                        {
                            choice = Some(item);
                        }
                    }
                });
            ui.horizontal_wrapped(|ui| {
                if ui.button("New offline profile").clicked() {
                    self.new_offline_account = true;
                    self.offline_account_error = None;
                }
                if ui.button("Microsoft sign-in").clicked() {
                    let mut settings = self.settings.clone();
                    settings.offline_mode = false;
                    if self.save_account_settings(settings) {
                        self.start(Action::SignIn);
                    }
                }
            });
        });
        if let Some(choice) = choice {
            self.choose_account(choice);
        }
    }

    fn account_card(&mut self, ui: &mut egui::Ui, width: f32) {
        card().show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width((width - card().total_margin().sum().x).max(0.));
                if ui.ctx().content_rect().height() >= 560. {
                    ui.set_min_height(250.);
                }
                ui.label(RichText::new("Player profile").size(16.).strong());
                ui.add_space(8.);
                if self.settings.offline_mode || !self.snapshot.owner {
                    let name = self
                        .settings
                        .selected_offline_account
                        .as_deref()
                        .unwrap_or("Guest");
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::splat(38.), egui::Sense::hover());
                        ui.painter()
                            .rect_filled(rect, 8., Color32::from_rgb(45, 57, 49));
                        cube(ui.painter(), rect.center() - egui::vec2(0., 5.), 11., GREEN);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(name).size(16.).strong());
                            ui.label(
                                RichText::new("Local player · no sign-in needed")
                                    .size(11.)
                                    .color(MUTED),
                            );
                        });
                    });
                } else if let Some(account) = self
                    .snapshot
                    .accounts
                    .iter()
                    .find(|a| a.selected)
                    .or(self.snapshot.accounts.first())
                    .cloned()
                {
                    ui.horizontal(|ui| {
                        self.profile_avatar(ui, &account, 40.);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&account.name).size(17.).strong());
                            ui.label(RichText::new("Microsoft account").size(11.).color(GREEN));
                        });
                    });
                    if ui
                        .add_enabled(
                            !self.settings.offline_mode,
                            egui::Button::new("Change skin"),
                        )
                        .clicked()
                    {
                        self.edit_skin(account);
                    }
                }
                ui.add_space(10.);
                self.account_menu(ui, "play_account");
                if ui
                    .add_enabled(
                        !self.busy,
                        egui::Checkbox::new(&mut self.settings.offline_mode, "Offline mode"),
                    )
                    .on_hover_text(
                        "Use installed resources without sign-in, downloads, or update checks.",
                    )
                    .changed()
                {
                    self.pending_check = false;
                    self.save();
                }
                ui.add_space(8.);
                ui.separator();
                ui.add_space(4.);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("{} native mods", self.snapshot.mods.len()))
                            .size(12.)
                            .color(MUTED),
                    );
                    if ui.link("Manage mods").clicked() {
                        self.page = Page::Mods;
                    }
                });
            });
        });
    }

    fn mods(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Mods",
            "Make your world your own with native Horizon mods.",
        );
        ui.add_space(20.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Add a mod from GitHub").size(16.).strong());
            ui.add_space(12.);
            ui.add(egui::TextEdit::singleline(&mut self.mod_url).hint_text("https://github.com/owner/horizon-mod").desired_width(f32::INFINITY).margin(egui::vec2(12., 11.)));
            ui.add_space(8.);
            ui.label(RichText::new("Paste a repository, branch, commit, or release URL. Java .jar mods are not supported.").size(12.).color(MUTED));
            ui.add_space(12.);
            ui.checkbox(&mut self.trust_mod, "I trust this source to run native code on my computer.");
            ui.add_space(10.);
            let valid = github::validate_mod_url(&self.mod_url);
            if !self.mod_url.trim().is_empty() && let Err(error) = &valid {
                ui.label(RichText::new(error.to_string()).size(11.).color(ACCENT));
            }
            if ui.add_enabled(!self.busy && self.trust_mod && valid.is_ok(), egui::Button::new(RichText::new("Import & build").strong().color(BG)).fill(ACCENT).min_size(egui::vec2(148., 38.))).clicked() {
                self.trust_mod = false;
                self.start(Action::ImportMod(self.mod_url.trim().to_string()));
            }
        });
        ui.add_space(24.);
        if self.receipt.mods_dirty {
            ui.label(
                RichText::new("Mod changes need a successful build before play. Rebuild to retry.")
                    .color(ACCENT),
            );
        }
        ui.vertical(|ui| {
            ui.label(
                RichText::new(format!("Installed mods ({})", self.snapshot.mods.len()))
                    .size(18.)
                    .strong(),
            );
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        !self.busy && !self.snapshot.mods.is_empty(),
                        egui::Button::new("Rebuild"),
                    )
                    .clicked()
                {
                    self.start(Action::BuildMods);
                }
                if ui
                    .add_enabled(
                        !self.busy && !self.snapshot.mods.is_empty(),
                        egui::Button::new("Check updates"),
                    )
                    .clicked()
                {
                    self.show_logs = true;
                    self.start(Action::CheckModUpdates);
                }
                if ui
                    .add_enabled(
                        !self.busy && !self.snapshot.mods.is_empty(),
                        egui::Button::new("Update & build"),
                    )
                    .clicked()
                {
                    self.confirm = Some(Confirm::UpdateMods);
                }
            });
        });
        ui.add_space(12.);
        if self.snapshot.mods.is_empty() {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(22.);
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(44.), egui::Sense::hover());
                    cube(ui.painter(), rect.center() - egui::vec2(0., 6.), 17., GREEN);
                    ui.add_space(8.);
                    ui.label(RichText::new("No mods installed yet").size(17.).color(TEXT));
                    ui.add_space(5.);
                    ui.label(
                        RichText::new("Add your first mod using a GitHub link above.")
                            .size(12.)
                            .color(MUTED),
                    );
                });
                ui.add_space(22.);
            });
        }
        for item in self.snapshot.mods.clone() {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    let details_width = (ui.available_width() - 160.).max(160.);
                    ui.vertical(|ui| {
                        ui.set_max_width(details_width);
                        ui.label(RichText::new(&item.id).size(16.).strong());
                        ui.label(
                            RichText::new(format!("{} · {}", item.repository, item.version))
                                .size(11.)
                                .color(MUTED),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Remove"))
                            .clicked()
                        {
                            self.confirm = Some(Confirm::Remove(item.id.clone()));
                        }
                        if ui.link("Source ↗").clicked() {
                            self.open(&item.source_url);
                        }
                    });
                });
            });
            ui.add_space(8.);
        }
        ui.add_space(12.);
        ui.label(RichText::new("Incremental builds reuse compiled dependencies. The first native build can take several minutes and several GB of disk space.").size(11.).color(MUTED));
    }

    fn updates(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Updates",
            "Keep your launcher and Horizon game up to date.",
        );
        ui.add_space(22.);
        self.launcher_updates(ui);
        ui.add_space(16.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Release channel").size(16.).strong());
                let before = self.settings.channel;
                egui::ComboBox::from_id_salt("update_channel").selected_text(self.settings.channel.label()).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.channel, Channel::Nightly, "Nightly · newest builds");
                    ui.selectable_value(&mut self.settings.channel, Channel::Stable, "Stable · published releases");
                });
                if before != self.settings.channel {
                    self.catalog = None;
                    self.save();
                    self.pending_check = self.settings.auto_check;
                }
                if ui.add_enabled(!self.busy, egui::Button::new("Check now")).clicked() { self.start(Action::CheckUpdates); }
            });
            ui.add_space(10.);
            ui.label(RichText::new("Nightly follows the newest published Horizon build. Minecraft assets match the version required by that build.").size(12.).color(MUTED));
            ui.add_space(18.);
            ui.separator();
            ui.add_space(14.);
            if let Some(message) = &self.update_unavailable {
                ui.label(RichText::new(message).color(ACCENT));
                ui.add_space(10.);
            }
            if let Some(catalog) = self.catalog.clone().filter(|_| self.catalog_channel == self.settings.channel) {
                ui.horizontal(|ui| {
                    dot(ui, if self.update_unavailable.is_some() || self.has_update() { ACCENT } else { GREEN });
                    ui.label(RichText::new(if self.update_unavailable.is_some() { "Last known builds · update status unavailable" } else if self.has_update() { "Update available" } else { "You're up to date" }).size(16.).strong());
                });
                ui.add_space(10.);
                ui.label(format!("Game: {}", catalog.game.name));
                ui.label(RichText::new(format!("Build published: {}", catalog.game_asset.updated_at.replace('T', " ").trim_end_matches('Z'))).size(12.).color(MUTED));
                ui.label(RichText::new(format!("Engine: {}", catalog.engine.tag_name)).size(12.).color(MUTED));
                ui.add_space(14.);
                if primary(ui, if self.has_update() { "Install latest build  >" } else { "Verify & install  >" }, !self.busy).clicked() { self.start(Action::InstallGame); }
            } else {
                if let Some(version) = &self.snapshot.selected_version {
                    ui.label(format!("Installed: {version}"));
                }
                ui.label(RichText::new("Check for the latest build to see what's new.").color(MUTED));
            }
        });
        if let Some(catalog) = self.catalog.clone() {
            ui.add_space(24.);
            ui.label(RichText::new("Release notes").size(19.).strong());
            ui.add_space(10.);
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                let notes = if catalog.game.body.is_empty() {
                    "No release notes for this build."
                } else {
                    &catalog.game.body
                };
                ui.label(
                    RichText::new(notes.chars().take(16000).collect::<String>())
                        .size(12.)
                        .color(MUTED),
                );
                ui.add_space(12.);
                if ui.link("Read on GitHub >").clicked() {
                    self.open(format!(
                        "{}/releases/tag/{}",
                        github::REPOSITORY,
                        catalog.game.tag_name
                    ));
                }
            });
        }
    }

    fn launcher_updates(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("MCRS Launcher").size(17.).strong());
                badge(ui, concat!("v", env!("CARGO_PKG_VERSION")), MUTED);
            });
            ui.add_space(8.);
            if updater::is_flatpak() {
                ui.label("Update through your system software updater, then close and reopen MCRS. GitHub releases appear there after publication to the signed Flatpak repository.");
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(!self.busy && !self.settings.offline_mode, egui::Button::new("Open software updater")).clicked() {
                        self.open(format!("appstream://{}", updater::FLATPAK_ID));
                    }
                    if ui.button("Copy Flatpak update command").clicked() {
                        ui.ctx().copy_text(updater::FLATPAK_UPDATE_COMMAND.into());
                        self.notice = "Update command copied. Run it in a terminal, then reopen MCRS.".into();
                    }
                });
                ui.label(RichText::new(updater::FLATPAK_UPDATE_COMMAND).monospace().size(11.).color(MUTED));
            } else if let Some(outcome) = self.launcher_update.clone() {
                match outcome {
                    UpdateOutcome::Installed(executable) => {
                        ui.label(RichText::new("Update installed · restart to use it").color(GREEN));
                        if ui.add_enabled(!self.busy, egui::Button::new("Restart launcher")).clicked() {
                            match std::process::Command::new(executable).args(["--page", "updates"]).spawn() {
                                Ok(_) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                                Err(error) => self.error = Some(format!("Could not restart MCRS. Close and reopen it: {error}")),
                            }
                        }
                    }
                    UpdateOutcome::Downloaded { binary, .. } => {
                        ui.label("Verified update downloaded. Close MCRS, then replace its executable with this file.");
                        ui.label(binary.display().to_string());
                        if ui.button("Open update folder").clicked()
                            && let Some(folder) = binary.parent() {
                            self.open(folder);
                        }
                    }
                }
            }
            if let Some(release) = self.launcher_release.clone() {
                let newer = release.newer_than(env!("CARGO_PKG_VERSION")).unwrap_or(false);
                ui.label(RichText::new(if newer { format!("Available: {}", release.tag) } else { format!("Latest release: {} · you're up to date", release.tag) }).color(if newer { ACCENT } else { GREEN }));
                if newer && !updater::is_flatpak() && self.launcher_update.is_none() {
                    if ui.add_enabled(!self.busy && !self.settings.offline_mode && release.asset.is_some(), egui::Button::new("Update MCRS Launcher")).clicked() {
                        self.start(Action::InstallLauncherUpdate(Box::new(release.clone())));
                    }
                    if release.asset.is_none() { ui.label("No native update package is available for this platform. See Releases for downloads."); }
                }
                if !release.notes.is_empty() {
                    ui.collapsing("Launcher release notes", |ui| { ui.label(release.notes.chars().take(8000).collect::<String>()); });
                }
            } else {
                ui.label(RichText::new("Launcher updates are checked only when you request them.").size(11.).color(MUTED));
            }
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!self.busy && !self.settings.offline_mode, egui::Button::new("Check launcher update")).clicked() {
                    self.start(Action::CheckLauncherUpdate);
                }
                if ui.add_enabled(!self.settings.offline_mode, egui::Button::new("Open launcher releases")).clicked() { self.open(updater::RELEASES_URL); }
            });
            if self.settings.offline_mode { ui.label("Turn off Offline mode to check or download launcher updates."); }
        });
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Settings",
            "Your launcher, the way you like it. Changes save automatically.",
        );
        ui.add_space(20.);
        let before = serde_json::to_string(&self.settings).unwrap_or_default();
        let previous_update_checks = (
            self.settings.auto_check,
            self.settings.update_check_interval,
        );
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("General").size(17.).strong());
            ui.add_space(12.);
            ui.add_enabled(
                !self.busy,
                egui::Checkbox::new(&mut self.settings.offline_mode, "Offline mode"),
            );
            ui.label(
                RichText::new("Play with installed resources, without sign-in or update checks.")
                    .size(12.)
                    .color(MUTED),
            );
            ui.add_space(6.);
            ui.checkbox(
                &mut self.settings.minimize_on_play,
                "Minimize the launcher while playing",
            );
        });
        ui.add_space(16.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Game updates").size(17.).strong());
            ui.add_space(12.);
            ui.add_enabled_ui(!self.settings.offline_mode, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Automatic update checks");
                    egui::ComboBox::from_id_salt("automatic_update_checks")
                        .selected_text(if self.settings.auto_check { self.settings.update_check_interval.label() } else { "Never" })
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(!self.settings.auto_check, "Never").clicked() {
                                self.settings.auto_check = false;
                                self.settings.update_before_play = false;
                            }
                            for interval in [UpdateCheckInterval::StartupOnly, UpdateCheckInterval::Every15Minutes, UpdateCheckInterval::Every30Minutes, UpdateCheckInterval::EveryHour] {
                                if ui.selectable_label(self.settings.auto_check && self.settings.update_check_interval == interval, interval.label()).clicked() {
                                    self.settings.auto_check = true;
                                    self.settings.update_check_interval = interval;
                                }
                            }
                        });
                });
                ui.checkbox(&mut self.settings.update_before_play, "Check for updates and install before playing");
                ui.label(RichText::new("Choose Never for manual updates. Check for new builds on the Updates page.").size(12.).color(MUTED));
            });
            if self.settings.offline_mode {
                ui.label(RichText::new("Turn off Offline mode to change update preferences.").size(12.).color(MUTED));
            }
        });
        ui.add_space(16.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Native mod builds").size(17.).strong());
            ui.add_space(12.);
            ui.horizontal_wrapped(|ui| {
                ui.label("Build profile");
                egui::ComboBox::from_id_salt("build_profile").selected_text(match self.settings.build_profile { BuildProfile::Performance => "Performance", BuildProfile::Release => "Release", BuildProfile::Dev => "Development" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.build_profile, BuildProfile::Performance, "Performance · fast incremental rebuilds");
                    ui.selectable_value(&mut self.settings.build_profile, BuildProfile::Release, "Release · thin LTO, slower builds");
                    ui.selectable_value(&mut self.settings.build_profile, BuildProfile::Dev, "Development · quickest build, slower game");
                });
            });
            ui.add_space(6.);
            ui.label(RichText::new("Performance is recommended for everyday play. Rebuild mods after changing this setting.").size(12.).color(MUTED));
        });
        if before != serde_json::to_string(&self.settings).unwrap_or_default() {
            self.pending_check = false;
            if previous_update_checks
                != (
                    self.settings.auto_check,
                    self.settings.update_check_interval,
                )
            {
                self.last_check = Instant::now();
            }
            self.save();
        }
        ui.add_space(16.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Player profiles").size(17.).strong());
            ui.add_space(10.);
            self.account_menu(ui, "settings_account");
            for name in self.settings.offline_accounts.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{name} · offline"));
                    if self.settings.selected_offline_account.as_deref() == Some(name.as_str()) && (self.settings.offline_mode || !self.snapshot.owner) {
                        badge(ui, "ACTIVE", GREEN);
                    }
                    if ui.add_enabled(!self.busy, egui::Button::new("Remove offline account")).clicked() {
                        let mut settings = self.settings.clone();
                        match settings.remove_offline_account(&name) {
                            Ok(()) => { self.save_account_settings(settings); }
                            Err(error) => self.error = Some(error.to_string()),
                        }
                    }
                });
            }
            ui.label(RichText::new("Offline profiles are local names. Removing one keeps your worlds and game files.").size(11.).color(MUTED));
            for account in self.snapshot.accounts.clone() {
                ui.horizontal_wrapped(|ui| {
                    self.profile_avatar(ui, &account, 28.);
                    ui.label(RichText::new(&account.name).strong());
                    if account.selected { badge(ui, "ACTIVE", GREEN); }
                    ui.horizontal_wrapped(|ui| {
                        if ui.add_enabled(!self.settings.offline_mode, egui::Button::new("Change skin")).clicked() { self.edit_skin(account.clone()); }
                        if ui.add_enabled(!self.busy, egui::Button::new("Sign out")).clicked() { self.start(Action::SignOut(account.uuid.clone())); }
                        if ui.add_enabled(!self.busy, egui::Button::new("Use Microsoft account")).clicked() { self.choose_account(AccountChoice::Microsoft(account.uuid.clone())); }
                    });
                });
                ui.add_space(8.);
            }
            if self.engine_ready() && !self.snapshot.sign_in_configured {
                ui.label(RichText::new("This engine has no Microsoft application ID configured. Use an official release, or set HORIZON_MSA_CLIENT_ID before starting MCRS.").size(11.).color(ACCENT));
            }
            ui.add_space(6.);
            ui.label(RichText::new("Authentication and credentials are managed by Horizon's official engine and your system keychain.").size(11.).color(MUTED));
        });
        ui.add_space(16.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Storage & support").size(17.).strong());
            ui.add_space(10.);
            ui.label(
                RichText::new(self.paths.root.display().to_string())
                    .size(11.)
                    .color(MUTED),
            );
            ui.add_space(10.);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open game folder").clicked() {
                    self.open(self.game_folder());
                }
                if ui.button("Open data folder").clicked() {
                    self.open(self.paths.root.clone());
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Refresh library"))
                    .clicked()
                {
                    self.start(Action::Refresh);
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Restore engine"))
                    .clicked()
                {
                    // Setup repairs a missing/modified binary using the trusted release digest.
                    self.start(Action::RepairEngine);
                }
            });
        });
        ui.add_space(15.);
        ui.label(RichText::new("Native Rust UI · event-driven rendering · bounded logs · no bundled browser runtime").size(11.).color(MUTED));
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.new_offline_account {
            let mut open = true;
            egui::Window::new("Create offline account").open(&mut open).collapsible(false)
                .resizable(true).anchor(egui::Align2::CENTER_CENTER, [0., 0.]).default_width(360.)
                .min_width(260.).max_width((ctx.content_rect().width() - 32.).max(260.))
                .max_height((ctx.content_rect().height() - 32.).max(180.)).vscroll(true)
                .show(ctx, |ui| {
                    ui.label("Choose a local player name. No Microsoft sign-in is needed.");
                    let name = ui.add(egui::TextEdit::singleline(&mut self.offline_account_name).hint_text("Player name").desired_width(f32::INFINITY));
                    if name.changed() { self.offline_account_error = None; }
                    ui.label(RichText::new("3–16 letters, digits, or underscores. Offline profiles play single-player and offline-mode servers.").size(11.).color(MUTED));
                    if let Some(error) = &self.offline_account_error { ui.colored_label(ACCENT, error); }
                    let create = ui.add_enabled(!self.busy, egui::Button::new("Create & select")).clicked()
                        || (name.lost_focus() && ctx.input(|input| input.key_pressed(egui::Key::Enter)) && !self.busy);
                    if create {
                        let mut settings = self.settings.clone();
                        match settings.create_offline_account(&self.offline_account_name) {
                            Ok(()) => {
                                if self.save_account_settings(settings) {
                                    self.new_offline_account = false;
                                    self.offline_account_name.clear();
                                    self.offline_account_error = None;
                                    self.notice = "Offline account created and selected".into();
                                }
                            }
                            Err(error) => self.offline_account_error = Some(error.to_string()),
                        }
                    }
                });
            self.new_offline_account &= open;
        }
        if let Some((url, code)) = self.login.clone() {
            egui::Window::new("Connect your Minecraft account")
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, [0., 0.])
                .default_width(390.)
                .min_width(260.)
                .max_width((ctx.content_rect().width() - 32.).max(260.))
                .max_height((ctx.content_rect().height() - 32.).max(180.))
                .vscroll(true)
                .show(ctx, |ui| {
                    ui.add_space(8.);
                    ui.label("Your browser is ready. Enter this code at Microsoft to sign in:");
                    ui.add_space(16.);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&code).monospace().size(30.).color(ACCENT));
                        if ui.button("Copy code").clicked() {
                            ctx.copy_text(code.clone());
                        }
                    });
                    ui.add_space(14.);
                    if ui.link(&url).clicked() {
                        self.open(&url);
                    }
                    ui.add_space(10.);
                    ui.label(
                        RichText::new("Waiting for Microsoft… This window closes after sign-in.")
                            .size(12.)
                            .color(MUTED),
                    );
                    if ui.button("Cancel sign-in").clicked() {
                        self.worker.cancel();
                    }
                });
        }
        if self.confirm.is_some() {
            egui::Window::new("Native mod changes").collapsible(false).resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, [0., 0.]).default_width(390.)
                .min_width(260.).max_width((ctx.content_rect().width() - 32.).max(260.))
                .max_height((ctx.content_rect().height() - 32.).max(180.)).vscroll(true).show(ctx, |ui| {
                    let removing = matches!(self.confirm, Some(Confirm::Remove(_)));
                    ui.label(if removing { "Remove this mod and rebuild the game with the remaining mods?" }
                        else { "Updating imports new source code from GitHub. Review the changes before trusting them to run on your computer." });
                    ui.add_space(12.);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Cancel").clicked() { self.confirm = None; }
                        if ui.add(egui::Button::new(if removing { "Remove & rebuild" } else { "Trust, update & build" }).fill(ACCENT)).clicked()
                            && let Some(confirm) = self.confirm.take() {
                            self.start(match confirm { Confirm::Remove(id) => Action::RemoveMod(id), Confirm::UpdateMods => Action::UpdateMods });
                        }
                    });
                });
        }
    }
}

impl Launcher {
    fn game_folder(&self) -> std::path::PathBuf {
        if self.snapshot.game_home.as_os_str().is_empty() {
            self.paths.game_home()
        } else {
            self.snapshot.game_home.clone()
        }
    }

    fn profile_avatar(&self, ui: &mut egui::Ui, account: &Account, size: f32) {
        if let Some(head) = self.heads.get(&account.uuid) {
            ui.add(egui::Image::new((head.id(), egui::vec2(size, size))).corner_radius(4.));
        } else {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
            ui.painter().rect_filled(rect, 5., LINE);
            let initial = account
                .name
                .chars()
                .next()
                .unwrap_or('?')
                .to_ascii_uppercase()
                .to_string();
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                initial,
                FontId::proportional(size * 0.5),
                MUTED,
            );
        }
    }

    fn edit_skin(&mut self, account: Account) {
        if self.skin_editor.is_some() {
            return;
        }
        let model = if account
            .skin
            .as_ref()
            .is_some_and(|(_, model)| model == "slim")
        {
            SkinModel::Slim
        } else {
            SkinModel::Classic
        };
        self.skin_editor = Some(SkinEditor {
            account,
            model,
            skin: None,
            preview: None,
            choosing: false,
            uploading: false,
        });
    }

    fn skin_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.skin_editor.take() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Change Minecraft skin")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, [0., 0.])
            .default_width(390.)
            .min_width(260.)
            .max_width((ctx.content_rect().width() - 32.).max(260.))
            .max_height((ctx.content_rect().height() - 32.).max(180.))
            .vscroll(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(preview) = &editor.preview {
                        ui.add(
                            egui::Image::new((preview.id(), egui::vec2(64., 64.)))
                                .corner_radius(5.),
                        );
                    } else {
                        self.profile_avatar(ui, &editor.account, 64.);
                    }
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&editor.account.name).size(18.).strong());
                        ui.label(RichText::new("Minecraft: Java Edition").color(MUTED));
                    });
                });
                ui.add_space(12.);
                ui.label(
                    "Choose a 64×64 or 64×32 PNG skin. Applying it updates this Minecraft account.",
                );
                ui.add_space(8.);
                if ui
                    .add_enabled(
                        !editor.choosing && !editor.uploading,
                        egui::Button::new("Choose skin PNG…"),
                    )
                    .clicked()
                {
                    match self.profiles.send(ProfileCommand::Choose {
                        uuid: editor.account.uuid.clone(),
                    }) {
                        Ok(()) => editor.choosing = true,
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
                if let Some(skin) = &editor.skin {
                    ui.label(RichText::new(&skin.name).color(GREEN));
                }
                if editor.choosing {
                    ui.label("Waiting for file selection…");
                }
                ui.add_enabled_ui(!editor.uploading, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.radio_value(
                            &mut editor.model,
                            SkinModel::Classic,
                            "Classic (wide arms)",
                        );
                        ui.radio_value(&mut editor.model, SkinModel::Slim, "Slim arms");
                    });
                });
                ui.add_space(12.);
                if primary(
                    ui,
                    if editor.uploading {
                        "Applying skin…"
                    } else {
                        "Apply skin"
                    },
                    editor.skin.is_some()
                        && !editor.uploading
                        && !editor.choosing
                        && !self.settings.offline_mode,
                )
                .clicked()
                    && let Some(skin) = editor.skin.clone()
                {
                    match self.profiles.send(ProfileCommand::Upload {
                        account: editor.account.clone(),
                        model: editor.model,
                        skin,
                    }) {
                        Ok(()) => editor.uploading = true,
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
                ui.label(
                    RichText::new("Your new skin appears in new game sessions.")
                        .size(11.)
                        .color(MUTED),
                );
            });
        if open {
            self.skin_editor = Some(editor);
        }
    }
}

impl eframe::App for Launcher {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        for (key, page) in [
            (egui::Key::Num1, Page::Play),
            (egui::Key::Num2, Page::Mods),
            (egui::Key::Num3, Page::Updates),
            (egui::Key::Num4, Page::Settings),
        ] {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, key)) {
                self.page = page;
            }
        }
        self.receive(ctx);
        self.sidebar(ctx);
        self.footer(ctx);
        if self.show_logs {
            egui::TopBottomPanel::bottom("activity")
                .default_height(150.)
                .min_height(70.)
                .max_height((ctx.content_rect().height() * 0.35).max(70.))
                .resizable(true)
                .frame(
                    Frame::new()
                        .fill(Color32::from_rgb(15, 18, 19))
                        .inner_margin(14),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("ACTIVITY").size(10.).color(ACCENT));
                        if ui.small_button("Copy").clicked() {
                            ctx.copy_text(self.logs.iter().cloned().collect::<Vec<_>>().join("\n"));
                        }
                        if ui.small_button("Clear").clicked() {
                            self.logs.clear();
                        }
                    });
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for line in &self.logs {
                                ui.label(RichText::new(line).monospace().size(10.).color(MUTED));
                            }
                        });
                });
        }
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(BG)
                    .inner_margin(if ctx.content_rect().width() < 900. {
                        Margin::symmetric(16, 16)
                    } else {
                        Margin::symmetric(30, 24)
                    }),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt(match self.page {
                        Page::Play => "play",
                        Page::Mods => "mods",
                        Page::Updates => "updates",
                        Page::Settings => "settings",
                    })
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        if let Some(error) = self.error.clone() {
                            Frame::new()
                                .fill(Color32::from_rgb(63, 36, 32))
                                .inner_margin(12)
                                .corner_radius(6)
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.vertical(|ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(error)
                                                    .size(12.)
                                                    .color(Color32::from_rgb(247, 177, 154)),
                                            )
                                            .wrap(),
                                        );
                                        if ui.small_button("Dismiss").clicked() {
                                            self.error = None;
                                        }
                                    });
                                });
                            ui.add_space(12.);
                        }
                        match self.page {
                            Page::Play => self.play(ui),
                            Page::Mods => self.mods(ui),
                            Page::Updates => self.updates(ui),
                            Page::Settings => self.settings(ui),
                        }
                    });
            });
        self.dialogs(ctx);
        self.skin_dialog(ctx);
    }
}

fn configure(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.faint_bg_color = PANEL;
    visuals.selection.bg_fill = Color32::from_rgb(95, 66, 43);
    visuals.selection.stroke = Stroke::new(1_f32, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(36, 43, 41);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(36, 43, 41);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1_f32, LINE);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(48, 57, 51);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(48, 57, 51);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1_f32, MUTED);
    visuals.widgets.active.bg_fill = Color32::from_rgb(58, 67, 58);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(58, 67, 58);
    visuals.widgets.active.bg_stroke = Stroke::new(1_f32, ACCENT);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1_f32, LINE);
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
    ] {
        widget.corner_radius = CornerRadius::same(4);
    }
    ctx.set_visuals(visuals);
    ctx.style_mut(|s| {
        s.wrap_mode = Some(egui::TextWrapMode::Wrap);
        s.spacing.item_spacing = egui::vec2(8., 8.);
        s.spacing.button_padding = egui::vec2(12., 8.);
        s.spacing.interact_size.y = 24.;
        s.spacing.combo_width = 220.;
        s.text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(13.));
        s.text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(12.));
    });
}

fn navigation_item(ui: &mut egui::Ui, page: Page, active: bool) -> egui::Response {
    let response = ui.add_sized(
        [ui.available_width(), 44.],
        egui::Button::new("")
            .fill(if active {
                Color32::from_rgb(49, 43, 36)
            } else {
                Color32::TRANSPARENT
            })
            .stroke(Stroke::NONE)
            .corner_radius(8),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), page.label())
    });
    let color = if active {
        ACCENT
    } else if response.hovered() || response.has_focus() {
        TEXT
    } else {
        MUTED
    };
    let center = response.rect.left_center() + egui::vec2(22., 0.);
    let painter = ui.painter();
    let stroke = Stroke::new(1.5_f32, color);
    match page {
        Page::Play => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    center + egui::vec2(-5., -7.),
                    center + egui::vec2(7., 0.),
                    center + egui::vec2(-5., 7.),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Page::Mods => cube(painter, center - egui::vec2(0., 4.), 8., color),
        Page::Updates => {
            painter.circle_stroke(center, 7., stroke);
            painter.line_segment(
                [center + egui::vec2(0., 4.), center + egui::vec2(0., -4.)],
                stroke,
            );
            painter.line_segment(
                [center + egui::vec2(-3., -1.), center + egui::vec2(0., -4.)],
                stroke,
            );
            painter.line_segment(
                [center + egui::vec2(3., -1.), center + egui::vec2(0., -4.)],
                stroke,
            );
        }
        Page::Settings => {
            for (y, offset) in [(-6., 3.), (0., -3.), (6., 1.)] {
                painter.line_segment(
                    [center + egui::vec2(-8., y), center + egui::vec2(8., y)],
                    stroke,
                );
                painter.circle_filled(center + egui::vec2(offset, y), 2.5, color);
            }
        }
    }
    painter.text(
        response.rect.left_center() + egui::vec2(44., 0.),
        egui::Align2::LEFT_CENTER,
        page.label(),
        FontId::proportional(14.),
        color,
    );
    response
}

fn card() -> Frame {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1_f32, LINE))
        .corner_radius(12)
        .inner_margin(20)
}

fn primary(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).size(14.).strong().color(BG))
            .fill(ACCENT)
            .corner_radius(7)
            .min_size(egui::vec2(ui.available_width(), 46.)),
    )
}

fn title(ui: &mut egui::Ui, text: &str, description: &str) {
    ui.label(RichText::new(text).size(28.).strong());
    ui.label(RichText::new(description).size(13.).color(MUTED));
}

fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.13))
        .corner_radius(5)
        .inner_margin(Margin::symmetric(7, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.).strong().color(color));
        });
}

fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10., 12.), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3., color);
}

fn cube(p: &egui::Painter, center: egui::Pos2, size: f32, color: Color32) {
    let top = center - egui::vec2(0., size * 0.5);
    let right = center + egui::vec2(size, 0.);
    let bottom = center + egui::vec2(0., size * 0.5);
    let left = center - egui::vec2(size, 0.);
    let depth = egui::vec2(0., size);
    p.add(egui::Shape::convex_polygon(
        vec![top, right, bottom, left],
        color,
        Stroke::NONE,
    ));
    p.add(egui::Shape::convex_polygon(
        vec![left, bottom, bottom + depth, left + depth],
        color.gamma_multiply(0.65),
        Stroke::NONE,
    ));
    p.add(egui::Shape::convex_polygon(
        vec![bottom, right, right + depth, bottom + depth],
        color.gamma_multiply(0.82),
        Stroke::NONE,
    ));
}

/// Static vector art is drawn by the native renderer: no images to decode, no animation loop.
fn landscape(ui: &mut egui::Ui) {
    let compact = ui.available_width() < 760.;
    let height = if compact { 150. } else { 174. };
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, CornerRadius::same(12), Color32::from_rgb(33, 49, 42));
    if !compact {
        let origin = rect.right_top() + egui::vec2(-150., 58.);
        p.circle_filled(origin, 31., Color32::from_rgb(211, 170, 110));
        p.circle_stroke(
            origin,
            44.,
            Stroke::new(1_f32, Color32::from_rgb(88, 99, 68)),
        );
        for (y, x, w) in [(38., 330., 56.), (70., 100., 43.), (25., 50., 60.)] {
            p.rect_filled(
                egui::Rect::from_min_size(rect.right_top() + egui::vec2(-x, y), egui::vec2(w, 5.)),
                0.,
                Color32::from_rgb(88, 101, 80),
            );
        }
        // Back-to-front rows of isometric grass and stone blocks.
        for row in 0..9 {
            for col in 0..13 {
                let x = rect.right() - 330. + col as f32 * 26. - row as f32 * 23.;
                let y = rect.top() + 106. + row as f32 * 14.;
                let height = ((col * 7 + row * 11) % 5) as f32 * 7.;
                let water = (4..8).contains(&col) && row < 5;
                let color = if water {
                    Color32::from_rgb(74, 125, 121)
                } else if (col + row) % 3 == 0 {
                    Color32::from_rgb(115, 139, 86)
                } else {
                    Color32::from_rgb(94, 123, 76)
                };
                cube(&p, egui::pos2(x, y - height), 24., color);
                if !water && (col * 3 + row * 5) % 17 == 0 {
                    let trunk = egui::pos2(x, y - height - 21.);
                    cube(&p, trunk, 6., Color32::from_rgb(105, 85, 59));
                    cube(
                        &p,
                        trunk - egui::vec2(0., 22.),
                        19.,
                        Color32::from_rgb(54, 90, 63),
                    );
                    cube(
                        &p,
                        trunk - egui::vec2(0., 39.),
                        14.,
                        Color32::from_rgb(65, 105, 68),
                    );
                }
            }
        }
    }
    // Quiet left side keeps the title readable over the landscape.
    p.rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + 300., rect.bottom())),
        12.,
        Color32::from_rgb(33, 49, 42),
    );
    let x = rect.left() + 27.;
    p.text(
        egui::pos2(x, rect.top() + 22.),
        egui::Align2::LEFT_TOP,
        "POWERED BY HORIZON",
        FontId::proportional(10.),
        GREEN,
    );
    p.text(
        egui::pos2(x, rect.top() + 46.),
        egui::Align2::LEFT_TOP,
        "A familiar world.\nA new horizon.",
        FontId::proportional(if compact { 26. } else { 30. }),
        TEXT,
    );
    p.text(
        egui::pos2(x, rect.bottom() - 26.),
        egui::Align2::LEFT_TOP,
        "Your Minecraft journey, in Rust.",
        FontId::proportional(12.),
        MUTED,
    );
}
